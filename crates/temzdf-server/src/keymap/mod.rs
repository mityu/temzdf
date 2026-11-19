//! Modes and key mappings (see doc/design/script-host.md): key notation, modes with
//! their properties, the matcher for key sequences, counts and the mode
//! history.

pub mod notation;

pub use notation::Stroke;
pub mod modes;

pub use modes::{Cond, Fallback, History, Keepalive, Layers, Mapping, Matcher, ModeDef, Modes, Step};

use temzdf_protocol::script::FuncId;
#[cfg(test)]
use temzdf_protocol::script::ScriptValue;

/// What a mapping runs: a function of the script host. Scripts map
/// functions only (`temzdf action` runs its command directly); the shipped
/// keys are tested through the real script host (session/host_tests.rs).
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Commands of the registry, run in order by the session itself: only
    /// in Rust tests of the matcher and the modes (sessions without a
    /// script host), so the release build has no such mappings.
    #[cfg(test)]
    Actions(Vec<Act>),
    /// A function of the script host (what every script mapping runs).
    Func(FuncId),
}

/// One command of the registry with its arguments (tests' mappings).
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub struct Act {
    pub name: String,
    pub args: Vec<ScriptValue>,
}

#[cfg(test)]
impl Act {
    pub fn new(name: &str, args: Vec<ScriptValue>) -> Self {
        Self { name: name.to_owned(), args }
    }
}
