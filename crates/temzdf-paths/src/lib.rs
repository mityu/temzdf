//! Which file a path names, as the host's file system sees it: a canonical
//! name to know a document by, the identity of a file (the same file by
//! another name: a symlink, a hard link, another case where the file
//! system ignores it), and whether the file system ignores the case of
//! names, for files that are not there to compare.
//!
//! The rules (doc/config.md, `o.fileIgnoreCase`):
//! 1. Two files that are both there are the same when their identity is
//!    (Unix: device and inode; Windows: volume serial number and file ID),
//!    whatever the setting.
//! 2. Otherwise their names are compared, ignoring case when the setting
//!    says so; `"auto"` asks the file system about the nearest directory
//!    that is there (Neovim compares by identity, Vim by name with
//!    'fileignorecase', Emacs asks the file system on macOS).
//! 3. Looking up a path among open documents: the same canonical name
//!    first, then the same identity, then the same name ignoring case
//!    (rule 2), so that changing the setting never merges two documents
//!    already open.

use std::path::{Component, Path, PathBuf};

mod uri;
pub use uri::{file_uri, file_uri_path};

/// `path` as a document's identity: canonical (links resolved, `.` and `..`
/// gone), so that the same file opened from another directory or through a
/// symlink is the same document. A file that is not there (yet) keeps the
/// name typed after its nearest existing ancestor, which is made canonical:
/// once written, its canonical path is the same. On Windows without the
/// `\\?\` prefix std's canonical paths have (`dunce`), so that it matches
/// paths written as usual (`C:\...`), and joining a relative path with `/`
/// to it still works.
pub fn canonical(path: &Path) -> PathBuf {
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

/// What tells a file from every other one on the host: its device and
/// inode on Unix, its volume's serial number and its file ID on Windows.
pub type FileId = (u64, u128);

/// The identity of the file `path` names (symlinks followed), or none when
/// nothing is there (or it cannot be opened).
#[cfg(unix)]
pub fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some((m.dev(), m.ino().into()))
}

/// The identity of the file `path` names (symlinks followed), or none when
/// nothing is there (or it cannot be opened).
#[cfg(windows)]
pub fn file_id(path: &Path) -> Option<FileId> {
    windows::file_id(path)
}

#[cfg(not(any(unix, windows)))]
pub fn file_id(_path: &Path) -> Option<FileId> {
    None
}

/// Whether names that differ only in case name the same file: the
/// setting `o.fileIgnoreCase`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IgnoreCase {
    /// As the file system of the nearest directory there says
    /// ([`fs_ignores_case`]).
    #[default]
    Auto,
    Yes,
    No,
}

impl IgnoreCase {
    /// Whether case is ignored for names like `path`.
    pub fn at(self, path: &Path) -> bool {
        match self {
            IgnoreCase::Auto => fs_ignores_case(path),
            IgnoreCase::Yes => true,
            IgnoreCase::No => false,
        }
    }
}

/// Whether the file system ignores the case of names in the nearest
/// directory of `path` that is there (`path` itself if it is one): asked
/// of it (Windows: a directory's case-sensitivity flag; macOS:
/// `pathconf(_PC_CASE_SENSITIVE)`; Linux: a directory's casefold flag),
/// else the system's usual (ignored on Windows and macOS, not elsewhere).
pub fn fs_ignores_case(path: &Path) -> bool {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let dir = abs.ancestors().find(|a| a.is_dir());
    dir.and_then(dir_ignores_case).unwrap_or(cfg!(any(windows, target_os = "macos")))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn dir_ignores_case(dir: &Path) -> Option<bool> {
    use rustix::fs::{Mode, OFlags};
    const FS_CASEFOLD_FL: u32 = 0x4000_0000;
    let fd = rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).ok()?;
    // File systems without inode flags (tmpfs before 6.?, NFS, ...) refuse:
    // the usual then.
    let flags = rustix::fs::ioctl_getflags(&fd).ok()?;
    Some(flags.bits() & FS_CASEFOLD_FL != 0)
}

#[cfg(target_os = "macos")]
fn dir_ignores_case(dir: &Path) -> Option<bool> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: a valid NUL-terminated path.
    let sensitive = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_CASE_SENSITIVE) };
    // -1: not known (or an error).
    (sensitive >= 0).then_some(sensitive == 0)
}

#[cfg(windows)]
fn dir_ignores_case(dir: &Path) -> Option<bool> {
    windows::dir_ignores_case(dir)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", windows)))]
fn dir_ignores_case(_dir: &Path) -> Option<bool> {
    None
}

/// `path` with case folded, to compare names ignoring it.
pub fn fold(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Whether `a` and `b` name the same file: by identity when both are there,
/// else by name, ignoring case as `ignore_case` says for `a`.
pub fn same_file(a: &Path, b: &Path, ignore_case: IgnoreCase) -> bool {
    if a == b {
        return true;
    }
    match (file_id(a), file_id(b)) {
        (Some(x), Some(y)) => x == y,
        _ => ignore_case.at(a) && fold(a) == fold(b),
    }
}

/// The entry of `entries` (key, canonical path) whose file `wanted` (a
/// canonical path) names: the same name, else the same file, else (one of
/// the two not there) the same name ignoring case as `ignore_case` says.
/// In that order, so that an entry never stands for another one's file.
pub fn find<'a, K: Copy>(
    wanted: &Path,
    entries: impl IntoIterator<Item = (K, &'a Path)>,
    ignore_case: IgnoreCase,
) -> Option<K> {
    let entries: Vec<(K, &Path)> = entries.into_iter().collect();
    if let Some((k, _)) = entries.iter().find(|(_, p)| *p == wanted) {
        return Some(*k);
    }
    let id = file_id(wanted);
    let ids: Vec<Option<FileId>> = entries.iter().map(|(_, p)| file_id(p)).collect();
    if id.is_some()
        && let Some(i) = ids.iter().position(|i| *i == id)
    {
        return Some(entries[i].0);
    }
    if !ignore_case.at(wanted) {
        return None;
    }
    let folded = fold(wanted);
    entries
        .iter()
        .zip(&ids)
        .find(|((_, p), i)| !(id.is_some() && i.is_some()) && fold(p) == folded)
        .map(|((k, _), _)| *k)
}

#[cfg(windows)]
mod windows {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO,
        FILE_READ_ATTRIBUTES, FileCaseSensitiveInfo, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx,
    };
    use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

    use super::FileId;

    /// `path` opened to read its attributes (a directory too; links
    /// followed).
    fn open(path: &Path) -> Option<std::fs::File> {
        std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .ok()
    }

    pub(super) fn file_id(path: &Path) -> Option<FileId> {
        let file = open(path)?;
        let handle = file.as_raw_handle();
        // The 128-bit ID (ReFS's do not fit in 64 bits), else (FAT) the
        // file index.
        let mut info = FILE_ID_INFO::default();
        // SAFETY: an open handle; `info` is the struct the class asks for,
        // its size given.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                (&raw mut info).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if ok != 0 {
            return Some((info.VolumeSerialNumber, u128::from_le_bytes(info.FileId.Identifier)));
        }
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: an open handle and a struct to fill.
        if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
            return None;
        }
        let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        Some((info.dwVolumeSerialNumber.into(), index.into()))
    }

    /// Whether `dir` ignores case: not when its case-sensitivity flag is
    /// on (`fsutil file setCaseSensitiveInfo`, WSL); none when it cannot be
    /// asked.
    pub(super) fn dir_ignores_case(dir: &Path) -> Option<bool> {
        let file = open(dir)?;
        let mut info = FILE_CASE_SENSITIVE_INFO { Flags: 0 };
        // SAFETY: an open handle; `info` is the struct the class asks for,
        // its size given.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileCaseSensitiveInfo,
                (&raw mut info).cast(),
                std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
            )
        };
        (ok != 0).then_some(info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory for test `name`.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("temzdf-paths-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        canonical(&dir)
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn a_missing_file_is_named_after_its_canonical_ancestor() {
        let dir = scratch("canonical");
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

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn files_there_are_the_same_by_identity_whatever_the_setting() {
        let dir = scratch("identity");
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        std::fs::hard_link(dir.join("a.txt"), dir.join("hard.txt")).unwrap();
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        for ic in [IgnoreCase::Auto, IgnoreCase::Yes, IgnoreCase::No] {
            assert!(same_file(&dir.join("a.txt"), &dir.join("hard.txt"), ic), "{ic:?}");
            assert!(!same_file(&dir.join("a.txt"), &dir.join("b.txt"), ic), "{ic:?}");
        }
        assert_ne!(file_id(&dir.join("a.txt")), None);
        assert_eq!(file_id(&dir.join("a.txt")), file_id(&dir.join("hard.txt")));
        assert_eq!(file_id(&dir.join("gone.txt")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn names_of_files_not_there_follow_the_setting() {
        let dir = scratch("names");
        let (a, b) = (dir.join("New.txt"), dir.join("new.txt"));
        assert!(same_file(&a, &b, IgnoreCase::Yes));
        assert!(!same_file(&a, &b, IgnoreCase::No));
        assert_eq!(same_file(&a, &b, IgnoreCase::Auto), fs_ignores_case(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn the_file_system_is_asked_about_the_nearest_directory_there() {
        let dir = scratch("ask");
        let here = fs_ignores_case(&dir);
        assert_eq!(fs_ignores_case(&dir.join("no/such/dir/file.txt")), here);
        // Where nothing says otherwise (the temporary directory of the
        // hosts that run the tests): the system's usual.
        assert_eq!(here, cfg!(any(windows, target_os = "macos")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn lookup_takes_the_name_then_the_file_then_the_name_ignoring_case() {
        // Not there: by name alone. Exactly that name first, even when
        // another one matches it ignoring case.
        let dir = std::env::temp_dir().join(format!("temzdf-paths-{}-no-such-dir", std::process::id()));
        let (upper, lower) = (dir.join("NEW.txt"), dir.join("new.txt"));
        let entries = [(1, upper.as_path()), (2, lower.as_path())];
        assert_eq!(find(&lower, entries, IgnoreCase::Yes), Some(2));
        assert_eq!(find(&dir.join("New.txt"), entries, IgnoreCase::Yes), Some(1));
        assert_eq!(find(&dir.join("New.txt"), entries, IgnoreCase::No), None);
    }
}
