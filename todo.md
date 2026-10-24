# TODO

Long-running and deferred work. Milestones M0–M6 are the current plan; the items
below them are deferred.

## Milestones

- [x] M0: scaffolding (flake, workspace, docs, this file)
- [ ] M1: `termpix` (tty, probe, input, graphics, compositor Layered/Composited, scroll, mock, `scroll_demo` example)
- [ ] M2: minimal viewer (open PDF, continuous layout, tile cache, smooth scroll, zoom, page navigation, status bar)
- [ ] M3: `termpix-ui` and viewer features (outline sidebar, search, links + history, text selection + OSC 52 copy, config file)
- [ ] M4: auto-reload (see `doc/reload.md`)
- [ ] M5: SyncTeX (vendored parser, IPC socket, forward/inverse search, `reload.anchor = "synctex"`)
- [ ] M6: compatibility (Composited backend on WezTerm and zellij, terminal detection table modelled on yazi)

## Deferred

- tmux support via Unicode placeholders (cell-granular scrolling only).
- macOS support (shm quirks, `select()` instead of `poll()` on `/dev/tty`).
- GPU rasterisation experiment with `vello_gpu` (measure readback + transfer cost).
- Two-page (spread) layout.
- Markdown viewer reusing `termpix` (webview rendered offscreen).
- Showing PDF annotations.
- Check crate names (`termpix`, `termpix-ui`, `temzdf`) for collisions before publishing.
- CI: GitHub Actions running `cargo test` and `cargo clippy`.
- Tune the raster worker pool and measure performance.
- Long text input through `$EDITOR` (do this last):
  1. Suspend the UI, run `$EDITOR` full screen, resume on exit.
  2. Later: split the screen and embed the editor (terminal emulation with e.g. `alacritty_terminal`).
  Short inputs (search, page number, commands) use the built-in one-line input.
