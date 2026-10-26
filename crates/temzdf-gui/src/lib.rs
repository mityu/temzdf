//! temzdf's GUI client: a window that attaches to a session as the
//! terminal client does (doc/gui-client.md). The scene becomes a draw list
//! ([`drawlist`]) that the GPU stage ([`render`]) executes; [`app`] is the
//! window, [`input`] turns its events into the session's.

pub mod app;
pub mod cli;
pub mod connection;
pub mod drawlist;
pub mod fonts;
pub mod headless;
pub mod input;
pub mod palette;
pub mod render;
pub mod view;
