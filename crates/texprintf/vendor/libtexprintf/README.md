# Vendored libtexprintf

Unmodified copy of the library sources of libtexprintf (the library behind
`utftex`) version 1.31, tag `v1.31` of https://github.com/bartp5/libtexprintf
(commit 18977837b20649d56a651eb6bf846f1c914db77a), the same version the
Nix overlay that built it before (removed) used.

Files, all from upstream's `src/`: `boxes.c`, `drawbox.c`, `error.c`,
`lexer.c`, `parser.c`, `stringutils.c`, `texprintf.c` and the headers they
include (`boxes.h`, `drawbox.h`, `drawchars.h`, `error.h`, `lexer.h`,
`mapunicode.h`, `parsedef.h`, `parser.h`, `stringutils.h`, `texprintf.h`,
`unicodeblocks.h`); `COPYING` and `AUTHORS` from the top level. Not taken:
the `utftex` and `utfstringinfo` programs (`main.c`, `testfonts.*`,
`utf2unicode.c`), manual pages, the autotools files, the tests, the npm
package, and the generated files (below).

License: GPL-3.0-or-later (`COPYING`), which this project's AGPL-3.0 can
combine with.

Built by `crates/texprintf/build.rs` with the `cc` crate. `errorflags.h` and
`errormessages.h` are not upstream files: upstream's `gen_errorflags.sh`
writes them from the `// ERRORFLAG NAME "message"` comments in `boxes.c`,
`drawbox.c`, `lexer.c` and `parser.c`, and `build.rs` does the same in Rust
(into `OUT_DIR`, so no shell is needed). The Rust declarations (three
functions) are in `crates/texprintf/src/lib.rs`. To update, replace the
files with a newer upstream version, check the file lists in `build.rs`
against upstream's `src/Makefile.am`, and run `cargo test -p texprintf -p temzdf-markdown`.
