# Documents

Every document under `doc/`, by kind. Start with
[architecture.md](architecture.md). `scripts/check-docs.sh` (run by
`scripts/check.sh`) checks that each one is listed here and that paths to
documents written anywhere in the repository exist.

- [architecture.md](architecture.md): overall design, crates, threads, data
  flow, invariants.
- [config.md](config.md): the configuration (JavaScript/TypeScript), for
  users: modes, key notation, default mappings, options, commands,
  environment variables. Keep it in sync with
  `crates/temzdf-script/runtime/defaults.ts` and the command registry in
  `crates/temzdf-server/src/session/commands.rs`.

## Design (`design/`)

How the parts that exist are built, and what was decided with the user on
the way, with the reasons.

- [design/session-server.md](design/session-server.md): the session server
  (tmux-like; terminal, PDF and later web panes), clients, the scene
  protocol, shared documents, the crate layout, tab pages.
- [design/scripting.md](design/scripting.md): the scripting and
  configuration design: where scripts run, the execution model, commands,
  modes, hooks, buffers, layout.
- [design/script-host.md](design/script-host.md): the script host
  (QuickJS in WebAssembly, run in the server): the sandbox, its protocol
  and API.
- [design/keymaps.md](design/keymaps.md): the key mapping system: modes,
  conditions on mappings (`when`), namespaces, transient modes (submodes).
- [design/floating-panes.md](design/floating-panes.md): floating panes,
  and bars on panes and the window.
- [design/tui-panes.md](design/tui-panes.md): script-made TUI panes, a
  draft (nothing built): styled text, a minimal layout, keyed updates,
  which libraries were looked at.
- [design/text-editor.md](design/text-editor.md): the text editor
  framework's specification (nothing built): positions, cursor and
  selection, primitives, search, decorations, undo, events, what hooks may
  do, key input, base modes, files, drawing; editors such as Vim are
  scripts.
- [design/dot-repeat.md](design/dot-repeat.md): Vim's `.` for the future
  text editor, a draft (nothing built): recorded as actions, units,
  `repeat.set`, nesting.
- [design/gui-client.md](design/gui-client.md): the native GUI client
  (`temzdf-gui`): the design, the decisions and how it was built.
- [design/reload.md](design/reload.md): auto-reload (learned from Skim and
  Okular).
- [design/synctex.md](design/synctex.md): SyncTeX forward and inverse
  search, editor setup, the vendored parser.

## Terminals and platforms (`terminals/`)

What terminals and operating systems do, and what we rely on.

- [terminals/terminal-compat.md](terminals/terminal-compat.md):
  per-terminal support matrix and known bugs (record new findings here).
- [terminals/kitty-protocol.md](terminals/kitty-protocol.md): the parts of
  the kitty graphics protocol we rely on.
- [terminals/input.md](terminals/input.md): mouse and keyboard reporting
  modes, smooth scrolling.
- [terminals/macos.md](terminals/macos.md): macOS specifics, verification
  results, and how to drive GUI terminals there.

## Development (`dev/`)

How to work on temzdf and where it stands.

- [dev/testing.md](dev/testing.md): how to verify changes without a human
  (unit, simulated terminal, e2e in real terminals, Miri).
- [dev/performance.md](dev/performance.md): measurements (raster pool,
  display lists, transfer, the session socket) and the benchmarks to rerun.
- [dev/known-issues.md](dev/known-issues.md): problems known and left in
  place, why, and what a fix would take. Planned work is in `todo.md`.

## Surveys (`survey/`)

Research done before deciding (libraries, prior art, experiments), as of
when it was written; most of it came from the former `research/notes`
branch. The decisions taken from it are in the design documents.

- [survey/research.md](survey/research.md): the library survey and why
  each dependency was chosen.
- [survey/multiplexer.md](survey/multiplexer.md): what a terminal
  multiplexer can reuse (tmux, zellij, libraries).
- [survey/gui-stack.md](survey/gui-stack.md): GUI toolkits for the GUI
  client.
- [survey/config-language.md](survey/config-language.md): configuration
  languages (its Lua pick was superseded by JavaScript/TypeScript,
  `doc/design/scripting.md`).
- [survey/markdown-viewer.md](survey/markdown-viewer.md) and
  [survey/webview-viewer.md](survey/webview-viewer.md): previewers on
  `termpix` (Markdown, web pages).
- [survey/text-editor.md](survey/text-editor.md): an embedded text editor:
  text and decorations (VS Code, Neovim, Zed), Vim's undo tree, undo files,
  swap files and regular expressions, syntax highlighting and Tree-sitter
  grammars, the editor buffer as a base for script-made panes.
- [survey/script-host-transport.md](survey/script-host-transport.md):
  round trips between scripts and the server, measured for the script host
  over pipes, rquickjs in process and QuickJS in wasmtime, also on one core
  and under a CPU quota (`experiments/script-host-bench/`).
- [survey/gpu.md](survey/gpu.md): the GPU rasterisation experiment
  (`experiments/gpu-raster/`): why not, and the `vello_cpu` lead.
