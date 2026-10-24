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
- GPU rasterisation experiment with `vello_gpu` (measure readback + transfer cost).
- Two-page (spread) layout.
- Markdown viewer reusing `termpix` (webview rendered offscreen).
- Showing PDF annotations.
- Check crate names (`termpix`, `termpix-ui`, `temzdf`) for collisions before publishing.
- Decide how to judge e2e scenarios automatically (later, once behaviour settles;
  today Claude reads the screenshots). Avoid committing many reference images.
  Candidates: pixel probes at known coordinates (e.g. a band edge moved 360 px),
  assertions on `TERMPIX_TRACE` output (no page re-upload, one strip sent),
  clipboard text and exit codes; where a whole image must match, store only a
  hash, or render references from the previous build into `target/` and diff.
  Promising (user idea, 2026-10-25): render each scenario's expected screen
  with `--render-frame` (same geometry, cell size, `--keys` for the same
  actions; the mock terminal shows what a kitty-compatible terminal should) and
  compare it with the e2e screenshot. Both sides use the same MuPDF rendering,
  so this cannot tell whether the PDF is rendered correctly; it tests the path
  from viewer state to the terminal's screen (placements, crops, strip updates,
  stale images left behind, what the terminal does with our commands). Fits
  view-state checks such as zoom, page jumps and search highlights, and needs
  no stored images. Needs: crop the
  terminal's padding/decorations, wait until tiles settled, a small tolerance
  for terminal-side scaling, and headless equivalents of mouse actions (wheel
  and drag go through the kinetic scroll, so drive it with the same steps).
- Tune the raster worker pool and measure performance.
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
