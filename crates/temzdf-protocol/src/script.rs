//! Messages between a session and its script host, which runs the user's
//! JavaScript configuration (as a WebAssembly guest in the server). They
//! cross the guest's imports with the framing of [`crate::send`] and
//! [`crate::recv`]. See doc/design/script-host.md.
//!
//! Calls are synchronous both ways: the server asks the host to run a mapped
//! or hook function ([`ToHost::Call`]) and waits for [`FromHost::Done`]; the
//! function's commands are [`FromHost::Request`]s the host waits on until
//! [`ToHost::Reply`]. While waiting, each side keeps serving the other (a hook
//! can run nested inside a command, as Vim's autocmds do). A call the server
//! sends while it is not answering a request (input, a scheduled function)
//! may cross code the host started on its own (a timer, a dispatched task):
//! the host runs it once that code has returned, never inside its commands,
//! so a task's atomic section stays atomic (`nested` tells the two apart).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A JavaScript value as it crosses the process boundary (JSON-like; postcard
/// cannot carry self-describing formats such as `serde_json::Value`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScriptValue {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    List(Vec<ScriptValue>),
    /// An object, its keys in order.
    Map(Vec<(String, ScriptValue)>),
}

impl ScriptValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_num(&self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// The field `key` of an object.
    pub fn get(&self, key: &str) -> Option<&ScriptValue> {
        match self {
            Self::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn map(fields: impl IntoIterator<Item = (impl Into<String>, ScriptValue)>) -> Self {
        Self::Map(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }
}

/// A registered JavaScript function (a mapping's or a hook's), numbered by the host.
pub type FuncId = u32;
/// One run of a mapped or hook function, numbered by the server.
pub type CallId = u64;
/// A task started with `dispatch`, numbered by the host.
pub type TaskId = u64;
/// A request from the host, numbered by the host.
pub type ReqId = u64;

/// What a command belongs to: the synchronous call or the dispatched task it
/// was issued from. Commands of a call or task that has ended are refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Token {
    Call(CallId),
    Task(TaskId),
    /// The configuration being evaluated at startup.
    Load,
}

/// A script failed: an exception, a syntax error, or an interrupt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptError {
    pub message: String,
    /// The JavaScript stack, positions in the source as written (type
    /// stripping keeps them).
    pub stack: Option<String>,
    /// A command failed and nothing caught its error: `message` is the
    /// command's, shown as it would be without a script.
    #[serde(default)]
    pub command: bool,
}

impl ScriptError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), stack: None, command: false }
    }
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(stack) = &self.stack {
            write!(f, "\n{}", stack.trim_end())?;
        }
        Ok(())
    }
}

/// How paths are written: the module loader's and `temzdf/path`'s rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathStyle {
    #[default]
    Posix,
    /// Drive letters, UNC, `\` and `/` both separators.
    Windows,
}

impl PathStyle {
    /// This OS's.
    pub fn native() -> Self {
        if cfg!(windows) { Self::Windows } else { Self::Posix }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToHost {
    /// Evaluate the shipped defaults, then the user's configuration (none
    /// with `--clean`); answered by [`FromHost::Ready`].
    Load {
        config: Option<PathBuf>,
        /// The actions scripts may call (`action.<name>(...)`), by name.
        commands: Vec<String>,
        /// How the session's OS writes paths (the guest is one build for
        /// every OS).
        paths: PathStyle,
    },
    /// Run a registered function; answered by [`FromHost::Done`].
    Call {
        id: CallId,
        func: FuncId,
        args: Vec<ScriptValue>,
        /// Sent while the server answers one of the host's requests (a hook
        /// that request caused): the host runs it inside that request's
        /// wait. Otherwise the host runs it once nothing runs.
        nested: bool,
    },
    /// Evaluate conditions (`when`, `keepalive`) with `args`; answered by
    /// [`FromHost::Values`]. Their commands carry [`Token::Call`] with `id`
    /// and may only read. `nested` as for [`ToHost::Call`].
    Eval {
        id: CallId,
        funcs: Vec<FuncId>,
        args: Vec<ScriptValue>,
        nested: bool,
    },
    /// The answer to a [`FromHost::Request`].
    Reply {
        req: ReqId,
        result: Result<ScriptValue, String>,
    },
    /// Settle the Promise a command answered with (`{ "$promise": n }`):
    /// its value, or why it failed.
    Resolve {
        promise: u64,
        result: Result<ScriptValue, String>,
    },
    /// Run code from `temzdf eval` as a dispatched task: its steps in
    /// order, each `-c` (or the one file) a step, stopping at the first
    /// that fails; answered by [`FromHost::CodeDone`].
    RunCode {
        id: u64,
        code: Code,
    },
    /// Stop the running synchronous code (`<C-c>`).
    Interrupt,
    Shutdown,
}

/// What `temzdf eval` runs (doc/design/script-host.md, "temzdf eval").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Code {
    pub steps: Vec<CodeStep>,
    /// The directory relative imports of steps not named by a path start
    /// from (the caller's).
    pub base: PathBuf,
    /// Answer with the last step's value as JSON (`temzdf eval -p`).
    pub print: bool,
}

/// One `-c` of `temzdf eval`, or its file: JavaScript or TypeScript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodeStep {
    /// Its name in messages and stacks: `-c 1`, the file's absolute path,
    /// `[stdin]`.
    pub name: String,
    pub code: String,
}

/// A line `temzdf eval`'s code printed (`console.log`: `Out`,
/// `console.error`: `Err`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Printed {
    Out(String),
    Err(String),
}

/// How `temzdf eval`'s code ended: what it printed, then its value as
/// JSON (with `print`; `None` without), or the error as text (message and
/// where).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeResult {
    pub printed: Vec<Printed>,
    pub result: Result<Option<String>, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FromHost {
    /// The configuration was evaluated (or failed to).
    Ready(Result<(), ScriptError>),
    /// Do something and answer with [`ToHost::Reply`].
    Request {
        req: ReqId,
        token: Token,
        op: HostOp,
    },
    /// A [`ToHost::Call`] finished, with what the function returned (a
    /// hook's answer, e.g. `{ cancel: true, reason }`; null for nothing).
    Done {
        id: CallId,
        result: Result<ScriptValue, ScriptError>,
    },
    /// A [`ToHost::Eval`] finished: each condition's value, in order.
    Values {
        id: CallId,
        results: Vec<Result<bool, ScriptError>>,
    },
    /// `dispatch` started a task; its commands carry [`Token::Task`].
    TaskStart(TaskId),
    /// The task (`waitResolved`'s) holds the user's input until it is done
    /// (its `TaskDone`); `<C-c>` ([`ToHost::Interrupt`]) gives it up.
    Waiting(TaskId),
    /// The task's Promise settled.
    TaskDone {
        task: TaskId,
        result: Result<(), ScriptError>,
    },
    /// `atomic(fn)` began or ended: hold everything else meanwhile.
    AtomicBegin(Token),
    AtomicEnd(Token),
    /// A [`ToHost::RunCode`] ended (its task's `TaskDone` follows).
    CodeDone {
        id: u64,
        result: CodeResult,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostOp {
    /// Run a command from the server's registry.
    Command {
        name: String,
        args: Vec<ScriptValue>,
    },
    /// Run `func` on `event` (`nested`: also for events caused by hooks).
    On {
        event: String,
        func: FuncId,
        nested: bool,
    },
    /// Stop running `func` on `event` (what `on` returned does it).
    Off {
        event: String,
        func: FuncId,
    },
    /// Call `func` at the safe point: after the current invocation and its
    /// hooks, before the next key (how hooks change things); in the order
    /// they were scheduled.
    Schedule {
        func: FuncId,
    },
    /// Create a mode, or change a mode's properties (`None` keeps them).
    DefineMode {
        name: String,
        /// What keys no mapping takes do.
        fallback: Option<Fallback>,
        count: Option<bool>,
        transient: Option<bool>,
        keepalive: Option<Keepalive>,
        /// Milliseconds; 0: none.
        timeout: Option<f64>,
    },
    /// Map `keys` (Neovim notation) in each of `modes`.
    Map {
        modes: Vec<String>,
        keys: String,
        /// The mapped function.
        func: FuncId,
        nowait: bool,
        desc: Option<String>,
        when: Option<FuncId>,
        namespace: Option<String>,
        priority: i32,
    },
    /// Remove `keys`' mappings: those of `namespace`, or all.
    Unmap {
        modes: Vec<String>,
        keys: String,
        namespace: Option<String>,
    },
    /// Remove the mappings of `mode` (every mode when `None`) and
    /// `namespace` (every namespace when `None`).
    ClearMappings {
        mode: Option<String>,
        namespace: Option<String>,
    },
    /// The mappings of `mode`, `namespace` and `keys` (Neovim notation);
    /// `None` is any.
    GetMappings {
        mode: Option<String>,
        namespace: Option<String>,
        keys: Option<String>,
    },
    SetOption {
        name: String,
        value: ScriptValue,
    },
    GetOption {
        name: String,
    },
}

/// What a mode does with keys no mapping takes (`defineMode`'s `fallback`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fallback {
    Drop,
    /// To the focused pane (typed into a terminal, an input or a text pane).
    Passthrough,
    /// A function given the key (reading one key, leaving a mode).
    Func(FuncId),
}

/// How long a transient mode stays (`defineMode`'s `keepalive`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Keepalive {
    /// While its own mappings take the keys; another key leaves it and is
    /// handled again below.
    #[default]
    Mapped,
    /// Always: keys it does not map are looked up in the modes below.
    Always,
    /// While the function returns true (asked at each sequence's first key).
    Func(FuncId),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(msg: &T) {
        let mut buf = Vec::new();
        crate::send(&mut buf, msg).unwrap();
        assert_eq!(crate::recv::<T>(&mut buf.as_slice()).unwrap().as_ref(), Some(msg));
    }

    #[test]
    fn script_messages_round_trip() {
        let v = ScriptValue::map([
            ("pane", ScriptValue::Num(3.0)),
            ("keys", ScriptValue::List(vec![ScriptValue::Str("C-h".into()), ScriptValue::Null])),
            ("ok", ScriptValue::Bool(true)),
        ]);
        assert_eq!(v.get("pane").and_then(ScriptValue::as_num), Some(3.0));
        round_trip(&ToHost::Call { id: 7, func: 2, args: vec![v.clone()], nested: false });
        round_trip(&ToHost::Load {
            config: Some("config.ts".into()),
            commands: vec!["split".into()],
            paths: PathStyle::Windows,
        });
        round_trip(&FromHost::Request {
            req: 1,
            token: Token::Task(4),
            op: HostOp::Command { name: "split".into(), args: vec![ScriptValue::Str("right".into())] },
        });
        round_trip(&FromHost::Done {
            id: 7,
            result: Err(ScriptError { message: "boom".into(), stack: Some("at f".into()), command: false }),
        });
        round_trip(&ToHost::Reply { req: 1, result: Ok(v) });
        round_trip(&ToHost::Eval { id: 8, funcs: vec![3, 4], args: vec![], nested: true });
        round_trip(&ToHost::RunCode {
            id: 2,
            code: Code {
                steps: vec![CodeStep { name: "-c 1".into(), code: "1 + 1".into() }],
                base: "/tmp".into(),
                print: true,
            },
        });
        round_trip(&FromHost::CodeDone {
            id: 2,
            result: CodeResult {
                printed: vec![Printed::Out("hi".into()), Printed::Err("oh".into())],
                result: Ok(Some("2".into())),
            },
        });
        round_trip(&FromHost::Values {
            id: 8,
            results: vec![Ok(true), Err(ScriptError { message: "x".into(), stack: None, command: true })],
        });
        round_trip(&FromHost::Request {
            req: 2,
            token: Token::Load,
            op: HostOp::Map {
                modes: vec!["normal".into()],
                keys: "<Tab>".into(),
                func: 1,
                nowait: false,
                desc: None,
                when: Some(2),
                namespace: Some("default".into()),
                priority: -1,
            },
        });
        round_trip(&FromHost::Request {
            req: 3,
            token: Token::Load,
            op: HostOp::DefineMode {
                name: "resize".into(),
                fallback: Some(Fallback::Func(3)),
                count: None,
                transient: Some(true),
                keepalive: Some(Keepalive::Func(5)),
                timeout: Some(1500.0),
            },
        });
    }
}
