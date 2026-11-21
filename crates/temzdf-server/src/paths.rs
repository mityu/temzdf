//! How a file's path becomes its identity and its name on screen.

use std::path::{Path, PathBuf};

/// `path` as a document's identity: canonical, or (a file that is not there
/// yet, or not reachable) absolute, so that the same file opened from
/// another directory is the same document.
pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_owned()))
}

/// A file's name as panes and buffer lists show it: its last component, else
/// (`/`) the whole path.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}
