# Architecture

## Goals

- Terminal PDF viewer for LaTeX previews, with GUI-grade mouse interaction.
- Everything on screen is pixels sent with the kitty graphics protocol. Math-heavy
  pages and UI chrome are drawn by us, not by the terminal's text renderer.
- The terminal layer is a reusable library (`termpix`), independent of PDF.

## Crates

```
temzdf (bin) ──> termpix-ui ──> termpix
   │                              ▲
   └──────────────────────────────┘
```

### termpix

| Module | Responsibility |
|---|---|
| `tty` | Open `/dev/tty`, raw mode, enable modes (1003, 1006, 1016, 2004, 1004, 2048, kitty keyboard `CSI >31u`), restore on drop and on panic, SIGWINCH. |
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

### temzdf

- Threads: a `doc` thread owns the MuPDF `Document` (it is not `Send`) and builds
  `DisplayList`s (which are `Send + Sync`); a worker pool rasterises them; an input
  thread; the main loop. Channels in between.
- Display: continuous vertical layout. Pages are rendered as tiles into an LRU
  cache; uploaded images are kept by the terminal; neighbours are pre-rendered.
  After a zoom, a low-resolution thumbnail of each page (scaled to the new size)
  stands in until tiles at the new scale arrive.
- Search hits and selections are separate translucent overlay layers, never baked
  into page pixels.
- Frame pacing (`app.rs`): the first frame waits (up to 1.5 s) until the visible
  pages are fully rendered, frames wait while inline transfers are unacknowledged,
  and Composited frames are capped at ~30 fps (5 fps over inline links or inside a
  multiplexer, where every strip is forwarded uncompressed by zellij 0.45).
- Auto-reload: see `reload.md`. SyncTeX: vendored `synctex_parser.c` via `cc`,
  forward search over a Unix socket, inverse search via a configurable command.

## Invariants

- `termpix` and `termpix-ui` never depend on PDF, SyncTeX or viewer code.
- Rendering is deterministic on the CPU so screens can be snapshot-tested.
- Every frame sent to the terminal is wrapped in synchronized output.
- The terminal is always restored, including on panic.
