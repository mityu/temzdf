# Testing

Layers of verification, cheapest first. All of them run without a human.

| Layer | How | What it catches |
|---|---|---|
| Unit tests | `scripts/check.sh` | Input parsing, kitty command encoding, compositor diffing (replayed through `termpix::mock`), layout maths, tile rendering vs full-page rendering |
| Headless frames | `target/debug/temzdf FILE.pdf --render-frame out.png [--size 100x30] [--cell 10x20] [--page N] [--scale S] [--keys=...]` | What a screen looks like: runs the real `Session` with the shipped default configuration (keys in Vim feedkeys notation, e.g. `jjj<C-b>%`) and compositor, replays the bytes through the mock terminal. Read the PNG to check it. `scripts/frames.sh` renders a fixed set and compares two sets pixel by pixel (for refactorings). |
| Sessions | `Session` unit tests (`crates/temzdf-server`), `tests/e2e/session.actions` | Splitting, focus, composition, shared rendering; detach, list, re-attach, actions from other processes, kill. |
| Simulated terminal | `crates/temzdf/tests/tui.rs` on `crates/tuitest` (part of `scripts/check.sh`) | The real binary behind Ghostty's terminal emulation: what the screen says (text, colours, cursor, image placements), keys in both keyboard modes, session commands end to end (`-s`, detach, `attach`, `ls`, `action`, `kill-session`, exit messages), resizing, and the same inside a private tmux. No X server; about a second for all. |
| Real terminal | `scripts/e2e.sh [-t kitty\|ghostty\|wezterm] [-m zellij\|tmux] ACTIONS -- COMMAND` | Terminal behaviour: probing, transports (shm, inline with `TERMPIX_MEDIUM=direct`), pixel mouse, actual placement semantics, terminal crashes (exit code 3). Screenshots land in `target/e2e/`. |
| Judged e2e | `expect NAME KEYS` in an action file (e.g. `tests/e2e/expect.actions`) | The screen matches what `--render-frame` draws after the same keys (exit code 4 otherwise): placements, crops, strip updates, stale images, terminal-side scaling and blending. Not PDF rendering itself (both sides use MuPDF). |
| Real terminal (macOS) | `scripts/macos-gui.sh launch kitty\|ghostty\|wezterm -- COMMAND`, then `key`, `click`, `shot`, ... | The same on macOS, step by step instead of action files (`doc/macos.md`). |
| User | Ghostty / WezTerm on a real desktop | The feel of scrolling and zooming. |

Action files live in `tests/e2e/`; each starts with a comment saying how to run it
and what the screenshots should show. To look at many screenshots at once:
`nix develop -c montage target/e2e/*.png -tile 2x -geometry 450x300+4+4 target/e2e/montage.png`.

## Simulated terminal tests

`crates/tuitest` runs a program on a pty behind `libghostty-vt` (Ghostty's
terminal emulation, kitty graphics included), answers its startup queries
(device attributes, XTVERSION `ghostty(tuitest)`, cell size, graphics), and
gives tests:

- `Term::spawn(argv, &Options)`, or `Term::inside_tmux(...)` to put a private
  tmux server (passthrough on, no status line, no prefix key) in between; `capture_pane()`
  then shows what tmux itself holds.
- Input: `key("C-h")`, `keys(&["C-b", "%"])`, `click(col, row)`,
  `type_text("ls\n")`, `paste`,
  `resize(cols, rows)`. Keys are encoded the way the program asked for (legacy
  or the kitty keyboard protocol) by the harness's own translation (a US
  layout, as Ghostty's frontend does), not by the code under test.
- Output: `screen()` with `lines()`, `cell(x, y)` (text, colours, attributes),
  `cursor`, `images` (kitty placements: cell, size, z) and
  `placeholder_cells()` (images shown through Unicode placeholders, as inside
  tmux).
- Waiting by condition only: `wait_for(what, |screen| ...)`, `wait_text`,
  `wait_exit`. A failed wait panics with the screen and leaves it, plus the raw
  bytes received, in `target/tuitest/<test>.txt` and `.out`.

`crates/temzdf/tests/tui.rs` gives every test its own `XDG_RUNTIME_DIR`,
`XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `TEMZDF_LOG_DIR`, so the user's
sessions, configuration and logs are never touched, and kills leftover
servers at the end. Shells are `/bin/sh` with `PS1='$ '`; a test that needs a
particular program in the first pane points `SHELL` at a script
(`Env::with_shell_script`). Expectations are written in the tests; long
hand-written expected screens may go to files next to them, but never record
snapshots automatically.

Check that a new test can fail: break the code it covers for a moment and see
it fail (the keyboard-mode test fails with the C-h bug fixed in 8598211).

What stays with `scripts/e2e.sh`: differences between real terminals (kitty,
Ghostty, WezTerm, zellij), transports and placement semantics as each terminal
implements them, terminal crashes, and pixels compared against headless
renders.

## Judged e2e scenarios

`expect NAME KEYS` (see the `scripts/e2e.sh` header) takes a screenshot, reads
the geometry temzdf logged at startup (`OUTDIR/state/temzdf/temzdf.log`),
renders the same keys headlessly and compares with ImageMagick (`-fuzz 3%`, no
pixel may differ). Results go to `OUTDIR/expect.txt`; `NAME.actual.png`,
`NAME.expected.png` and `NAME.diff.png` show a mismatch. Rules for scenarios:

- Keys only (mouse actions have no headless equivalent yet), and the KEYS
  argument is the whole sequence since startup.
- Send scrolling keys one at a time with a pause: quick repeats accelerate in
  the real terminal (flick) but not headlessly, where every key settles first.
- Keep Esc apart from the next key: with legacy encoding (tmux) ESC followed
  quickly by another key reads as Alt+key.
- Works in kitty (blending in linear light, passed as `--linear-blending`),
  WezTerm, tmux and zellij. Not in Ghostty (its window decorations move the
  terminal area).

Both sides run the same viewer code, so a bug in viewer state shows up on
both and passes: zooming anchored at the top-left instead of the pointer did
(found by the user). Behaviour of the view itself needs unit tests
(e.g. `session::tests::ctrl_wheel_zooms_around_the_pointer`).

`tests/e2e/expect.actions` passes in all four (2026-10-25). Comparing against
headless renders already found two bugs: kitty darkened the screen edges of the
stretched 1x1 background image, and Enter pressed before search results
arrived left no match selected.

## Fixtures

`tests/fixtures/*.tex` are the sources. `scripts/build-fixtures.sh` builds them
into `target/fixtures/` (PDF and SyncTeX) with a global TeX installation (or
the one in `nix develop .#alldeps`), reproducibly
(`SOURCE_DATE_EPOCH`), and skips fixtures newer than their source. Unit tests
call it on first use (`pdf::fixture`); run it yourself before e2e scenarios.
Generated files are never committed (see AGENTS.md).
