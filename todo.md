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
- Content-based reload anchor: remember the text of the line at the anchor
  point and find it again in the reloaded PDF, so insertions above the reading
  position do not shift the view (an alternative to SyncTeX anchoring, which was
  dropped; see `doc/reload.md`).
- Document a vimtex setup that forward-searches after every successful compile.
- Configurable key bindings (the config file has no `[keys]` section yet).
- Auto-scroll while drag-selecting past the top or bottom edge.
- Verify IME preedit placement in the search prompt on kitty and Ghostty (the
  terminal cursor is shown at the caret while the prompt is open).
- macOS support (shm quirks, `select()` instead of `poll()` on `/dev/tty`).
- GPU rasterisation experiment with `vello_gpu` (measure readback + transfer cost).
- Two-page (spread) layout.
- Markdown viewer reusing `termpix` (webview rendered offscreen).
- Showing PDF annotations.
- Check crate names (`termpix`, `termpix-ui`, `temzdf`) for collisions before publishing.
- CI: GitHub Actions running `cargo test` and `cargo clippy`.
- Decide how to judge e2e scenarios automatically (later, once behaviour settles;
  today Claude reads the screenshots). Avoid committing many reference images.
  Candidates: pixel probes at known coordinates (e.g. a band edge moved 360 px),
  assertions on `TERMPIX_TRACE` output (no page re-upload, one strip sent),
  clipboard text and exit codes; where a whole image must match, store only a
  hash, or render references from the previous build into `target/` and diff.
- Tune the raster worker pool and measure performance.
- Measure frame pacing over a real slow SSH link (fences are in place but were
  only tested locally), and consider smaller frames for the Composited backend
  (update only changed strips) since zellij 0.45.1 forwards raw RGBA.
- Send RGB (`f=24`) instead of RGBA for opaque tiles: 25% fewer bytes; needs `Compositor` to accept RGB images.
- While zooming, show old-scale tiles scaled instead of the blurry thumbnail (Layered backend can only scale to whole cells, so this needs care).
- Keep display lists for all pages only while memory allows; add an LRU for huge documents.
- Long text input through `$EDITOR` (do this last):
  1. Suspend the UI, run `$EDITOR` full screen, resume on exit.
  2. Later: split the screen and embed the editor (terminal emulation with e.g. `alacritty_terminal`).
  Short inputs (search, page number, commands) use the built-in one-line input.
- Make `nix build` build temzdf (a package output in flake.nix: crane or rustPlatform, with the vendored SyncTeX/MuPDF C builds).
