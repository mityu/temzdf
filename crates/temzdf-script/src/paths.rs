//! Paths as the session's OS writes them ([`PathStyle`]), for the module
//! loader: the guest is one WebAssembly build for every OS, whose
//! `std::path` is always Unix's, so `C:\x\config.ts` would be one relative
//! name there. typed-path parses either way on any OS.

use temzdf_protocol::script::PathStyle;
use typed_path::{PathType, Utf8TypedPath};

fn typed(style: PathStyle, p: &str) -> Utf8TypedPath<'_> {
    Utf8TypedPath::new(
        p,
        match style {
            PathStyle::Posix => PathType::Unix,
            PathStyle::Windows => PathType::Windows,
        },
    )
}

pub(crate) fn is_absolute(style: PathStyle, p: &str) -> bool {
    typed(style, p).is_absolute()
}

/// The directory a module's relative imports start from.
pub(crate) fn parent(style: PathStyle, p: &str) -> String {
    match typed(style, p).parent() {
        Some(d) if !d.as_str().is_empty() => d.as_str().to_owned(),
        _ => ".".to_owned(),
    }
}

/// `name` (`./x`, `../x`, `/x`) from `dir`, `a/./b/../c` as `a/c`, without
/// touching the file system.
pub(crate) fn join(style: PathStyle, dir: &str, name: &str) -> String {
    typed(style, dir).join(name).normalize().to_string()
}

/// What a relative import may name, in order, as TypeScript finds it: the
/// path itself, `x.ts` for `x.js` (TypeScript sources import each other by
/// their output's name), `x.ts` or `x.js` for `x`, or a directory's
/// `index.ts` or `index.js`.
pub(crate) fn module_candidates(style: PathStyle, p: &str) -> Vec<String> {
    let path = typed(style, p);
    let mut out = vec![p.to_owned()];
    if path.extension() == Some("js") {
        out.push(path.with_extension("ts").to_string());
    }
    out.extend([format!("{p}.ts"), format!("{p}.js")]);
    out.extend(["index.ts", "index.js"].map(|i| path.join(i).to_string()));
    out
}

// runtime/path.test.ts checks temzdf/path (JSR's @std/path) on the same
// cases, so that the two agree.
#[cfg(test)]
mod tests {
    use super::*;
    use PathStyle::{Posix, Windows};

    #[test]
    fn posix_paths_resolve_as_before() {
        assert_eq!(parent(Posix, "/home/u/.config/temzdf/config.ts"), "/home/u/.config/temzdf");
        assert_eq!(parent(Posix, "config.ts"), ".");
        assert_eq!(join(Posix, "/a/b", "./c.ts"), "/a/b/c.ts");
        assert_eq!(join(Posix, "/a/b", "../c/./d.ts"), "/a/c/d.ts");
        assert_eq!(join(Posix, "/a/b", "/x/y.ts"), "/x/y.ts");
        assert!(is_absolute(Posix, "/a") && !is_absolute(Posix, "a") && !is_absolute(Posix, r"C:\a"));
        assert_eq!(
            module_candidates(Posix, "/a/b.js"),
            ["/a/b.js", "/a/b.ts", "/a/b.js.ts", "/a/b.js.js", "/a/b.js/index.ts", "/a/b.js/index.js"]
        );
    }

    #[test]
    fn windows_paths_keep_their_drive_and_take_both_separators() {
        let config = r"C:\Users\x\AppData\temzdf\config.ts";
        assert_eq!(parent(Windows, config), r"C:\Users\x\AppData\temzdf");
        assert_eq!(join(Windows, r"C:\Users\x", "./lib.ts"), r"C:\Users\x\lib.ts");
        assert_eq!(join(Windows, r"C:\Users\x", "../y/./z.ts"), r"C:\Users\y\z.ts");
        assert_eq!(join(Windows, r"\\server\share\x", "../lib/a.ts"), r"\\server\share\lib\a.ts");
        assert!(is_absolute(Windows, r"C:\a") && is_absolute(Windows, "C:/a"));
        assert!(is_absolute(Windows, r"\\server\share\x"));
        assert!(!is_absolute(Windows, r"a\b") && !is_absolute(Windows, "C:a"));
        assert_eq!(
            module_candidates(Windows, r"C:\a\b"),
            [r"C:\a\b", r"C:\a\b.ts", r"C:\a\b.js", r"C:\a\b\index.ts", r"C:\a\b\index.js"]
        );
    }
}
