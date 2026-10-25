//! Compiles the vendored SyncTeX parser (see vendor/synctex/README.md).

fn main() {
    let dir = "vendor/synctex";
    let mut build = cc::Build::new();
    build
        .file(format!("{dir}/synctex_parser.c"))
        .file(format!("{dir}/synctex_parser_utils.c"))
        .include(dir)
        // Upstream code; its warnings are not ours to fix.
        .warnings(false);
    // zlib headers from libz-sys (built statically, so no system zlib is needed).
    if let Some(inc) = std::env::var_os("DEP_Z_INCLUDE") {
        build.include(inc);
    }
    build.compile("synctex");
    println!("cargo:rerun-if-changed={dir}");
}
