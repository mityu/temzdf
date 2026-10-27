//! The session's side of scripting: the script host process
//! (`temzdf script-host`, see `temzdf-script`), and the work it does for the
//! session. See doc/script-host.md.
//!
//! Everything is synchronous from the user's point of view: while the host
//! runs a mapped or hook function (or an `atomic` section, or loads the
//! configuration), user input is held and replayed afterwards, and frames
//! wait (for up to a second, then a notice says how to interrupt). Background
//! work (documents, terminals) goes on.
//!
//! The work is a stack of [`Frame`]s: waiting for a call to finish, hooks
//! still to run, an operation deferred until its "before" hooks ran, a reply
//! to send once the hooks of its command ran. Hooks thus run nested inside
//! the command that caused them, as Vim's autocmds do.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use temzdf_protocol::script::{CallId, FromHost, FuncId, HostOp, ScriptError, ScriptValue, TaskId, ToHost, Token};
use termpix_core::input::{KeyCode, KeyEvent, Modifiers, MouseEvent, MouseEventKind};

use super::{Outcome, PaneId, Session, SessionEvent};
use crate::action::PaneSpec;

/// How to start the script host, and the configuration it loads.
#[derive(Clone, Debug)]
pub struct ScriptSettings {
    /// The host program and its arguments (`temzdf script-host`).
    pub command: Vec<String>,
    /// The user's configuration (`None` with `--clean`: the shipped defaults only).
    pub config: Option<PathBuf>,
}

/// How long startup (and a reload) waits for the configuration.
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
/// After this, a running script shows how to interrupt it, and frames resume.
const BUSY_NOTICE: Duration = Duration::from_secs(1);
/// Nesting limit for hooks that cause hooks (Vim's is 10, E218).
const MAX_HOOK_DEPTH: usize = 10;

/// Events scripts can hook (`on(name, fn)`).
pub const EVENTS: &[&str] = &[
    "SessionReady",
    "PaneOpened",
    "PaneClosePre",
    "PaneClosed",
    "PaneFocused",
    "TabEntered",
    "TabClosed",
    "DocumentReloaded",
    "ModeChanged",
    "BufferCreated",
    "BufferDeleted",
];

/// Input that arrived while a script ran.
enum Held {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Text(String),
}

/// Work waiting for the host, innermost (top of the stack) last.
enum Frame {
    /// A call sent to the host; waiting for `Done`.
    Call { id: CallId, hook: bool },
    /// Conditions sent to the host for the matcher; waiting for `Values`.
    Eval { id: CallId, funcs: Vec<FuncId> },
    /// Hooks still to run (one at a time), at this nesting depth.
    Hooks { queue: VecDeque<(FuncId, Vec<ScriptValue>)>, depth: usize },
    /// An operation to perform once the frames above it are done (after its
    /// "before" hooks).
    Op(Deferred),
    /// The answer to a request, sent once the frames above it are done. The
    /// result is filled in by a deferred operation when there is one.
    Reply { req: u64, result: Option<Result<ScriptValue, String>> },
}

enum Deferred {
    ClosePane(PaneId),
    CloseTab(super::tabs::TabId),
}

struct Hook {
    func: FuncId,
    nested: bool,
}

struct Host {
    child: Child,
    stdin: ChildStdin,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = temzdf_protocol::send(&mut self.stdin, &ToHost::Shutdown);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The session's scripting state.
#[derive(Default)]
pub struct Bridge {
    host: Option<Host>,
    /// Which host is current (one is started per load).
    pub(super) generation: u32,
    /// Loading the configuration, until this time.
    loading: Option<Instant>,
    /// The first pane, created once the configuration has loaded.
    pub(super) first: Option<(PaneSpec, Option<PathBuf>)>,
    frames: Vec<Frame>,
    next_call: CallId,
    tasks: HashSet<TaskId>,
    atomic: Option<Token>,
    hooks: HashMap<String, Vec<Hook>>,
    held: VecDeque<Held>,
    /// Events that happened while a script ran, for after it.
    deferred_events: Vec<(String, Vec<ScriptValue>)>,
    busy_since: Option<Instant>,
    notice_shown: bool,
    /// A startup problem to show once there is a pane.
    startup_error: Option<String>,
}

/// `<C-c>`, whatever the lock keys (kitty's keyboard protocol reports Caps
/// Lock and Num Lock as modifiers) and the layout (Ctrl+С on a Russian one
/// is the C key).
pub(super) fn is_interrupt(k: &KeyEvent) -> bool {
    (k.code == KeyCode::Char('c') || k.base == Some('c')) && k.modifiers.without_locks() == Modifiers::CTRL
}

impl Bridge {
    /// The script host is up.
    pub(super) fn running(&self) -> bool {
        self.host.is_some()
    }

    pub(super) fn busy(&self) -> bool {
        self.loading.is_some() || !self.frames.is_empty() || self.atomic.is_some()
    }

    fn send(&mut self, msg: &ToHost) -> bool {
        match &mut self.host {
            Some(h) => temzdf_protocol::send(&mut h.stdin, msg).is_ok(),
            None => false,
        }
    }

    fn token_valid(&self, token: Token) -> bool {
        match token {
            Token::Load => self.loading.is_some(),
            Token::Call(id) => self
                .frames
                .iter()
                .any(|f| matches!(f, Frame::Call { id: c, .. } | Frame::Eval { id: c, .. } if *c == id)),
            Token::Task(t) => self.tasks.contains(&t),
        }
    }

    /// The nesting depth of hooks running now.
    fn hook_depth(&self) -> usize {
        self.frames.iter().filter(|f| matches!(f, Frame::Call { hook: true, .. })).count()
    }
}

impl Session {
    /// Start the session: load the configuration (when there is one), then
    /// open the first pane and announce `SessionReady`.
    pub fn start(&mut self, first: PaneSpec, cwd: Option<PathBuf>) -> anyhow::Result<()> {
        if let Some(s) = self.settings.script.clone()
            && self.spawn_host(&s)
        {
            self.script.first = Some((first, cwd));
            return Ok(());
        }
        self.apply_overrides();
        self.new_pane(&first, cwd.as_deref())?;
        if self.settings.script.is_some() {
            let why = self.script.startup_error.take().unwrap_or_else(|| "the script host could not start".into());
            self.host_down(&why);
        }
        self.event("SessionReady", vec![]);
        self.flush_events();
        Ok(())
    }

    /// Start the host and ask it to load the configuration.
    fn spawn_host(&mut self, s: &ScriptSettings) -> bool {
        let Some((program, args)) = s.command.split_first() else { return false };
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.script.startup_error = Some(format!("cannot start the script host: {e}"));
                return false;
            }
        };
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else { return false };
        let events = self.events.clone();
        self.script.generation += 1;
        let generation = self.script.generation;
        let spawned = std::thread::Builder::new().name("temzdf-script-in".into()).spawn(move || {
            let mut r = BufReader::new(stdout);
            while let Ok(Some(msg)) = temzdf_protocol::recv::<FromHost>(&mut r) {
                if !events(SessionEvent::Script(generation, msg)) {
                    return;
                }
            }
            events(SessionEvent::ScriptExited(generation));
        });
        if spawned.is_err() {
            return false;
        }
        self.script.host = Some(Host { child, stdin });
        self.clear_host_notice();
        self.script.loading = Some(Instant::now() + LOAD_TIMEOUT);
        self.script.busy_since = Some(Instant::now());
        self.script.send(&ToHost::Load { config: s.config.clone(), commands: super::commands::command_specs() })
    }

    /// `reload-config`: a new host, every mapping and hook registered again.
    pub(super) fn reload_scripts(&mut self) -> anyhow::Result<Outcome> {
        let Some(s) = self.settings.script.clone() else { anyhow::bail!("no configuration script") };
        let mut out = self.drop_host();
        self.script.hooks.clear();
        // Mappings start over too (the configuration registers them again).
        self.modes = super::builtin_modes();
        if !self.spawn_host(&s) {
            let e = self.script.startup_error.take().unwrap_or_else(|| "cannot start the script host".into());
            anyhow::bail!(e);
        }
        out = out.or(true);
        Ok(out)
    }

    /// Stop the host; run deferred operations so nothing waits for it.
    fn drop_host(&mut self) -> Outcome {
        self.script.host = None;
        self.script.loading = None;
        self.script.tasks.clear();
        self.script.atomic = None;
        let mut out = Outcome::None;
        for f in std::mem::take(&mut self.script.frames) {
            if let Frame::Op(op) = f {
                out = out.merge(self.run_deferred(op));
            }
        }
        out
    }

    /// Say the host is down and which keys still work.
    fn host_down(&mut self, why: &str) {
        let text = format!(
            "{why}: mappings are off. <C-b>d detaches, <C-b>q ends the session, <C-b>r restarts the script host"
        );
        self.notify(super::notices::Kind::Host, &text);
    }

    /// The host runs something: input is held and frames wait.
    pub fn holds_input(&self) -> bool {
        self.script.busy()
    }

    /// Frames wait while a script runs, for up to [`BUSY_NOTICE`].
    pub fn holds_frames(&self) -> bool {
        self.script.busy() && self.script.busy_since.is_some_and(|t| t.elapsed() < BUSY_NOTICE)
    }

    /// Hold input that arrives while a script runs. `<C-c>` interrupts it
    /// instead. Returns true when the input was taken.
    pub(super) fn hold_key(&mut self, k: &KeyEvent) -> bool {
        if !self.script.busy() {
            return false;
        }
        if is_interrupt(k) {
            // As Vim's CTRL-C: typeahead is dropped too.
            self.matcher.cancel();
            self.script.held.clear();
            if !self.script.send(&ToHost::Interrupt) && self.script.loading.is_some() {
                self.finish_loading(Err(ScriptError::new("interrupted")));
            }
            // A task's atomic section is let go at once: an interrupted one
            // may never say it ended (the host closes it too).
            if let Some(Token::Task(t)) = self.script.atomic {
                self.script.atomic = None;
                self.script.tasks.remove(&t);
                if !self.script.busy() {
                    let _ = self.after_scripts();
                }
            }
        } else {
            self.script.held.push_back(Held::Key(k.clone()));
        }
        true
    }

    pub(super) fn hold_mouse(&mut self, m: &MouseEvent) -> bool {
        if !self.script.busy() {
            return false;
        }
        // Pointer motion is stale by the time it would be replayed.
        if !matches!(m.kind, MouseEventKind::Move) {
            self.script.held.push_back(Held::Mouse(*m));
        }
        true
    }

    pub(super) fn hold_paste(&mut self, text: &str) -> bool {
        if !self.script.busy() {
            return false;
        }
        self.script.held.push_back(Held::Paste(text.to_owned()));
        true
    }

    pub(super) fn hold_text(&mut self, text: &str) -> bool {
        if !self.script.busy() {
            return false;
        }
        self.script.held.push_back(Held::Text(text.to_owned()));
        true
    }

    /// Send a call to the host and wait for it on the frame stack.
    pub(super) fn call(&mut self, func: FuncId, args: Vec<ScriptValue>, hook: bool) {
        let id = self.script.next_call;
        self.script.next_call += 1;
        if self.script.frames.is_empty() && self.script.atomic.is_none() {
            self.script.busy_since = Some(Instant::now());
            self.script.notice_shown = false;
        }
        self.script.frames.push(Frame::Call { id, hook });
        if !self.script.send(&ToHost::Call { id, func, args }) {
            self.script.frames.pop();
        }
    }

    /// Ask the host for conditions of mappings (`when`) or a submode
    /// (`keepalive`), with the context of the key being matched; the
    /// matcher goes on when the values come.
    pub(super) fn eval(&mut self, funcs: Vec<FuncId>, args: Vec<ScriptValue>) {
        let id = self.script.next_call;
        self.script.next_call += 1;
        if self.script.frames.is_empty() && self.script.atomic.is_none() {
            self.script.busy_since = Some(Instant::now());
            self.script.notice_shown = false;
        }
        self.script.frames.push(Frame::Eval { id, funcs: funcs.clone() });
        if !self.script.send(&ToHost::Eval { id, funcs: funcs.clone(), args }) {
            self.script.frames.pop();
            // No host: no condition holds.
            self.matcher.set_conditions(funcs.into_iter().map(|f| (f, false)));
        }
    }

    /// Record an event for hooks.
    pub(super) fn event(&mut self, name: &str, args: Vec<ScriptValue>) {
        if self.script.hooks.contains_key(name) {
            self.script_events.push((name.to_owned(), args));
        }
    }

    /// The hooks for `events`, given the nesting `depth` of the code that
    /// caused them: inside a hook only `nested` hooks run, up to the limit.
    fn hooks_for(&self, events: &[(String, Vec<ScriptValue>)], depth: usize) -> VecDeque<(FuncId, Vec<ScriptValue>)> {
        let mut out = VecDeque::new();
        if depth >= MAX_HOOK_DEPTH {
            if !events.is_empty() {
                eprintln!("temzdf: hooks nested too deep; {} event(s) dropped", events.len());
            }
            return out;
        }
        for (name, args) in events {
            for h in self.script.hooks.get(name).into_iter().flatten() {
                if depth == 0 || h.nested {
                    out.push_back((h.func, args.clone()));
                }
            }
        }
        out
    }

    /// Run hooks for events caused outside scripts (keys, documents), or keep
    /// them for after the script that runs now.
    pub(super) fn flush_events(&mut self) -> Outcome {
        let events = std::mem::take(&mut self.script_events);
        if events.is_empty() {
            return Outcome::None;
        }
        if self.script.busy() {
            self.script.deferred_events.extend(events);
            return Outcome::None;
        }
        let queue = self.hooks_for(&events, 0);
        if queue.is_empty() {
            return Outcome::None;
        }
        self.script.frames.push(Frame::Hooks { queue, depth: 0 });
        self.pump()
    }

    /// Close a pane after its `PaneClosePre` hooks, if any.
    pub(super) fn request_close(&mut self, id: PaneId) -> Outcome {
        let pre = vec![("PaneClosePre".to_owned(), vec![ScriptValue::Num(id as f64)])];
        let queue = self.hooks_for(&pre, self.script.hook_depth());
        if queue.is_empty() || self.script.host.is_none() {
            return self.close(id);
        }
        let depth = self.script.hook_depth();
        self.script.frames.push(Frame::Op(Deferred::ClosePane(id)));
        self.script.frames.push(Frame::Hooks { queue, depth });
        self.pump()
    }

    /// Close tab page `i` (`closeTab`, a middle click on the tab line) once
    /// the `PaneClosePre` hooks of its panes have run, as for closing a pane.
    pub(super) fn request_close_tab(&mut self, i: usize) -> Outcome {
        let pre: Vec<(String, Vec<ScriptValue>)> = self
            .tab_panes(i)
            .into_iter()
            .map(|p| ("PaneClosePre".to_owned(), vec![ScriptValue::Num(p as f64)]))
            .collect();
        let queue = self.hooks_for(&pre, self.script.hook_depth());
        if queue.is_empty() || self.script.host.is_none() {
            return self.remove_tab(i);
        }
        let depth = self.script.hook_depth();
        self.script.frames.push(Frame::Op(Deferred::CloseTab(self.tabs[i].id)));
        self.script.frames.push(Frame::Hooks { queue, depth });
        self.pump()
    }

    fn run_deferred(&mut self, op: Deferred) -> Outcome {
        match op {
            Deferred::ClosePane(id) if self.panes.contains_key(&id) => self.close(id),
            Deferred::ClosePane(_) => Outcome::None,
            Deferred::CloseTab(id) => match self.tab_index(id) {
                Some(i) if self.tabs.len() > 1 => self.remove_tab(i),
                _ => Outcome::None,
            },
        }
    }

    /// Advance the frame stack until it waits for the host (or is empty),
    /// then replay held input.
    fn pump(&mut self) -> Outcome {
        let mut out = Outcome::None;
        loop {
            match self.script.frames.pop() {
                None => break,
                Some(f @ (Frame::Call { .. } | Frame::Eval { .. })) => {
                    self.script.frames.push(f);
                    return out;
                }
                Some(Frame::Hooks { mut queue, depth }) => {
                    if let Some((func, args)) = queue.pop_front() {
                        self.script.frames.push(Frame::Hooks { queue, depth });
                        self.call(func, args, true);
                        if matches!(self.script.frames.last(), Some(Frame::Call { .. })) {
                            return out;
                        }
                    }
                }
                Some(Frame::Op(op)) => {
                    let depth = self.script.hook_depth();
                    let before = std::mem::take(&mut self.script_events);
                    let o = self.run_deferred(op);
                    out = out.merge(o);
                    let events = std::mem::replace(&mut self.script_events, before);
                    if let Some(Frame::Reply { result, .. }) = self.script.frames.last_mut()
                        && result.is_none()
                    {
                        *result = Some(Ok(ScriptValue::Null));
                    }
                    let queue = self.hooks_for(&events, depth);
                    if !queue.is_empty() {
                        self.script.frames.push(Frame::Hooks { queue, depth });
                    }
                }
                Some(Frame::Reply { req, result }) => {
                    let result = result.unwrap_or(Ok(ScriptValue::Null));
                    self.script.send(&ToHost::Reply { req, result });
                }
            }
        }
        if !self.script.busy() {
            out = out.merge(self.after_scripts());
        }
        out
    }

    /// Nothing runs any more: deferred hooks, then held input.
    fn after_scripts(&mut self) -> Outcome {
        self.script.busy_since = None;
        let mut out = Outcome::Redraw;
        if !self.script.deferred_events.is_empty() {
            let events = std::mem::take(&mut self.script.deferred_events);
            let queue = self.hooks_for(&events, 0);
            if !queue.is_empty() {
                self.script.frames.push(Frame::Hooks { queue, depth: 0 });
                return out.merge(self.pump());
            }
        }
        let now = Instant::now();
        // Keys the matcher still holds came before the held ones.
        out = out.merge(self.drive(now));
        while !self.script.busy()
            && let Some(h) = self.script.held.pop_front()
        {
            let o = match h {
                Held::Key(k) => self.key(&k, now),
                Held::Mouse(m) => self.mouse(&m, now),
                Held::Paste(t) => self.paste(&t),
                Held::Text(t) => self.text(&t, now),
            };
            out = out.merge(o);
        }
        out
    }

    /// A message from the script host.
    pub(super) fn handle_script(&mut self, msg: FromHost) -> Outcome {
        match msg {
            FromHost::Ready(result) => self.finish_loading(result),
            FromHost::Request { req, token, op } => self.request(req, token, op),
            FromHost::Done { id, result } => {
                match self.script.frames.last() {
                    Some(Frame::Call { id: top, .. }) if *top == id => {
                        self.script.frames.pop();
                    }
                    _ => {
                        eprintln!("temzdf: the script host finished call {id}, which is not running");
                        return Outcome::None;
                    }
                }
                let mut out = Outcome::None;
                if let Err(e) = result {
                    self.script_error(&e);
                    out = Outcome::Redraw;
                }
                out.merge(self.pump())
            }
            FromHost::Values { id, results } => {
                let funcs = match self.script.frames.last() {
                    Some(Frame::Eval { id: top, funcs }) if *top == id => funcs.clone(),
                    _ => {
                        eprintln!("temzdf: the script host answered conditions {id}, which were not asked");
                        return Outcome::None;
                    }
                };
                self.script.frames.pop();
                let mut out = Outcome::None;
                let mut values = Vec::new();
                for (i, f) in funcs.into_iter().enumerate() {
                    // A condition that failed (or was not answered) is false.
                    let v = match results.get(i) {
                        Some(Ok(v)) => *v,
                        Some(Err(e)) => {
                            self.script_error(e);
                            out = Outcome::Redraw;
                            false
                        }
                        None => false,
                    };
                    values.push((f, v));
                }
                self.matcher.set_conditions(values);
                out.merge(self.pump())
            }
            FromHost::TaskStart(t) => {
                self.script.tasks.insert(t);
                Outcome::None
            }
            FromHost::TaskDone { task, result } => {
                self.script.tasks.remove(&task);
                // A task that ended holds no atomic section any more.
                let released = self.script.atomic == Some(Token::Task(task));
                if released {
                    self.script.atomic = None;
                }
                let mut out = Outcome::None;
                if let Err(e) = result {
                    self.script_error(&e);
                    out = Outcome::Redraw;
                }
                if released { out.merge(self.pump()) } else { out }
            }
            FromHost::AtomicBegin(token) => {
                if self.script.token_valid(token) && self.script.atomic.is_none() {
                    if self.script.frames.is_empty() {
                        self.script.busy_since = Some(Instant::now());
                        self.script.notice_shown = false;
                    }
                    self.script.atomic = Some(token);
                }
                Outcome::None
            }
            FromHost::AtomicEnd(token) => {
                if self.script.atomic == Some(token) {
                    self.script.atomic = None;
                    return self.pump();
                }
                Outcome::None
            }
        }
    }

    /// The host process ended (crashed, or was killed).
    pub(super) fn script_exited(&mut self) -> Outcome {
        if self.script.host.is_none() {
            return Outcome::None;
        }
        let loading = self.script.loading.is_some();
        let mut out = self.drop_host();
        self.host_down("the script host exited");
        if loading {
            out = out.merge(self.open_first());
        }
        out.merge(self.after_scripts())
    }

    fn finish_loading(&mut self, result: Result<(), ScriptError>) -> Outcome {
        if self.script.loading.take().is_none() {
            return Outcome::None;
        }
        if let Err(e) = result {
            let msg = format!("configuration: {e}");
            eprintln!("temzdf: {msg}");
            self.script.startup_error = Some(e.message);
        }
        let out = self.open_first();
        out.merge(self.pump())
    }

    /// Command-line options win over the configuration's.
    pub(super) fn apply_overrides(&mut self) {
        for (name, value) in self.settings.overrides.clone() {
            if let Err(e) = self.set_option(&name, &value) {
                eprintln!("temzdf: {e}");
            }
        }
    }

    /// Open the first pane (startup) once the configuration has loaded.
    pub(super) fn open_first(&mut self) -> Outcome {
        self.apply_overrides();
        if let Some((spec, cwd)) = self.script.first.take() {
            // The configuration may have opened tab pages (`newTab`): the
            // first pane goes into the first one without a pane, which it
            // left behind, and the configuration's tab page stays current.
            let current = self.tab;
            let empty = (0..self.tabs.len()).find(|&i| !self.layouts().any(|(t, _)| t == i));
            if let Some(i) = empty {
                self.enter_tab(i);
            }
            let opened = self.new_pane(&spec, cwd.as_deref());
            if empty.is_some_and(|i| i != current) {
                self.enter_tab(current);
            }
            if let Err(e) = opened {
                if self.layout.is_none() && self.docs.is_empty() {
                    self.error = Some(format!("{e:#}"));
                    return Outcome::Quit;
                }
                self.message(&format!("{e:#}"));
            }
            self.event("SessionReady", vec![]);
        }
        if let Some(e) = self.script.startup_error.take() {
            self.message(&format!("configuration: {e}"));
        }
        Outcome::Redraw.merge(self.flush_events())
    }

    fn script_error(&mut self, e: &ScriptError) {
        eprintln!("temzdf: script error: {e}");
        self.message(&format!("script error: {}", e.message));
    }

    /// Timers: the load timeout and the busy notice. Returns whether to redraw.
    pub(super) fn script_tick(&mut self, now: Instant) -> Outcome {
        if self.script.loading.is_some_and(|d| now >= d) {
            self.script.host = None;
            self.script.startup_error =
                Some(format!("the configuration did not finish loading within {} s", LOAD_TIMEOUT.as_secs()));
            eprintln!("temzdf: {}", self.script.startup_error.as_deref().unwrap_or_default());
            self.script.loading = Some(now);
            let out = self.finish_loading(Ok(()));
            self.host_down("the configuration did not finish loading");
            return out;
        }
        if self.script.busy()
            && !self.script.notice_shown
            && self.script.busy_since.is_some_and(|t| now >= t + BUSY_NOTICE)
        {
            self.script.notice_shown = true;
            self.notify(super::notices::Kind::Info, "running a script; <C-c> interrupts it");
            return Outcome::Redraw;
        }
        Outcome::None
    }

    pub(super) fn script_deadline(&self) -> Option<Instant> {
        let notice = (self.script.busy() && !self.script.notice_shown)
            .then(|| self.script.busy_since.map(|t| t + BUSY_NOTICE))
            .flatten();
        [self.script.loading, notice].into_iter().flatten().min()
    }

    /// A request from the host: do it, run the hooks it caused, answer.
    fn request(&mut self, req: u64, token: Token, op: HostOp) -> Outcome {
        if !self.script.token_valid(token) {
            let e = "the function that issued this command has finished (use the ctx of a dispatched task)";
            self.script.send(&ToHost::Reply { req, result: Err(e.into()) });
            return Outcome::None;
        }
        // Conditions only read: what they ask must not change anything.
        let reading = matches!(token, Token::Call(id)
            if self.script.frames.iter().any(|f| matches!(f, Frame::Eval { id: e, .. } if *e == id)));
        if reading && !read_only(&op) {
            let e = "a condition (when, keepalive) cannot change anything; it may only read";
            self.script.send(&ToHost::Reply { req, result: Err(e.into()) });
            return Outcome::None;
        }
        let depth = match token {
            // Commands of a hook cause only nested hooks.
            Token::Call(_) => self.script.hook_depth(),
            Token::Task(_) | Token::Load => 0,
        };
        // Events from before this request are not its own.
        self.script.deferred_events.append(&mut self.script_events);
        let frames_before = self.script.frames.len();
        self.script.frames.push(Frame::Reply { req, result: None });
        let (result, mut out) = self.run_op(op);
        let events = std::mem::take(&mut self.script_events);
        // A deferred operation (a close waiting for its "before" hooks)
        // fills in the reply later.
        if self.script.frames.len() == frames_before + 1
            && let Some(Frame::Reply { result: r, .. }) = self.script.frames.last_mut()
        {
            *r = Some(result);
        }
        let queue = self.hooks_for(&events, depth);
        if !queue.is_empty() {
            self.script.frames.push(Frame::Hooks { queue, depth });
        }
        out = out.merge(self.pump());
        out
    }

    /// Perform one operation of a script.
    fn run_op(&mut self, op: HostOp) -> (Result<ScriptValue, String>, Outcome) {
        match op {
            HostOp::On { event, func, nested } => {
                if !EVENTS.contains(&event.as_str()) {
                    return (Err(format!("unknown event {event:?}; known: {}", EVENTS.join(", "))), Outcome::None);
                }
                self.script.hooks.entry(event).or_default().push(Hook { func, nested });
                (Ok(ScriptValue::Null), Outcome::None)
            }
            HostOp::DefineMode { name, unmapped, count, transient, keepalive, timeout } => {
                let unmapped = match unmapped.as_deref() {
                    None => None,
                    Some("drop") => Some(crate::keymap::Unmapped::Drop),
                    Some("passthrough") => Some(crate::keymap::Unmapped::Passthrough),
                    Some(other) => {
                        return (
                            Err(format!("defineMode: unmapped {other:?} is not drop or passthrough")),
                            Outcome::None,
                        );
                    }
                };
                if timeout.is_some_and(|t| !t.is_finite() || t < 0.0) {
                    return (Err("defineMode: timeout is a number of milliseconds".into()), Outcome::None);
                }
                let d = self.modes.define(&name);
                if let Some(t) = transient {
                    d.transient = t;
                }
                if let Some(k) = keepalive {
                    use temzdf_protocol::script::Keepalive as K;
                    d.keepalive = match k {
                        K::Mapped => crate::keymap::Keepalive::Mapped,
                        K::Always => crate::keymap::Keepalive::Always,
                        K::Func(f) => crate::keymap::Keepalive::Func(f),
                    };
                }
                if let Some(t) = timeout {
                    d.timeout = (t > 0.0).then(|| Duration::from_secs_f64(t / 1000.0));
                }
                if unmapped.is_some() {
                    d.unmapped = unmapped;
                }
                if count.is_some() {
                    d.count = count;
                }
                (Ok(ScriptValue::Null), Outcome::None)
            }
            HostOp::Map { modes, keys, rhs, nowait, desc, when, namespace, priority } => {
                let m = MapSpec { nowait, desc, when, namespace, priority };
                match self.script_map(&modes, &keys, rhs, m) {
                    Ok(()) => (Ok(ScriptValue::Null), Outcome::None),
                    Err(e) => (Err(e), Outcome::None),
                }
            }
            HostOp::GetMappings { mode, namespace } => {
                let modes = match mode {
                    Some(m) if !self.modes.exists(&m) => {
                        return (Err(format!("getMappings: no mode {m:?}")), Outcome::None);
                    }
                    Some(m) => vec![m],
                    None => self.modes.names(),
                };
                let mut out = Vec::new();
                for mode in modes {
                    for (keys, m) in self.modes.mappings(&mode, namespace.as_deref()) {
                        out.push(mapping_value(&mode, &keys, &m));
                    }
                }
                (Ok(ScriptValue::List(out)), Outcome::None)
            }
            HostOp::Unmap { modes, keys, namespace } => {
                let keys = match crate::keymap::notation::parse(&keys) {
                    Ok(k) => k,
                    Err(e) => return (Err(format!("unmap: {e:#}")), Outcome::None),
                };
                for m in &modes {
                    if !self.modes.exists(m) {
                        return (Err(format!("unmap: no mode {m:?}")), Outcome::None);
                    }
                    self.modes.unmap(m, &keys, namespace.as_deref());
                }
                (Ok(ScriptValue::Null), Outcome::None)
            }
            HostOp::SetOption { name, value } => match self.set_option(&name, &value) {
                Ok(()) => (Ok(ScriptValue::Null), Outcome::None),
                Err(e) => (Err(e), Outcome::None),
            },
            HostOp::GetOption { name } => (self.get_option(&name), Outcome::None),
            HostOp::Command { name, args } => match self.run_command(&name, &args, None, Instant::now()) {
                Ok((v, o)) => (Ok(v), o),
                Err(e) => (Err(e), Outcome::None),
            },
        }
    }
}

impl Session {
    /// `map(modes, keys, rhs, options)` from a script.
    fn script_map(
        &mut self,
        modes: &[String],
        keys: &str,
        rhs: temzdf_protocol::script::MapRhs,
        spec: MapSpec,
    ) -> Result<(), String> {
        use crate::keymap::{Act, Mapping, Target, notation};
        let keys = notation::parse(keys).map_err(|e| format!("map: {e:#}"))?;
        let ctrl_c = notation::parse("<C-c>").expect("notation");
        if keys.first() == ctrl_c.first() {
            return Err("map: <C-c> is reserved (it cancels and interrupts)".into());
        }
        let target = match rhs {
            temzdf_protocol::script::MapRhs::Func(f) => Target::Func(f),
            temzdf_protocol::script::MapRhs::Actions(list) => {
                let known: Vec<String> = super::command_specs().into_iter().map(|c| c.name).collect();
                for (name, _) in &list {
                    if !known.contains(name) {
                        return Err(format!("map: no command {name:?}"));
                    }
                }
                Target::Actions(list.into_iter().map(|(n, a)| Act { name: n, args: a }).collect())
            }
        };
        for m in modes {
            if !self.modes.exists(m) {
                return Err(format!("map: no mode {m:?} (create it with defineMode)"));
            }
            let mut mapping = Mapping::new(target.clone());
            mapping.nowait = spec.nowait;
            mapping.desc = spec.desc.clone();
            mapping.when = spec.when;
            mapping.namespace = spec.namespace.clone();
            mapping.priority = spec.priority;
            self.modes.map(m, keys.clone(), mapping);
        }
        Ok(())
    }

    pub(super) fn set_option(&mut self, name: &str, value: &ScriptValue) -> Result<(), String> {
        let num = || value.as_num().filter(|n| *n >= 0.0).ok_or(format!("o.{name}: expected a number"));
        let ms = || num().map(|n| Duration::from_millis(n as u64));
        let flag = || match value {
            ScriptValue::Bool(b) => Ok(*b),
            _ => Err(format!("o.{name}: expected true or false")),
        };
        let text = || match value {
            ScriptValue::Null => Ok(None),
            ScriptValue::Str(s) => Ok(Some(s.clone())),
            _ => Err(format!("o.{name}: expected a string or null")),
        };
        let o = &mut self.options;
        match name {
            "timeoutlen" => o.timeoutlen = ms()?,
            "splitright" => o.splitright = flag()?,
            "splitbelow" => o.splitbelow = flag()?,
            "modehistory" => o.modehistory = (num()? as usize).max(1),
            "scrollstep" => o.scrollstep = num()?,
            "scrollsmoothness" => o.scrollsmoothness = ms()?,
            "linkopener" => {
                o.linkopener = match value {
                    ScriptValue::Null => None,
                    ScriptValue::List(l) => {
                        let words: Option<Vec<String>> = l.iter().map(|w| w.as_str().map(str::to_owned)).collect();
                        Some(words.filter(|w| !w.is_empty()).ok_or("o.linkopener: expected a list of strings")?)
                    }
                    _ => return Err("o.linkopener: expected a list of strings or null".into()),
                }
            }
            "linkpreview" => o.linkpreview = flag()?,
            "linkpreviewdelay" => o.linkpreviewdelay = ms()?,
            "reloaddebounce" => o.reloaddebounce = ms()?,
            "reloadanchor" => {
                o.reloadanchor_text = match value.as_str() {
                    Some("position") => false,
                    Some("text") => true,
                    _ => return Err("o.reloadanchor: expected \"position\" or \"text\"".into()),
                }
            }
            "synctexeditor" => o.synctexeditor = text()?,
            "remainonexit" => o.remainonexit = flag()?,
            "foreground" => o.foreground = color(value).map_err(|e| format!("o.foreground: {e}"))?,
            "background" => o.background = color(value).map_err(|e| format!("o.background: {e}"))?,
            "palette" => {
                let ScriptValue::List(l) = value else {
                    return Err("o.palette: expected a list of \"#rrggbb\"".into());
                };
                if l.len() > 256 {
                    return Err("o.palette: at most 256 colours".into());
                }
                o.palette = l
                    .iter()
                    .map(|c| color(c)?.ok_or_else(|| "null in a palette".to_owned()))
                    .collect::<Result<_, String>>()
                    .map_err(|e| format!("o.palette: {e}"))?;
            }
            "guifont" => {
                o.guifont = fonts(value).map_err(|e| format!("o.guifont: {e}"))?;
                if o.guifont.is_empty() {
                    return Err("o.guifont: at least one font".into());
                }
                self.configured_fonts = None;
            }
            "guifontwide" => {
                o.guifontwide = fonts(value).map_err(|e| format!("o.guifontwide: {e}"))?;
                self.configured_fonts = None;
            }
            "showtabline" => match value.as_num() {
                Some(n) if [0.0, 1.0, 2.0].contains(&n) => o.showtabline = n as u8,
                _ => return Err("o.showtabline: expected 0, 1 or 2".into()),
            },
            other => return Err(format!("no option {other:?}")),
        }
        self.apply_options();
        if name == "showtabline" {
            self.relayout();
        }
        Ok(())
    }

    pub(super) fn get_option(&self, name: &str) -> Result<ScriptValue, String> {
        let o = &self.options;
        let ms = |d: Duration| ScriptValue::Num(d.as_millis() as f64);
        Ok(match name {
            "timeoutlen" => ms(o.timeoutlen),
            "splitright" => ScriptValue::Bool(o.splitright),
            "splitbelow" => ScriptValue::Bool(o.splitbelow),
            "modehistory" => ScriptValue::Num(o.modehistory as f64),
            "scrollstep" => ScriptValue::Num(o.scrollstep),
            "scrollsmoothness" => ms(o.scrollsmoothness),
            "linkopener" => o.linkopener.as_ref().map_or(ScriptValue::Null, |l| {
                ScriptValue::List(l.iter().map(|w| ScriptValue::Str(w.clone())).collect())
            }),
            "linkpreview" => ScriptValue::Bool(o.linkpreview),
            "linkpreviewdelay" => ms(o.linkpreviewdelay),
            "reloaddebounce" => ms(o.reloaddebounce),
            "reloadanchor" => ScriptValue::Str(if o.reloadanchor_text { "text" } else { "position" }.into()),
            "synctexeditor" => o.synctexeditor.clone().map_or(ScriptValue::Null, ScriptValue::Str),
            "remainonexit" => ScriptValue::Bool(o.remainonexit),
            "showtabline" => ScriptValue::Num(o.showtabline as f64),
            "foreground" => o.foreground.map_or(ScriptValue::Null, |c| ScriptValue::Str(hex(c))),
            "background" => o.background.map_or(ScriptValue::Null, |c| ScriptValue::Str(hex(c))),
            "palette" => ScriptValue::List(o.palette.iter().map(|c| ScriptValue::Str(hex(*c))).collect()),
            "guifont" => font_list(&o.guifont),
            "guifontwide" => font_list(&o.guifontwide),
            other => return Err(format!("no option {other:?}")),
        })
    }
}

/// A font list option: `{ font: string, size?: number }[]`.
fn fonts(v: &ScriptValue) -> Result<Vec<temzdf_scene::FontEntry>, String> {
    const EXPECTED: &str = "expected a list of { font: string, size?: number }";
    let ScriptValue::List(l) = v else { return Err(EXPECTED.into()) };
    l.iter()
        .map(|entry| {
            let ScriptValue::Map(fields) = entry else { return Err(EXPECTED.to_owned()) };
            let field = |k: &str| fields.iter().find(|(name, _)| name == k).map(|(_, v)| v);
            let family = match field("font") {
                Some(ScriptValue::Str(s)) if !s.trim().is_empty() => s.trim().to_owned(),
                _ => return Err(format!("{EXPECTED} (font: a family name)")),
            };
            let size = match field("size") {
                None | Some(ScriptValue::Null) => None,
                Some(ScriptValue::Num(n)) if n.is_finite() && *n > 0.0 => {
                    Some(n.clamp(super::commands::MIN_FONT_POINTS, super::commands::MAX_FONT_POINTS) as f32)
                }
                Some(_) => return Err(format!("size of {family:?}: expected a number of points")),
            };
            if let Some((k, _)) = fields.iter().find(|(k, _)| k != "font" && k != "size") {
                return Err(format!("{family:?}: no field {k:?} (font, size)"));
            }
            Ok(temzdf_scene::FontEntry { family, size })
        })
        .collect()
}

/// A font list as scripts see it.
fn font_list(fonts: &[temzdf_scene::FontEntry]) -> ScriptValue {
    ScriptValue::List(
        fonts
            .iter()
            .map(|f| {
                let mut m = vec![("font".to_owned(), ScriptValue::Str(f.family.clone()))];
                if let Some(size) = f.size {
                    m.push(("size".to_owned(), ScriptValue::Num(size as f64)));
                }
                ScriptValue::Map(m)
            })
            .collect(),
    )
}

/// A colour option: "#rrggbb", or null for none.
fn color(v: &ScriptValue) -> Result<Option<[u8; 3]>, String> {
    match v {
        ScriptValue::Null => Ok(None),
        ScriptValue::Str(s) => {
            let h = s.strip_prefix('#').filter(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()));
            let h = h.ok_or_else(|| format!("{s:?} is not \"#rrggbb\""))?;
            let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).expect("checked");
            Ok(Some([byte(0), byte(2), byte(4)]))
        }
        other => Err(format!("{other:?} is not \"#rrggbb\"")),
    }
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `map`'s options.
struct MapSpec {
    nowait: bool,
    desc: Option<String>,
    when: Option<FuncId>,
    namespace: Option<String>,
    priority: i32,
}

/// Whether `op` only reads (what conditions may do).
fn read_only(op: &HostOp) -> bool {
    match op {
        HostOp::GetOption { .. } | HostOp::GetMappings { .. } => true,
        HostOp::Command { name, .. } => {
            matches!(name.as_str(), "current-mode" | "panes" | "focused-pane" | "session-name")
        }
        _ => false,
    }
}

/// A mapping as `getMappings` gives it: functions as `{ func: id }`, which
/// the host turns back into the functions.
fn mapping_value(
    mode: &str,
    keys: &[crate::keymap::Stroke],
    m: &crate::keymap::Mapping<crate::keymap::Target>,
) -> ScriptValue {
    use crate::keymap::Target;
    let func = |f: FuncId| ScriptValue::map([("func", ScriptValue::Num(f as f64))]);
    let rhs = match &m.target {
        Target::Func(f) => func(*f),
        Target::Actions(acts) => ScriptValue::List(
            acts.iter()
                .map(|a| {
                    ScriptValue::map([
                        ("name", ScriptValue::Str(a.name.clone())),
                        ("args", ScriptValue::List(a.args.clone())),
                    ])
                })
                .collect(),
        ),
    };
    let text = |s: &Option<String>| s.clone().map_or(ScriptValue::Null, ScriptValue::Str);
    ScriptValue::map([
        ("mode", ScriptValue::Str(mode.to_owned())),
        ("lhs", ScriptValue::Str(keys.iter().map(|k| k.notation()).collect())),
        ("rhs", rhs),
        ("when", m.when.map_or(ScriptValue::Null, func)),
        ("nowait", ScriptValue::Bool(m.nowait)),
        ("desc", text(&m.desc)),
        ("namespace", text(&m.namespace)),
        ("priority", ScriptValue::Num(m.priority as f64)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_c_releases_a_tasks_atomic_section_the_host_never_ended() {
        let sink: super::super::EventSink = std::sync::Arc::new(|_| true);
        let settings = super::super::Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let geometry = termpix_core::geom::Geometry {
            cols: 80,
            rows: 24,
            cell: termpix_core::geom::CellSize { width: 10, height: 20 },
        };
        let mut s = Session::new("t".into(), geometry, Default::default(), settings, sink);
        // A task began an atomic section and was cut off: no AtomicEnd.
        s.script.tasks.insert(1);
        s.script.atomic = Some(Token::Task(1));
        assert!(s.holds_input());
        s.key(&KeyEvent::new(KeyCode::Char('c'), Modifiers::CTRL), Instant::now());
        assert!(!s.holds_input(), "<C-c> lets the session go");
        // And a task's end releases its section.
        s.script.tasks.insert(2);
        s.script.atomic = Some(Token::Task(2));
        let _ = s.handle_script(FromHost::TaskDone { task: 2, result: Err(ScriptError::new("interrupted")) });
        assert!(!s.holds_input());
    }

    #[test]
    fn tab_entered_numbers_the_tab_as_it_is_after_a_close() {
        let sink: super::super::EventSink = std::sync::Arc::new(|_| true);
        let settings = super::super::Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let geometry = termpix_core::geom::Geometry {
            cols: 80,
            rows: 24,
            cell: termpix_core::geom::CellSize { width: 10, height: 20 },
        };
        let mut s = Session::new("t".into(), geometry, Default::default(), settings, sink);
        s.script.hooks.insert("TabEntered".into(), vec![Hook { func: 1, nested: false }]);
        s.open_tab();
        s.open_tab();
        for (closed, entered) in [(0, 1), (2, 2)] {
            // Tabs 1, 2, 3: close the first (the second is entered, then
            // numbered 1), or the last (the second is entered).
            s.enter_tab(closed);
            s.script_events.clear();
            let _ = s.remove_tab(closed);
            assert_eq!(s.tab + 1, entered);
            let events: Vec<_> = s.script_events.iter().filter(|(n, _)| n == "TabEntered").collect();
            assert_eq!(events, [&("TabEntered".to_owned(), vec![ScriptValue::Num(entered as f64)])]);
            s.open_tab();
        }
    }

    #[test]
    fn font_sizes_stay_within_reason() {
        let list = |size: f64| {
            let entry = ScriptValue::map([("font", ScriptValue::Str("x".into())), ("size", ScriptValue::Num(size))]);
            fonts(&ScriptValue::List(vec![entry])).unwrap()[0].size
        };
        assert_eq!(list(12.5), Some(12.5));
        assert_eq!(list(100000.0), Some(200.0), "glyph atlases past the GPU's limits draw nothing");
        assert_eq!(list(0.5), Some(4.0));
        let inf = ScriptValue::map([("font", ScriptValue::Str("x".into())), ("size", ScriptValue::Num(f64::INFINITY))]);
        assert!(fonts(&ScriptValue::List(vec![inf])).is_err());
    }

    #[test]
    fn ctrl_c_interrupts_with_lock_keys_on() {
        let key = |m| KeyEvent::new(KeyCode::Char('c'), m);
        assert!(is_interrupt(&key(Modifiers::CTRL)));
        assert!(is_interrupt(&key(Modifiers::CTRL | Modifiers::CAPS_LOCK)));
        assert!(is_interrupt(&key(Modifiers::CTRL | Modifiers::NUM_LOCK | Modifiers::CAPS_LOCK)));
        assert!(!is_interrupt(&key(Modifiers::CTRL | Modifiers::SHIFT)));
        assert!(!is_interrupt(&key(Modifiers::CAPS_LOCK)));
        let russian = KeyEvent { base: Some('c'), ..KeyEvent::new(KeyCode::Char('с'), Modifiers::CTRL) };
        assert!(is_interrupt(&russian), "the C key on another layout");
    }
}
