//! Watching a file for changes made elsewhere: a document a typesetter
//! rewrites, a text or Markdown buffer's file edited in another program.

use std::ffi::{OsStr, OsString};
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
/// Where the file system ignores the case of names, names are compared
/// ignoring it: events may name the file in another case than `path`.
pub fn watch_file(path: &Path, on_change: impl Fn() + Send + 'static) -> Result<FileWatcher> {
    let abs = std::path::absolute(path)?;
    let mut targets = vec![abs.clone()];
    // Canonical without Windows' `\\?\` (`dunce`): the same directory is
    // not watched twice.
    let real = temzdf_paths::canonical(&abs);
    if real != abs {
        targets.push(real);
    }
    let names: Vec<(OsString, bool)> =
        targets.iter().filter_map(|t| Some((t.file_name()?.to_owned(), temzdf_paths::fs_ignores_case(t)))).collect();
    let named = move |n: &OsStr| {
        use temzdf_paths::fold as f;
        names.iter().any(|(m, fold)| m == n || (*fold && f(Path::new(m)) == f(Path::new(n))))
    };
    let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if ev.kind.is_access() {
            return;
        }
        if ev.paths.iter().any(|p| p.file_name().is_some_and(&named)) {
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
