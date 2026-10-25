//! Modes and key mappings (see doc/script-host.md): key notation, modes with
//! their properties, the matcher for key sequences, counts and the mode
//! history.

pub mod notation;

pub use notation::Stroke;
pub mod modes;

pub use modes::{Entry, History, Matcher, ModeDef, Modes, Step, Unmapped};

use temzdf_protocol::script::{FuncId, ScriptValue};

/// What a mapping runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Commands of the registry, run in order by the session itself (no
    /// script involved, so they cannot race the next key).
    Actions(Vec<Act>),
    /// A function of the script host.
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
