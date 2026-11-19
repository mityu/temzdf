//! Modes and key mappings (see doc/design/script-host.md): key notation, modes with
//! their properties, the matcher for key sequences, counts and the mode
//! history.

pub mod notation;

pub use notation::Stroke;
pub mod modes;

pub use modes::{Cond, Fallback, History, Keepalive, Layers, Mapping, Matcher, ModeDef, Modes, Step};

use temzdf_protocol::script::{FuncId, ScriptValue};

/// What a mapping runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Commands of the registry, run in order by the session itself. Only
    /// Rust tests map these (sessions without a script host); scripts map
    /// functions only, and `temzdf action` runs its command directly.
    Actions(Vec<Act>),
    /// A function of the script host (what every script mapping runs).
    Func(FuncId),
}

/// One command of the registry with its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Act {
    pub name: String,
    pub args: Vec<ScriptValue>,
}

impl Act {
    pub fn new(name: &str, args: Vec<ScriptValue>) -> Self {
        Self { name: name.to_owned(), args }
    }
}
