//! How a file's path becomes its identity and its name on screen.

use std::path::{Component, Path, PathBuf};

/// `path` as a document's identity: canonical (links resolved, `.` and `..`
/// gone), so that the same file opened from another directory or through a
/// symlink is the same document. A file that is not there (yet) keeps the
/// name typed after its nearest existing ancestor, which is made canonical:
/// once written, its canonical path is the same. On Windows without the
/// `\\?\` prefix std's canonical paths have (`dunce`), so that it matches
/// paths written as usual (`C:\...`).
pub(crate) fn canonical(path: &Path) -> PathBuf {
    if let Ok(c) = dunce::canonicalize(path) {
        return c;
    }
    let Ok(abs) = std::path::absolute(path) else { return path.to_owned() };
    // The nearest ancestor that is there, made canonical, and the rest as
    // typed (`..` taking off what the rest named).
    let mut tail = Vec::new();
    let mut base = abs.as_path();
    let canonical_base = loop {
        match (dunce::canonicalize(base), base.parent()) {
            (Ok(c), _) => break c,
            (Err(_), Some(parent)) => {
                tail.extend(base.components().next_back());
                base = parent;
            }
            // The root (or a drive) is not there either.
            _ => return abs,
        }
    };
    let mut out = canonical_base;
    for c in tail.into_iter().rev() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// A file's name as panes and buffer lists show it: its last component, else
/// (`/`) the whole path.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn a_missing_file_is_named_after_its_canonical_ancestor() {
        let dir = std::env::temp_dir().join(format!("temzdf-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        let real = dunce::canonicalize(dir.join("real")).unwrap();
        // `..` and `.` go, below the part that is there too.
        assert_eq!(canonical(&dir.join("real/./gone/../new.txt")), real.join("new.txt"));
        assert_eq!(canonical(&dir.join("real/a/b.txt")), real.join("a").join("b.txt"));
        #[cfg(unix)]
        {
            // Through a symlinked directory: the real one's.
            std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
            assert_eq!(canonical(&dir.join("link/new.txt")), real.join("new.txt"));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
