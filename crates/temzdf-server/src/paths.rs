//! How a file's path becomes its identity and its name on screen.

use std::path::Path;

/// `path` as a document's identity ([`temzdf_paths::canonical`]): the
/// same file opened from another directory or through a symlink is the
/// same document.
pub(crate) use temzdf_paths::canonical;

/// A file's name as panes and buffer lists show it: its last component, else
/// (`/`) the whole path.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}
