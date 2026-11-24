//! Which file a path names: canonical names, the same file by another
//! name, and the case of names.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use temzdf_protocol::script::ScriptValue;

use super::*;
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

fn open(s: &mut Session, path: &Path) {
    let spot = s.next_spot();
    s.open_text(path, spot, true).unwrap();
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
