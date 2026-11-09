//! Markdown for temzdf's viewer (`doc/design/markdown-viewer.md`): a
//! document of blocks that keep their source lines ([`doc`]). No terminal
//! code; the server shows it.

pub mod doc;
pub mod highlight;
pub mod render;

pub use doc::{Block, Document, Heading, Inline, Lines};
pub use render::{Deco, Rendered, render};
