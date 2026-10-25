# Architecture

## Goals

- Terminal PDF viewer for LaTeX previews, with GUI-grade mouse interaction.
- Everything on screen is pixels sent with the kitty graphics protocol. Math-heavy
  pages and UI chrome are drawn by us, not by the terminal's text renderer.
- The terminal layer is a reusable library (`termpix`), independent of PDF.

## Crates

| Crate | Depends on (ours) |
|---|---|
| `termpix-core` | - |
| `termpix` | `termpix-core` |
| `termpix-ui` | - |
| `temzdf-scene` | `termpix-core` |
| `temzdf-protocol` | `temzdf-scene`, `termpix-core` |
| `temzdf-pdfcore` | `temzdf-scene` |
| `temzdf-pdfview` | `temzdf-pdfcore`, `temzdf-scene`, `termpix-core`, `termpix-ui` |
| `temzdf-term` | `temzdf-scene`, `termpix-core` (and `libghostty-vt`, `portable-pty`) |
| `temzdf-tty` | `temzdf-scene`, `termpix` |
| `temzdf-server` | `temzdf-protocol`, `temzdf-pdfview`, `temzdf-pdfcore`, `temzdf-term`, `temzdf-scene`, `termpix-core`, `termpix-ui` |
| `temzdf` (bin) | `temzdf-server`, `temzdf-protocol`, `temzdf-pdfview`, `temzdf-tty`, `temzdf-scene`, `termpix` |

- `termpix-core`: frontend-independent primitives (geometry, raster helpers,
  kinetic scroll, key/mouse events).
- `termpix`: the terminal library (below); re-exports `termpix-core`.
- `termpix-ui`: pixel widgets (below); no dependency on `termpix`.
- `temzdf-scene`: what a pane shows, independent of the frontend: an image
  store whose changes frontends mirror, and frames (layers, text cursor,
  pointer shape, title).
- `temzdf-pdfcore`: the PDF backend: MuPDF wrapper, `DocCache` (display lists
  and tiles), page text and selection, SyncTeX, background workers.
- `temzdf-pdfview`: the PDF pane (`PdfPane`, `Viewer`, outline, search, keys)
  and what panes on one file share (`Document`, `DocumentHost`: workers,
  reload, SyncTeX routing).
- `temzdf-term`: the terminal pane (`TermPane`: a program on a pty, its
  output interpreted by `libghostty-vt`, the screen as scene text cells).
- `temzdf-tty`: the terminal frontend (`TtyFrontend`: terminal setup, frame
  pacing, effects; `Presenter`, `Screen`).
- `temzdf-protocol`: the session protocol (messages, framing, sockets).
- `temzdf-server`: `Session` (panes, layout, focus, prefix keys, actions, the
  composed frame) and the session server process.
- `temzdf`: the binary: standalone viewer, session commands and client,
  config, headless rendering. The standalone viewer, `--render-frame` and the
  session server all run a `Session`.

This layout prepares the session server (`doc/session-server.md`): backends and
panes know nothing about terminals, only frontends draw.

### termpix

| Module | Responsibility |
|---|---|
| `tty` | Open `/dev/tty`, raw mode, enable modes (1003, 1006, 1016, 2004, 1004, 2048, kitty keyboard `CSI >31u`), restore on drop and on panic, SIGWINCH. Writes are queued for a writer thread (`output`), so a slow terminal never blocks input handling; on exit the backlog is dropped and a cut string sequence terminated before the reset. |
| `probe` | Kitty graphics support (`a=q` + DA1), shm/file transmission support, XTVERSION, DECRQM for 1016/2026, cell size (`TIOCGWINSZ` -> `CSI 16 t` -> `CSI 14 t`), env hints, user override. |
| `input` | Hand-written parser: bytes -> `Event` (pure, unit-tested). SGR mouse in cell or pixel coordinates, kitty keyboard / CSI u, bracketed paste, focus, in-band resize (2048), APC `_G` replies. crossterm is not used: it does not parse 1016 and mishandles APC replies. |
| `graphics` | Kitty graphics transport: `t=s` (POSIX shm), `t=f`, `t=d` (zlib + base64, 4096-byte chunks). Image id management, re-upload on `ENOENT` (eviction), fences for inline transfers, shm cleanup. |
| `compositor` | The app submits a list of `Layer { image, src_rect, dst_px, z }` per frame. The compositor diffs against the previous frame and emits placements/deletions/uploads wrapped in synchronized output (2026). Two backends: `Layered` (kitty, Ghostty; images uploaded once, re-placed per frame) and `Composited` (WezTerm, zellij; CPU-composite the screen and send it as one image (WezTerm) or as horizontal strips of N cell rows, re-sending only strips whose pixels changed (zellij)). A third backend, `Placeholders` (tmux), composites strips like `Composited` but shows them as `U=1` virtual images through Unicode placeholder text written once, wrapping graphics in tmux passthrough. Inline (direct) frames are fenced with an `a=q` query so callers can wait for slow links (`is_congested`). |
| `scroll` | Kinetic scrolling: wheel ticks are impulses on a target offset, eased with exponential decay; flick acceleration; emits ticks only while animating. |
| `mock` | Interprets emitted kitty commands and reconstructs the screen as an image, for tests. |

Pixel positioning: a placement sits at a cell plus a sub-cell offset (`X`, `Y`)
and shows a cropped source rect (`x,y,w,h`) scaled to `c x r` cells. Negative or
off-screen parts are handled by cropping.

### termpix-ui

Widgets drawn with `vello_cpu` into RGBA layers, text by `parley`/`fontique`,
layout by `taffy`. Text, virtualised list (outline, search results), one-line
input, status bar, popup, scrollbar. Each widget is its own layer so only changed
widgets are re-uploaded. IME: the terminal draws the preedit at the cursor, so the
input widget moves the terminal cursor to its caret; we only receive committed text.

### temzdf (pdfcore, pdfview, tty, bin)

- Threads: a `doc` thread owns the MuPDF `Document` (it is not `Send`) and builds
  `DisplayList`s (which are `Send + Sync`); a worker pool rasterises them; an input
  thread; the main loop. Channels in between.
- Display: continuous vertical layout. Pages are rendered as tiles into an LRU
  cache; uploaded images are kept by the terminal; neighbours are pre-rendered.
  After a zoom, the tiles of the previous scale stand in until tiles at the new
  scale arrive (`temzdf_pdfview::viewer::StandIn`): as scaled layers with Composited backends,
  composed into one unscaled image per page with Layered over shm/files
  (terminals scale images to whole cells only, which would misplace tiles by up
  to half a cell), and not at all over inline transfers, where each zoom step
  would cost an upload. A low-resolution thumbnail of each page covers what
  remains.
- Search hits and selections are separate translucent overlay layers, never baked
  into page pixels.
- Frame pacing (`crates/temzdf/src/app.rs`): the first frame waits (up to 1.5 s) until the visible
  pages are fully rendered, frames wait while inline transfers are unacknowledged,
  and Composited frames are capped at ~30 fps (5 fps over inline links or inside a
  multiplexer, where every strip is forwarded uncompressed by zellij 0.45).
- Auto-reload: see `reload.md`. SyncTeX: vendored `synctex_parser.c` via `cc`,
  forward search over a Unix socket, inverse search via a configurable command.
- Keys: `keys.rs` maps key presses to `Command`s (defaults plus `[keys]` from
  the config, see `config.md`); `PdfPane::command` runs them.
- Images: panes register images in a `temzdf_scene::Images` store and return
  layers referencing them; `temzdf_tty::Presenter` mirrors the store into the
  compositor (pixels are shared, not copied) and maps the layers.

## Invariants

- `termpix-core`, `termpix` and `termpix-ui` never depend on PDF, SyncTeX or
  viewer code.
- `temzdf-pdfcore` and `temzdf-pdfview` never depend on `termpix` (tests
  excepted); only `temzdf-tty` and the binary talk to the terminal.
- Rendering is deterministic on the CPU so screens can be snapshot-tested.
- Every frame sent to the terminal is wrapped in synchronized output.
- The terminal is always restored, including on panic.
