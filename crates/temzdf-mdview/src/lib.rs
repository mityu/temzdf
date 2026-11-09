//! The Markdown viewer's graphics mode (`doc/design/markdown-viewer.md`):
//! [`page::Page`] is a document laid out by Blitz at a width and zoom,
//! painted in strips; [`pane::MdPane`] is a pane scrolled over one.
//! Math is drawn by RaTeX ([`math`]). No terminal code.

pub mod math;
pub mod page;
pub mod pane;

pub use page::{Fetched, Page};
pub use pane::{MdPane, Request};
