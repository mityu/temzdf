//! Markdown for temzdf's viewer (`doc/design/markdown-viewer.md`): a
//! document of blocks that keep their source lines ([`doc`]). No terminal
//! code; the server shows it.

pub mod doc;

pub use doc::{Block, Document, Heading, Inline, Lines};
