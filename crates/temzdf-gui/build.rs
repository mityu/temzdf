//! Embed the fonts listed in `TEMZDF_BUNDLED_FONTS` (paths joined by `:`,
//! set by the Nix dev shell and package: `bundledFontsFor` in flake.nix) as
//! `BUNDLED`. Without it the GUI builds with none and relies on the
//! system's fonts.

use std::fmt::Write;

fn main() {
    println!("cargo:rerun-if-env-changed=TEMZDF_BUNDLED_FONTS");
    let fonts = std::env::var("TEMZDF_BUNDLED_FONTS").unwrap_or_default();
    let paths: Vec<&str> = fonts.split(':').filter(|p| !p.is_empty()).collect();
    if paths.is_empty() {
        println!("cargo:warning=TEMZDF_BUNDLED_FONTS is not set: temzdf-gui has no built-in fonts");
    }
    let mut out = String::from("/// The fonts built in (build.rs).\npub const BUNDLED: &[&[u8]] = &[\n");
    for p in paths {
        assert!(std::path::Path::new(p).is_file(), "TEMZDF_BUNDLED_FONTS: no file {p}");
        println!("cargo:rerun-if-changed={p}");
        writeln!(out, "    include_bytes!({p:?}),").unwrap();
    }
    out.push_str("];\n");
    let dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(std::path::Path::new(&dir).join("bundled_fonts.rs"), out).unwrap();
}
