//! `file:` URIs of local paths, and the local paths of `file:` URIs.
//!
//! The paths come from untrusted links (documents, programs in terminal
//! panes), so on Windows only a plain drive path is taken: no UNC or
//! device path (`//server/share`, `\\?\`, `\\.\`), no stream (`x.exe::$DATA`),
//! no name Windows would change on the way (`x.exe.`, `x.exe `) and no
//! device name (`NUL`, `COM1.txt`). What the name means is then left to
//! [`crate::canonical`].

use std::path::{Path, PathBuf};

/// The `file:` URI of the absolute `path`, percent-encoded (everything but
/// letters, digits, `-._~` and `/`): `file:///home/me/a%20b.pdf`, on
/// Windows `file:///C:/Users/me/a.pdf`.
pub fn file_uri(path: &Path) -> String {
    #[cfg(unix)]
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()).to_vec();
    #[cfg(not(unix))]
    let bytes = path.to_string_lossy().replace('\\', "/").into_bytes();
    encode(&bytes, cfg!(windows))
}

/// The local path a `file:` URI names (`file:///p`, `file://localhost/p`,
/// `file:/p`), percent-decoded, its query and fragment left out. None for
/// another host, a malformed escape, a relative path, and on Windows
/// anything but a plain drive path (the module's comment).
pub fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let bytes = decode(uri, cfg!(windows))?;
    #[cfg(unix)]
    let path = <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(bytes);
    #[cfg(not(unix))]
    let path = String::from_utf8(bytes).ok()?;
    Some(PathBuf::from(path))
}

/// [`file_uri`] of a path's bytes (`/` separated), Windows' rules or not.
fn encode(path: &[u8], windows: bool) -> String {
    let mut out = String::from("file://");
    // `C:/x` is `/C:/x` in a URI.
    let drive = windows && path.len() >= 2 && path[0].is_ascii_alphabetic() && path[1] == b':';
    if drive {
        out.push('/');
    }
    for (i, &b) in path.iter().enumerate() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) || (drive && i == 1) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// [`file_uri_path`]'s bytes, Windows' rules or not (so that both are
/// tested everywhere).
fn decode(uri: &str, windows: bool) -> Option<Vec<u8>> {
    let (scheme, rest) = uri.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    let path = match rest.strip_prefix("//") {
        Some(r) => {
            let (host, path) = r.split_at(r.find('/')?);
            if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                return None;
            }
            path
        }
        None => rest,
    };
    let path = path.split(['?', '#']).next()?;
    if !path.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(path.len());
    let mut it = path.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex = [it.next()?, it.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    if bytes.contains(&0) {
        return None;
    }
    if windows {
        let path = std::str::from_utf8(&bytes).ok()?;
        return windows_drive_path(path).then(|| bytes[1..].to_vec());
    }
    Some(bytes)
}

/// Whether `/C:/...` (a decoded URI path) is a plain path on a drive, one
/// Windows takes as written (the module's comment).
fn windows_drive_path(path: &str) -> bool {
    let b = path.as_bytes();
    if !(b.len() >= 4 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' && b[3] == b'/') {
        return false;
    }
    path[4..].split('/').filter(|c| !c.is_empty()).all(|c| {
        let plain = !c.chars().any(|ch| ch < ' ' || "\\:<>\"|?*".contains(ch));
        let changed = c != "." && c != ".." && (c.ends_with('.') || c.ends_with(' '));
        plain && !changed && !device_name(c)
    })
}

/// Whether Win32 takes the name `c` for a device (`NUL`, `com1.txt`,
/// `LPT¹ `): the part before the first `.`, spaces at its end dropped.
fn device_name(c: &str) -> bool {
    let base = c.split('.').next().unwrap_or(c).trim_end_matches(' ').to_ascii_uppercase();
    let numbered = |prefix: &str| {
        base.strip_prefix(prefix)
            .is_some_and(|n| matches!(n, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
    };
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") || numbered("COM") || numbered("LPT")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(uri: &str) -> Option<String> {
        decode(uri, true).map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn windows_takes_plain_drive_paths() {
        assert_eq!(win("file:///C:/Users/me/a%20b.pdf").as_deref(), Some("C:/Users/me/a b.pdf"));
        assert_eq!(win("file://localhost/d:/x/y.txt").as_deref(), Some("d:/x/y.txt"));
        assert_eq!(win("FILE:/C:/x.pdf#page=2").as_deref(), Some("C:/x.pdf"));
        assert_eq!(win("file:///C:/%E6%95%B0/../a.md").as_deref(), Some("C:/数/../a.md"));
        assert_eq!(win("file:///C:/").as_deref(), Some("C:/"));
    }

    #[test]
    fn windows_refuses_what_is_not_a_plain_drive_path() {
        for uri in [
            "file://server/share/a.pdf",
            "file:////server/share/a.pdf",
            "file://///server/share/a.pdf",
            "file:///%5C%5Cserver%5Cshare%5Ca.pdf",
            "file:///%5C%5C%3F%5CC:%5Ca.pdf",
            "file:///%5C%5C.%5CC:%5Ca.pdf",
            "file:///C:%5Ca.pdf",
            "file:///C:a.pdf",
            "file:///a.pdf",
            "file:///C:/x/evil.exe::$DATA",
            "file:///C:/x/evil.exe%3A%3A%24DATA",
            "file:///C:/x/evil.exe.",
            "file:///C:/x/evil.exe%20",
            "file:///C:/x/evil.exe%2E",
            "file:///C:/x./a.pdf",
            "file:///C:/x/NUL",
            "file:///C:/x/com1.pdf",
            "file:///C:/x/LPT%C2%B9.txt",
            "file:///C:/x/a%00.pdf",
            "file:///C:/x/a%3F.pdf",
            "file:///C:/x/a%0A.pdf",
            "file:///C:/x/a%ZZ.pdf",
            "file:///C:/x/a%E6.pdf",
            "file:C:/x.pdf",
            "https:///C:/x.pdf",
        ] {
            assert_eq!(win(uri), None, "{uri}");
        }
        // Names only like devices' are names.
        assert!(win("file:///C:/x/console.pdf").is_some());
        assert!(win("file:///C:/x/COM10.txt").is_some());
    }

    #[test]
    fn unix_takes_absolute_paths_on_this_host() {
        let unix = |uri| decode(uri, false);
        assert_eq!(unix("file:///home/me/a%20b.pdf"), Some(b"/home/me/a b.pdf".to_vec()));
        assert_eq!(unix("file://LOCALHOST/x"), Some(b"/x".to_vec()));
        assert_eq!(unix("file:/x?q"), Some(b"/x".to_vec()));
        assert_eq!(unix("file:///x/%FF"), Some(b"/x/\xff".to_vec()), "any bytes");
        for uri in ["file://elsewhere/x", "file:x", "file:///x%00", "file:///x%2", "http:///x"] {
            assert_eq!(unix(uri), None, "{uri}");
        }
    }

    #[test]
    fn paths_go_there_and_back() {
        for (path, windows) in [
            (&b"/home/me/a b#1%?.pdf"[..], false),
            (b"/x/\xff\x01:", false),
            ("C:/Users/me/数 x#.pdf".as_bytes(), true),
            (b"d:/", true),
        ] {
            let uri = encode(path, windows);
            assert!(!uri.contains(' ') && !uri.contains('#') && !uri.contains('?'), "{uri}");
            assert_eq!(decode(&uri, windows).as_deref(), Some(path), "{uri}");
        }
        assert_eq!(encode(b"C:/a b", true), "file:///C:/a%20b");
        assert_eq!(encode(b"/C:/a", false), "file:///C%3A/a", "no drives on Unix");
    }

    #[test]
    fn the_uri_of_a_path_names_it() {
        let path = std::env::temp_dir().join("temzdf paths #1.pdf");
        assert_eq!(file_uri_path(&file_uri(&path)), Some(path));
    }
}
