//! Which file a path names: canonical names, the same file by another
//! name, and the case of names.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use temzdf_protocol::script::ScriptValue;

use super::*;
use temzdf_paths::IgnoreCase;
use termpix_core::geom::CellSize;

const GEOMETRY: Geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };

fn session() -> Session {
    let sink: EventSink = Arc::new(|_| true);
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    Session::new("paths".into(), GEOMETRY, Hints::default(), settings, sink)
}

/// A fresh directory for test `name`.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("temzdf-paths-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The files of the session's text buffers, sorted.
fn text_files(s: &Session) -> Vec<PathBuf> {
    let mut files: Vec<_> = s.texts.values().filter_map(|t| t.file.as_ref().map(|f| f.path.clone())).collect();
    files.sort();
    files
}

/// Open `path` as text (a new file if it is not there): its buffer.
fn open(s: &mut Session, path: &Path) -> BufferId {
    let spot = s.next_spot();
    let pane = s.open_text(path, spot, true).unwrap();
    match &s.panes[&pane].kind {
        Kind::Text { buf, .. } => *buf,
        _ => panic!("not a text pane"),
    }
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn a_written_buffers_path_is_canonical() {
    let dir = scratch("write");
    std::fs::create_dir(dir.join("real")).unwrap();
    let real = crate::paths::canonical(&dir.join("real"));
    let mut s = session();
    let write = |s: &mut Session, buf: &ScriptValue, path: &Path| {
        let opts =
            ScriptValue::map([("buffer", buf.clone()), ("path", ScriptValue::Str(path.to_str().unwrap().into()))]);
        s.run_command("write", &[opts], Instant::now()).unwrap();
    };
    let (buf, _) = s.run_command("new-buffer", &[], Instant::now()).unwrap();
    // `..` through a directory that is not there: gone from its name.
    write(&mut s, &buf, &dir.join("real/sub/../one.txt"));
    assert_eq!(text_files(&s), [real.join("one.txt")]);
    // Opened by its canonical name: the same buffer.
    open(&mut s, &real.join("one.txt"));
    assert_eq!(s.texts.len(), 1, "one buffer");
    #[cfg(unix)]
    {
        // Through a symlinked directory: the real one's name.
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let (buf, _) = s.run_command("new-buffer", &[], Instant::now()).unwrap();
        write(&mut s, &buf, &dir.join("link/two.txt"));
        assert!(text_files(&s).contains(&real.join("two.txt")), "{:?}", text_files(&s));
        open(&mut s, &real.join("two.txt"));
        assert_eq!(s.texts.len(), 2, "two buffers");
        // A directory that was not there when the buffer was named, a
        // symlink when it is written: the name of the file written.
        let path = dir.join("later/three.txt");
        let opts = ScriptValue::map([("path", ScriptValue::Str(path.to_str().unwrap().into()))]);
        let (buf, _) = s.run_command("new-buffer", &[opts], Instant::now()).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("later")).unwrap();
        let opts = ScriptValue::map([("buffer", buf)]);
        s.run_command("write", &[opts], Instant::now()).unwrap();
        assert!(text_files(&s).contains(&real.join("three.txt")), "{:?}", text_files(&s));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn a_file_by_another_name_is_the_same_document() {
    let dir = scratch("links");
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    let mut s = session();
    let a = open(&mut s, &dir.join("a.txt"));
    // A hard link: another canonical name, the same file.
    std::fs::hard_link(dir.join("a.txt"), dir.join("hard.txt")).unwrap();
    assert_eq!(open(&mut s, &dir.join("hard.txt")), a);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("a.txt"), dir.join("soft.txt")).unwrap();
        assert_eq!(open(&mut s, &dir.join("soft.txt")), a);
    }
    assert_eq!(s.texts.len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn files_there_differing_in_case_are_two_documents_where_the_file_system_tells_them_apart() {
    let dir = scratch("two-cases");
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    if dir.join("A.txt").exists() {
        // (A file system ignoring case: one file.)
        eprintln!("skipped: {} ignores case", dir.display());
        return;
    }
    std::fs::write(dir.join("A.txt"), "A\n").unwrap();
    // Both there: told apart by identity, even when the setting says to
    // ignore case.
    for ic in [IgnoreCase::Auto, IgnoreCase::Yes, IgnoreCase::No] {
        let mut s = session();
        s.options.fileignorecase = ic;
        let a = open(&mut s, &dir.join("a.txt"));
        assert_ne!(open(&mut s, &dir.join("A.txt")), a, "{ic:?}");
        assert_eq!(open(&mut s, &dir.join("a.txt")), a, "{ic:?}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn names_of_files_not_there_follow_the_setting() {
    let dir = scratch("missing");
    let usual = cfg!(any(windows, target_os = "macos"));
    for (ic, same) in [(IgnoreCase::Auto, usual), (IgnoreCase::Yes, true), (IgnoreCase::No, false)] {
        let mut s = session();
        s.options.fileignorecase = ic;
        let a = open(&mut s, &dir.join("New.txt"));
        assert_eq!(open(&mut s, &dir.join("new.txt")) == a, same, "{ic:?}");
    }
    // Auto: as the file system says (where the tests run, the system's
    // usual).
    assert_eq!(temzdf_paths::fs_ignores_case(&dir), usual);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn changing_the_setting_never_merges_documents_already_open() {
    let dir = scratch("switch");
    let mut s = session();
    s.options.fileignorecase = IgnoreCase::No;
    let upper = open(&mut s, &dir.join("NEW.txt"));
    let lower = open(&mut s, &dir.join("new.txt"));
    assert_ne!(upper, lower);
    // Ignoring case now: each name still finds its own buffer first.
    s.options.fileignorecase = IgnoreCase::Yes;
    assert_eq!(open(&mut s, &dir.join("new.txt")), lower);
    assert_eq!(open(&mut s, &dir.join("NEW.txt")), upper);
    assert_eq!(s.texts.len(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "the file system")]
fn a_markdown_file_reads_images_below_its_directory_by_one_name() {
    // Relative to the document's canonical directory (on Windows std's
    // was `\\?\C:\...`; std's join turns `/` into `\` there, so
    // `img/pic.png` loaded with it as well: checked in the VM, 2026-11-24).
    let dir = scratch("markdown");
    std::fs::create_dir(dir.join("img")).unwrap();
    write_png(&dir.join("img/pic.png"));
    std::fs::write(dir.join("doc.md"), "![pic](img/pic.png)\n").unwrap();
    let mut s = session();
    let md = s.md_doc(&dir.join("doc.md")).unwrap();
    assert!(s.md_docs[&md].images.contains_key("img/pic.png"), "the image read");
    // The same file by another name: the same document.
    std::fs::hard_link(dir.join("doc.md"), dir.join("hard.md")).unwrap();
    assert_eq!(s.md_doc(&dir.join("hard.md")).unwrap(), md);
    // Not there: an error, as before.
    assert!(s.md_doc(&dir.join("gone.md")).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A 1x1 PNG at `path`.
fn write_png(path: &Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1, 1);
    enc.set_color(png::ColorType::Rgba);
    enc.write_header().unwrap().write_image_data(&[200; 4]).unwrap();
}

#[cfg(windows)]
#[test]
fn a_file_named_in_another_case_is_one_document_on_windows() {
    let dir = scratch("windows-case");
    std::fs::write(dir.join("Foo.txt"), "foo\n").unwrap();
    let mut s = session();
    let foo = open(&mut s, &dir.join("Foo.txt"));
    // `c:\...\FOO.TXT`: the drive and the name in other cases.
    let shouted = dir.join("FOO.TXT").to_str().unwrap().to_owned();
    let shouted = format!("{}{}", shouted[..1].to_lowercase(), &shouted[1..]);
    assert_eq!(open(&mut s, Path::new(&shouted)), foo);
    assert_eq!(text_files(&s), [crate::paths::canonical(&dir.join("Foo.txt"))], "named as on disk");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A directory whose case-sensitivity flag is on (`fsutil file
/// setCaseSensitiveInfo`; administrators only, and only on an empty
/// directory), or why there is none.
#[cfg(windows)]
fn case_sensitive_dir(name: &str) -> Result<PathBuf, String> {
    let dir = scratch(name);
    let out = std::process::Command::new("fsutil")
        .args(["file", "setCaseSensitiveInfo"])
        .arg(&dir)
        .arg("enable")
        .output()
        .map_err(|e| format!("fsutil: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!(
            "fsutil: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(dir)
}

#[cfg(windows)]
#[test]
fn a_case_sensitive_directory_tells_names_apart_on_windows() {
    let dir = match case_sensitive_dir("windows-sensitive") {
        Ok(d) => d,
        Err(why) => {
            // Not an administrator (the gate in the VM is one).
            eprintln!("skipped: no case-sensitive directory: {why}");
            return;
        }
    };
    assert!(!IgnoreCase::Auto.at(&dir), "the file system asked");
    std::fs::write(dir.join("Foo.txt"), "foo\n").unwrap();
    std::fs::write(dir.join("FOO.TXT"), "FOO\n").unwrap();
    let mut s = session();
    let foo = open(&mut s, &dir.join("Foo.txt"));
    assert_ne!(open(&mut s, &dir.join("FOO.TXT")), foo, "two files");
    // Files not there: as the directory says.
    let new = open(&mut s, &dir.join("New.txt"));
    assert_ne!(open(&mut s, &dir.join("new.txt")), new);
    assert_eq!(s.texts.len(), 4);
    std::fs::remove_dir_all(&dir).unwrap();
}
