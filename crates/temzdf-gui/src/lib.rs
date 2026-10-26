//! temzdf's GUI client: a window that attaches to a session as the
//! terminal client does (doc/gui-client.md). The scene becomes a draw list
//! ([`drawlist`]) that the GPU stage executes.

pub mod drawlist;
pub mod fonts;
pub mod palette;
pub mod render;
