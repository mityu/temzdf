//! Embed the built-in fonts as `BUNDLED`, in the order of `fonts.toml`:
//! - the files `TEMZDF_BUNDLED_FONTS` names (paths joined by `:`), as the
//!   Nix dev shell and package set it (`bundleFontFiles` in flake.nix, made
//!   from `fonts.toml`);
//! - else those of `fonts.toml` found in `target/fonts/`, where `cargo xtask
//!   fetch-fonts` puts them (which also checks their sha256).
//!
//! Without either the GUI builds with none and relies on the system's fonts.

use std::fmt::Write;
use std::path::{Path, PathBuf};

#[derive(serde::Deserialize)]
struct Manifest {
    font: Vec<Font>,
}

#[derive(serde::Deserialize)]
struct Font {
    file: String,
}

fn main() {
    println!("cargo:rerun-if-env-changed=TEMZDF_BUNDLED_FONTS");
    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");
    println!("cargo:rerun-if-changed=fonts.toml");
    let paths = match std::env::var("TEMZDF_BUNDLED_FONTS") {
        Ok(v) if !v.is_empty() => v.split(':').filter(|p| !p.is_empty()).map(PathBuf::from).collect(),
        _ => fetched(),
    };
    let mut out = String::from("/// The fonts built in (build.rs).\npub const BUNDLED: &[&[u8]] = &[\n");
    for p in paths {
        assert!(p.is_file(), "no font file {}", p.display());
        println!("cargo:rerun-if-changed={}", p.display());
        writeln!(out, "    include_bytes!({p:?}),").unwrap();
    }
    out.push_str("];\n");
    let dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(Path::new(&dir).join("bundled_fonts.rs"), out).unwrap();
}

/// The fonts of `fonts.toml` in `target/fonts/`, warning about missing ones.
fn fetched() -> Vec<PathBuf> {
    let list = std::fs::read_to_string("fonts.toml").expect("fonts.toml");
    let fonts = toml::from_str::<Manifest>(&list).expect("fonts.toml").font;
    let dir = target_dir().join("fonts");
    // Watched for `cargo xtask fetch-fonts`; created so that a missing
    // directory does not rerun this script on every build.
    let _ = std::fs::create_dir_all(&dir);
    println!("cargo:rerun-if-changed={}", dir.display());
    let (found, missing): (Vec<_>, Vec<_>) = fonts.iter().map(|f| dir.join(&f.file)).partition(|p| p.is_file());
    if found.is_empty() {
        println!(
            "cargo:warning=no built-in fonts for temzdf-gui (TEMZDF_BUNDLED_FONTS is not set and {} is empty): \
             `cargo xtask fetch-fonts` downloads them",
            dir.display()
        );
    } else if !missing.is_empty() {
        let names: Vec<_> = missing.iter().filter_map(|p| p.file_name()?.to_str()).collect();
        println!("cargo:warning=temzdf-gui built-in fonts missing: {} (`cargo xtask fetch-fonts`)", names.join(", "));
    }
    found
}

/// Cargo's target directory: `CARGO_TARGET_DIR`, else `target/` at the
/// workspace root (two levels above this crate).
fn target_dir() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).expect("crates/temzdf-gui");
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(d) => root.join(d),
        None => root.join("target"),
    }
}
