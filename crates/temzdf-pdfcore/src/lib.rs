//! The PDF backend of temzdf, independent of how pages are shown: MuPDF
//! documents and display lists (`pdf`), the tile cache (`doc_cache`), page text
//! and selection (`selection`), SyncTeX (`synctex`) and the background threads
//! that load, render, search and watch (`workers`). Knows nothing about
//! terminals or clients.

pub mod doc_cache;
pub mod pdf;
pub mod selection;
pub mod synctex;
pub mod workers;

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    pub use crate::pdf::fixture;
}
