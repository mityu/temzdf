# Known issues

Problems we know about and left in place, with why and what a fix would
take. Most came out of a bug audit in October 2026 (IDs like `PDF-9` are
that report's); the fixed findings are in the git log (`git log --grep`
with the subject, not the ID: commit messages do not carry IDs). Larger
planned work stays in `todo.md`.

## PDF and SyncTeX

- **SyncTeX ignores page rotation and the CropBox (PDF-9).** Forward and
  inverse search map SyncTeX's coordinates (TeX points from the MediaBox's
  top-left) straight onto the rendered page
  (`crates/temzdf-pdfcore/src/synctex.rs`). A rotated page (pdflscape's
  landscape pages, `/Rotate`) or a CropBox that does not start at the
  MediaBox origin puts the highlight, and the spot inverse search looks up,
  in the wrong place. A fix transforms through the page's rotation and
  CropBox origin, as MuPDF's page transform does; it needs a fixture with
  such pages to test against.
- **Copying a long selection leaves out pages not loaded (PV-6).** Selected
  text comes from the pages' display lists; pages whose lists are not loaded
  (far from the view, or evicted) are skipped, and the notice says how many
  (`1ea3f0e`). A full fix loads those pages on the workers first and copies
  once they arrive.

## Terminal panes

- **F26–F35 are not sent (TERM-10).** The key encoder is libghostty-vt's,
  whose key set ends at F25, so higher function keys typed into a terminal
  pane are dropped (`crates/temzdf-term/src/keys.rs`). Few keyboards have
  them; a fix encodes them by hand (kitty protocol: `CSI 57389..57398 u`).

- **Scrollback is limited by bytes, not lines.** A terminal pane keeps
  10 MB of history (`SCROLLBACK_BYTES` in `crates/temzdf-term/src/lib.rs`),
  so the rows kept depend on the pane's width: about 13,600 at 80 columns,
  5,500 at 200. libghostty-vt's `max_scrollback` says lines but is bytes
  (it was 10,000 here once, which kept a few hundred rows). A limit in
  lines waits for the line-limit API in the Rust crate (`todo.md`).

## Session protocol and CLI

- **The protocol version is checked only after decoding (SRV-10).** `Hello`
  carries `VERSION`, but a client and server of different versions may
  already fail to decode each other's first message and report a decode
  error rather than a version mismatch. A fixed-length version prefix
  before the first message would make the mismatch always readable.
  (The rest of that finding, reading lengths incrementally and a deadline
  for the first message, is fixed.)
- **Paths that are not UTF-8 are refused (CLI-7).** The protocol carries
  paths as strings, so the client refuses such paths with an error
  (`864d9ac`) instead of mangling them; so does starting a session whose
  log would be in such a directory (the server reports its log to `ls`).
  Carrying them as bytes (`OsString` on Unix) would accept them.

## Script host

- **A recursion a few hundred calls deep ends the host.** Found on
  2026-11-18 (`doc/survey/script-host-transport.md`, "boa instead of
  QuickJS"): `function g(n){return n?g(n-1)+1:0}` ran at depth 400 and
  trapped at 700 (`wasm trap: call stack exhausted`), even inside
  `try`/`catch`. The host ends and the session's mappings are off until
  `<C-b>r`. QuickJS (quickjs-ng 0.16.2, from `rquickjs-sys` 0.14.0)
  checks its stack against a limit (1 MiB by default, which throws a
  catchable `RangeError`), but not when built for WASI: `JS_NewRuntime2`
  sets the size to 0 and `update_stack_limit` ignores it under `__wasi__`,
  so `JS_SetMaxStackSize` does nothing there. Its check measures the C
  stack in linear memory, not the wasm call stack that runs out here
  (wasmtime's `max_wasm_stack`, 512 KiB by default: probably the limit hit,
  not checked). A larger `Config::max_wasm_stack`
  (`crates/temzdf-script-runner/src/engine_config.rs`, with a thread stack
  to match) only moves the depth.

  Why QuickJS turned the check off for WASI: no reason is recorded. It came
  with quickjs-ng's "Add WASI support" (`fb03ca2`, PR #264, 2024-02),
  neither its message nor the PR mentioning the stack. Probably the limit
  underflowed: the default WASI linker stack (64 KiB in wasm-ld, from
  memory) is smaller than the 1 MiB limit, so "stack top minus limit" goes
  below address 0 (inferred). quickjs-ng PR #1700 (`a2563ad`, merged
  2026-09-16, in v0.17.0) turned it back on: the 1 MiB default on WASI,
  `JS_SetMaxStackSize` effective, the check off only where the limit would
  underflow the stack top, a 2 MiB stack in its own CMake build, and a test
  that an overflow is caught and the runtime goes on. `rquickjs-sys` 0.14.0
  (the newest) still bundles 0.16.2.

  Even with it, the two stacks are independent, which the PR's test says
  itself ("Use a large interpreter frame so QuickJS's linear-memory stack
  guard trips before Wasmtime's independent WebAssembly call stack is
  exhausted": a function of 1024 arguments). How much of each stack a call
  takes depends on the code (interpreted functions, native code recursing
  as in `JSON.stringify`, proxies and getters between them), so no limit
  makes QuickJS's check always come first; our guest also has Rust's 1 MiB
  wasm stack, equal to the default limit, so the check would probably stay
  off unless the limit is set lower (inferred). Tuning the limits is only
  a best effort. The more robust direction: tell a `call stack exhausted`
  trap apart, restart the host on its own and say which call recursed too
  deep, instead of leaving the mappings off until `<C-b>r`.
- **An interrupted task may not report that it ended (part of SCR-4).** When
  the host interrupts a script (past `UNATTENDED_LIMIT`, or on the session's
  interrupt), QuickJS's interrupt is uncatchable: `finally` blocks and promise
  handlers are skipped, so an async task cut off mid-way never sends
  `taskDone` itself. Inside `atomic` this used to wedge the session (all input
  held, SCR-N1); now the host closes an atomic section left open once no code
  runs (`close_cut_off` in `crates/temzdf-script/src/host.rs`) and ends its
  task, and the session lets a task's section go on `<C-c>` and on its
  `taskDone`. A task cut off outside `atomic` still stays in `script.tasks`
  until the host is dropped; nothing waits on it.

## GUI client

- **A DPI set after `temzdf-gui` started on X11 without one is not
  followed**: the scale stays 1 (`WINIT_X11_SCALE_FACTOR`, set at start by
  `crates/temzdf-gui/src/x11scale.rs`) until the window is opened again.
  Following it would need winit to fall back to 1 itself, or the GUI to
  watch XSETTINGS and rescale.

## Dependencies

- **zlib-rs fails Miri's aliasing checks.** Compressing through flate2's
  `zlib-rs` backend (`termpix` transmissions; the tests of `temzdf-term`'s
  PNG and zlib images, which compress their input) makes Miri report
  undefined behaviour inside zlib-rs 0.6.8: dropping a compressor calls
  `deflate::end`, which deallocates the stream state (`src/allocate.rs`)
  while a `&mut` passed as a function argument still points into it, under
  both Stacked and Tree Borrows. No miscompilation is known
  to come of it, and 0.6.8 is the newest release flate2 accepts. Those
  tests are ignored under Miri; check a newer zlib-rs when one comes, or
  report it upstream.
- **MuPDF mishandles an ActualText that ends with the start of its
  glyphs.** In `do_extract_within_actualtext` (`source/fitz/stext-device.c`,
  MuPDF 1.27.2 and master as of 2026-11-03), an ActualText that is all of
  the start of the glyphs it covers ("Ab" over "Abc") leaves no ActualText
  after the matching prefix, which an `assert(z != 0)` says cannot happen
  ("Shut coverity up"). Typst 0.14 writes such PDFs for tall brackets: a
  "(" built from a top piece, extenders and a bottom piece carries the
  ActualText "(", and its ToUnicode CMap maps the top piece alone to "(".
  A debug build of MuPDF aborted there when searching
  or selecting such a PDF, taking the session down; so MuPDF is built
  optimized, without assertions, in debug builds too (`Cargo.toml`,
  `[profile.dev.package.mupdf-sys]`), as it ships. Past the assertion the
  code goes on with the count `z` at 0: the remaining glyphs lose their text
  (searching finds "Ab", not "Abc"), and nothing is read past the string as
  long as no glyph's Unicode value is -1. One that is would match
  `rune_index`'s -1 and make `z--` wrap around (a `size_t`), and the
  following loops read past the end of the ActualText string. MuPDF's PDF
  interpreter gives glyphs without Unicode U+FFFD, so no path to -1 is
  known, but none is ruled out (a crafted ToUnicode CMap, say). The test is
  `search_through_actualtext_that_ends_before_its_glyphs` in
  `crates/temzdf-pdfcore/src/pdf.rs`. A fix belongs in MuPDF (report it
  upstream: handle `z == 0` there by extracting the rest as plain text);
  meanwhile `TextPageFlags::IGNORE_ACTUALTEXT` for search and selection
  would avoid the code entirely, at the price of searching what the glyphs
  say instead of their ActualText.

## Tests and CI

- **CI runs tests on Linux only.** macOS gets `nix build` but not
  `scripts/check.sh`, so platform-specific code (socket path limits,
  `SUN_PATH` 104, shm, the pty) is tested on macOS only by hand
  (`doc/terminals/macos.md`).
- **`Term::settle` can stop early under load.** It returns after `quiet`
  passes with no output, which says nothing came in that time, not that the
  program is done. Tests should wait for what they expect (`wait_for`,
  `wait_text`) and use `settle` only to let trailing frames pass.
