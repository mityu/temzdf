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

use temzdf_pdfview::keys::Key;
use temzdf_protocol::script::{
    CallId, CommandSpec, FromHost, FuncId, HostOp, ScriptError, ScriptValue, TaskId, ToHost, Token,
};
use termpix_core::input::{KeyCode, KeyEvent, Modifiers, MouseEvent, MouseEventKind};

use super::{Kind, Outcome, PaneId, Session, SessionEvent};
use crate::action::{Action, PaneSpec};
use crate::layout::{Direction, Split};

/// How to start the script host, and the configuration it loads.
#[derive(Clone, Debug)]
pub struct ScriptSettings {
    /// The host program and its arguments (`temzdf script-host`).
    pub command: Vec<String>,
    pub config: PathBuf,
}

/// How long startup (and a reload) waits for the configuration.
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
/// After this, a running script shows how to interrupt it, and frames resume.
const BUSY_NOTICE: Duration = Duration::from_secs(1);
/// Nesting limit for hooks that cause hooks (Vim's is 10, E218).
const MAX_HOOK_DEPTH: usize = 10;

/// Events scripts can hook (`on(name, fn)`).
pub const EVENTS: &[&str] =
    &["SessionReady", "PaneOpened", "PaneClosePre", "PaneClosed", "PaneFocused", "DocumentReloaded"];

/// Input that arrived while a script ran.
enum Held {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
}

/// Work waiting for the host, innermost (top of the stack) last.
enum Frame {
    /// A call sent to the host; waiting for `Done`.
    Call { id: CallId, hook: bool },
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
    first: Option<(PaneSpec, Option<PathBuf>)>,
    frames: Vec<Frame>,
    next_call: CallId,
    tasks: HashSet<TaskId>,
    atomic: Option<Token>,
    hooks: HashMap<String, Vec<Hook>>,
    prefix: HashMap<Key, FuncId>,
    held: VecDeque<Held>,
    /// Events that happened while a script ran, for after it.
    deferred_events: Vec<(String, Vec<ScriptValue>)>,
    busy_since: Option<Instant>,
    notice_shown: bool,
    /// A startup problem to show once there is a pane.
    startup_error: Option<String>,
}

impl Bridge {
    fn busy(&self) -> bool {
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
            Token::Call(id) => self.frames.iter().any(|f| matches!(f, Frame::Call { id: c, .. } if *c == id)),
            Token::Task(t) => self.tasks.contains(&t),
        }
    }

    /// The nesting depth of hooks running now.
    fn hook_depth(&self) -> usize {
        self.frames.iter().filter(|f| matches!(f, Frame::Call { hook: true, .. })).count()
    }
}

/// The commands scripts get (`temzdf.cmd.*`), with their arguments.
fn commands() -> Vec<CommandSpec> {
    [
        ("split", "split(\"right\" | \"down\"): another pane like the focused one"),
        ("new-pane", "newPane(...): as `temzdf action new-pane` (a file, or \"--\" and a command)"),
        ("move-focus", "moveFocus(\"left\" | \"right\" | \"up\" | \"down\")"),
        ("focus-next-pane", "focusNextPane()"),
        ("close-pane", "closePane()"),
        ("detach", "detach(): the operating client detaches"),
        ("message", "message(text): show text in the status bar"),
        ("send-text", "sendText(text): type text into the focused terminal pane"),
        ("pdf", "pdf(command, count?): a page-view command in the focused PDF pane (\"scroll-down\", ...)"),
        ("panes", "panes(): [{ id, kind, focused }]"),
        ("focused-pane", "focusedPane(): the focused pane's id"),
        ("session-name", "sessionName()"),
    ]
    .into_iter()
    .map(|(name, doc)| CommandSpec { name: name.into(), doc: doc.into() })
    .collect()
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
        self.new_pane(&first, cwd.as_deref())?;
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
        self.script.loading = Some(Instant::now() + LOAD_TIMEOUT);
        self.script.busy_since = Some(Instant::now());
        self.script.send(&ToHost::Load { config: s.config.clone(), commands: commands() })
    }

    /// `reload-config`: a new host, every mapping and hook registered again.
    pub(super) fn reload_scripts(&mut self) -> anyhow::Result<Outcome> {
        let Some(s) = self.settings.script.clone() else { anyhow::bail!("no configuration script") };
        let mut out = self.drop_host();
        self.script.hooks.clear();
        self.script.prefix.clear();
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
        if k.code == KeyCode::Char('c') && k.modifiers == Modifiers::CTRL {
            // As Vim's CTRL-C: typeahead is dropped too.
            self.prefix = false;
            self.script.held.clear();
            if !self.script.send(&ToHost::Interrupt) && self.script.loading.is_some() {
                self.finish_loading(Err(ScriptError::new("interrupted")));
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

    /// Run a prefix mapping registered by a script, if `k` has one.
    pub(super) fn prefix_mapping(&mut self, k: &KeyEvent) -> bool {
        let Some(&func) = self.script.prefix.get(&Key::of(k)) else { return false };
        if self.script.host.is_none() {
            return false;
        }
        self.call(func, vec![ScriptValue::Num(self.focus as f64)], false);
        true
    }

    /// Send a call to the host and wait for it on the frame stack.
    fn call(&mut self, func: FuncId, args: Vec<ScriptValue>, hook: bool) {
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

    fn run_deferred(&mut self, op: Deferred) -> Outcome {
        match op {
            Deferred::ClosePane(id) if self.panes.contains_key(&id) => self.close(id),
            Deferred::ClosePane(_) => Outcome::None,
        }
    }

    /// Advance the frame stack until it waits for the host (or is empty),
    /// then replay held input.
    fn pump(&mut self) -> Outcome {
        let mut out = Outcome::None;
        loop {
            match self.script.frames.pop() {
                None => break,
                Some(f @ Frame::Call { .. }) => {
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
        while !self.script.busy()
            && let Some(h) = self.script.held.pop_front()
        {
            let o = match h {
                Held::Key(k) => self.key(&k, now),
                Held::Mouse(m) => self.mouse(&m, now),
                Held::Paste(t) => self.paste(&t),
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
            FromHost::TaskStart(t) => {
                self.script.tasks.insert(t);
                Outcome::None
            }
            FromHost::TaskDone { task, result } => {
                self.script.tasks.remove(&task);
                if let Err(e) = result {
                    self.script_error(&e);
                    return Outcome::Redraw;
                }
                Outcome::None
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
        self.message("the script host exited; scripts are off until reload-config");
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

    /// Open the first pane (startup) once the configuration has loaded.
    fn open_first(&mut self) -> Outcome {
        if let Some((spec, cwd)) = self.script.first.take() {
            if let Err(e) = self.new_pane(&spec, cwd.as_deref()) {
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
            return self.finish_loading(Ok(()));
        }
        if self.script.busy()
            && !self.script.notice_shown
            && self.script.busy_since.is_some_and(|t| now >= t + BUSY_NOTICE)
        {
            self.script.notice_shown = true;
            self.message("running a script; <C-c> interrupts it");
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
            HostOp::PrefixMap { key, func } => match Key::parse(&key) {
                Ok(k) => {
                    self.script.prefix.insert(k, func);
                    (Ok(ScriptValue::Null), Outcome::None)
                }
                Err(e) => (Err(format!("{e:#}")), Outcome::None),
            },
            HostOp::Command { name, args } => match self.command(&name, &args) {
                Ok((v, o)) => (Ok(v), o),
                Err(e) => (Err(e), Outcome::None),
            },
        }
    }

    /// The command registry ([`commands`]).
    fn command(&mut self, name: &str, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        let text = |i: usize| args.get(i).and_then(ScriptValue::as_str).map(str::to_owned);
        let now = Instant::now();
        let ok = |o: Outcome| Ok((ScriptValue::Null, o));
        let act = |s: &mut Self, a: Action| {
            s.action_inner(a, now).map(|o| (ScriptValue::Null, o)).map_err(|e| format!("{e:#}"))
        };
        match name {
            "split" => {
                let split = match text(0).as_deref() {
                    Some("right") | None => Split::Horizontal,
                    Some("down") => Split::Vertical,
                    Some(d) => return Err(format!("split: {d:?} is not right or down")),
                };
                act(self, Action::Split(split))
            }
            "new-pane" => {
                let mut words = vec!["new-pane".to_owned()];
                words.extend(args.iter().filter_map(|a| a.as_str().map(str::to_owned)));
                let a = Action::parse(&words).map_err(|e| format!("{e:#}"))?;
                act(self, a)
            }
            "move-focus" => {
                let dir = match text(0).as_deref() {
                    Some("left") => Direction::Left,
                    Some("right") => Direction::Right,
                    Some("up") => Direction::Up,
                    Some("down") => Direction::Down,
                    other => return Err(format!("moveFocus: {other:?} is not left, right, up or down")),
                };
                act(self, Action::MoveFocus(dir))
            }
            "focus-next-pane" => act(self, Action::FocusNextPane),
            "close-pane" => ok(self.request_close(self.focus)),
            "detach" => ok(Outcome::Detach),
            "message" => {
                self.message(&text(0).unwrap_or_default());
                ok(Outcome::Redraw)
            }
            "send-text" => match self.panes.get_mut(&self.focus).map(|e| &mut e.kind) {
                Some(Kind::Term(t)) => ok(Outcome::None.or(t.send_text(&text(0).unwrap_or_default()))),
                _ => Err("sendText: the focused pane is not a terminal".into()),
            },
            "pdf" => {
                let name = text(0).ok_or("pdf: which command?")?;
                let cmd = pdf_command(&name)?;
                let count = args.get(1).and_then(ScriptValue::as_num).map(|n| n.max(0.0) as usize);
                let Some(pane) = self.panes.get_mut(&self.focus).and_then(super::PaneEntry::pdf) else {
                    return Err("pdf: the focused pane is not a PDF".into());
                };
                match pane.command(cmd, count, now) {
                    temzdf_pdfview::pane::Action::Quit => ok(self.request_close(self.focus)),
                    temzdf_pdfview::pane::Action::Redraw => ok(Outcome::Redraw),
                    temzdf_pdfview::pane::Action::None => ok(Outcome::None),
                }
            }
            "panes" => {
                let mut ids: Vec<_> = self.panes.keys().copied().collect();
                ids.sort();
                let list = ids
                    .into_iter()
                    .map(|id| {
                        let kind = match self.panes[&id].kind {
                            Kind::Pdf { .. } => "pdf",
                            Kind::Term(_) => "terminal",
                        };
                        ScriptValue::map([
                            ("id", ScriptValue::Num(id as f64)),
                            ("kind", ScriptValue::Str(kind.into())),
                            ("focused", ScriptValue::Bool(id == self.focus)),
                        ])
                    })
                    .collect();
                Ok((ScriptValue::List(list), Outcome::None))
            }
            "focused-pane" => Ok((
                if self.panes.contains_key(&self.focus) {
                    ScriptValue::Num(self.focus as f64)
                } else {
                    ScriptValue::Null
                },
                Outcome::None,
            )),
            "session-name" => Ok((ScriptValue::Str(self.name.clone()), Outcome::None)),
            other => Err(format!("no command {other:?}")),
        }
    }
}

/// A page-view command by its kebab-case name (`scroll-down`).
fn pdf_command(name: &str) -> Result<temzdf_pdfview::keys::Command, String> {
    use serde::Deserialize;
    use serde::de::IntoDeserializer;
    let snake = name.replace('-', "_");
    let d: serde::de::value::StrDeserializer<'_, serde::de::value::Error> = snake.as_str().into_deserializer();
    temzdf_pdfview::keys::Command::deserialize(d).map_err(|_| format!("pdf: no command {name:?}"))
}
