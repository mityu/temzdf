# TODO

Long-running and deferred work. Milestones M0–M6 are the current plan; the items
below them are deferred.

## Milestones

- [x] M0: scaffolding (flake, workspace, docs, this file)
- [x] M1: `termpix` (tty, probe, input, graphics, compositor Layered/Composited, scroll, mock, `scroll_demo` example)
- [x] M2: minimal viewer (open PDF, continuous layout, tile cache, smooth scroll, zoom, page navigation); the window title stands in for the status bar
- [x] M3: `termpix-ui` and viewer features (status bar, outline sidebar, search, links + history, text selection + OSC 52 copy, config file)
- [x] M4: auto-reload (see `doc/reload.md`)
- [x] M5: SyncTeX (vendored parser, IPC socket, forward/inverse search). SyncTeX anchoring on reload was dropped, see `doc/reload.md`
- [x] M6: compatibility (Composited backend verified on WezTerm and in zellij; zellij/tmux recognised from XTVERSION; inline frames fenced for slow links)

## Session server (`doc/session-server.md`)

Steps 1-3 are done (PDF and terminal panes, standalone viewer and session
server on one `Session`). Next and open:

- [ ] UI chrome as cell grids in the scene instead of pixels.
- [ ] Per-client pixel density (every client now gets frames for the latest
  client's geometry and cell size) and zoom stored relative to the pane width.
- [ ] Shared memory instead of the socket for local clients' pixels (measure
  the socket first).
- [ ] Windows/tabs; a Vim-like command line on the `Action` text form.
- [ ] User report (2026-10-26, deferred): after resizing the terminal window
  with split panes, the bottom-right pane's status bar (file name) stayed where
  it was. Not reproduced in kitty or WezTerm (standalone and session) nor
  through the mock terminal (`resizing_split_panes_redraws_like_a_fresh_start`).
  Suspect: the cell size derived from the kernel's pixel size after a resize
  (`Winsize::geometry` divides by rows/cols; padding would skew it, worst at the
  bottom). Needs the terminal, mode and the `resized to geometry` log line.
- [ ] Terminal panes, what is left: kitty graphics through Unicode
  placeholders (tmux inside a pane), animation frames, and images re-emitted
  per client when clients differ in cell size.
- [ ] Scripting and configuration (JavaScript/TypeScript in a script-host
  process, replacing the TOML file): design in `doc/scripting.md` on the
  `research/notes` branch; the host and execution model are implemented
  (`doc/script-host.md`), and so are modes and key mappings, the shipped
  defaults in JavaScript, options, `print-types`, floating messages and the
  escape keys (TOML is gone), buffers separated from panes, and terminal
  panes' selection, links and images, and pane sizes kept when the screen
  changes size. Next: default mappings for switching buffers, changing pane
  sizes (a command, dragging borders). The
  questions under "Open questions, to decide later" there (modes and
  mappings, escape hatches, message area, mouse, defaults, layout details,
  hidden buffers, client-side settings, reload, CLI scripting,
  `check-config`) must be decided before or while implementing.
- [ ] Step 4: GUI client (winit + wgpu). Step 5: web panes (headless
  Chromium). Step 6: programmable configuration.

## Deferred

- zellij delays keyboard input (even its own prefix commands) while it
  processes a frame, so `q` waits there; sending less data per frame is the
  only lever we have (user agreed to defer, 2026-10-25).
- Multiplexers: zellij (Composited strips) and tmux 3.8 (Unicode placeholders
  through passthrough) work; see `doc/terminal-compat.md`. Left: zellij over SSH
  is still slow (zellij 0.45.1 forwards images uncompressed; a half-resolution
  mode could help), and tmux master's native kitty support (z=0 only) is untested.
- macOS GUI checks (`scripts/macos-gui.sh`) are driven step by step; action
  files and `expect` judging as in `scripts/e2e.sh` could be shared.
- Vector-heavy pages: MuPDF's anti-aliased rasteriser is slow there and
  `vello_cpu` about 20x faster (`doc/gpu.md`; GPU rendering does not pay off).
  Measure real TikZ/pgfplots documents first; then maybe render such pages
  through a MuPDF-device-to-`vello_cpu` translation.
- Two-page (spread) layout.
- Markdown viewer reusing `termpix` (webview rendered offscreen).
- Showing PDF annotations.
- Check crate names (`termpix`, `termpix-ui`, `temzdf`) for collisions before publishing.
- Judged e2e (`expect` in `scripts/e2e.sh`, `doc/testing.md`) covers key-driven
  scenarios in kitty, WezTerm, tmux and zellij. Left: mouse actions (wheel and
  drag go through the kinetic scroll; a headless `--events` list with timing
  could replay them), Ghostty (find the terminal area below its decorations),
  and converting more of the existing screenshot scenarios.
- MuPDF's allocation lock makes text rendering slower with more threads (see
  `doc/performance.md`). `fz_malloc`/`fz_free` take `FZ_LOCK_ALLOC` around the
  allocator itself, and `mupdf-sys` backs every context with the same static
  mutexes, so neither a custom allocator nor separate base contexts help. Only
  matters if rasterising ever becomes the bottleneck; then: patch the
  `mupdf-sys` lock wrapper, or render text-heavy tiles on fewer threads.
- Measure frame pacing over a real slow SSH link (fences and the writer thread
  are in place but were only tested locally, with a throttling pty proxy).
- The page thumbnail is misplaced by up to half a cell with the Layered
  backend (terminals scale images to whole cells). It only shows briefly; the
  `StandIn::Composed` path could compose it too.
- Long text input through `$EDITOR` (do this last):
  1. Suspend the UI, run `$EDITOR` full screen, resume on exit.
  2. Later: split the screen and embed the editor (terminal emulation with e.g. `alacritty_terminal`).
  Short inputs (search, page number, commands) use the built-in one-line input.
