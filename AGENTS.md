# AGENTS.md

Notes for coding agents (and humans) maintaining this repository.

## What this is

`temzdf` is a terminal PDF viewer aimed at previewing LaTeX documents. It draws
everything as pixels with the kitty graphics protocol (pages *and* UI chrome) and
aims for GUI-grade mouse interaction (pixel-smooth scrolling, drag, selection).
Key features: outline sidebar, in-document search, links, SyncTeX forward/inverse
search, and auto-reload that keeps the reading position.

The terminal plumbing is a separate, reusable library so that other viewers
(e.g. a future markdown viewer rendering a webview offscreen) can share it.

## Layout

| Path | Purpose |
|---|---|
| `crates/termpix-core` | Library. Frontend-independent primitives: geometry, RGBA raster helpers, kinetic scroll helper, key/mouse event types. No terminal code. |
| `crates/termpix` | Library. Terminal I/O: tty setup, capability probing, input parsing (SGR-pixel mouse, kitty keyboard), kitty graphics transport, layered compositor, mock terminal for tests; re-exports `termpix-core`. **Knows nothing about PDF.** |
| `crates/termpix-ui` | Library. Small widget kit rendering UI layers to RGBA (`vello_cpu` + `parley` + `taffy`). Independent of `termpix` (`termpix-core` only in its tests). |
| `crates/temzdf-scene` | Library. What a pane shows, frontend-independent: image store and frames of layers, frontend hints, effects. |
| `crates/temzdf-protocol` | Library. Session protocol: client/server messages, framing, socket locations, finding sessions (`discovery`). |
| `crates/temzdf-gui` | The GUI client (`temzdf-gui`): a winit window drawing the session with wgpu and glyphon (draw list, renderer, fonts with built-in fallbacks embedded from nixpkgs, input, IME, clipboard), `--render-frame` offscreen, `ls-fonts`. Uses `temzdf-protocol`, `temzdf-scene`, `temzdf-client`, `termpix-core`; no `termpix`, no server code. See `doc/gui-client.md`. |
| `crates/temzdf-client` | Library. What every client shares: naming, resolving and starting sessions (`spawn_server`), log locations, carrying out effects (links, inverse search). No terminal or window code. |
| `crates/temzdf-pdfcore` | Library. PDF backend: MuPDF (`mupdf` crate), display list and tile cache, text, SyncTeX, background workers, reload. No terminal code. |
| `crates/temzdf-pdfview` | Library. The PDF pane: view state, input handling, UI (status bar, outline, search, link preview), key bindings; `Document`/`DocumentHost` shared by the panes on one file (workers, reload, SyncTeX routing). No terminal code. |
| `crates/temzdf-term` | Library. The terminal pane: a program on a pty (`portable-pty`), its output interpreted by `libghostty-vt` (from Nix: `nix/libghostty-vt.nix`), the screen as scene text cells. No `termpix`. |
| `crates/temzdf-tty` | Library. Terminal frontend (`TtyFrontend`): shows scene frames through `termpix`, frame pacing, effects. |
| `crates/temzdf-server` | Library. `Session` (panes, layout, focus, modes and key mappings, the command registry, composed frame), the session server process, forward-search sockets. No terminal code. |
| `crates/temzdf-script` | Library. The script host (`temzdf script-host`): runs `config.ts`/`config.js` in QuickJS (`rquickjs`, TypeScript stripped with swc) and talks to the session over `temzdf-protocol`. The server never links it (child process). |
| `crates/tuitest` | Test-only library: runs a program on a pty behind `libghostty-vt` (optionally inside a private tmux), drives it with keys and reads the screen (text, colours, cursor, image placements). Used by `crates/temzdf/tests/tui.rs`. |
| `crates/temzdf` | The binary: `temzdf [FILE]` starts a session (named alfa, bravo, ... unless `-s NAME`), session commands and client (zellij names: `attach`, `ls`, `kill-session`, `action`), the standalone viewer (`temzdf standalone pdf`), `ls-fonts`, config, headless `--render-frame`. |
| `doc/` | Design notes and research. Start with `doc/architecture.md`. |
| `tests/fixtures/` | LaTeX sources of the test documents. Their PDFs and SyncTeX files are generated into `target/fixtures/` (never committed). |
| `todo.md` | Long-running tasks and deferred work. |
| `experiments/` | Standalone experiment crates outside the workspace (see the matching doc). |

Dependency direction: `termpix-core <- termpix <- temzdf-tty <- temzdf` and
`temzdf-scene <- temzdf-pdfcore <- temzdf-pdfview <- temzdf-server <- temzdf`
(and `temzdf-scene <- temzdf-term <- temzdf-server`)
(details in `doc/architecture.md`). Never make `termpix*` depend on PDF,
SyncTeX or viewer code, and never make `temzdf-pdfcore`, `temzdf-pdfview`,
`temzdf-term` or `temzdf-server` depend on `termpix` (tests excepted): backends, panes and the
session produce scenes, only frontends draw. The direction the project is
heading (a tmux-like session server with terminal, PDF and web panes) is in
`doc/session-server.md`.

## Environment

Everything runs inside the Nix dev shell (`flake.nix`). Agents are started
inside it, so run tools directly, not through `nix develop -c` (it costs a
shell evaluation each time); if a tool the dev shell should provide is
missing, say so instead of working around it. A tool added to `flake.nix`
needs the shell, and the agent in it, restarted.

```sh
scripts/check.sh            # fmt check + clippy -D warnings + tests + Miri, terse output
scripts/check.sh --fix      # same, but apply rustfmt
scripts/check.sh --no-miri  # without Miri (it takes about a minute and a half)
cargo build --workspace
```

Run `scripts/check.sh` before every commit. The scripts enter the dev shell by
themselves when started outside it. A unit test Miri cannot run (foreign
code, ptys, processes, sockets) gets `#[cfg_attr(miri, ignore = "why")]`;
see "Miri" in `doc/testing.md`.

Design notes and research live in `doc/` with the code (they used to be on a
separate `research/notes` branch, merged in on 2026-10-27).

Behaviour visible on screen (terminal panes, sessions, CLI, keys) is best
tested in `crates/temzdf/tests/tui.rs`: the real binary behind a simulated
terminal (`crates/tuitest`), checked by text, cells and image placements, also
inside tmux, without an X server. See "Simulated terminal tests" in
`doc/testing.md`.

End-to-end checks in a real kitty (virtual X server, no display needed):

```sh
cargo build --example scroll_demo
scripts/e2e.sh tests/e2e/scroll_demo.actions -- target/debug/examples/scroll_demo
```

Action files in `tests/e2e/` drive the app (wheel, drag, click, keys) and save
screenshots to `target/e2e/`; read the PNGs or measure pixels to verify. For
key-driven checks prefer `expect NAME KEYS`, which compares the terminal with
a headless render and fails the run on a mismatch (`tests/e2e/expect.actions`). See the
script header for the helpers. `temzdf standalone pdf FILE --render-frame out.png`
renders one screen headlessly. `scripts/build-fixtures.sh` builds `target/fixtures/` from
`tests/fixtures/*.tex` (tests run it automatically; e2e scenarios need it first).
For refactorings, `scripts/frames.sh render-rev HEAD A` (the last commit),
`scripts/frames.sh render B` (your build) and `scripts/frames.sh compare A B`
prove the screen is pixel-identical. A test written for a fix should fail
without it: `scripts/mutate.sh FILE OLD NEW -- cargo test ...` puts the bug
back for one run and restores the file (it refuses a pattern that does not
match once, and tells a mutation that did not compile or a filter that ran
no test from a caught one). The
full testing approach is in `doc/testing.md`.

`scripts/e2e.sh -t gui ... -- target/debug/temzdf-gui FILE` drives the GUI
client the same way (xfwm4 on Xvfb, lavapipe; `-I` adds fcitx5 with Mozc for
Japanese input); its GPU tests (`cargo test -p temzdf-gui --test gpu`) run
offscreen on lavapipe.

`scripts/e2e.sh` is Linux-only. On macOS, `scripts/macos-gui.sh` runs kitty,
Ghostty, WezTerm or `temzdf-gui` (`launch app`) in the background on a
virtual display and drives them (keys, mouse, window screenshots) without
disturbing the desktop; see its header and `doc/macos.md`. Scripted macOS
scenarios are in `tests/macos/` (`gui-redraw.sh`: the GUI redraws after its
window was covered).

Debugging terminal problems:

- `cargo run -p termpix --example probe` prints what detection found.
- `cargo run -p termpix --example events -- events.log` logs every parsed input
  event (what the terminal really sends).
- In interactive mode `temzdf` sends stdout/stderr (MuPDF and SyncTeX warnings)
  to `temzdf.log`, and session servers theirs to `NAME.log`, in the log
  directory: `$TEMZDF_LOG_DIR`, else `$XDG_STATE_HOME/temzdf`. `temzdf ls -v` shows each session's log file.
- `TERMPIX_TRACE=out.bin <app>` records every byte written to the terminal;
  replay prefixes of it into a terminal (`cat out.bin`) to bisect crashes.
- `TERMPIX_BACKEND=layered|composited` and `TERMPIX_MEDIUM=direct|shm|file`
  override detection (e.g. `direct` mimics SSH).
- `scripts/e2e.sh -t ghostty ...` runs the same scenarios in Ghostty; it exits
  with 3 when the terminal crashed.

The host has no `rustc` or `python3` on `PATH` outside the dev shell (`python3` is in it for scratch scripts). The default
dev shell leaves out large tools that are usually installed globally: TeX
(`latexmk`, `pdflatex`, `synctex`, needed for the fixtures) and the extra
terminals/multiplexers for e2e (Ghostty, WezTerm, zellij). On this machine they
come from the host profile. `nix develop .#alldeps` provides all of them.
`nix develop .#ci` is the minimal shell CI uses (toolchain, TeX, DejaVu fonts
only); `nix build` builds the `temzdf` package (`nix run . -- FILE` runs it).
CI (`.github/workflows/ci.yml`) runs `scripts/check.sh` in that shell and
`nix build` on Linux and macOS.

## Conventions

- Everything written into the repository (docs, comments, `todo.md`, skills,
  commit messages) is in English.
- Never commit binary files (`.pdf`, `.gz`, images, ...) or anything a tool
  generates (LaTeX/pandoc/typst output, SyncTeX files, logs, screenshots).
  Generated files can embed machine-specific absolute paths without anyone
  noticing. Commit the source plus a script that generates the output into
  `target/` instead, as `scripts/build-fixtures.sh` does. Exceptions: lock files
  (`Cargo.lock`, `flake.lock`), which pin dependencies and contain no paths.
- Paths written in docs, comments and commit messages are relative to the
  repository root. Never write absolute paths (home directories, scratch
  directories, `/nix/store/...`).
- Commit at natural checkpoints (a coherent, self-contained change), only when
  `cargo test` and `cargo clippy` pass. Use Conventional Commits:
  `type(scope): summary`, types `feat|fix|refactor|test|docs|build|chore`,
  scope = crate or area (`termpix`, `termpix-ui`, `temzdf`, `synctex`, `nix`, ...).
  Do not push unless asked. Never put session URLs (e.g. `Claude-Session:`
  trailers) in commit messages; a `Co-Authored-By` trailer is fine.
- Prefer designs that can be verified without a human: deterministic CPU
  rendering, PNG snapshots (inspect them by reading the image), the `termpix`
  mock terminal, the simulated terminal (`crates/tuitest`), and automated tests. Only the "feel" of scrolling in a real
  terminal needs the user.
- When you learn something new about terminal behaviour, record it in
  `doc/terminal-compat.md`.
- When a task starts repeating, follow `.claude/skills/automate-repeated-work`
  and turn it into a script or skill, then link it from here.

## Documents

- `doc/architecture.md`: overall design, threads, data flow, invariants.
- `doc/research.md`: library survey and why each dependency was chosen.
- `doc/kitty-protocol.md`: the parts of the kitty graphics protocol we rely on.
- `doc/terminal-compat.md`: per-terminal support matrix and known bugs.
- `doc/input.md`: mouse/keyboard reporting modes and smooth scrolling.
- `doc/reload.md`: auto-reload design (learned from Skim and Okular).
- `doc/testing.md`: how to verify changes without a human.
- `doc/synctex.md`: forward/inverse search, editor setup, vendored parser.
- `doc/config.md`: the configuration (JavaScript/TypeScript): modes, key notation, default mappings, options, commands, environment variables (keep in sync with `crates/temzdf-script/runtime/defaults.ts` and the registry in `crates/temzdf-server/src/session/commands.rs`).
- `doc/performance.md`: measurements (raster pool, display lists, transfer) and the benchmark to rerun.
- `doc/macos.md`: macOS specifics, verification results, and how to drive GUI terminals there.
- `doc/session-server.md`: the session server design, what is implemented and what comes next.
- `doc/script-host.md`: the JavaScript/TypeScript configuration: the script host, execution model, API.
- `doc/gpu.md`: GPU rasterisation experiment (`experiments/gpu-raster/`): why not, and the `vello_cpu` lead.
- `doc/known-issues.md`: problems known and left in place, why, and what a fix would take.
- `doc/scripting.md`: the scripting and configuration design: decisions with the user, open questions.
- `doc/keymaps.md`: the key mapping system: modes, conditions on mappings (`when`), namespaces, transient modes (submodes); decisions and what is deferred.
- `doc/floating-panes.md`: the floating panes design, and bars on panes and the window.
- `doc/gui-client.md`: groundwork and plan for a native GUI client (not implemented).
- Research behind the session server: `doc/multiplexer.md` (what a multiplexer can reuse), `doc/gui-stack.md` (GUI toolkits), `doc/markdown-viewer.md` and `doc/webview-viewer.md` (previewers on `termpix`), `doc/config-language.md` (configuration languages; superseded by `doc/scripting.md`).
