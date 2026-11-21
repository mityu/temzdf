//! Watching a file for changes made elsewhere: a document a typesetter
//! rewrites, a text or Markdown buffer's file edited in another program.

use std::path::Path;

use notify::{RecursiveMode, Watcher};

pub use notify::{Error, Result};

/// A file being watched; dropped, the watching ends.
pub struct FileWatcher {
    _inner: notify::RecommendedWatcher,
}

impl std::fmt::Debug for FileWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileWatcher").finish_non_exhaustive()
    }
}

/// Watch `path` for changes: `on_change` hears each change (not accesses),
/// several for one write. Tools rewrite the file in place or replace it
/// (write a new file, rename over the old one), so the parent directory is
/// watched and events are filtered by file name; that survives the inode
/// changing. Symlinks are resolved and their target is watched as well.
pub fn watch_file(path: &Path, on_change: impl Fn() + Send + 'static) -> Result<FileWatcher> {
    let abs = std::path::absolute(path)?;
    let mut targets = vec![abs.clone()];
    if let Ok(real) = std::fs::canonicalize(&abs)
        && real != abs
    {
        targets.push(real);
    }
    let names: Vec<_> = targets.iter().filter_map(|t| t.file_name().map(|n| n.to_owned())).collect();
    let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if ev.kind.is_access() {
            return;
        }
        if ev.paths.iter().any(|p| p.file_name().is_some_and(|n| names.iter().any(|m| m == n))) {
            on_change();
        }
    })?;
    for t in &targets {
        if let Some(dir) = t.parent() {
            watcher.watch(dir, RecursiveMode::NonRecursive)?;
        }
    }
    Ok(FileWatcher { _inner: watcher })
}
