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

## Tests and CI

- **CI runs tests on Linux only.** macOS gets `nix build` but not
  `scripts/check.sh`, so platform-specific code (socket path limits,
  `SUN_PATH` 104, shm, the pty) is tested on macOS only by hand
  (`doc/macos.md`).
- **`Term::settle` can stop early under load.** It returns after `quiet`
  passes with no output, which says nothing came in that time, not that the
  program is done. Tests should wait for what they expect (`wait_for`,
  `wait_text`) and use `settle` only to let trailing frames pass.
