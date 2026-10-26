//! temzdf sessions. [`session::Session`] holds the panes, their documents,
//! layout and focus and composes the frame clients show; it has no sockets or
//! terminal, so the standalone viewer, headless rendering and the session
//! server share it. See doc/session-server.md.

pub mod action;
mod clip;
pub mod daemon;
pub mod forward;
pub mod keymap;
pub mod layout;
pub mod session;
