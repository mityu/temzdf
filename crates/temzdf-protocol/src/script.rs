//! Messages between a session and its script host (`temzdf script-host`), a
//! child process running the user's JavaScript configuration. They travel
//! over the child's stdin and stdout with the framing of [`crate::send`] and
//! [`crate::recv`]. See doc/script-host.md.
//!
//! Calls are synchronous both ways: the server asks the host to run a mapped
//! or hook function ([`ToHost::Call`]) and waits for [`FromHost::Done`]; the
//! function's commands are [`FromHost::Request`]s the host waits on until
//! [`ToHost::Reply`]. While waiting, each side keeps serving the other (a hook
//! can run nested inside a command, as Vim's autocmds do).

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

/// A command the server offers to scripts (`temzdf.cmd.<name>(...)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: String,
    pub doc: String,
    /// Its TypeScript signature after the name (`(text: string): void`).
    pub ts: String,
}

/// A script failed: an exception, a syntax error, or an interrupt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptError {
    pub message: String,
    /// The JavaScript stack, positions in the source as written (type
    /// stripping keeps them).
    pub stack: Option<String>,
}

impl ScriptError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), stack: None }
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToHost {
    /// Evaluate the shipped defaults, then the user's configuration (none
    /// with `--clean`); answered by [`FromHost::Ready`].
    Load {
        config: Option<PathBuf>,
        commands: Vec<CommandSpec>,
    },
    /// Run a registered function; answered by [`FromHost::Done`].
    Call {
        id: CallId,
        func: FuncId,
        args: Vec<ScriptValue>,
    },
    /// Evaluate conditions (`when`, `keepalive`) with `args`; answered by
    /// [`FromHost::Values`]. Their commands carry [`Token::Call`] with `id`
    /// and may only read.
    Eval {
        id: CallId,
        funcs: Vec<FuncId>,
        args: Vec<ScriptValue>,
    },
    /// The answer to a [`FromHost::Request`].
    Reply {
        req: ReqId,
        result: Result<ScriptValue, String>,
    },
    /// Stop the running synchronous code (`<C-c>`).
    Interrupt,
    Shutdown,
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
    /// A [`ToHost::Call`] finished.
    Done {
        id: CallId,
        result: Result<(), ScriptError>,
    },
    /// A [`ToHost::Eval`] finished: each condition's value, in order.
    Values {
        id: CallId,
        results: Vec<Result<bool, ScriptError>>,
    },
    /// `dispatch` started a task; its commands carry [`Token::Task`].
    TaskStart(TaskId),
    /// The task's Promise settled.
    TaskDone {
        task: TaskId,
        result: Result<(), ScriptError>,
    },
    /// `atomic(fn)` began or ended: hold everything else meanwhile.
    AtomicBegin(Token),
    AtomicEnd(Token),
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
    /// Create a mode, or change a mode's properties (`None` keeps them).
    DefineMode {
        name: String,
        unmapped: Option<String>,
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
        rhs: MapRhs,
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

/// How long a transient mode stays (`defineMode`'s `keepalive`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keepalive {
    Mapped,
    Always,
    Func(FuncId),
}

/// What a mapping runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MapRhs {
    Func(FuncId),
    /// Commands by name with their arguments, run by the session itself.
    Actions(Vec<(String, Vec<ScriptValue>)>),
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
        round_trip(&ToHost::Call { id: 7, func: 2, args: vec![v.clone()] });
        round_trip(&ToHost::Load {
            config: Some("config.ts".into()),
            commands: vec![CommandSpec { name: "split".into(), doc: "split right|down".into(), ts: "(): void".into() }],
        });
        round_trip(&FromHost::Request {
            req: 1,
            token: Token::Task(4),
            op: HostOp::Command { name: "split".into(), args: vec![ScriptValue::Str("right".into())] },
        });
        round_trip(&FromHost::Done {
            id: 7,
            result: Err(ScriptError { message: "boom".into(), stack: Some("at f".into()) }),
        });
        round_trip(&ToHost::Reply { req: 1, result: Ok(v) });
        round_trip(&ToHost::Eval { id: 8, funcs: vec![3, 4], args: vec![] });
        round_trip(&FromHost::Values {
            id: 8,
            results: vec![Ok(true), Err(ScriptError { message: "x".into(), stack: None })],
        });
        round_trip(&FromHost::Request {
            req: 2,
            token: Token::Load,
            op: HostOp::Map {
                modes: vec!["normal".into()],
                keys: "<Tab>".into(),
                rhs: MapRhs::Func(1),
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
                unmapped: None,
                count: None,
                transient: Some(true),
                keepalive: Some(Keepalive::Func(5)),
                timeout: Some(1500.0),
            },
        });
    }
}
