# Testing

Layers of verification, cheapest first. All of them run without a human.

| Layer | How | What it catches |
|---|---|---|
| Unit tests | `scripts/check.sh` | Input parsing, kitty command encoding, compositor diffing (replayed through `termpix::mock`), layout maths, tile rendering vs full-page rendering |
| Miri | `scripts/miri.sh` (part of `scripts/check.sh` when `cargo miri` is there) | Undefined behaviour, data races and leaked threads or memory in the unit tests' Rust code (ours and the Rust dependencies). See "Miri" below. |
| Headless frames | `target/debug/temzdf FILE.pdf --render-frame out.png [--size 100x30] [--cell 10x20] [--page N] [--scale S] [--keys=...]` | What a screen looks like: runs the real `Session` with the shipped default configuration (keys in Vim feedkeys notation, e.g. `jjj<C-b>%`) and compositor, replays the bytes through the mock terminal. Read the PNG to check it. `scripts/frames.sh` renders a fixed set and compares two sets pixel by pixel (for refactorings). |
| Sessions | `Session` unit tests (`crates/temzdf-server`), `tests/e2e/session.actions` | Splitting, focus, composition, shared rendering; detach, list, re-attach, actions from other processes, kill. |
| Simulated terminal | `crates/temzdf/tests/tui.rs` on `crates/tuitest` (part of `scripts/check.sh`) | The real binary behind Ghostty's terminal emulation: what the screen says (text, colours, cursor, image placements), keys in both keyboard modes, session commands end to end (`-s`, detach, `attach`, `ls`, `action`, `kill-session`, exit messages), resizing, and the same inside a private tmux. No X server; about a second for all. |
| Real terminal | `scripts/e2e.sh [-t kitty\|ghostty\|wezterm] [-m zellij\|tmux] ACTIONS -- COMMAND` | Terminal behaviour: probing, transports (shm, inline with `TERMPIX_MEDIUM=direct`), pixel mouse, actual placement semantics, terminal crashes (exit code 3). Screenshots land in `target/e2e/`. |
| Judged e2e | `expect NAME [KEYS]` in an action file (e.g. `tests/e2e/expect.actions`, `viewer.actions`) | The screen matches what `--render-frame` draws after the same input, keys or a replay of the keys and mouse input temzdf handled (exit code 4 otherwise): placements, crops, strip updates, stale images, terminal-side scaling and blending, wheel, drag, clicks, hover. Not PDF rendering itself (both sides use MuPDF). |
| GUI client | `crates/temzdf-gui` unit tests and `--test gpu` (offscreen on lavapipe), `scripts/e2e.sh -t gui [-I]` with `tests/e2e/gui*.actions` | The draw list and key translation; layers and text drawn on a GPU; the window under xfwm4 against `temzdf-gui --render-frame` on the same session; Japanese input through fcitx5 and Mozc; the clipboard. |
| Real terminal (macOS) | `scripts/macos-gui.sh launch kitty\|ghostty\|wezterm -- COMMAND`, then `key`, `click`, `shot`, ... | The same on macOS, step by step instead of action files (`doc/macos.md`). |
| User | Ghostty / WezTerm on a real desktop | The feel of scrolling and zooming. |

Action files live in `tests/e2e/`; each starts with a comment saying how to run it
and what the screenshots should show. To look at many screenshots at once:
`montage target/e2e/*.png -tile 2x -geometry 450x300+4+4 target/e2e/montage.png`.

## Mutation checks

A test written for a fix should fail without the fix. `scripts/mutate.sh
FILE OLD NEW -- COMMAND` replaces OLD (exactly one occurrence) with NEW,
runs COMMAND (after checking it passes unmutated; `-n` skips that) and
restores FILE whatever happens. It answers CAUGHT (exit 0), SURVIVED (1:
the tests do not notice), or why it proves nothing: OLD not found once (2),
the mutated code did not compile (3), no test ran (4: a filter matching
nothing).

## Miri

`scripts/miri.sh [PACKAGE...]` runs the workspace's unit tests (`--lib`,
`--bins`; not `crates/*/tests` or `tuitest`) under Miri, one process per
test and all cores at once, in about a minute and a half. `scripts/check.sh`
runs it after the normal tests, or skips it with a note when the toolchain
has no Miri (the `ci` shell); `--no-miri` skips it on purpose. Isolation is
off, so tests can read files and the real clock.

Miri interprets Rust only. Tests that reach foreign code (MuPDF, the
SyncTeX parser, libghostty-vt, fontconfig, QuickJS), open a pty, spawn a
process (building fixtures counts), or use Unix sockets, shared memory,
`select` or inotify cannot run there; they carry
`#[cfg_attr(miri, ignore = "why")]`, so the reason shows in the output.
Mark a new test the same way when Miri stops on an "unsupported operation".
Tests with deadlines get much longer ones under Miri (`cfg!(miri)`), which
runs code tens to hundreds of times slower; where that would not keep the
test meaningful it is ignored instead.

What Miri found so far: the output writer thread of `termpix` lived on when
every `Output` was dropped without `close` (fixed), and zlib-rs violates
the aliasing rules (`doc/known-issues.md`; those tests are ignored).

## Simulated terminal tests

`crates/tuitest` runs a program on a pty behind `libghostty-vt` (Ghostty's
terminal emulation, kitty graphics included), answers its startup queries
(device attributes, XTVERSION `ghostty(tuitest)`, cell size, graphics), and
gives tests:

- `Term::spawn(argv, &Options)`, or `Term::inside_tmux(...)` to put a private
  tmux server (passthrough on, no status line, no prefix key) in between; `capture_pane()`
  then shows what tmux itself holds.
- Input: `key("C-h")`, `keys(&["C-b", "%"])`, `click(col, row)`,
  `drag((col, row), (col, row))`,
  `type_text("ls\n")`, `paste`,
  `resize(cols, rows)`. Keys are encoded the way the program asked for (legacy
  or the kitty keyboard protocol) by the harness's own translation (a US
  layout, as Ghostty's frontend does), not by the code under test.
- Output: `copied()` (what the program put on the clipboard with OSC 52),
  `screen()` with `lines()`, `cell(x, y)` (text, colours, attributes),
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

`expect` (see the `scripts/e2e.sh` header) takes a screenshot, reads the
geometry temzdf logged at startup (`OUTDIR/state/temzdf/temzdf.log`), renders
the same input headlessly and compares with ImageMagick (`-fuzz 3%`, no pixel
may differ). Results go to `OUTDIR/expect.txt`; `NAME.actual.png`,
`NAME.expected.png` and `NAME.diff.png` show a mismatch. Two forms:

- `expect NAME KEYS`: `--render-frame --keys=KEYS`, the whole key sequence
  since startup, each key run to its end. Send scrolling keys one at a time
  with a pause: quick repeats accelerate in the real terminal (flick) but
  not here.
- `expect NAME`: a replay of all the input since startup with its timing
  (`--render-frame --events`). The input is what temzdf itself handled,
  recorded through `TEMZDF_RECORD_INPUT` (`OUTDIR/input.txt`), not what the
  harness sent: terminals deliver late and in bursts (WezTerm turned wheel
  steps 30 ms apart into 1 to 90 ms), and their pixels differ by one
  (WezTerm). The replay runs time between inputs in 16 ms steps with the
  session's timers, so wheel flicks, easing and link previews (after 300 ms
  resting) come out as in the terminal. It ends as long after temzdf's last
  input as the screenshot came after the last one sent; pause before
  `expect` until animations end. The first screen is up, its pages loaded,
  before the first input (as in the terminal, where the scenario starts
  seconds later).

Rules for scenarios:

- Only `temzdf [--page N] [--scale S] FILE` (one viewer, no session).
- Use the helpers (`wheel`, `dragto`, `click`, `move`, `keys`, `type`), not
  raw xdotool: their input is what reaches temzdf either way, but the
  harness's own record (`OUTDIR/events.txt`) times the screenshot from it.
- A moment that depends on rendering speed (the old tiles scaled right
  after a zoom, before new ones arrive) cannot be judged: the replay waits
  for the workers after each input. Take a plain `shot` there.
- Keep Esc apart from the next key: with legacy encoding (tmux) ESC followed
  quickly by another key reads as Alt+key.
- Works in kitty and Ghostty (both blend in linear light, passed as
  `--linear-blending`; Ghostty without decorations, padding or its
  clipboard toast), WezTerm, and for keys inside tmux and zellij (mouse
  input is not judged there: multiplexers report cells).

Both sides run the same viewer code, so a bug in viewer state shows up on
both and passes: zooming anchored at the top-left instead of the pointer did
(found by the user). Behaviour of the view itself needs unit tests
(e.g. `session::tests::ctrl_wheel_zooms_around_the_pointer`).

`tests/e2e/expect.actions` passes in kitty, WezTerm, tmux and zellij
(2026-10-25) and Ghostty (2026-10-28); `search`, `viewer`, `links`,
`select` and `preview` (mouse input) pass in kitty, WezTerm and Ghostty
(2026-10-28). Comparing against
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
