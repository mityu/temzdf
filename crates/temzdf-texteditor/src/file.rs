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
    read_as(path, None)
}

/// [`read`], in `encoding` (an Encoding Standard label: "shift_jis",
/// "euc-jp", ...) rather than UTF-8. Nothing is guessed: text that is not
/// valid in the encoding is an error, not replaced.
pub fn read_as(path: &Path, encoding: Option<&str>) -> Result<(Text, FileInfo), String> {
    let shown = path.display();
    let bytes = std::fs::read(path).map_err(|e| format!("{shown}: {e}"))?;
    let enc = match encoding {
        Some(label) => {
            encoding_rs::Encoding::for_label(label.as_bytes()).ok_or_else(|| format!("{label:?}: no such encoding"))?
        }
        None => encoding_rs::UTF_8,
    };
    // UTF-16 has NUL bytes; anything else with one is binary.
    if enc != encoding_rs::UTF_16LE && enc != encoding_rs::UTF_16BE && bytes[..bytes.len().min(SNIFF)].contains(&0) {
        return Err(format!("{shown}: a binary file"));
    }
    let (body, bom) = match encoding_rs::Encoding::for_bom(&bytes) {
        Some((e, len)) if e == enc => (&bytes[len..], true),
        _ => (&bytes[..], false),
    };
    let s = enc.decode_without_bom_handling_and_without_replacement(body).ok_or_else(|| {
        if encoding.is_none() {
            format!("{shown}: not UTF-8 (open it naming its encoding)")
        } else {
            format!("{shown}: not valid {}", enc.name())
        }
    })?;
    let (text, line_ending) = Text::from_file_text(&s);
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let encoding = enc.name().to_ascii_lowercase();
    Ok((text, FileInfo { path: path.to_owned(), bom, encoding, line_ending, mtime }))
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
    let body = text.to_file_text(info.line_ending);
    let enc = encoding_rs::Encoding::for_label(info.encoding.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let mut bytes = Vec::new();
    let encoded: Vec<u8> = if enc == encoding_rs::UTF_8 {
        body.into_bytes()
    } else if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
        // encoding_rs encodes nothing into UTF-16: by hand.
        let be = enc == encoding_rs::UTF_16BE;
        body.encode_utf16().flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() }).collect()
    } else {
        let (out, _, unmappable) = enc.encode(&body);
        if unmappable {
            return Err(format!("{shown}: the text has characters {} cannot hold", enc.name()));
        }
        out.into_owned()
    };
    if info.bom {
        bytes.extend_from_slice(match enc.name() {
            "UTF-16LE" => &[0xff, 0xfe][..],
            "UTF-16BE" => &[0xfe, 0xff][..],
            _ => "\u{feff}".as_bytes(),
        });
    }
    bytes.extend_from_slice(&encoded);
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
    fn other_encodings_are_read_and_written_when_named() {
        let d = dir("encodings");
        let p = d.join("sjis.txt");
        // 「あ」 in Shift_JIS, CRLF.
        std::fs::write(&p, b"\x82\xa0\r\n").unwrap();
        assert!(read(&p).unwrap_err().contains("not UTF-8"));
        let (text, mut info) = read_as(&p, Some("shift_jis")).unwrap();
        assert_eq!((text.to_string().as_str(), info.encoding.as_str()), ("あ\n", "shift_jis"));
        info.mtime = write(&Text::from("あい\n"), &info, false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"\x82\xa0\x82\xa2\r\n");
        let e = write(&Text::from("😀\n"), &info, true).unwrap_err();
        assert!(e.contains("cannot hold"), "{e}");
        assert!(read_as(&p, Some("euc-jp")).unwrap_err().contains("not valid EUC-JP"));
        assert!(read_as(&p, Some("klingon")).is_err());
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
