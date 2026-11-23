# The e2e harness in TypeScript: a survey

What moving `scripts/e2e.sh`, `scripts/e2e-all.sh` and the action files
(`tests/e2e/*.actions`) to TypeScript on Node involves. The plan is in
`todo.md` ("The e2e harness in TypeScript"). This survey was written
before `windows-port` was merged, and it describes that branch as of
commit 7e8d609 (2026-11-23).

## Where things stand

- `scripts/e2e.sh` is 2071 lines on `windows-port` (1668 on dev). Its
  `-d windows` backend adds about 400 lines. `scripts/e2e-all.sh` is 253
  lines. There are 27 action files, 19 to 61 lines each.
- Display backends are `be_*` functions:
  - x11: xdotool, import, xclip.
  - wayland: headless sway, `crates/wlinput` as a coprocess, grim and
    wl-clipboard.
  - macos: `termctl serve` as a coprocess, screencapture, pbcopy and
    pbpaste, kitty's remote control for its keys.
  - windows: QMP over a socat coprocess, and one guest PowerShell over
    SSH fed through fifos (`GPS`), with scp and `schtasks`.
- Terminals: kitty, Ghostty, WezTerm and the GUI. Multiplexers: zellij
  and tmux.
- What the action files use, counted on `windows-port`:
  - `pause` 161 times, `expect` 72, `keys` 63, `check` 32, `type` 29,
    `shot` 13, `wheel` 12, `move` 10, and the rest a few times each.
  - 12 files have shell logic beyond helper calls:
    - `for` loops;
    - five functions of their own (`band_offset`, `band_moved`,
      `inverse_on_title`, `dark_middle`, `dismissed`);
    - `grep | cut` for line numbers;
    - `magick` for a gradient and a band offset, with awk;
    - `cp`, `mv`, `rm`, `head -c` and `touch` on fixtures;
    - `check ... sh -c '...'`;
    - harness globals: `$outdir` 29 times, and `$term`, `$display`,
      `$term_pid`, `$origin_x` and `$origin_y` once each.
- ImageMagick: 28 calls in the harness. Most crop to the cell grid,
  composite a mask, or run `compare -metric AE -fuzz 3%`. There is also
  one `identify` and the shapes `mask_window_edges` builds.
  - `compare` exits 1 when the images differ and 2 when it fails. Both
    are swallowed by `|| true` today.
  - The Windows guest has no ImageMagick (`scripts/windows/provision.ps1`
    does not install it). That is fine while the harness runs on the
    Linux host.

## Facts checked for the move

Checked in the dev shell (Node 24.22) on 2026-11-23.

- **Node runs `.ts` as it is.** `scripts/ts-types.ts` and
  `scripts/check-docs.ts` already do. `tsc -p scripts` type-checks
  `scripts/**/*.ts` in the gate (`xtask/src/check.rs`) under
  `erasableSyntaxOnly`, so there are no enums and no namespaces.
  - A helper may be named `type`: `await type("ls\n")` parses and runs.
- **Octal escapes are a SyntaxError in a module.** Text typed into the
  terminal's shell that uses `\033` (the `terminal`, `floats`, `tabs` and
  `terminal-extras` scenarios) has to become `\x1b`, or be wrapped in
  `String.raw` when the shell itself should see `\033`.
- **Starting a process is no slower.** `execFile("true")` takes 2.4 ms in
  Node; `env true` from bash takes 3.5 ms. xdotool per key or button costs
  the same either way.
- **Children outlive Node.** A child started with `spawn` (detached or
  not) kept running after Node threw, and after Node alone got SIGINT.
  - The harness has to keep a list of what it started and end it all:
    in `finally`, on SIGINT and SIGTERM, on an uncaught exception, and
    in a synchronous `exit` handler as the last resort.
  - That list replaces `trap cleanup EXIT`. Ctrl+C in a terminal reaches
    the whole process group, but `e2e-all` runs in the background do not.
- **Xvfb can be started without re-executing.** `Xvfb -displayfd 3`
  started from Node printed a free display number in 41 ms. That
  replaces both:
  - the `xvfb-run` re-execution;
  - `E2E_XVFB_DISPLAY`, which exists because two runs could pick the
    same free display.

  dbus-run-session (for `-I`) and sway can be started the same way:
  dbus-daemon's `--print-address`, and sway's sockets in the run's
  runtime directory, as now.
- **The coprocesses become plain child processes or sockets.**
  - wlinput and `termctl serve` speak one command per line and answer
    `ok`.
  - QMP is JSON lines on a Unix socket, so `node:net` replaces socat.
  - The guest PowerShell (`@@done STATUS` after each command) is an
    `ssh` child with piped stdin and stdout, read with `node:readline`.
  - None of them needs fifos or file descriptors kept in shell globals.
- **Two tsconfigs.** `tests/e2e/configs/*.ts` are temzdf configurations,
  typed against `temzdf-all.d.ts` with no Node types (their own
  tsconfig, and Biome's `files.includes`). Action files in TypeScript
  would be Node code typed against the harness. So they need either a
  tsconfig of their own, or to sit where `scripts/tsconfig.json` covers
  them, and Biome's `includes` needs the new paths.

## What is POSIX-only

Windows without a Linux host (CI, or a Windows machine) is the reason for
the move, per `windows-port`'s `todo.md`. So the parts of the harness
fall into three groups.

- **Portable core.**
  - The action API.
  - Recording the events (`events.txt`).
  - The `expect` and `check` bookkeeping (`expect.txt`: PASS, FAIL,
    SKIP; exit statuses 1, 3 and 4).
  - Rendering the expected screens (`temzdf ... --render-frame`,
    `render-session`).
  - The image comparisons.
  - Waiting for files and for a still screen.
  - Timing (`timing.txt`).
  - The scenario table of `e2e-all`.
- **Host-specific, but only to be redone per OS.**
  - Finding this run's temzdf: `temzdf_alive` reads
    `/proc/*/environ`, or `ps -E` on macOS.
  - `env -u` and the PATH shim that records `xdg-open` and `open`.
  - `mktemp -d` for the runtime directory, which is short on macOS for
    socket paths.
  - `kill -0`.
  - The `sh -c 'CMD; exec sh'` convention for a shell after the command.
    `draw_references` types `cd`, `r() {...}` and `cat NAME.ansi` into
    that shell, so it is POSIX sh in the terminal, whatever runs the
    harness.
- **Per backend.** The `be_*` functions, starting the display server and
  the terminal, the window's geometry, and the clipboard.
  - A Windows-native backend would need input and screenshots of its
    own: PowerShell, or a small native helper.
  - It would also need an image tool: ImageMagick installed in the
    guest, or comparisons done in Node or Rust.

So the action files need no shell helper (`todo.md` suggests a
`` sh`...` `` one). A helper that runs a command from an argument list
(`run(["target/debug/temzdf", "kill-session", "e2e-f"])`) is enough. It
needs no quoting, and no `sh` on Windows. The shell lines of the action
files as of `windows-port` 7e8d609 go as follows:

- **Commands with fixed arguments** (`temzdf kill-session` and `temzdf
  action ...`, 13 lines) become that helper.
- **Pipes** (`grep -n | cut` for a line number, `tail | head | grep`)
  are not needed: read the file and do the rest in TypeScript.
- **File changes** (`cp`, `mv`, `rm`, `touch`, `head -c`) become
  `node:fs`.
- **Judgements written as commands** (`check saved sh -c 'printf ... |
  cmp -s - FILE'`, `check copied-rows test "$(cat F)" = ...`) become
  TypeScript, with `check` taking a function.
- **ImageMagick** goes through its typed wrapper.

Shell stays only where the scenario is about the shell: text typed into
the terminal (`type`), and the `sh -c 'CMD; exec sh'` a scenario runs in
it.

Nothing in the harness's history or docs meant the action files to
depend on the shell beyond this. Earlier versions of the action files,
on both branches, also called xdotool and xclip and started `xterm &` in
the background. "e2e beyond X11" moved those into helpers (`be_*`,
`other_window`, `set_clipboard`), so that "action files use helpers
only" (`todo.md`). The docs describe an action file only as a bash file
that can use the helpers. `scripts/macos-gui.sh` and `tests/macos/*.sh`
are a separate tool for poking at a terminal by hand, outside this move.

## The requirement: the same tests

This is not open for decision (the user, 2026-11-23). After the move,
every scenario has to keep what it does now:

- every judgement (`expect`, `check`, `NAME.cells`) runs where it runs
  now, is skipped where it is skipped now, and fails where it fails now;
- every judgement tests the same thing as now: the same input, the same
  phenomenon, the same comparison.

How bash's behaviour carries over:

- **`set -e`.** A helper that fails outside `check` stops the run. In
  TypeScript, the helper throws, which does the same.
- **`check` and exit 77.** `check` turns exit 77 into SKIP ("this
  terminal cannot tell"). `read_screen` and `screen_contains` return 77
  outside kitty and WezTerm, and inside multiplexers; `band_moved`
  returns 77 on macOS. In TypeScript, a `Skip` exception (or a returned
  `"skip"`) carries this, in the same places.
- **`|| true`.** After `clipboard`, it lets a clipboard that cannot be
  read go on. In TypeScript, the API says so instead, for example
  `clipboard()` returning `undefined`.

**How to check it.** For each terminal and display, compare the baseline
with the run after the move, judgement by judgement: the same names, with
the same PASS, FAIL or SKIP. A judgement that is flaky in the baseline
has to be run several times on both sides before the two are compared.

## Decided with the user

Decided on 2026-11-23, as proposed here before.

1. **Where a scenario's command lives.** Today it is written three times:
   - the "Used with:" header of the action file;
   - `scenario()` in `scripts/e2e-all.sh`;
   - `doc/dev/testing.md` and other docs.

   The harness then parses COMMAND back out of a shell string:
   - `runs_temzdf`;
   - `xargs printf` for `sh -c`;
   - the `; exec sh` regex for `has_shell`;
   - the `--page`, `--scale` and `--config` it copies to `expect`'s
     renderer.

   A TypeScript action module could export the scenario as data: the
   command as an argument list, whether a shell follows it, the files
   to prepare, and the options (`-s 800x600`, `-I`). Then `e2e-all` and
   the harness read the same definition. Decided: yes. One-off runs
   with a command given on the command line (`e2e.sh ACTIONS -- CMD`)
   stay possible.
2. **The entry point's name.** About 50 files name `scripts/e2e.sh` or
   `scripts/e2e-all.sh`: action headers, docs, `scripts/macos-vm.sh`,
   `flake.nix` comments, and Rust comments. Decided: keep both as
   thin bash shims. Each would `source` `scripts/lib/devshell.sh` (the
   dev shell) and `exec node scripts/e2e/main.ts "$@"`, so callers and
   habits stay. A Windows-native run calls `node` directly.
3. **The order of the move.** The same action file cannot run under both
   harnesses, but scenarios are independent. Decided:
   1. Run `e2e-all` as a baseline right after the merge: X11 and
      Wayland here, macOS and Windows in their VMs.
   2. Write the TypeScript harness with the X11 backend.
   3. Let `e2e-all` run `.ts` scenarios with it and `.actions` with
      bash.
   4. Port the scenarios in small groups, and compare each group's
      `expect.txt` with the baseline.
   5. Then Wayland, then macOS (VM), then Windows (VM).
   6. Then remove the bash harness: `scripts/e2e.sh` and
      `scripts/e2e-all.sh` become the shims of 2.

   Set aside: one change, judged by one full comparison at the end.

Still open, and not needed for the move itself:

- **ImageMagick on a Windows-native run.** This only matters when
  Windows runs without the Linux host. The choice is between
  installing ImageMagick in the guest (winget) and comparing images
  without it.

## Size and risks

- **Size.** The TypeScript harness would likely be about as long as the
  bash one, split by concern:
  - command line and scenarios;
  - the action API;
  - expect and judging, with the ImageMagick wrapper;
  - launching terminals and multiplexers;
  - one file per backend;
  - the list of processes to clean up.
- **The riskiest part is cleanup and start-up order.**
  - The display server, the terminal, the coprocesses, the multiplexer's
    server, fcitx5 and mozc_server (found by their HOME), `caffeinate`,
    and the SSH control master all have to end on every path.
  - Linux checks this through `e2e-all`; macOS and Windows need their
    VMs.
- **Timing.**
  - `wheel` sends a flick as one `xdotool click --repeat` and records
    the steps 30 ms apart. That stays one call.
  - The recorded times come from `EPOCHREALTIME`, which becomes
    `performance.now()`.
  - `pause` becomes a timer; a 30 ms timer took 30.3 ms.
  - Nothing here needs more precision than bash gives now.
- **Docs.** The helper reference is the header comment of `e2e.sh`,
  about 200 lines. It would move to the action API's doc comments, which
  the type checker and editors then show. `doc/dev/testing.md`
  ("X11, Wayland and macOS", "Windows in a VM", "Judged e2e scenarios")
  and `doc/terminals/terminal-compat.md` quote helper calls in bash form.
