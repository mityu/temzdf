//! Modes and key mappings (see doc/script-host.md): key notation, modes with
//! their properties, the matcher for key sequences, counts and the mode
//! history.

pub mod notation;

pub use notation::Stroke;
pub mod modes;

pub use modes::{Entry, History, Matcher, ModeDef, Modes, Step, Unmapped};
