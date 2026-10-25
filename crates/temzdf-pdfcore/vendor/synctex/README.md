# Vendored SyncTeX parser

Unmodified copy of the SyncTeX parser library, version 1.21 (CLI 1.5), from TeX
Live: `texk/web2c/synctexdir/` in https://github.com/TeX-Live/texlive-source
(trunk, fetched 2026-10-25).

Files: `synctex_parser.c`, `synctex_parser.h`, `synctex_parser_advanced.h`,
`synctex_parser_utils.c`, `synctex_parser_utils.h`, `synctex_version.h`.

License: MIT/X11-style, by Jérôme Laurens (see the header of each file);
compatible with this project's AGPL-3.0.

Built by `crates/temzdf-pdfcore/build.rs` with zlib from the `libz-sys` crate. The Rust
bindings (a handful of functions) are in `crates/temzdf-pdfcore/src/synctex.rs`. To
update, replace the files with a newer upstream version and run the SyncTeX
tests (`cargo test -p temzdf-pdfcore synctex`).
