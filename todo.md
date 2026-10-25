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

## Deferred

- Multiplexers: zellij (Composited strips) and tmux 3.8 (Unicode placeholders
  through passthrough) work; see `doc/terminal-compat.md`. Left: zellij over SSH
  is still slow (zellij 0.45.1 forwards images uncompressed; a half-resolution
  mode could help), and tmux master's native kitty support (z=0 only) is untested.
- Verify IME preedit placement in the search prompt on kitty and Ghostty (the
  terminal cursor is shown at the caret while the prompt is open).
- macOS support (shm quirks, `select()` instead of `poll()` on `/dev/tty`).
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
- CI has not run on GitHub yet (written and linted locally); check the first run,
  especially the macOS `nix build` job.
