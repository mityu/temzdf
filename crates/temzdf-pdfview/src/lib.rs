//! A PDF pane: what part of the document is shown and how input changes it
//! (`viewer`, `pane`), the outline sidebar, search and key bindings. Produces
//! `temzdf-scene` images and layers; knows nothing about terminals.

pub mod document;
pub mod host;
pub mod keys;
pub mod layout;
pub mod outline;
pub mod pane;
pub mod search;
pub mod viewer;

pub(crate) use temzdf_pdfcore::{doc_cache, pdf, selection, synctex, workers};
