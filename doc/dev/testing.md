# Testing

Layers of verification, cheapest first. All of them run without a human.

| Layer | How | What it catches |
|---|---|---|
| Unit tests | `cargo xtask check` (the gate; `cargo xtask test` runs the tests alone) | Input parsing, kitty command encoding, compositor diffing (replayed through `termpix::mock`), layout maths, tile rendering vs full-page rendering |
| Miri | `scripts/miri.sh` (`cargo xtask check --miri`) | Undefined behaviour, data races and leaked threads or memory in the unit tests' Rust code (ours and the Rust dependencies). See "Miri" below. |
| Headless frames | `target/debug/temzdf standalone pdf FILE.pdf --render-frame out.png [--size 100x30] [--cell 10x20] [--page N] [--scale S] [--keys=...]` | What a screen looks like: runs the real `Session` with the shipped default configuration (keys in Vim feedkeys notation, e.g. `jjj<C-b>%`) and compositor, replays the bytes through the mock terminal (images only: `--text FILE` writes the status line's text, `--mask PNG` where it is). Read the PNG to check it. `scripts/frames.sh` renders a fixed set, with the `shots` scenes of `tui.rs` (sessions, Markdown, terminal panes, tabs), and compares two sets pixel by pixel (for refactorings). |
| Script tests | `target/debug/temzdf test [-j N] [--timeout SECS] FILE...` (part of `cargo xtask check` for `crates/temzdf-script/runtime/**/*.test.ts`) | temzdf's behaviour driven from TypeScript (`temzdf/test`, QUnit): keys typed into a session and what the scripting API then sees (buffers, modes, registers). Each file runs in a session of its own in a process of its own, its `XDG_*`, `TEMZDF_LOG_DIR` and `TMPDIR` in a temporary directory; TAP out, a file printing nothing for `--timeout` (30 s) stopped and failed. |
| Sessions | `Session` unit tests (`crates/temzdf-server`), `tests/e2e/session.actions` | Splitting, focus, composition, shared rendering; detach, list, re-attach, actions from other processes, kill. |
| Simulated terminal | `crates/temzdf/tests/tui.rs` on `crates/tuitest` (part of `cargo xtask check`) | The real binary behind Ghostty's terminal emulation: what the screen says (text, colours, cursor, image placements), keys in both keyboard modes, session commands end to end (`-s`, detach, `attach`, `ls`, `action`, `kill-session`, exit messages), resizing, and the same inside a private tmux. No X server; about a second for all. |
| Real terminal | `scripts/e2e.sh [-t kitty\|ghostty\|wezterm] [-d x11\|wayland\|macos] [-m zellij\|tmux] ACTIONS -- COMMAND` | Terminal behaviour: probing, transports (shm, inline with `TERMPIX_MEDIUM=direct`), pixel mouse, actual placement semantics, terminal crashes (exit code 3). Screenshots land in `target/e2e/`. |
| Judged e2e | `expect NAME [KEYS]` in an action file (e.g. `tests/e2e/expect.actions`, `viewer.actions`) | The screen matches what `--render-frame` draws after the same input, keys or a replay of the keys and mouse input temzdf handled (exit code 4 otherwise): placements, crops, strip updates, stale images, terminal-side scaling and blending, wheel, drag, clicks, hover. Not PDF rendering itself (both sides use MuPDF). |
| GUI client | `crates/temzdf-gui` unit tests and `--test gpu` (offscreen on lavapipe), `scripts/e2e.sh -t gui [-I]` with `tests/e2e/gui*.actions` | The draw list and key translation; layers and text drawn on a GPU; the window under xfwm4 against `temzdf-gui --render-frame` on the same session; Japanese input through fcitx5 and Mozc; the clipboard. |
| Real terminal (macOS, by hand) | `scripts/macos-gui.sh launch kitty\|ghostty\|wezterm -- COMMAND`, then `key`, `click`, `shot`, ... | Poking at a terminal step by step (`doc/terminals/macos.md`); `launch app` runs `temzdf-gui`, and `tests/macos/*.sh` are scripted scenarios. `scripts/e2e.sh` runs the action files on macOS too. |
| User | Ghostty / WezTerm on a real desktop | The feel of scrolling and zooming. |

Action files live in `tests/e2e/`; each starts with a comment saying how to run it
and what the screenshots should show. To look at many screenshots at once:
`montage target/e2e/*.png -tile 2x -geometry 450x300+4+4 target/e2e/montage.png`.

## Where to start

Behaviour visible on screen (terminal panes, sessions, CLI, keys) is best
tested in `crates/temzdf/tests/tui.rs`: the real binary behind a simulated
terminal (`crates/tuitest`), checked by text, cells and image placements, also
inside tmux, without an X server. See "Simulated terminal tests" below.

End-to-end checks in a real kitty (virtual X server, no display needed):

```sh
cargo build --example scroll_demo
scripts/e2e.sh tests/e2e/scroll_demo.actions -- target/debug/examples/scroll_demo
```

Action files in `tests/e2e/` drive the app (wheel, drag, click, keys) and save
screenshots to `target/e2e/`; read the PNGs or measure pixels to verify. For
key-driven checks prefer `expect NAME KEYS`, which compares the terminal with
a headless render and fails the run on a mismatch (`tests/e2e/expect.actions`,
"Judged e2e scenarios" below). See the `scripts/e2e.sh` header for the
helpers. `temzdf standalone pdf FILE --render-frame out.png` renders one
screen headlessly. `scripts/build-fixtures.sh` builds `target/fixtures/`
from `tests/fixtures/*.tex` (tests run it automatically; e2e scenarios
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
terminals on a Mac without a human"): the same action files run there,
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
Node scripts' types (`tsc -p scripts`), shellcheck, references to
documents (`node scripts/check-docs.ts`), clippy and the tests (and Miri with `--miri`). Each
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
every `Output` was dropped without `close` (fixed), and zlib-rs violates
the aliasing rules (`doc/dev/known-issues.md`; those tests are ignored).

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
(`tests/e2e/search.actions`, `expect.actions`' `search` and `next`).

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

Action files use the helpers only (`keys`, `type`, `click`, `down`/`up`,
`move`, `wheel`, `hold`, `clipboard`, `focus_window`, ...); the helpers
drive the display server through a few `be_*` functions in
`scripts/e2e.sh`. `-d x11` (the default): Xvfb, xdotool, `import`, xclip,
xwininfo, xfwm4 for the GUI. `-d wayland`: headless sway
(`WLR_BACKENDS=headless`, the pixman renderer, one output of the window's
size, no borders, so the window starts at 0,0), screenshots with grim,
windows through `swaymsg`, the clipboard with wl-clipboard, and keys and
the pointer through `crates/wlinput` (built by the script). Headless sway
has no input devices, so its own `seat cursor` commands reach no client;
wtype and wlrctl make a virtual device per call, which lost clicks and
wheel steps and made every key a focus change. wlinput keeps one virtual
keyboard (a US layout at the real evdev codes, so kitty's base-layout key
is right; other characters get free codes as typed) and one virtual
pointer for the run. Links are not opened on any display: `xdg-open` (`open`
on macOS) is replaced for the command by a script that notes the URI in
`OUTDIR/opened.txt` (`links` checks it), since through D-Bus a real one
could reach the desktop of whoever runs the tests, past Xvfb and sway.
kitty, Ghostty, WezTerm and the GUI pass every judged
scenario on both (2026-10-29; Ghostty cannot tell its text, so those checks
SKIP). `-I` (fcitx5 with Mozc) works on both: over XIM on X11, over
input-method-v2 (sway) and text-input-v3 (the GUI) on Wayland; the run
ends the fcitx5 and mozc_server it started (Mozc's server daemonizes; a
leftover one kept a pipe open, see `scripts/lib/devshell.sh`'s NIX_CONFIG note).
WezTerm on X11 failed `viewer`'s wheel and
zoom once in a long run and passed the reruns (it delivers wheel steps
unevenly).

`-d macos` (the default on macOS): a virtual display at scale 1 beside the
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
runs the judged scenarios (each with its "Used with:" command, kept in the
script) in the given terminals and sums up PASS/FAIL/SKIP per scenario;
`scripts/macos-vm.sh e2e-all` runs it in the VM in one go. Each run writes
when its phases began to `OUTDIR/timing.txt` (launch, window, written,
ready, actions, `expect NAME` and `expected NAME` around each headless
render, quit, done; milliseconds), for seeing where the time goes. The
screens after leaving temzdf (`session`, `terminal`: detached, killed) are
judged as images against the same terminal's own drawing of the messages
(`draw_lines` types them with `printf` at the end; `screen_restored`
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
- `-j N` (Linux): N scenarios at once, each with its own Xvfb (display
  300 + slot: `xvfb-run -a` gave two runs started together the same
  display, and kitty crashed) or sway. A scenario runs in its terminals
  one after another, as some use fixed paths (`target/e2e-sync`, ...).

X11, kitty, Ghostty, WezTerm and the GUI (46 runs, 175 checks; 28 hardware
threads, 2026-10-29): 884 s before; 678 s waiting for the first screen
(starting took 232 s of it, then 33 s); 630 s with `-R`; 165 s with
`-j 4`, 88 s with `-j 8`, everything passing (`-j 8` twice more on X11
and Wayland too). Wayland: 683 s before. With `gui-focus`, `gui-ime` and
`synctex-reload` added (182 checks): 96 s on X11, 65 s on Wayland (`-R -j 8`).

## Judged e2e scenarios

`expect` (see the `scripts/e2e.sh` header) takes a screenshot, reads the
geometry temzdf logged at startup (`OUTDIR/state/temzdf/temzdf.log`), renders
the same input headlessly and compares with ImageMagick (`-fuzz 3%`, no pixel
may differ). The cells the terminal draws itself (the pane's status line,
text since 2026-10-31) are left out of that comparison on both sides
(`--mask`, `NAME.mask.png`); their text is compared with the headless
frame's (`--text`, `NAME.expected.txt`) where the terminal can tell it
(kitty, WezTerm), as for sessions. Results go to `OUTDIR/expect.txt`;
`NAME.actual.png`, `NAME.expected.png`, `NAME.diff.png` and
`NAME.text.diff` show a mismatch. Two forms:

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
  session's timers, drawing a frame each step and waiting for the work it
  asks for (the pages it shows), so a selection autoscroll extends reaches
  the pages it scrolls onto, and wheel flicks, easing and link previews
  (after 300 ms resting) come out as in the terminal. It ends as long after temzdf's last
  input as the screenshot came after the last one sent; pause before
  `expect` until animations end, or wait for the screen to stand still
  (`wait_still`, for motion of unknown length such as autoscroll). The first screen is up, its pages loaded,
  before the first input (as in the terminal, where the scenario starts
  seconds later).

Rules for scenarios:

- Only `temzdf standalone pdf --clean [--page N] [--scale S] FILE` (one
  viewer, no session). `--clean`: the headless side draws with the shipped
  defaults, so the terminal must too; `scripts/e2e.sh` refuses a COMMAND
  running temzdf without it.
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
Ghostty gets the pixels only. `tests/e2e/session.actions`,
`terminal.actions`, `terminal-extras.actions`, `tabs.actions`,
`floats.actions`, `resize.actions` and `float-wide.actions` (WezTerm's
wide characters under images, `doc/terminals/terminal-compat.md`) use it.

`screen_contains TEXT` (kitty, WezTerm) tells whether the screen shows TEXT, for
moments outside a session (`check detached screen_contains "[detached from
session e2e]"`); elsewhere it exits 77 and `check` records SKIP.

`tests/e2e/reload.actions` judges the reload with `expect NAME "2G"`: the
headless side opens the file as it is on disk at that moment, so the
screens match only if the reload kept the place.

`check NAME CMD...` judges what a screenshot cannot: a command's exit
status or a file it wrote (`tests/e2e/synctex.actions`: the forward search
succeeds, Ctrl+click runs the editor on the title's lines). Its results go to
`OUTDIR/expect.txt` too, and a failure exits 4 as well.

Both sides run the same viewer code, so a bug in viewer state shows up on
both and passes: zooming anchored at the top-left instead of the pointer did
(found by the user). Behaviour of the view itself needs unit tests
(e.g. `ctrl_wheel_zooms_around_the_pointer` in
`crates/temzdf-pdfview/src/pane.rs`).

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
