# Architecture

## Goals

- Terminal PDF viewer for live preview of documents written in typesetting
  systems that output PDF (LaTeX, Typst, SATySFi, ...), with GUI-grade mouse
  interaction.
- Everything on screen is pixels sent with the kitty graphics protocol. Math-heavy
  pages and UI chrome are drawn by us, not by the terminal's text renderer.
- The terminal layer is a reusable library (`termpix`), independent of PDF.

## Crates

Each crate lives in `crates/NAME` (`crates/termpix-core`, `crates/termpix-ui`,
`crates/temzdf-watchfs`, ...), except `xtask` at the top.

| Crate | Depends on (ours) |
|---|---|
| `termpix-core` | - |
| `termpix` | `termpix-core` |
| `termpix-ui` | - |
| `temzdf-scene` | `termpix-core` |
| `temzdf-protocol` | `temzdf-scene`, `termpix-core` |
| `temzdf-client` | `temzdf-paths`, `temzdf-protocol`, `temzdf-scene`, `termpix-core` |
| `temzdf-paths` | - |
| `temzdf-watchfs` | `temzdf-paths` (and `notify`) |
| `temzdf-pdfcore` | `temzdf-scene`, `temzdf-paths`, `temzdf-watchfs` |
| `temzdf-pdfview` | `temzdf-pdfcore`, `temzdf-paths`, `temzdf-watchfs`, `temzdf-scene`, `termpix-core`, `termpix-ui` |
| `temzdf-term` | `temzdf-scene`, `termpix-core` (and `libghostty-vt`, `portable-pty`) |
| `temzdf-texteditor` | `temzdf-scene` (and `ropey`) |
| `temzdf-markdown` | `temzdf-texteditor` (and `comrak`, `syntect`) |
| `temzdf-mdview` | `temzdf-scene`, `termpix-core`, `termpix-ui` (and Blitz, RaTeX) |
| `texprintf` | - (libtexprintf, C) |
| `temzdf-script` | `temzdf-protocol` (and `rquickjs`, `swc_ts_fast_strip`) |
| `temzdf-script-guest` | `temzdf-script`, `temzdf-protocol` (on `wasm32-wasip1` only) |
| `temzdf-script-runner` | `temzdf-protocol`, `temzdf-script` (and wasmtime; builds the guest) |
| `temzdf-server` | `temzdf-paths`, `temzdf-protocol`, `temzdf-pdfview`, `temzdf-pdfcore`, `temzdf-term`, `temzdf-texteditor`, `temzdf-markdown`, `temzdf-mdview`, `temzdf-watchfs`, `texprintf`, `temzdf-script-runner`, `temzdf-scene`, `termpix-core`, `termpix-ui` |
| `temzdf-tty` | `temzdf-client`, `temzdf-scene`, `termpix` |
| `temzdf` (bin) | `temzdf-server`, `temzdf-client`, `temzdf-protocol`, `temzdf-pdfview`, `temzdf-script`, `temzdf-tty`, `temzdf-scene`, `termpix` |
| `temzdf-gui` (bin) | `temzdf-client`, `temzdf-protocol`, `temzdf-scene`, `termpix-core` (and winit, wgpu, glyphon) |
| `temzdf-testhelper` (tests only) | none of ours (a program for tests to run instead of `sh`) |
| `tuitest` (tests only) | none of ours (`libghostty-vt`, `portable-pty`; `temzdf` uses it in `tests/tui.rs`) |
| `wlinput` (tests only) | none of ours (Wayland virtual keyboard and pointer) |
| `temzdf-fixtures` (tests only) | none of ours (builds `target/fixtures/` with latexmk) |
| `xtask` | `temzdf-fixtures` (repository tasks: `cargo xtask help`) |

- `termpix-core`: frontend-independent primitives (geometry, RGBA raster
  helpers, kinetic scroll, key/mouse events, and the viewport the PDF and
  Markdown panes scroll through: both axes, sizes, scrolling input,
  Ctrl+wheel as a zoom request). No terminal code.
- `termpix`: the terminal library (below): tty setup, capability probing,
  input parsing (SGR-pixel mouse, kitty keyboard), kitty graphics
  transport, the layered compositor, a mock terminal for tests;
  re-exports `termpix-core`. It knows nothing about PDF.
- `termpix-ui`: pixel widgets (below), a small widget kit rendering UI
  layers to RGBA (`vello_cpu` + `parley`, laid out by hand); no
  dependency on `termpix` (`termpix-core` only in its tests).
- `temzdf-scene`: what a pane shows, independent of the frontend: an image
  store whose changes frontends mirror, and frames (layers, text cells,
  text cursor, pointer shape, title), frontend hints and effects; `tiles`:
  content painted in tiles by worker threads, shared by the PDF and
  Markdown panes (a tile cache with the work in flight and LRU eviction,
  the grid of tiles on and near the screen, a frame's work in order, a
  worker pool whose queue each frame replaces, keeping the queued work it
  still wants).
- `temzdf-protocol`: the session protocol (client/server messages,
  framing, socket locations, finding sessions in `discovery`), and the
  sockets in `transport` (`Stream`, `Listener`: std's Unix sockets, or
  `uds_windows`'s on Windows).
- `temzdf-client`: what every client shares: naming, resolving and
  starting sessions (`spawn_server`), the connection to a session
  (`Connection`, one-shot `request`s), log locations, carrying out effects
  (links, inverse search). No terminal or window code.
- `temzdf-paths`: which file a path names, as the host's file system
  sees it: canonical names documents are known by (a file not there yet
  after its nearest ancestor that is), files' identity (device and inode,
  volume and file ID), and whether a directory's file system ignores the
  case of names (`o.fileIgnoreCase`), used to tell whether a file is
  already open; `file:` URIs of paths and the paths of `file:` URIs
  (on Windows plain drive paths only).
- `temzdf-watchfs`: watching a file for changes made elsewhere
  (`notify`; `watch_file` returns a `FileWatcher`, kept alive), also when
  a tool replaces it or it is a symlink: the PDF a typesetter writes
  (auto-reload in `temzdf-pdfcore`), the files of the server's text and
  Markdown buffers.
- `temzdf-pdfcore`: the PDF backend: MuPDF wrapper (`mupdf` crate),
  `DocCache` (display lists and tiles), page text and selection, SyncTeX,
  background workers, reload. No terminal code.
- `temzdf-pdfview`: the PDF pane (`PdfPane`, `Viewer`: view state, input
  handling, UI (status bar, outline, search, link preview), key bindings)
  and what panes on one file share (`Document`, `DocumentHost`: workers,
  reload, SyncTeX routing). No terminal code.
- `temzdf-term`: the terminal pane's buffer (`TermBuffer`: a program on a
  pty (`portable-pty`), its output interpreted by `libghostty-vt`, from
  Nix: `nix/libghostty-vt.nix`) and its view (`TermView`: the screen as
  scene text cells). No `termpix`.
- `temzdf-texteditor`: a framework for text editors
  (`doc/design/text-editor.md`): text (`ropey`, LF inside), positions
  (`{ line, character }`, UTF-16), edits, cursor and selection,
  decorations and marks, the undo tree, motions, search, files, and the
  layout and drawing of lines into scene text cells. Editors are scripts
  on it; the server's text panes use it
  (`crates/temzdf-server/src/session/text.rs`). No terminal code.
- `temzdf-markdown`: Markdown for the viewer (`doc/design/markdown-viewer.md`):
  a document of blocks keeping their source lines (comrak), the text
  mode's drawing (lines and decorations at a width; syntect for code),
  and the graphics mode's HTML. No terminal code.
- `temzdf-mdview`: the viewer's graphics mode: pages laid out and painted
  by Blitz in shared strips, panes scrolled, zoomed and searched over
  them, their status line; math by RaTeX. No terminal code.
- `texprintf`: TeX math as Unicode text, wrapping libtexprintf (utftex's
  library, C, vendored in `crates/texprintf/vendor/libtexprintf/` and
  built with `cc`).
- `temzdf-script`: the script host (QuickJS through `rquickjs`,
  TypeScript stripped with swc), see `doc/design/script-host.md`: it runs
  `config.ts`/`config.js` and talks to the session through a `Link` with
  `temzdf-protocol`'s messages (feature `host`; without it only the type
  declarations). `temzdf-script-guest` builds it for WebAssembly
  (`wasm32-wasip1`, with imports as its `Link`; empty on other targets),
  and `temzdf-script-runner` runs that in wasmtime inside the server
  (`ScriptHost`: a thread, a memory cap, stop). The runner's `build.rs`
  builds the guest with a nested cargo (`target/script-guest`, profile
  `script-guest`) and precompiles it with Cranelift.
- `temzdf-server`: `Session` (panes, layout, focus, modes and key
  mappings, the command registry, the composed frame), the session server
  process and the forward-search sockets. No terminal code.
- `temzdf-tty`: the terminal frontend (`TtyFrontend`: shows scene frames
  through `termpix`; terminal setup, frame pacing, effects; `Presenter`,
  `Screen`, text cells, notices for terminals without graphics).
- `temzdf`: the binary: `temzdf [FILE]` starts a session (named alfa,
  bravo, ... unless `-s NAME`), session commands and client (zellij
  names: `attach`, `ls`, `kill-session`, `action`), the standalone viewer
  (`temzdf standalone pdf`), config, `ls-fonts`, headless rendering
  (`--render-frame`). The standalone viewer, its `--render-frame` and the
  session server all run a `Session`.
- `temzdf-gui`: the GUI client, a winit window drawing the session with
  wgpu and glyphon (draw list, renderer, fonts with built-in fallbacks
  embedded from nixpkgs, input, IME, clipboard), `--render-frame`
  offscreen, `ls-fonts`; no `termpix`, no server code
  (`doc/design/gui-client.md`).
- `tuitest`, `wlinput`: test tools, see `doc/dev/testing.md`. `tuitest`
  runs a program on a pty behind `libghostty-vt` (optionally inside a
  private tmux), drives it with keys and reads the screen (text, colours,
  cursor, image placements); `crates/temzdf/tests/tui.rs` uses it.
  `wlinput` is one virtual keyboard and pointer on a wlroots compositor,
  driven by commands on stdin; `scripts/e2e.sh -d wayland` drives
  headless sway with it.
- `temzdf-testhelper`: a program for tests to run instead of `sh -c`
  (which is not on Windows): print bytes with escapes and numbered
  lines, report the terminal's size, set the terminal raw, read lines and
  answer them, echo input as hex, start itself in the background, sleep,
  exit. Its library's `path()` builds it and returns the executable (the
  terminal pane tests of `temzdf-term`, `temzdf-server` and `tuitest`
  use it).
- `temzdf-fixtures`: builds the test fixtures (`tests/fixtures/*.tex`
  into `target/fixtures/` with latexmk) when stale, for
  `temzdf_pdfcore::pdf::fixture` (feature `test-support`) and `cargo
  xtask fixtures` (`scripts/build-fixtures.sh`).
- `xtask`: repository tasks as `cargo xtask TASK` (alias in
  `.cargo/config.toml`; `cargo xtask help`): `check` and `test` (the gate,
  `doc/dev/testing.md`), `fetch-fonts` downloads the fonts `temzdf-gui`
  embeds (`crates/temzdf-gui/fonts.toml`) into `target/fonts/` for builds
  without Nix, `update-fonts` moves that list to newer releases
  (`doc/dev/building.md`), `mutate` puts a bug back for one run, and
  `fixtures` builds the test fixtures (`temzdf-fixtures`).

Other top-level directories and files:

- `doc/`: design notes and research, indexed in `doc/README.md`.
- `tests/fixtures/`: LaTeX sources of the test documents. Their PDFs and
  SyncTeX files are generated into `target/fixtures/` (never committed).
- `tests/e2e/`, `tests/macos/`: end-to-end scenarios (`doc/dev/testing.md`).
- `todo.md`: long-running tasks and deferred work.
- `experiments/`: standalone experiment crates outside the workspace (see
  the matching doc).
- `scripts/`: repository scripts (the gate's dev-shell entry, e2e,
  fixtures, frames, Miri, ...).

Dependency direction: `termpix-core <- termpix <- temzdf-tty <- temzdf` and
`temzdf-scene <- temzdf-pdfcore <- temzdf-pdfview <- temzdf-server <- temzdf`
(and `temzdf-scene <- temzdf-term <- temzdf-server`); the rules are under
"Invariants" below.

This layout prepares the session server (`doc/design/session-server.md`): backends and
panes know nothing about terminals, only frontends draw.

### termpix

| Module | Responsibility |
|---|---|
| `tty` | Open `/dev/tty`, raw mode, enable modes (1003, 1006, 1016, 2004, 1004, 2048, kitty keyboard `CSI >31u`), restore on drop and on panic, SIGWINCH. Writes are queued for a writer thread (`output`), so a slow terminal never blocks input handling; on exit the backlog is dropped and a cut string sequence terminated before the reset. |
| `probe` | Kitty graphics support (`a=q` + DA1), shm/file transmission support, XTVERSION, DECRQM for 1016/2026, cell size (`TIOCGWINSZ`, else `CSI 16 t`, else a fallback), env hints, user override. |
| `input` | Hand-written parser: bytes -> `Event` (pure, unit-tested). SGR mouse in cell or pixel coordinates, kitty keyboard / CSI u, bracketed paste, focus, in-band resize (2048), APC `_G` replies. crossterm is not used: it does not parse 1016 and mishandles APC replies. |
| `graphics` | Kitty graphics transport: `t=s` (POSIX shm), `t=t` (temporary file), `t=d` (zlib + base64, 4096-byte chunks). Image id management, re-upload on `ENOENT` (eviction), fences for inline transfers, shm cleanup. |
| `compositor` | The app submits a list of `Layer { image, src_rect, dst_px, z }` per frame. The compositor diffs against the previous frame and emits placements/deletions/uploads wrapped in synchronized output (2026). Two backends: `Layered` (kitty, Ghostty; images uploaded once, re-placed per frame) and `Composited` (WezTerm, zellij; CPU-composite the screen and send it as one image (WezTerm) or as horizontal strips of N cell rows, re-sending only strips whose pixels changed (zellij)). A third backend, `Placeholders` (tmux), composites strips like `Composited` but shows them as `U=1` virtual images through Unicode placeholder text written once, wrapping graphics in tmux passthrough. Inline (direct) frames are fenced with an `a=q` query so callers can wait for slow links (`is_congested`). |
| `placeholder` | Unicode placeholders (`U=1`) and tmux passthrough wrapping, for the `Placeholders` backend. |
| `scroll` (from `termpix-core`) | Kinetic scrolling: wheel ticks are impulses on a target offset, eased with exponential decay; flick acceleration; emits ticks only while animating. |
| `mock` | Interprets emitted kitty commands and reconstructs the screen as an image, for tests. |

Pixel positioning: a placement sits at a cell plus a sub-cell offset (`X`, `Y`)
and shows a cropped source rect (`x,y,w,h`) scaled to `c x r` cells. Negative or
off-screen parts are handled by cropping.

### termpix-ui

Widgets drawn with `vello_cpu` into RGBA layers, text by `parley`/`fontique`,
layout by hand (no layout engine). `canvas`: a canvas, colours and laid-out
text; `widgets`: a theme, a status bar and a virtualised list (outline,
search results). Each widget is its own layer so only changed widgets are
re-uploaded. IME: the terminal draws the preedit at its cursor, so a frame
carries a text cursor cell (`temzdf_scene::Frame::cursor`) where text is
typed; we only receive committed text.

### temzdf (pdfcore, pdfview, tty, bin)

- Threads: a `doc` thread owns the MuPDF `Document` (it is not `Send`) and builds
  `DisplayList`s (which are `Send + Sync`); a raster pool (`temzdf-raster`)
  rasterises them; `synctex` and `search` threads answer those queries.
  Elsewhere: the Markdown pane's tile pool, a pty's reader, writer and
  waiter (`temzdf-term`), the server's accept, per-client and IPC threads,
  and termpix's input, signal and output threads. The session loop runs
  on the main thread. Channels in between.
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
- Frame pacing (`crates/temzdf-tty/src/frontend.rs`, `TtyFrontend::ready`):
  the first frame waits (up to 1.5 s, `FIRST_FRAME_WAIT`) until the visible
  pages are fully rendered, frames wait while inline transfers are
  unacknowledged, and Composited frames are capped (`min_interval`) at
  ~30 fps, or 5 fps on a slow link (`slow_link`: inside zellij, which
  forwards every strip uncompressed, or inline transfers over SSH outside
  tmux). tmux and local inline transfers keep ~30 fps.
- Auto-reload: see `doc/design/reload.md`. SyncTeX: vendored `synctex_parser.c` via `cc`,
  forward search over a Unix socket, inverse search via a configurable command.
- Keys: modes and mappings live in the session
  (`crates/temzdf-server/src/keymap/`: Neovim notation, modes and
  transient modes with `keepalive`, the Vim-like sequence matcher, the mode
  history); see `doc/design/keymaps.md`. A mapping's right-hand side is a
  script function, which calls `action.*`. The shipped scripts
  (`crates/temzdf-script/runtime/`) map the keys, the prefix too
  (`lib/prefix.ts`); PDF actions reach the page-view `Command`s
  (`crates/temzdf-pdfview/src/keys.rs`), which `PdfPane::command` runs.
  See `doc/config.md` and `doc/design/script-host.md`.
- Images: panes register images in a `temzdf_scene::Images` store and return
  layers referencing them; `temzdf_tty::Presenter` mirrors the store into the
  compositor (pixels are shared, not copied) and maps the layers.

## Invariants

- `termpix-core`, `termpix` and `termpix-ui` never depend on PDF, SyncTeX or
  viewer code.
- `temzdf-pdfcore`, `temzdf-pdfview`, `temzdf-term` and `temzdf-server`
  never depend on `termpix` (tests excepted); only `temzdf-tty` and the
  binary talk to the terminal.
- Rendering is deterministic on the CPU so screens can be snapshot-tested.
- Every frame sent to the terminal is wrapped in synchronized output.
- The terminal is always restored, including on panic.
