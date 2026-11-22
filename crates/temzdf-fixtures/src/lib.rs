//! The test fixtures: every `tests/fixtures/*.tex` becomes a PDF and a
//! SyncTeX file in `target/fixtures/` (never committed; see AGENTS.md),
//! built with latexmk (pdfLaTeX). Fixtures already newer than their source
//! and than this file are skipped, so tests can call [`build`] cheaply.
//! Each is built in a scratch directory and renamed into place, so a reader
//! (a test, a viewer watching it, a concurrent build) never sees a partly
//! written file; builds in several processes at once wait for each other
//! on a lock file. Used by `temzdf_pdfcore::pdf::fixture` and `cargo xtask
//! fixtures` (which `scripts/build-fixtures.sh` runs).

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

/// Builds the fixtures under `root` (the repository's root) that are
/// missing or stale (all of them with `force`), returning the PDFs built.
pub fn build(root: &Path, force: bool) -> io::Result<Vec<PathBuf>> {
    let out = root.join("target/fixtures");
    fs::create_dir_all(&out)?;
    // Held until the end: another process builds the same files.
    let lock = File::create(out.join(".lock"))?;
    lock.lock()?;
    // This file stands for how they are built (as the script itself did).
    let recipe = mtime(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"));
    let mut sources: Vec<PathBuf> =
        fs::read_dir(root.join("tests/fixtures"))?.map(|e| e.map(|e| e.path())).collect::<io::Result<_>>()?;
    sources.retain(|p| p.extension().is_some_and(|e| e == "tex"));
    sources.sort();
    let mut built = Vec::new();
    for tex in sources {
        let base = tex.file_stem().expect("a file name").to_string_lossy().into_owned();
        let pdf = out.join(format!("{base}.pdf"));
        let synctex = out.join(format!("{base}.synctex.gz"));
        let source = mtime(&tex);
        let fresh = |p: &Path| mtime(p).is_some_and(|t| Some(t) > source && Some(t) > recipe);
        if !force && fresh(&pdf) && fresh(&synctex) {
            continue;
        }
        // pdfTeX's /ID depends on the output's name as given: relative to
        // the root and the same each time (the lock keeps other builds
        // out), the same source gives the same PDF.
        let rel = Path::new("target/fixtures").join(format!(".build-{base}"));
        let work = root.join(&rel);
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work)?;
        let source_rel = Path::new("tests/fixtures").join(tex.file_name().expect("a file name"));
        let result = latexmk(root, &source_rel, &rel).and_then(|()| {
            // The PDF last: its time tells the next run both are there.
            fs::rename(work.join(format!("{base}.synctex.gz")), &synctex)?;
            fs::rename(work.join(format!("{base}.pdf")), &pdf)
        });
        if let Err(e) = result {
            let log = out.join(format!("{base}.log"));
            let _ = fs::copy(work.join(format!("{base}.log")), &log);
            let _ = fs::remove_dir_all(&work);
            return Err(io::Error::other(format!("building {} failed ({e}); see {}", tex.display(), log.display())));
        }
        let _ = fs::remove_dir_all(&work);
        built.push(pdf);
    }
    Ok(built)
}

fn latexmk(root: &Path, tex: &Path, work: &Path) -> io::Result<()> {
    let status = Command::new("latexmk")
        .current_dir(root)
        .args(["-pdf", "-synctex=1", "-interaction=nonstopmode", "-halt-on-error", "-quiet"])
        .arg(format!("-outdir={}", work.display()))
        .arg(tex)
        // Reproducible output: pdfTeX otherwise embeds the build time (and
        // a time-based /ID).
        .env("SOURCE_DATE_EPOCH", "0")
        .env("FORCE_SOURCE_DATE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => {
                io::Error::other("latexmk not found; install TeX or use 'nix develop .#alldeps'")
            }
            _ => e,
        })?;
    if status.success() { Ok(()) } else { Err(io::Error::other(format!("latexmk: {status}"))) }
}

fn mtime(p: impl AsRef<Path>) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}
