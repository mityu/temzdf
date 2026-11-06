//! Reading and writing files: UTF-8 (a BOM kept), line endings as they
//! were, never a binary file, written safely (a temporary file renamed over
//! the target) and not over changes made on disk since it was read.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::text::{LineEnding, Text};

/// What a buffer remembers of its file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileInfo {
    pub path: PathBuf,
    pub bom: bool,
    /// The encoding's name (only UTF-8 so far).
    pub encoding: String,
    pub line_ending: LineEnding,
    /// When it was last read or written, to notice changes made elsewhere.
    pub mtime: Option<SystemTime>,
}

/// Bytes looked at for a NUL, which makes a file binary.
const SNIFF: usize = 8192;

/// Read `path`: its text (LF inside) and what writing it back needs.
pub fn read(path: &Path) -> Result<(Text, FileInfo), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes[..bytes.len().min(SNIFF)].contains(&0) {
        return Err(format!("{}: a binary file", path.display()));
    }
    let s = std::str::from_utf8(&bytes)
        .map_err(|_| format!("{}: not UTF-8 (open it naming its encoding)", path.display()))?;
    let (s, bom) = match s.strip_prefix('\u{feff}') {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    let (text, line_ending) = Text::from_file_text(s);
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    Ok((text, FileInfo { path: path.to_owned(), bom, encoding: "utf-8".into(), line_ending, mtime }))
}

/// Write `text` to `info.path` (or the file a symlink there points to):
/// into a temporary file beside it, with its permissions, renamed over it;
/// in place when the directory cannot take the temporary file. A file
/// changed on disk since `info.mtime` is an error unless `force`. Returns
/// the new modification time.
pub fn write(text: &Text, info: &FileInfo, force: bool) -> Result<Option<SystemTime>, String> {
    let path = &info.path;
    let shown = path.display();
    // A symlink's target is written, the link stays.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let existing = std::fs::metadata(&target).ok();
    if let (Some(meta), Some(read), false) = (&existing, info.mtime, force)
        && meta.modified().ok().is_some_and(|m| m != read)
    {
        return Err(format!("{shown}: changed on disk since it was read (write with force to replace it)"));
    }
    let mut bytes = Vec::new();
    if info.bom {
        bytes.extend_from_slice("\u{feff}".as_bytes());
    }
    bytes.extend_from_slice(text.to_file_text(info.line_ending).as_bytes());
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.temzdf-{}.tmp", std::process::id()));
    let renamed = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        if let Some(meta) = &existing {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, &target)
    })();
    if let Err(e) = renamed {
        let _ = std::fs::remove_file(&tmp);
        if e.kind() != std::io::ErrorKind::PermissionDenied {
            return Err(format!("{shown}: {e}"));
        }
        // The directory is not ours to write: the file in place.
        std::fs::write(&target, &bytes).map_err(|e| format!("{shown}: {e}"))?;
    }
    Ok(std::fs::metadata(&target).and_then(|m| m.modified()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("temzdf-texteditor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn files_round_trip_with_their_bom_and_line_endings() {
        let d = dir("round-trip");
        let p = d.join("a.txt");
        std::fs::write(&p, "\u{feff}one\r\ntwo\r\n").unwrap();
        let (text, mut info) = read(&p).unwrap();
        assert_eq!(text.to_string(), "one\ntwo\n");
        assert!(info.bom && info.line_ending == LineEnding::CrLf);
        info.mtime = write(&Text::from("one\nthree\n"), &info, false).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "\u{feff}one\r\nthree\r\n");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn binary_and_non_utf8_files_are_refused() {
        let d = dir("refused");
        std::fs::write(d.join("bin"), b"ab\0cd").unwrap();
        std::fs::write(d.join("sjis"), b"\x82\xa0").unwrap();
        assert!(read(&d.join("bin")).unwrap_err().contains("binary"));
        assert!(read(&d.join("sjis")).unwrap_err().contains("not UTF-8"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn changes_on_disk_are_not_overwritten_and_symlinks_and_modes_stay() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("disk");
        let real = d.join("real.sh");
        std::fs::write(&real, "x\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o750)).unwrap();
        let link = d.join("link.sh");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (_, mut info) = read(&link).unwrap();
        info.mtime = Some(SystemTime::UNIX_EPOCH);
        let e = write(&Text::from("y\n"), &info, false).unwrap_err();
        assert!(e.contains("changed on disk"), "{e}");
        write(&Text::from("y\n"), &info, true).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link stays");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "y\n");
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o750);
        let _ = std::fs::remove_dir_all(d);
    }
}
