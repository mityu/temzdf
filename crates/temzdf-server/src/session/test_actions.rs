//! The actions scripts' tests drive the session with (`test.*`, used by
//! `temzdf/test`, crates/temzdf-script/runtime/test/): typing keys and
//! waiting until the session has settled (`test.keys`, `test.settle`),
//! going back to a known state (`test.reset`), and handing the TAP lines
//! out (`test.report`, `test.end`).
//!
//! A wait is answered at the end of [`Session::poll`], never while the
//! request runs: the reply `{ "$promise": n }` must reach the host first.
//! Keys go in one at a time, as typed, each once the session settled
//! after the one before, as `type_keys` in host_tests.rs does; after the
//! last one a prefix waiting for a longer mapping is resolved at once
//! (`Matcher::expire`) unless `flush` is false.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use temzdf_protocol::script::{ScriptValue, TaskId, Token};

use super::{Outcome, Session};
use crate::keymap::{Stroke, notation};

/// The `test.*` actions that wait on the session or change its input:
/// only a script's function or dispatched task may run them, not
/// `temzdf action` and not the configuration while it loads.
pub(super) const WAITING: &[&str] = &["test.keys", "test.settle", "test.reset"];

/// How many error messages are kept for waits to look back on.
const ERRORS_KEPT: usize = 64;

/// How often the event loop looks at a wait while one is under way (the
/// session may settle without an event: a key no script hears).
const WAIT_POLL: Duration = Duration::from_millis(10);

/// What `test.report` and `test.end` hand out: a line of TAP, or the end
/// of the run with its counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestReport {
    Line(String),
    End { failed: u64, total: u64 },
}

/// A `test.keys` or `test.settle` under way.
struct Waiter {
    /// The Promise to settle.
    promise: u64,
    /// The dispatched task waiting (it runs on: not waited for).
    task: Option<TaskId>,
    /// Keys still to type.
    keys: VecDeque<Stroke>,
    remap: bool,
    /// Resolve a pending prefix at the end rather than answer with it.
    flush: bool,
    /// Errors shown after this number are the wait's.
    mark: u64,
}

/// The session's state for `test.*`.
#[derive(Default)]
pub(super) struct TestState {
    /// Waits in the order asked; the first one types its keys.
    waiters: VecDeque<Waiter>,
    /// `Some` once a driver collects the reports (`temzdf test`);
    /// otherwise they go to the log.
    reports: Option<Vec<TestReport>>,
    /// The latest error messages, numbered.
    errors: VecDeque<(u64, String)>,
    last_error: u64,
}

impl TestState {
    /// The host went (its Promises with it).
    pub(super) fn drop_waiters(&mut self) {
        self.waiters.clear();
    }

    /// An error message was shown.
    pub(super) fn note_error(&mut self, text: &str) {
        self.last_error += 1;
        self.errors.push_back((self.last_error, text.to_owned()));
        while self.errors.len() > ERRORS_KEPT {
            self.errors.pop_front();
        }
    }

    /// Keys are being typed for a wait: a pending prefix must not time out
    /// meanwhile.
    pub(super) fn typing(&self) -> bool {
        self.waiters.front().is_some_and(|w| !w.keys.is_empty() || w.flush)
    }

    fn errors_since(&self, mark: u64) -> Vec<ScriptValue> {
        self.errors.iter().filter(|(n, _)| *n > mark).map(|(_, t)| ScriptValue::Str(t.clone())).collect()
    }
}

impl Session {
    /// Collect `test.report` and `test.end` for [`Session::take_test_reports`]
    /// (`temzdf test`) instead of writing them to the log.
    pub fn enable_test_reports(&mut self) {
        self.test.reports.get_or_insert_with(Vec::new);
    }

    /// The reports since the last call (none unless enabled).
    pub fn take_test_reports(&mut self) -> Vec<TestReport> {
        self.test.reports.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// `test.report` and `test.end`, from anywhere (a script, `temzdf
    /// action`); `None` for other names. `test.keys`, `test.settle` and
    /// `test.reset` come through [`Session::test_command`] (with who asks);
    /// here they are refused.
    pub(super) fn test_report_command(
        &mut self,
        name: &str,
        args: &[ScriptValue],
    ) -> Option<Result<(ScriptValue, Outcome), String>> {
        let report = match name {
            "test.report" => {
                TestReport::Line(args.first().and_then(ScriptValue::as_str).unwrap_or_default().to_owned())
            }
            "test.end" => {
                let count = |k: &str| {
                    args.first().and_then(|o| o.get(k)).and_then(ScriptValue::as_num).map_or(0, |n| n.max(0.0) as u64)
                };
                TestReport::End { failed: count("failed"), total: count("total") }
            }
            "test.reporting" => return Some(Ok((ScriptValue::Bool(self.test.reports.is_some()), Outcome::None))),
            n if WAITING.contains(&n) => {
                return Some(Err(format!("{n}: only a script's function or dispatched task can run it")));
            }
            _ => return None,
        };
        match &mut self.test.reports {
            Some(r) => r.push(report),
            None => match report {
                TestReport::Line(l) => eprintln!("temzdf test: {l}"),
                TestReport::End { failed, total } => eprintln!("temzdf test: end: {failed} of {total} failed"),
            },
        }
        Some(Ok((ScriptValue::Null, Outcome::None)))
    }

    /// `test.keys`, `test.settle` and `test.reset` from a script, with the
    /// token of who asks.
    pub(super) fn test_command(
        &mut self,
        name: &str,
        args: &[ScriptValue],
        token: Token,
    ) -> Result<(ScriptValue, Outcome), String> {
        if token == Token::Load {
            return Err(format!("{name}: not while the configuration loads (use it in a dispatched task)"));
        }
        let task = match token {
            Token::Task(t) => Some(t),
            Token::Call(_) | Token::Load => None,
        };
        let opts = |i: usize| args.get(i).filter(|o| matches!(o, ScriptValue::Map(_)));
        let flag = |o: Option<&ScriptValue>, k: &str| match o.and_then(|o| o.get(k)) {
            None | Some(ScriptValue::Null) => Ok(true),
            Some(ScriptValue::Bool(b)) => Ok(*b),
            Some(v) => Err(format!("{name}: {k} is true or false, not {v:?}")),
        };
        let (keys, o) = match name {
            "test.keys" => {
                let keys = args.first().and_then(ScriptValue::as_str).ok_or(format!("{name}: which keys?"))?;
                (notation::parse(keys).map_err(|e| format!("{name}: {e:#}"))?, opts(1))
            }
            "test.settle" => (Vec::new(), opts(0)),
            "test.reset" => return Ok((ScriptValue::Null, self.test_reset())),
            _ => return Err(format!("{}: {name}", super::commands::NO_ACTION)),
        };
        let promise = self.script.new_promise();
        self.test.waiters.push_back(Waiter {
            promise,
            task,
            keys: keys.into(),
            remap: flag(o, "remap")?,
            flush: flag(o, "flush")?,
            mark: self.test.last_error,
        });
        Ok((ScriptValue::map([("$promise", ScriptValue::Num(promise as f64))]), Outcome::None))
    }

    /// `test.reset`: waits and their keys go (their Promises are
    /// rejected), pending keys too, and the focused pane's base mode is
    /// the mode again.
    fn test_reset(&mut self) -> Outcome {
        for w in std::mem::take(&mut self.test.waiters) {
            self.resolve_promise(w.promise, Err("test.reset: the wait was abandoned".into()));
        }
        self.matcher.cancel();
        self.reset_mode(self.focus);
        Outcome::Redraw
    }

    /// The session has settled for `task`'s wait: no script runs or waits
    /// to run (but `task`), no key waits to be matched (nor, `flush`, a
    /// prefix for its next key), the first pane is there, and nothing is
    /// opened, loaded, rendered or searched.
    fn test_settled(&self, task: Option<TaskId>, flush: bool) -> bool {
        self.script.quiet_but(task)
            && !self.matcher.has_input()
            && (!flush || self.matcher.pending().is_empty())
            && !self.is_starting()
            && self.is_idle()
    }

    /// Before the session's timers: a prefix keys are typed into waits for
    /// the next of them however long the session takes to settle.
    pub(super) fn test_hold_timeout(&mut self, now: Instant) {
        if self.test.typing() {
            self.matcher.restart(now);
        }
    }

    /// At the end of [`Session::poll`]: type the next key of the first wait
    /// once the session settled, or answer it after the last one.
    pub(super) fn test_waits(&mut self, now: Instant) -> Outcome {
        let mut out = Outcome::None;
        while let Some(task) = self.test.waiters.front().map(|w| w.task) {
            if !self.test_settled(task, false) {
                break;
            }
            let w = self.test.waiters.front_mut().expect("a wait");
            if let Some(k) = w.keys.pop_front() {
                let remap = w.remap;
                out = out.merge(self.test_type(k, remap, now));
                continue;
            }
            if w.flush && !self.matcher.pending().is_empty() {
                // As if its time ran out, now.
                self.matcher.expire();
                out = out.merge(self.drive(now));
                continue;
            }
            let w = self.test.waiters.pop_front().expect("a wait");
            let pending: String = self.matcher.pending().iter().map(|k| k.notation()).collect();
            let answer = ScriptValue::map([
                ("errors", ScriptValue::List(self.test.errors_since(w.mark))),
                ("pending", ScriptValue::Str(pending)),
                ("mode", ScriptValue::Str(self.history.current().to_owned())),
            ]);
            self.resolve_promise(w.promise, Ok(answer));
        }
        out
    }

    /// Type one key of a wait: as typed (KeyTyped hears `"typed"`), or
    /// (`remap` false) fed without mappings.
    fn test_type(&mut self, k: Stroke, remap: bool, now: Instant) -> Outcome {
        if remap {
            return self.key(&k.event(), now);
        }
        if self.script.hooks_on("KeyTyped") {
            let ev = k.event();
            let text = self.typed_text(&ev);
            self.key_typed(k.notation(), text, "typed");
        }
        // The hooks first; the matcher takes the key once they ran.
        let heard = self.flush_events();
        self.matcher.feed_front(&[k], false, 0);
        heard.merge(self.drive(now))
    }

    /// When the event loop should look at waits again (one is under way).
    pub(super) fn test_deadline(&self) -> Option<Instant> {
        (!self.test.waiters.is_empty()).then(|| Instant::now() + WAIT_POLL)
    }
}
