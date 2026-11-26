# Testing

Layers of verification, cheapest first. All of them run without a human.

| Layer | How | What it catches |
|---|---|---|
| Unit tests | `cargo xtask check` (the gate; `cargo xtask test` runs the tests alone) | Input parsing, kitty command encoding, compositor diffing (replayed through `termpix::mock`), layout maths, tile rendering vs full-page rendering |
| Miri | `scripts/miri.sh` (`cargo xtask check --miri`) | Undefined behaviour, data races and leaked threads or memory in the unit tests' Rust code (ours and the Rust dependencies). See "Miri" below. |
| Headless frames | `target/debug/temzdf standalone pdf FILE.pdf --render-frame out.png [--size 100x30] [--cell 10x20] [--page N] [--scale S] [--keys=...]` | What a screen looks like: runs the real `Session` with the shipped default configuration (keys in Vim feedkeys notation, e.g. `jjj<C-b>%`) and compositor, replays the bytes through the mock terminal (images only: `--text FILE` writes the status line's text, `--mask PNG` where it is, `--ansi FILE` its cells as bytes for a terminal to draw, `--cells PNG` which of them a terminal's drawing can be compared in). Read the PNG to check it. `scripts/frames.sh` renders a fixed set, with the `shots` scenes of `tui.rs` (sessions, Markdown, terminal panes, tabs), and compares two sets pixel by pixel (for refactorings). |
| Script tests | `target/debug/temzdf test [-j N] [--timeout SECS] FILE...` (part of `cargo xtask check` for `crates/temzdf-script/runtime/**/*.test.ts`) | temzdf's behaviour driven from TypeScript (`temzdf/test`, QUnit): keys typed into a session and what the scripting API then sees (buffers, modes, registers). Each file runs in a session of its own in a process of its own, its `XDG_*`, `TEMZDF_LOG_DIR` and `TMPDIR` in a temporary directory; TAP out, a file printing nothing for `--timeout` (30 s) stopped and failed. |
| Sessions | `Session` unit tests (`crates/temzdf-server`), `tests/e2e/session.ts` | Splitting, focus, composition, shared rendering; detach, list, re-attach, actions from other processes, kill. |
| Simulated terminal | `crates/temzdf/tests/tui.rs` on `crates/tuitest` (part of `cargo xtask check`) | The real binary behind Ghostty's terminal emulation: what the screen says (text, colours, cursor, image placements), keys in both keyboard modes, session commands end to end (`-s`, detach, `attach`, `ls`, `action`, `kill-session`, exit messages), resizing, and the same inside a private tmux. No X server; about a second for all. |
| Real terminal | `scripts/e2e.sh [-t kitty\|ghostty\|wezterm] [-d x11\|wayland\|macos] [-m zellij\|tmux] SCENARIO [-- COMMAND]` | Terminal behaviour: probing, transports (shm, inline with `TERMPIX_MEDIUM=direct`), pixel mouse, actual placement semantics, terminal crashes (exit code 3). Screenshots land in `target/e2e/`. |
| Judged e2e | `expect(NAME, KEYS?)` in a scenario (e.g. `tests/e2e/expect.ts`, `viewer.ts`) | The screen matches what `--render-frame` draws after the same input, keys or a replay of the keys and mouse input temzdf handled (exit code 4 otherwise): placements, crops, strip updates, stale images, terminal-side scaling and blending, wheel, drag, clicks, hover. Not PDF rendering itself (both sides use MuPDF). |
| GUI client | `crates/temzdf-gui` unit tests and `--test gpu` (offscreen on lavapipe), `scripts/e2e.sh -t gui [-I]` with `tests/e2e/gui*.ts` | The draw list and key translation; layers and text drawn on a GPU; the window under xfwm4 against `temzdf-gui --render-frame` on the same session; Japanese input through fcitx5 and Mozc; the clipboard. |
| Real terminal (macOS, by hand) | `scripts/macos-gui.sh launch kitty\|ghostty\|wezterm -- COMMAND`, then `key`, `click`, `shot`, ... | Poking at a terminal step by step (`doc/terminals/macos.md`); `launch app` runs `temzdf-gui`, and `tests/macos/*.sh` are scripted scenarios. `scripts/e2e.sh` runs the scenarios on macOS too (not run since the move to TypeScript: `todo.md`). |
| User | Ghostty / WezTerm on a real desktop | The feel of scrolling and zooming. |

Scenarios live in `tests/e2e/` (`NAME.ts`, on the harness in
`scripts/e2e/`, Node TypeScript run on `node:test`); each holds its command
and starts with a comment saying what it judges and what the screenshots
should show. To look at many screenshots at once:
`montage target/e2e/*.png -tile 2x -geometry 450x300+4+4 target/e2e/montage.png`.

## Where to start

Behaviour visible on screen (terminal panes, sessions, CLI, keys) is best
tested in `crates/temzdf/tests/tui.rs`: the real binary behind a simulated
terminal (`crates/tuitest`), checked by text, cells and image placements, also
inside tmux, without an X server. See "Simulated terminal tests" below.

End-to-end checks in a real kitty (virtual X server, no display needed):

```sh
cargo build --example scroll_demo
scripts/e2e.sh tests/e2e/scroll_demo.ts
```

Scenarios in `tests/e2e/` drive the app (wheel, drag, click, keys) and save
screenshots to `target/e2e/`; read the PNGs or measure pixels to verify. For
key-driven checks prefer `expect NAME KEYS`, which compares the terminal with
a headless render and fails the run on a mismatch (`tests/e2e/expect.ts`,
"Judged e2e scenarios" below). `scripts/e2e/helpers.ts` documents the
helpers, `scripts/e2e/main.ts` the options. `temzdf standalone pdf FILE --render-frame out.png` renders one
screen headlessly. `scripts/build-fixtures.sh` builds `target/fixtures/`
from `tests/fixtures/*.tex` (tests do it automatically; e2e scenarios
need it first; "Fixtures" below).

For refactorings, `scripts/frames.sh render-rev HEAD A` (the last commit),
`scripts/frames.sh render B` (your build) and `scripts/frames.sh compare A B`
prove the screen is pixel-identical: PDF frames and the `shots` scenes of
`crates/temzdf/tests/tui.rs` (sessions, Markdown, terminal panes, floats,
tabs), with an image of each difference in `B/diff/`.

A test written for a fix should fail without it: `cargo xtask mutate FILE
OLD NEW -- cargo test ...` puts the bug back for one run and restores the
file ("Mutation checks" below).

`scripts/e2e.sh -t gui ... -- target/debug/temzdf-gui --clean FILE` drives the GUI
client the same way (xfwm4 on Xvfb, lavapipe; `-I` adds fcitx5 with Mozc for
Japanese input); its GPU tests (`cargo test -p temzdf-gui --test gpu`) run
offscreen on lavapipe.

`scripts/e2e.sh -d wayland ...` runs the same scenarios on headless sway
instead of Xvfb (every terminal and the GUI, `-I` too; keys and pointer
through `crates/wlinput`, screenshots with grim).

Every judged scenario in every terminal and the GUI, summed up:
`scripts/e2e-all.sh -R -j 8` (about a minute and a half; `-d wayland` for
sway; `-R` renders the comparisons with release builds, `-j` runs
scenarios at once); the GUI's `gui-focus` and `gui-ime` (`-I`) included,
not on macOS. On macOS, read `doc/terminals/macos.md` ("Checking GUI
terminals on a Mac without a human"): the same scenarios run there (not run since
the move to TypeScript: `todo.md`),
best in a VM (`scripts/macos-vm.sh e2e ARGS...`).

## The gate

```sh
cargo xtask check            # fmt check + Biome (scripts) + declarations + docs + clippy -D warnings + tests + js tests, terse output
cargo xtask check --fix      # same, but apply rustfmt and Biome's formatting and safe fixes
cargo xtask check --miri     # also Miri (about a minute and a half more); --no-miri is the default, accepted
cargo xtask check --all      # the tests even when only Markdown under doc/ changed (skipped then)
cargo xtask check --verbose  # a failed step's whole output; always in target/check/
cargo xtask check --stream   # each step's output as it comes, under a header (CI: log groups)
cargo xtask test [ARGS...]   # the tests alone, as the gate runs them
cargo build --workspace
```

`cargo xtask check` (`xtask/src/check.rs`) is the pre-commit gate:
rustfmt, Biome, the generated declarations (`node scripts/ts-types.ts`
writes `temzdf-all.d.ts` again into `target/check/` and compares), the
Node scripts' types (`tsc -p scripts`, and the e2e scenarios' with
`tsc -p tests/e2e`) and tests (`node --test` on `scripts/**/*.test.ts`:
what the e2e harness computes without a terminal), shellcheck, references to
documents (`node scripts/check-docs.ts`), clippy, a clippy run for
`x86_64-pc-windows-gnu` over the crates that build there (step
`windows`, Unix only; `SKIP windows` without that standard library),
and the tests (and Miri with `--miri`). Each
step's whole output is in `target/check/STEP.log`; a failed step prints
`FAIL STEP` and its main lines, a passing run one `OK ...` line. When every
uncommitted change is Markdown under `doc/`, the tests are
skipped (`--all` or cargo test arguments run them). `--stream` prints each
step's output as it comes under a `==> STEP` header, or in a log group of
its own under GitHub Actions (`GITHUB_ACTIONS=true`), as CI runs it.

Runs of the gate in one checkout wait for each other: `cargo xtask check`
and `cargo xtask test` hold an exclusive lock on `target/check/lock` (the
holder's PID is in it; the system drops it when the process ends) and
print `waiting for another run of the gate (PID N) to finish` while
another run has it. Agents sharing a tree need not wait themselves, and
a worktree with its own `target/` does not wait for another's.

It is Rust rather than shell so that it can run on Windows one day: it
assumes no Nix and finds its tools on PATH (Biome and shellcheck also in
the npm packages' `.bin`), says what to install when a required one
(cargo, node, Biome, `runtime/node_modules`) is missing, and prints `SKIP
shellcheck` or `SKIP miri` where those Unix-only steps cannot run.
`scripts/check.sh` takes the same options and runs it inside the Nix dev
shell, entering it from any shell. The JavaScript tooling around it is
TypeScript run by Node as it is (type stripping, Node 22.18 or later), so
erasable syntax only and `.ts` in imports; `check-docs.ts` uses Node's
standard library alone, so the documents' CI runs it without `npm ci`.

## Fonts

Tests that draw text (the Markdown viewer's pages, `termpix-ui`'s widgets,
the session's frames) get their fonts from fontconfig on Linux, so a host's
fonts would change what they draw. `cargo xtask check` points
`FONTCONFIG_FILE` at `TEMZDF_TEST_FONTCONFIG`, a configuration the dev
shells provide (`testFontConfig` in `flake.nix`): the fonts `temzdf-gui`
embeds alone (JetBrains Mono, Noto Color Emoji, Symbols Nerd Font Mono),
every generic family (`sans-serif`, `monospace`, ...) JetBrains Mono first,
and nothing of the host's (no font directories of its, no `/etc/fonts`).
CI and every machine therefore draw the same text; `cargo test` run by hand
uses the host's fonts unless `FONTCONFIG_FILE` is set the same way. nixpkgs'
`makeFontsConf`, which CI used with DejaVu before (2026-11-16), adds the
host's directories and settings: on the CI runner Ubuntu's fonts came in,
here Noto Sans CJK, and a test passed here and failed there. macOS finds
fonts through Core Text, not fontconfig: tests there see the system's.

## Server tests

The `Session` tests in `crates/temzdf-server` come in three kinds, by what
they exercise:

- **Commands**: what a command does is tested by calling it, with
  `run_command` (the helper `cmd` in `session.rs`'s tests), in a session
  without a script host. Pressing the shipped keys there would test
  mappings that are not the shipped ones.
- **The matcher and the modes** (sequences, counts, transient modes,
  fallbacks, the recursion guard, `feedkeys`, text from input methods
  matched as keys, the visual mode stacked under the normal mode): tests'
  mappings of commands, the data form of a mapping (`Target::Actions`,
  `default_mappings` in `session.rs`), which the session runs at once. It
  exists under `#[cfg(test)]` only: scripts map functions, so the release
  build has no such mappings.
- **The shipped keys** (`crates/temzdf-script/runtime/`: the prefix, the
  normal mode, PDF and Markdown panes): sessions with the real script host
  and the defaults alone, in `session/host_tests.rs`, which checks the
  state keys leave. Its helpers (`host_session`, `settle`, `type_keys`)
  serve the Vim cases (`session/vim_tests.rs`) too. Counts are a table:
  a key pressed N times against the key with the count N, so a mapping
  that drops `c.count` fails; add a row for a new counted key.

Programs that tests run in terminal panes (and on tuitest's pty) are
`temzdf-testhelper` (its commands: the documentation of
`crates/temzdf-testhelper/src/lib.rs`), not `sh -c`, so that the tests run
on Windows too: `helper(&[...])` in the tests builds its command line, and
splits start `test_shell()`, which answers each line typed with
`said:LINE`. Write a missing command into the helper rather than a script
for `sh`; only tests about Unix itself (signals) run `sh`.

## Script tests

Tests of temzdf's behaviour can be TypeScript files run in a real
session, as Vim tests itself in Vim script: the module `temzdf/test`
(`crates/temzdf-script/runtime/test/index.ts`, on QUnit 3) and
`temzdf test`. The design is in `doc/design/scripting.md` (decision 31).

**Which kind to write.** A script test when what matters is what keys
and scripts do to the state scripts can read: text, cursor, modes,
registers, mappings, buffers, panes and tabs, messages shown. The
shipped scripts (the Vim mode, Ex commands, keysets) are best tested
this way, beside their code. A Rust test (`crates/temzdf-server`, "Server
tests" above) for what the session does that scripts cannot see or must
not depend on (the matcher's internals, the protocol, timing, the swap
file's bytes), and for commands without a script host. A
`crates/temzdf/tests/tui.rs` test for anything on screen (text, colours,
cursor, images), the CLI and sessions end to end: script tests do not
look at the screen.

**Where.** `crates/temzdf-script/runtime/**/*.test.ts`, beside what they
test (`runtime/command.test.ts`, `runtime/vim/ex.test.ts`). They are not
bundled, and editing one rebuilds nothing; Biome and tsc check them with
the other scripts.

**Running.**

```sh
cargo xtask test                    # with the Rust tests (also part of cargo xtask check)
target/debug/temzdf test FILE...    # some files (build temzdf first)
target/debug/temzdf test -j 4 --timeout 60 FILE...
```

Each file runs in a process and a session of its own (its `XDG_*`,
`TEMZDF_LOG_DIR` and `TMPDIR` in a temporary directory: nothing reaches
your sessions, swap files or configuration), with the shipped defaults
loaded and the file loaded as the configuration, on an empty
`scratch.txt` in that directory. `-j N` runs N files at once (the number
of CPUs by default). The output is each file's TAP in one piece (its
stderr too when it failed: `console.log` and script errors go there),
then the totals; the exit status is 1 when a test failed. A file printing
no TAP line for `--timeout` seconds (30) is stopped and failed: a
synchronous loop in a test (the script host stops a synchronous job after
10 s, which breaks QUnit's chain, and the file goes quiet).

**Writing one.** `runtime/vim/vim.test.ts` is a short example.

```ts
import { keymap } from "temzdf";
import { module, test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

test("dd deletes a line and u brings it back", async (assert, t) => {
  const pane = t.openText("one\ntwo\n");
  await t.keys("dd");
  assert.equal(t.action.text.getText(undefined, { pane }), "two\n");
});
```

- `test(name, fn, { timeout? })`: `fn(assert, t)`, usually `async`.
  `assert` is QUnit's (`equal`, `strictEqual`, `deepEqual`, `ok`,
  `true`, `false`, `throws`, `rejects`, `step`/`verifySteps`,
  `expect(n)`; a test with no assertion fails unless `assert.expect(0)`).
  Failures show both values with loupe (Maps, Sets, class names, `-0`).
  A test is stopped and fails after `timeout` ms (10 000).
- `module(name, hooks?, scope?)`: groups the tests registered in `scope`
  (or, without one, those after it until the next `module`) under
  `name`; `hooks.beforeEach(assert, t)` and `afterEach(assert, t)` run
  around each, inside its timeout.
- `t.keys(keys, { flush?, remap?, allowErrors? })`: type keys (Vim's
  notation) one at a time, each once the session settled, and wait until
  it settled again (mapped functions done, no script running or waiting
  to run, nothing loading). It resolves with `{ errors, messages,
  pending, mode }`. `flush` (true) resolves a prefix still waiting for a
  longer mapping as if its time ran out (false: it stays, in `pending`);
  `remap: false` types the keys without mappings. An error message shown
  meanwhile fails the test unless `allowErrors` (then read `errors`).
- `t.settle({ flush? })`: wait until the session settled, typing nothing.
- `t.action`: the session's actions (the test runs in a dispatched task,
  where the plain `action` does not work). `t.run(fn)`: run synchronous
  `fn` with everything else held, where `action`, `o`, `keymap` and the
  modules' functions work; it does not wait across keys.
- `t.openText(text, { basemode? })`: a new text buffer (no file) in a
  new pane beside the focused one, focused; returns the pane. Both go
  after the test.
- `t.after(fn)`: run `fn` after the test, passed or failed (the last
  registered first). `t.signal`: aborted when the test timed out; the
  helpers then throw instead of typing.
- `test.hooks(event)` (`t.action.test.hooks`) counts the hooks on an
  event, to see that a module took its own away.
- The file's top level only registers tests (no top-level `await`; it
  must load within 5 s). They start once the session is ready.

**Independence.** The tests of a file share its session. Before each
test the session is brought back to a known state: settled,
`test.reset()` (pending keys dropped, the focused pane's base mode), the
Vim mode's `reset()` (an insert, a selection, a macro being recorded), a
prompt closed. Registers, buffers, panes, options and mappings are not
reset: a test that changes them does so through a helper that undoes it
(`t.openText`) or undoes it itself in `t.after` (`keymap.clear({
namespace })`), and a test that needs, say, an empty register sets it
first. Never depend on the tests before: setting `config.seed = "x"`
(the `config` export of `temzdf/test`, QUnit's `QUnit.config`) at the
top of a file runs its tests in an order shuffled by that seed, to find
tests that do (`config.reorder`, QUnit's running first a test that
failed last time, needs storage the script host does not have). A test that needs a session of its own goes into a file of its
own.

**Limits.**

- A test file imports `"temzdf"`, `"temzdf/NAME"` and relative paths
  only, and a relative import loads the source unbundled (another
  instance, whose npm imports fail): import the shipped modules as
  `temzdf/NAME`. Functions they do not export are not tested from here.
- Type stripping takes erasable TypeScript only: no parameter
  properties (`constructor(private x)`), enums or namespaces.
- Scripts cannot write files (the guest's file system is read-only):
  buffers come without a file (`t.openText`); the session itself writes
  (`action.write`, `:w`) where a test needs a file.
- No screen: what is drawn is tui.rs' business.

## Mutation checks

A test written for a fix should fail without the fix. `cargo xtask mutate
FILE OLD NEW -- COMMAND` (`scripts/mutate.sh` is the same inside the Nix dev
shell, entered by itself) replaces OLD (exactly one occurrence) with NEW,
runs COMMAND (after checking it passes unmutated; `-n` skips that) and
restores FILE whatever happens. It answers CAUGHT (exit 0), SURVIVED (1:
the tests do not notice), or why it proves nothing: OLD not found once (2),
the mutated code did not compile (3), no test ran (4: a filter matching
nothing).

## Miri

Under Miri, `crates/temzdf-script-runner/build.rs` builds no guest and
embeds nothing (Miri runs no WebAssembly); the guest's tests are in
`crates/temzdf-script-runner/tests/`, which Miri does not run.

`scripts/miri.sh [PACKAGE...]` runs the workspace's unit tests (`--lib`,
`--bins`; not `crates/*/tests` or `tuitest`) under Miri, one process per
test and all cores at once, in about a minute and a half. `cargo xtask check --miri`
runs it after the normal tests (with a note when the toolchain has no
Miri); without `--miri` the gate skips it (`--no-miri` is accepted and does
nothing). Miri is not run routinely (not per commit, not per plan): it runs only
when someone asks (`cargo xtask check --miri`, or `env -u IN_NIX_SHELL
scripts/miri.sh` from inside the dev shell), and the user runs it from time
to time. A change that adds unsafe or FFI code should say Miri was not run. The
default dev shell leaves Miri out: it is in
`nix develop .#enable-miri` (and the `ci-miri` and `alldeps` shells), run
from time to time (`scripts/miri.sh` enters it by itself when started
outside a dev shell); CI runs it in a job of its own. Isolation is
off, so tests can read files and the real clock.

Miri interprets Rust only. Tests that reach foreign code (MuPDF, the
SyncTeX parser, libghostty-vt, fontconfig, QuickJS), open a pty, spawn a
process (building fixtures counts), or use Unix sockets, shared memory,
`select` or inotify cannot run there; they carry
`#[cfg_attr(miri, ignore = "why")]`, so the reason shows in the output.
Mark a new test the same way when Miri stops on an "unsupported operation".
Tests with deadlines get much longer ones under Miri (`cfg!(miri)`), which
runs code tens to hundreds of times slower; where that would not keep the
test meaningful it is ignored instead. Likewise tests on large inputs or
many random cases run smaller under Miri, or are ignored there: one such
test took hours and held CI's Miri job past two hours. `scripts/miri.sh`
stops a test still running after 15 minutes (`MIRI_TEST_TIMEOUT`, in
seconds) and reports it as failed.

What Miri found so far: the output writer thread of `termpix` lived on when
every `Output` was dropped without `close` (fixed), and zlib-rs 0.6.8
violated the aliasing rules when a compressor was dropped (fixed upstream
in 0.6.9).

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
- Cleanup: dropping a `Term` removes the `/dev/shm/termpix-PID-*` images that
  the program tree (and a private tmux server's) left unread; `tui.rs` gives
  each test its own `TMPDIR`, so temp-file transfers go with the test directory.
- Waiting by condition only: `wait_for(what, |screen| ...)`, `wait_text`,
  `wait_exit`. A failed wait panics with the screen and leaves it, plus the raw
  bytes received, in `target/tuitest/<test>.txt` and `.out`. A `Term`
  answers its program's queries only while the test pumps it (waits,
  `settle`): with several programs starting at once, wait on them together
  with `Term::wait_for_all`, or one's capability probe (one second) can go
  unanswered while the test waits on another and it starts without
  graphics.
- Pixels: `Screen::render()` draws the screen as a terminal would, for
  any kind of pane, in the kitty graphics protocol's layers as Ghostty
  draws them: the default background, images below the backgrounds, the
  cells' backgrounds, images with a negative z, the text, then the other
  images, each image cropped to its source rectangle, scaled by nearest
  neighbour to its size and placed at its cell plus offsets. The text is
  laid out one character per cell with `termpix-ui` (family `monospace`,
  bold; italic is drawn upright, `termpix-ui` has none) in the cell's
  colours (`Cell::fg_rgb`, `Cell::bg_rgb`, inverse swapped), centred in
  its cell (both cells for a wide character), at the largest size whose
  advance fits the cell's width and line height its height. Its fonts come
  from fontconfig, so they are the ones of the section "Fonts" only with
  `FONTCONFIG_FILE=$TEMZDF_TEST_FONTCONFIG` (`cargo xtask check` sets it):
  CJK has no font there and shows as `.notdef` boxes, and box drawing
  characters are the font's glyphs, not lines drawn as terminals do. The
  cursor is not drawn, nor images shown through Unicode placeholders
  (inside tmux). `text_mask()` marks the cells holding text, to leave
  out of comparisons that should not depend on the fonts.
  `save_render(NAME)` writes both to `target/tuitest/NAME.png` and
  `NAME.mask.png` (`a_pdf_screen_renders_to_pixels`,
  `a_markdown_screen_renders_to_pixels`,
  `a_markdown_text_screen_renders_to_pixels`, and
  `*_shot_is_the_same_each_time` checking two runs give the same pixels). Images do not prove a screen
  finished: the Markdown viewer places a blank stand-in for each strip not
  painted yet. The tests wait for the client's log (`temzdf.log`) to say
  last that a complete frame was written (`Env::wait_complete` in tui.rs,
  or `Env::complete_after` to send keys and wait in one call), as
  `scripts/e2e.sh` does; frames of text alone write no line, so the same
  wait serves screens without images.
- What the renderer will not add: underlines, strikethrough, the cursor,
  box drawing drawn as lines, ligatures, italics, images shown through
  tmux's placeholders. It is for comparing a screen before and after a
  change on the same machine and for people to look at, so it has to be
  the same each time, not look like a real terminal; what a real terminal
  shows is `scripts/e2e.sh`'s.

The scenes for `scripts/frames.sh` are the `shots` test in `tui.rs`
(ignored unless asked for): each one a function returning the `Screen`
to render, listed in `SCENES` by the name of its file. `frames.sh render
OUTDIR` runs it into `OUTDIR/tui/` with `TEMZDF_SHOTS_DIR` (where the
PNGs go; required) and `TEMZDF_SHOTS_BIN` (the temzdf to run, else this
build's), so `render-rev` plays today's scenes with an older revision's
binary; a scene that binary cannot play is named and left out, and
`compare` says `MISSING`. `compare` writes an image of each difference
to `DIR_B/diff/`. To add a scene, write its function (`env(NAME)` runs
the binary under test from the environment's directory) and add it to
`SCENES`; it has to come out the same each time: fixed file names
(files of its own opened by relative paths: no temporary directory on
the screen), the shell's `$ ` prompt, no clocks or notices that go away
by time, and a wait for a complete frame at the end. Check it with three `render`s and `compare`s
of the same build. To run it alone:
`FONTCONFIG_FILE=$TEMZDF_TEST_FONTCONFIG TEMZDF_SHOTS_DIR=DIR cargo test
-p temzdf --test tui shots -- --ignored`.

`crates/temzdf/tests/tui.rs` gives every test its own `XDG_RUNTIME_DIR`,
`XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `TEMZDF_LOG_DIR`, so the user's
sessions, configuration and logs are never touched, and kills leftover
servers at the end. Shells are `/bin/sh` with `PS1='$ '`; a test that needs a
particular program in the first pane points `SHELL` at a script
(`Env::with_shell_script`). Expectations are written in the tests; long
hand-written expected screens may go to files next to them, but never record
snapshots automatically.

`tui.rs` is Unix only. On Windows `crates/temzdf/tests/attach_windows.rs`
runs `temzdf` in tuitest there, which is the in-box ConPTY (no kitty
graphics, its own early DA1 answer): the message about the missing
graphics, keys, a resize, detaching and attaching again. Its PDF is
written by the test (no TeX for the fixtures there).

Clients take pixels in shared memory by default; tests whose frames carry
pixels run both ways (`both_pixel_paths!`: `NAME::shared` and `NAME::socket`,
through `Env::with_pixels` and `TEMZDF_PIXELS`), and
`clients_of_one_session_take_pixels_each_their_own_way` attaches one client of
each kind to a session. `scripts/e2e.sh -P socket` does the same for e2e runs.

Check that a new test can fail: break the code it covers for a moment and see
it fail (the keyboard-mode test fails with the C-h bug fixed in 8598211).

A test that fails now and then, or only in CI (GitHub's runners have 2 to 4
cores), often depends on which process gets there first. Run the tests on
one or two CPUs to make the slow order the usual one:

```sh
cargo test -p temzdf --test tui --no-run       # then, with the binary it names:
taskset -c 0 target/debug/deps/tui-HASH -q     # one CPU; `-c 0,1` for two
```

On one CPU `forward_search_goes_to_the_session_showing_the_pdf_or_starts_one`
failed every time (the PDF took the forward search before the first client
attached, and attaching resized the pane mid-scroll; fixed in 02949b7).
When a wait times out, the screen and the bytes received are in
`target/tuitest/` (CI uploads them with `target/check/`, see `.github/workflows/ci.yml`).

The shell matters too: Ubuntu's `/bin/sh` (CI's) is dash, which has no line
editing, so input typed before it was ready is echoed before its prompt and
the command's output follows the prompt on its line (`$ mapped-42`); bash
with readline shows the input again after the prompt. `TEMZDF_TEST_SHELL`
runs the tests with another shell, e.g. `TEMZDF_TEST_SHELL=$(nix build
--print-out-paths nixpkgs#dash)/bin/dash`; tests compare a command's
output with `shows_output`, which takes either.

Searches are typed into a prompt by the shipped configuration (`/`):
the PDF pane's side is tested with `PdfPane::search_preview`,
`search_for` and `search_cancel` (incremental search, Enter before
results, going back), the prompt's in the session's tests (typed text,
the cursor, hooks), the whole in `tui.rs` and the e2e searches
(`tests/e2e/search.ts`, `expect.ts`' `search` and `next`).

What a terminal shows *between* frames (a flicker) does not show on the
screen a test reads at the end. For those, record what temzdf wrote
(`TERMPIX_TRACE` in the test's environment) and feed it to
`termpix::mock::MockTerminal`: `take_shown_cursors` lists where the cursor
was visible at every moment a terminal may draw (outside synchronized
updates, and as each ends), so a cursor that blinked elsewhere for a moment
fails the test whatever the timing
(`the_cursor_stays_at_the_prompt_while_a_program_rewrites_the_line_above`).

What stays with `scripts/e2e.sh`: differences between real terminals (kitty,
Ghostty, WezTerm, zellij), transports and placement semantics as each terminal
implements them, terminal crashes, and pixels compared against headless
renders.

## X11, Wayland and macOS

Scenarios use the helpers only (`keys`, `type`, `click`, `down`/`up`,
`move`, `wheel`, `hold`, `clipboard`, `focusWindow`, ...); the helpers
drive the display server through a backend each (`scripts/e2e/backends/`).
They are handed to the scenario bound to its test's abort signal: after a
timeout (180 s by default) or Ctrl+C nothing more is sent, and everything
the run started ends (`scripts/e2e/procs.ts`). `-d x11` (the default): Xvfb, xdotool, `import`, xclip,
xwininfo, xfwm4 for the GUI. `-d wayland`: headless sway
(`WLR_BACKENDS=headless`, the pixman renderer, one output of the window's
size, no borders, so the window starts at 0,0), screenshots with grim,
windows through `swaymsg`, the clipboard with wl-clipboard, and keys and
the pointer through `crates/wlinput` (built by the harness, or
`E2E_WLINPUT`). Headless sway
has no input devices, so its own `seat cursor` commands reach no client;
wtype and wlrctl make a virtual device per call, which lost clicks and
wheel steps and made every key a focus change. wlinput keeps one virtual
keyboard (a US layout at the real evdev codes, so kitty's base-layout key
is right; other characters get free codes as typed) and one virtual
pointer for the run. Links are not opened on any display: `xdg-open` (`open`
on macOS) is replaced for the command by a script that notes the URI in
`OUTDIR/opened.txt` (`links` checks it), since through D-Bus a real one
could reach the desktop of whoever runs the tests, past Xvfb and sway.
kitty, Ghostty, WezTerm and the GUI passed every judged
scenario on both (2026-10-29; Ghostty cannot tell its text, so those checks
SKIP); on Wayland `terminal-extras`' `scrolled` and its cells fail in
every terminal (2026-11-24, with the bash harness and the TypeScript one
alike: it is judged as a notice goes, `todo.md`). `-I` (fcitx5 with Mozc) works on both: over XIM on X11, over
input-method-v2 (sway) and text-input-v3 (the GUI) on Wayland; the run
ends the fcitx5 and mozc_server it started (Mozc's server daemonizes; a
leftover one kept a pipe open, see `scripts/lib/devshell.sh`'s NIX_CONFIG note).
WezTerm on X11 failed `viewer`'s wheel and
zoom once in a long run and passed the reruns (it delivers wheel steps
unevenly).

`-d macos` (the default on macOS; not run since the move to TypeScript:
`todo.md`): a virtual display at scale 1 beside the
real screen (`vdisp`), so screenshot pixels are the terminal's as on Xvfb;
windows start through `open` and move there (the command waits until they
did, so temzdf starts at scale 1); keys and the pointer through `termctl
serve` (`scripts/macos/termctl.swift`; kitty's keys through its remote
control, WezTerm's posted globally), screenshots with `screencapture`, the
clipboard with pbcopy (the user's text clipboard is put back after), links
not opened (`open` is replaced; what would open is in `opened.txt`).
Details and why in `doc/terminals/macos.md`; what differs per terminal in
`doc/terminals/terminal-compat.md`. Results on macOS 26 (2026-10-29): WezTerm passes
every judged scenario (56); kitty all but `viewer`'s zoom (it reports
three wheel steps per notch, so the zoom is three times as deep and its
tiles are not there yet after the scenario's pause; they are with a longer
one); Ghostty all (text checks SKIP); the GUI `gui`. `gui-focus` (X11's and
Wayland's synthetic presses on focus-in) is not run there; `-I` and multiplexers are not tried there.
`scripts/macos-vm.sh` runs the same in a macOS VM, on its own screen, so
the host's pointer stays free ("Running in a VM" in `doc/terminals/macos.md`).

`scripts/e2e-all.sh [-t TERM]... [-d DISP] [-P PATH] [-j N] [-R] [SCENARIO...]`
runs the judged scenarios (each with the command in its module) in the
given terminals and sums up PASS/FAIL/SKIP per scenario;
`scripts/macos-vm.sh e2e-all` runs it in the VM in one go. Each run writes
when its phases began to `OUTDIR/timing.txt` (launch, window, written,
ready, actions, `expect NAME` and `expected NAME` around each headless
render, quit, done; milliseconds), for seeing where the time goes. The
screens after leaving temzdf (`session`, `terminal`: detached, killed) are
judged as images against the same terminal's own drawing of the messages
(`drawLines` types them with `printf` at the end; `screenRestored`
compares cell rows: the message, nothing below but the prompt, above it the
screen from before): the screen came back and nothing of temzdf's is left,
in Ghostty too, whose text cannot be read. (Images left on the alternate
screen do not show after it, so they are not caught; leaving the alternate
screen was, in every terminal.) The GUI's scenarios are `gui`, `gui-focus` and `gui-ime` (with `-I`; the last two
not on macOS); `gui-focus` and `gui-ime` judge what the shell wrote with
`check` (`focus.txt`, `ime.txt`, `pasted.txt`); `gui-ime`'s composing screen
stays unjudged.

What makes the runs quick, on every display:

- The first screen is waited for, not a fixed 3 to 6 s: temzdf logs
  `complete frame written` once a complete frame has left the writer
  after none or an incomplete one (`incomplete frame shown`; `temzdf-tty`), then the screen must stand still for 150 ms
  and not be of one colour (kitty took up to 2 s to show what temzdf had
  written: a black screen stood still). The old time is the limit. Inside
  a multiplexer the fixed wait stays: images that reach tmux before its
  client is attached show only later.
- The end waits for the terminal to go (at most half a second).
- `-R`: `expect`'s headless renders with release builds
  (`E2E_RENDERER`, `E2E_GUI_RENDERER` of `scripts/e2e.sh`; `-R` builds
  them), about 300 ms each instead of 630 on Linux.
- `-j N` (Linux): N scenarios at once, each with its own Xvfb (started
  with `-displayfd`, which picks a free display itself: `xvfb-run -a` once
  gave two runs started together the same display, and kitty crashed) or
  sway. A scenario runs in its terminals
  one after another, as some use fixed paths (`target/e2e-sync`, ...).

X11, kitty, Ghostty, WezTerm and the GUI (46 runs, 175 checks; 28 hardware
threads, 2026-10-29): 884 s before; 678 s waiting for the first screen
(starting took 232 s of it, then 33 s); 630 s with `-R`; 165 s with
`-j 4`, 88 s with `-j 8`, everything passing (`-j 8` twice more on X11
and Wayland too). Wayland: 683 s before. With `gui-focus`, `gui-ime` and
`synctex-reload` added (182 checks): 96 s on X11, 65 s on Wayland (`-R -j 8`).

## Windows in a VM

`scripts/windows-vm.sh` runs one disposable Windows 11 VM (QEMU/KVM) through
vitro, pinned in the script, as `scripts/macos-vm.sh` does for macOS. It
builds the golden image from an ISO, provisions it, starts, stops and
resets the VM from it, runs the gate there, and is where
`scripts/e2e.sh -d windows` runs the GUI's e2e scenarios (below):

```sh
scripts/windows-vm.sh lock --as NAME --why TEXT  # first: the VM lock (below)
scripts/windows-vm.sh build Win11_x64_en-us.iso  # about 20 minutes
scripts/windows-vm.sh provision              # tools; the next golden image
scripts/windows-vm.sh start                  # waits for the desktop
scripts/windows-vm.sh run 'cmd /c exit 3'    # PowerShell; exits 3
scripts/windows-vm.sh cp-in FILE guest.bin   # cp-out GUEST HOST
scripts/windows-vm.sh screenshot             # target/windows-vm/*.png
scripts/windows-vm.sh reset                  # a fresh overlay; stop: delete it
scripts/windows-vm.sh check [--fresh] [REV]  # the gate on REV in the guest
scripts/windows-vm.sh cursor hidden          # a transparent pointer; default: back
```

The VMs are shared by every worktree of the host (the Windows VM's `C:\t`
too), so they are used one worktree at a time, under a hand-held lock
(`scripts/lib/vm-lease.sh`, for `scripts/macos-vm.sh` as well): every
command that touches a VM (all but `tools`, `status`, `lock`, `unlock` and
`hold`) fails with status 3 unless this worktree holds it, and says who
does (the `--as` name, the `--why` text, since when). `lock` takes it
(`--wait`: until it is free) and `unlock` gives it back; `hold --as NAME
--why TEXT -- CMD...` holds it while CMD runs and gives it back afterwards
(also on Ctrl+C), so it cannot be forgotten. The holder is the worktree
(a hash of its path, or of `TEMZDF_VM_HOLDER`), so every command run from
it passes. Like pacman's `db.lck`, the lock never expires: a lock left
behind by a crashed run is removed with `unlock --force`, by a person who
checked nobody uses the VM. It only stops what goes through the scripts:
vitro, ssh or QMP used directly pass. `status` shows the holder.

- `build` takes an English x64 ISO (vitro's answer file is en-US). With
  Windows 11 build 26300 it ran through unattended in 20 minutes
  (2026-11-23), nothing done by hand. `scripts/windows/build-prepare.ps1`
  turns automatic updates off before vitro shuts the guest down, which it
  waits 180 s for; `--keep-failed` keeps a failed build's disk in
  `~/.local/state/vitro/builds/`.
- `provision` runs `scripts/windows/provision.ps1` in a fresh VM: Git,
  Node 24, Zig 0.15.2, Python 3.14 (`stylo`'s build script, for
  `temzdf-mdview`; winget), MSYS2 with UCRT64's Rust (clippy, rustfmt,
  and `wasm32-wasip1` from `-rust-wasm`), GCC, Clang, pkgconf, make and
  the WASI toolchain (`-wasi-libc`, `-wasi-compiler-rt`, `-lld`, `-llvm`;
  pacman; no MSVC, no rustup; see "The toolchain" in
  `doc/survey/windows-port.md`), Biome and esbuild 0.27.2
  (`temzdf-script`'s build script; npm, as in CI); the time zone UTC
  (QEMU's clock is UTC, and Windows reads it as local time), no sleep or
  screen lock, PowerShell scripts allowed (npm's shims). UCRT64's `bin`
  goes first on the machine's `PATH` and `LIBCLANG_PATH` points there
  (bindgen, for `mupdf-sys`). `WASI_SDK` is `C:/msys64/ucrt64`: without it
  `rquickjs-sys` (QuickJS for the script guest, `wasm32-wasip1`) downloads
  wasi-sdk, whose `clang.exe` needs the MSVC runtime (absent; exit
  `0xc0000135`), and with it uses UCRT64's `bin/clang`, `bin/ar` and
  `share/wasi-sysroot`. libghostty-vt is built once with Zig (Ghostty at
  the commit of `nix/libghostty-vt.nix`, target `x86_64-windows-gnu`) and
  installed into `C:/msys64/ucrt64`, so that pkgconf finds
  `libghostty-vt-static` as Nix provides it on Linux: Ghostty's build
  forces the MSVC ABI on Windows without a target, and
  `libghostty-vt-sys` passes `-Dtarget` only when cross-compiling. The
  static library links only with lld (see `.cargo/config.toml`). MSYS2's `usr/bin` stays off it (its
  `link.exe`, `find` and others would hide Windows' tools), so `make`,
  which `mupdf-sys` runs, is only on `PATH` in UCRT64's login shell
  (`$env:MSYSTEM='UCRT64'; $env:MSYS2_PATH_TYPE='inherit';
  $env:CHERE_INVOKING='1'; C:/msys64/usr/bin/bash.exe -lc 'cargo ...'`),
  as in CI's `shell: msys2`. It restarts the guest, so that the installers'
  pending reboot is not left to the shutdown vitro waits 180 s for, writes
  the versions to `target/windows-vm/provision-versions.txt` and keeps the
  VM as the next golden image (`vitro promote`). The image is about
  20 GB (2026-11-23). The guest's disk is 120 GB: after the gate and a
  `cargo check --workspace` in it, about 75 GB of C: were still free
  (measured with the earlier MSVC image).
- `check` checks a commit out in the guest's `C:\t` (a git bundle, so
  not the uncommitted changes), runs `npm ci` when the lock changed and
  `cargo xtask check --no-miri` (in UCRT64's login shell) there; the
  output is `target/windows-vm/check.log`. It uses the running VM and
  keeps `C:\t` and its `target` between runs, and leaves the VM running:
  checked out over the last tree, only the files that changed get new
  times, so cargo rebuilds only what they touch (an archive unpacked
  anew rebuilt every crate of ours, and a new VM everything. In a VM
  that had built the dependencies, the gate with every crate of ours
  rebuilt took 4 m 49 s, and over an unchanged tree 1 m 14 s;
  2026-11-23).
  `--fresh` starts a new VM first. It passes (2026-11-23, over
  `WINDOWS_CRATES` and `WINDOWS_ONLY_CRATES`); the crates outside them do
  not build or pass there yet (why: the comment on those lists in
  `xtask/src/check.rs`, and `todo.md`).
- A VM `start` starts gets Defender exclusions for the build
  (`C:\t`, `C:\msys64`, `.cargo`, Temp; cargo, rustc, the linkers and
  compilers) and no search indexer: during a build Defender had used
  about 480 s of CPU and the indexer 140 s, against 40 s of cargo and
  rustc (2026-11-23). Set on each start, not in the golden image, which
  is also used by hand.
- Each image is a new dated file in `~/.local/share/vitro/golden/`; the
  newest is used unless `TEMZDF_WINDOWS_GOLDEN` names another. Old ones are
  not deleted.

The nix build links of vitro and the OVMF firmware, and the config vitro
reads (one per VM name, written on every call; `XDG_CONFIG_HOME` points
there, so `~/.config/vitro/config.toml` is not used), are in
`~/.local/state/temzdf/windows-vm/`, outside `target/`, with the VM's
second QMP socket (`NAME.qmp`, short: a socket's path has room for about
108 bytes). The header of the script lists the known problems of vitro
and their workarounds.

### e2e runs on Windows

`scripts/e2e.sh -d windows -t gui SCENARIO -- C:/t/target/release/temzdf-gui.exe
--clean FILE` (under the lock: `scripts/windows-vm.sh hold --as NAME --why
TEXT -- scripts/e2e.sh ...`, or after `lock`) runs a GUI scenario in the running VM
(`TEMZDF_WINDOWS_VM`), driven from the Linux host; arguments naming files
of the host (the fixture) are copied into the guest. Build first, in the
guest (about 14 minutes; afterwards copy only the files that changed and
build again):

```sh
mkdir -p target/windows-vm && git ls-files -z | tar --null -T - -cf target/windows-vm/tree.tar
scripts/windows-vm.sh cp-in target/windows-vm/tree.tar tree.tar
scripts/windows-vm.sh run 'New-Item -ItemType Directory -Force C:/t > $null; tar -xf tree.tar -C C:/t; Set-Location C:/t;
  npm ci --prefix crates/temzdf-script/runtime --no-audit --no-fund; $env:MSYSTEM="UCRT64";
  $env:MSYS2_PATH_TYPE="inherit"; $env:CHERE_INVOKING="1";
  C:/msys64/usr/bin/bash.exe -lc "cargo xtask fetch-fonts && cargo build --release -p temzdf -p temzdf-gui"'
```

How it works, and why:

- Input: QEMU's USB tablet and keyboard through QMP's
  `input-send-event`, raw JSON lines over a second QMP socket of the VM's (`scripts/windows-vm.sh qmp-socket`;
  vitro keeps its own). As vitro's `src/qmp.rs` does: a press and its
  release in separate calls (together they are one HID report, which the
  guest ignores), modifiers 150 ms apart from what they modify (the
  keyboard and the tablet are read separately). Every move waits 30 ms
  before the next event, a precaution: once, without that wait, the
  selection of `gui.ts`' drag lost its first characters (the
  clipboard held "df sample document"; vitro's drag did it in the
  survey). That fits QEMU merging a move into a tablet report the guest
  has not read yet whose buttons are the same (`hw/input/hid.c`,
  `hid_pointer_sync`; the guest reads one report per poll): a press
  still queued when the drag's first move comes takes that move's
  place. It did not happen again: 5 more runs of `gui.ts` and 20
  drags in a row without the wait, 3 runs and 20 drags with it, all
  whole (2026-11-23). `expect` cannot see it (both sides show the same
  session); the clipboard can.
- Screenshots: QMP's `screendump`, written on the host; the waits for a
  still screen compare the drawing area only (the desktop has a clock).
- Commands: one PowerShell in the guest over SSH for the whole run (one
  per command took 1.5 s; `scripts/windows-vm.sh ssh-config`, one
  connection), files with scp. The run's sessions are in `C:/e2e`
  (`XDG_RUNTIME_DIR`, `XDG_STATE_HOME`), made anew each run; the guest's
  logs come back into OUTDIR.
- The desktop: SSH's session has no windows and no clipboard. The GUI
  starts from a scheduled task of the user's (`schtasks /it`) without a
  console window (`conhost.exe --headless`), through a batch file with
  the run's environment that waits for it and writes its exit status (an
  SSH command on the host waits for that file and stands for the GUI's
  process). The clipboard is PowerShell's `Get-Clipboard` and
  `Set-Clipboard` run the same way.
- The window: `temzdf-gui` writes where its drawing area is to the file
  `TEMZDF_GUI_GEOMETRY` names (`X Y W H SCALE`); the run waits for it
  (starting took over 5 s). Windows opens each new window lower and
  further right than the last (the third reached under the taskbar), and
  does not give the focus to a window a scheduled task opens: the harness
  drags the title bar so that the drawing area is in the middle of the
  screen, which focuses it too, then parks the pointer at the screen's
  right edge, off the window.
- The pointer: the screendumps show it. `expect` blackens a 48 px square
  around it (scale 1; the hot spot 16 px in) on both sides of the
  comparison: moving it off the window would change the drawing (temzdf
  sees it leave). `scripts/windows-vm.sh cursor hidden` makes it
  transparent (`scripts/windows/cursor.ps1`, `SetSystemCursor` until the
  next logon or `cursor default`; not in the golden image, which is also
  used by hand); the mask stays.
- `expect` runs `temzdf-gui --render-frame` in the guest at the window's
  scale and copies the picture back. PowerShell does not wait for a GUI
  program unless its output is piped, so the command is.

`tests/e2e/gui.ts` passes with the pointer visible and transparent
(2026-11-23, Windows 11 build 26300 at 1280x800 and 96 dpi, WARP): all
five `expect`s, and `clipboard.txt` holds the whole title. A run takes
about 37 s. Not there yet: `otherWindow`, `focusWindow`, `-I`, links
(they would open in the guest), and the other GUI scenarios.

## Judged e2e scenarios

`expect` (see it in `scripts/e2e/helpers.ts`) takes a screenshot, reads the
geometry temzdf logged at startup (`OUTDIR/state/temzdf/temzdf.log`), renders
the same input headlessly and compares with ImageMagick (`-fuzz 3%`, no pixel
may differ). The cells the terminal draws itself (the pane's status line,
text since 2026-10-31) are left out of that comparison on both sides
(`--mask`, `NAME.mask.png`); their text is compared with the headless
frame's (`--text`, `NAME.expected.txt`) where the terminal can tell it
(kitty, WezTerm), as for sessions, and their pixels against the terminal's
own drawing of them (`NAME.cells`, below). Results go to `OUTDIR/expect.txt`;
`NAME.actual.png`, `NAME.expected.png`, `NAME.diff.png` and
`NAME.text.diff` show a mismatch. Two forms:

- `expect(NAME, KEYS)`: `--render-frame --keys=KEYS`, the whole key sequence
  since startup, each key run to its end. Send scrolling keys one at a time
  with a pause: quick repeats accelerate in the real terminal (flick) but
  not here.
- `expect(NAME)`: a replay of all the input since startup with its timing
  (`--render-frame --events`): keys, the mouse and forward searches. The input is what temzdf itself handled,
  recorded through `TEMZDF_RECORD_INPUT` (`OUTDIR/input.txt`), not what the
  harness sent: terminals deliver late and in bursts (WezTerm turned wheel
  steps 30 ms apart into 1 to 90 ms), and their pixels differ by one
  (WezTerm). The replay runs time between inputs in 16 ms steps with the
  session's timers, drawing a frame each step and waiting for the work it
  asks for (the pages it shows), so a selection autoscroll extends reaches
  the pages it scrolls onto, and wheel flicks, easing and link previews
  (after 300 ms resting) come out as in the terminal. It ends as long after temzdf's last
  input as the screenshot came after the last one sent; pause before
  `expect` until animations end, or wait for the screen to stand still
  (`waitStill`, for motion of unknown length such as autoscroll). The first screen is up, its pages loaded,
  before the first input (as in the terminal, where the scenario starts
  seconds later).

Rules for scenarios:

- Only `temzdf standalone pdf --clean [--page N] [--scale S] FILE` (one
  viewer, no session), or `--config FILE` with a configuration of the
  repository's (`tests/e2e/configs/`) instead of `--clean`. `--clean`: the
  headless side draws with the shipped defaults, so the terminal must
  too; with `--config` both sides load that file on top of them
  (`expect` passes it on to `--render-frame`); `scripts/e2e.sh` refuses a
  COMMAND running temzdf with neither. Inside `sh -c 'COMMAND; exec sh'`
  for the shell `NAME.cells` needs (the harness reads COMMAND out of it).
- Forward searches go through the `forward("LINE:FILE", PDF)` helper (`temzdf
  --forward` from another process): temzdf records the search it got, the
  replay sends it at the same time, and the helper's record times the
  screenshot. `tests/e2e/configs/forward.ts` keeps the highlight until the
  next input, so it is there at the screenshot on both sides
  (`synctex.ts`, `synctex-reload.ts`).
- Use the helpers (`wheel`, `dragto`, `click`, `move`, `keys`, `type`), not
  input tools of your own (Biome refuses `node:child_process`,
  `node:timers` and `node:fs` in scenarios): their input is what reaches temzdf either way, but the
  harness's own record (`OUTDIR/events.txt`) times the screenshot from it.
- Information notices ("Copied N characters") fade after 3 s of
  temzdf's clock, and one fading between the screenshot and what it is
  compared with (the session's frame, the replay) fails the `expect`. Wait for it by what the
  screen shows, not by time: `mark(NAME)` while it shows,
  `waitChange(NAME)`, then `waitStill()` (`terminal-extras.ts`,
  `markdown.ts`, `select.ts`).
- A moment that depends on rendering speed (the old tiles scaled right
  after a zoom, before new ones arrive) cannot be judged: the replay waits
  for the workers after each input. Take a plain `shot` there.
- Keep Esc apart from the next key: with legacy encoding (tmux) ESC followed
  quickly by another key reads as Alt+key.
- Works in kitty and Ghostty (both blend in linear light, passed as
  `--linear-blending`; Ghostty without decorations, padding or its
  clipboard toast), WezTerm, and for keys inside tmux and zellij (mouse
  input is not judged there: multiplexers report cells).

In a session (COMMAND is `temzdf --clean -s NAME ...` rather than the
standalone viewer, often inside `sh -c`), `expect NAME` takes no keys:
`temzdf render-session` (hidden) attaches to the session as a terminal
client of the same geometry and hints (from `temzdf.log`), waits for its
frame to settle, and writes what a terminal should show: the images through
the same bytes into the mock terminal (`NAME.expected.png`), the text of the
cells (`NAME.expected.txt`) and a mask of the cells the terminal draws itself
(text, backgrounds, the cursor; `NAME.mask.png`). The pixels are compared
outside the mask, and in kitty and WezTerm the screen's text (`kitten @
get-text`, kitty listens on a socket for it; `wezterm cli get-text` through
the GUI's own socket, else the CLI starts a mux server of its own) with the
expected text (`NAME.text.diff`). This
judges the terminal client and the terminal: placements, text positions,
wide characters, images of programs in terminal panes, tab lines and
floats as drawn. Both sides see the same session, so what the session
decides (layout, focus, what a reload keeps) is not judged by it; that is
for `crates/temzdf/tests/tui.rs` and unit tests. Not inside multiplexers;
Ghostty's text is judged through `NAME.cells` (below). `tests/e2e/session.ts`,
`terminal.ts`, `terminal-extras.ts`, `tabs.ts`,
`floats.ts`, `resize.ts` and `float-wide.ts` (WezTerm's
wide characters under images, `doc/terminals/terminal-compat.md`) use it.

The cells the terminal draws itself are also judged against the same
terminal's own drawing of them (`NAME.cells`, since 2026-11-23), for
both forms of `expect` outside the GUI and multiplexers: their colours and
attributes (inverse, bold, underline, which the text comparison misses),
and their text where the terminal cannot report it (Ghostty).
`--render-frame` and `render-session` write the cells as bytes (`--ansi`,
`NAME.ansi`: the cursor hidden, then a CUP, SGR and the character per cell,
written apart from temzdf-tty's own encoding) and which cells to judge
(`--cells`, `NAME.cells.png`: the mask's, but for the cursor, which the
drawing hides, and for cells of the default background over images, where
the image shows through and the drawing has none). After the actions the
harness ends temzdf (Esc until the viewer quits; it kills the sessions
left) and, in the shell COMMAND leaves behind (`sh -c '...; exec sh'`;
`scripts/e2e-all.sh` runs every viewer scenario so), types `r NAME` for
each expect: a shell function that clears the screen, `cat`s `NAME.ansi`
and waits in `read` (no prompt over the cells). Once the screen stands
still (`waitStill`) it takes `NAME.ref.png` and compares the screenshot of
the expect with it inside `NAME.cells.png` (`-fuzz 3%`, no pixel may
differ): `PASS NAME.cells (N cells)` or `FAIL NAME.cells` in
`OUTDIR/expect.txt` (`NAME.cells.actual.png`, `NAME.cells.ref.png`,
`NAME.cells.diff.png`). SKIP without such a shell, and when the terminal
changed size since the expect (no scenario does so far). Each expect adds about 1.2 s
(kitty), each scenario about 2.5 s more to end temzdf and set up the shell.
Dropping inverse from `--ansi` fails `terminal-extras`' `selected.cells`
and `scrolled.cells` (the selection and the scrollback position), which the
text and the pixels outside the mask pass.

`screenContains(TEXT)` (kitty, WezTerm) tells whether the screen shows
TEXT, for moments outside a session (`check("detached-text", () =>
screenContains("[detached from session e2e]"))`); elsewhere it returns
`"skip"` and `check` records SKIP.

`tests/e2e/reload.ts` judges the reload with `expect(NAME, "2G")`: the
headless side opens the file as it is on disk at that moment, so the
screens match only if the reload kept the place.

`check(NAME, FN)` judges what a screenshot cannot: a command's exit
status or a file it wrote (FN returns true or false, or `"skip"`) (`tests/e2e/synctex.ts`: the forward search
is sent, Ctrl+click runs the editor on the title's lines). Its results go to
`OUTDIR/expect.txt` too, and a failure exits 4 as well.

Both sides run the same viewer code, so a bug in viewer state shows up on
both and passes: zooming anchored at the top-left instead of the pointer did
(found by the user). Behaviour of the view itself needs unit tests
(e.g. `ctrl_wheel_zooms_around_the_pointer` in
`crates/temzdf-pdfview/src/pane.rs`).

`tests/e2e/expect.ts` passes in kitty, WezTerm, tmux and zellij
(2026-10-25) and Ghostty (2026-10-28); `search`, `viewer`, `links`,
`select` and `preview` (mouse input) pass in kitty, WezTerm and Ghostty
(2026-10-28). Comparing against
headless renders already found two bugs: kitty darkened the screen edges of the
stretched 1x1 background image, and Enter pressed before search results
arrived left no match selected.

## Fixtures

`tests/fixtures/*.tex` are the sources. `crates/temzdf-fixtures` builds them
into `target/fixtures/` (PDF and SyncTeX) with latexmk from a global TeX
installation (or the one in `nix develop .#alldeps`), reproducibly
(`SOURCE_DATE_EPOCH`, and the same scratch directory name each time:
pdfTeX's `/ID` depends on it), and skips fixtures newer than their source
and than its own source. Each is built in a scratch directory and renamed
into place, and builds in several processes wait for each other on
`target/fixtures/.lock`. Unit tests build them on first use
(`pdf::fixture`, in Rust, so also where there is no shell);
`scripts/build-fixtures.sh` (`cargo xtask fixtures [--force]`) builds them
before e2e scenarios.
Generated files are never committed (see AGENTS.md).
