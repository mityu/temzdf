# Session server design (proposal)

Status (2026-10-25): proposal, nothing implemented. Builds on the research in
this branch: `doc/multiplexer.md`, `doc/gui-stack.md`, `doc/webview-viewer.md`,
`doc/config-language.md`.

## Vision

temzdf becomes a session server in the spirit of tmux: a persistent server
that multiplexes **terminal panes, PDF panes and webview panes**, which
clients attach to and detach from. The point is tmux's "resume the same work
from somewhere else", extended to screens full of images.

- All state lives in the server and is shared by every client: layout, panes,
  per-pane view state (PDF page, zoom, scroll, search; terminal grid and
  scrollback; web page), focus. Clients only display and forward input. This
  is tmux's model, not Emacs's per-client frames.
- Two kinds of clients:
  - **TTY client**: draws into the terminal it runs in, with text escape
    sequences and the kitty graphics protocol (today's `termpix`). Resuming
    over SSH means running this client on the server host through SSH, as with
    tmux.
  - **GUI client**: a local-only native window (macOS, X11, Wayland including
    wlroots compositors and xfwl4), Unix socket plus shared memory.
- Typical use: work in the GUI at the desk, detach, re-attach from a laptop
  over SSH in a terminal, and continue exactly where the session was. State is
  shared; rendering fidelity adapts to each client.

## Lessons from tmux and zellij

From their sources (tmux master acee541, zellij main 0.46.0-dev, read
2026-10-25):

- **One authoritative model, rendered per client.** tmux keeps one emulated
  screen per pane and emits different escape sequences per client terminal
  (`tty_write` fans out, each `struct tty` has its own capabilities). zellij
  sends ANSI per client to terminals and, on main, a structured binary scene
  (`structured_render.rs`: cell rows, pane geometry, graphics records) to its
  new native window client.
- **Per-client resident images.** zellij keeps one kitty image store and per
  client the set of images already on that terminal (`kitty_host_state`), with
  per-client zlib and medium. tmux master parses sixel and kitty images into a
  shared store and picks an output backend per tty (kitty output still
  stubbed, sixel, or shaded blocks as fallback).
- **Pitfall: one global cell pixel size.** zellij merges the cell size from
  whichever client reported last, so kitty placements are wrong for clients
  with other cell sizes. Keep it per client.
- **Size policy.** tmux `window-size` latest (default) / largest / smallest /
  manual, with a panned viewport when the window is bigger than a client
  (`pan_ox/oy`); zellij uses the smallest client viewing each tab, and gives
  each client its own active tab and focused pane.
- **Resync from state, never from a log.** tmux marks a client all-dirty on
  attach or resize and redraws from the model.
- **Flow control.** tmux drops output to a slow client and fully redraws once
  it drains (`TTY_BLOCK`); zellij's native client keeps one frame in flight
  and coalesces changes until the ack; zellij's bounded queue disconnects slow
  clients, a visible failure to avoid. For pixels over SSH: latest frame wins,
  with acks.
- **Terminal queries from panes** (DA, XTVERSION, kitty `a=q`) cannot be
  answered by one client's terminal when clients differ: tmux answers itself,
  zellij forwards to one host client. We answer from a fixed synthetic
  capability set (what our renderer supports), independent of clients.
- **Logical socket protocol, not fd passing.** tmux passes the client's tty fds
  to the server (local only); a message protocol carrying input, size, pixel
  size, capabilities and frames works for both client kinds.
- Control mode (raw pane streams, client-side rendering) is a third model that
  cost tmux many fixes; not needed.

## Architecture

```
            +---------------------------- server ----------------------------+
            | session state: layout tree, panes, focus, per-pane view state  |
            |                                                                |
            | document services (one per open file, shared by all views):    |
            |   PDF: MuPDF document, display lists, text, links, tiles       |
            |   terminal: pty + VT emulator (libghostty-vt)                  |
            |   web: engine process (headless Chromium over CDP)             |
            |                                                                |
            | per client: size, cell pixel size, capabilities, resident      |
            | images, frame pacing                                           |
            +-----------------------------+----------------------------------+
                     scene frames + image data | input, size, acks
                  +------------------------+---+--------------------+
                  |                                                 |
           TTY client (termpix)                         GUI client (wgpu)
           kitty graphics / text                        local, shm
```

### Scene protocol

The server sends each client a **scene**, not rendered bytes or whole-screen
pixels:

- **Cell grids** for text-centric surfaces (terminal panes, status bar,
  outline, prompts): styled cell runs. The TTY client writes real text (crisp,
  copyable, cheap over SSH, works without graphics); the GUI client draws it
  with its fonts (glyph atlas). This also moves today's pixel-drawn UI chrome
  to text, an improvement for the terminal case.
- **Image layers** for pixel-centric surfaces (PDF tiles, web frames, images
  from programs in terminal panes): image id, source rectangle, destination in
  the client's pixels, z. This is today's `termpix::compositor::Layer`, lifted
  into the protocol. Image data is sent once per client and referenced by id
  (shared memory for local clients, inline compressed otherwise). The TTY
  client maps layers to kitty placements (or composited strips, or a text
  fallback when the terminal has no graphics); the GUI client to textured
  quads.
- Pacing: one frame in flight per client, coalescing until the ack; on attach,
  resize or overload the client gets a full resync from state.

Where termpix runs: in the TTY client. It already probes the terminal, picks
backend and medium, and controls congestion; keeping it next to the terminal
means the server never needs to know escape sequences. Over SSH the TTY client
runs on the server host, so server-to-client transfer stays local (shared
memory) and only the client-to-terminal hop crosses the network, exactly the
hop termpix already optimises.

### Sizes and pixel density

- Layout in cells per window, tmux-style `latest` by default (the user
  resuming is the latest client); other attached clients see a panned viewport
  or padding.
- Cell pixel size, scale and capabilities are per client. Pixel surfaces are
  rendered per client at its own resolution: a PDF pane draws tiles at the
  client's pixel density (the tile cache is already keyed by scale), so the
  HiDPI GUI and an SSH terminal each get sharp pages.
- View state is stored resolution-independently: zoom as fit-width or
  relative to the pane width, scroll as a document position (page + offset,
  as reload anchoring already does).
- The web engine has one viewport per pane; frames are rendered at the
  session's logical size and a device scale factor, and scaled for clients
  whose density differs (or re-rendered at the most demanding client's
  density).

### Shared documents across panes

Showing different pages of the same PDF in several panes is a common use, so
documents are shared, and only view state is per pane:

- **One document service per file**, referenced by every pane showing it. It
  owns everything that does not depend on the view: the parsed MuPDF document
  (on its thread), per-page display lists, page text, links, outline, search
  results, the SyncTeX scanner and the file watcher with reload. A reload
  updates all panes at once, each keeping its own position.
- **One tile cache per document**, keyed by (generation, page, scale, tile):
  two panes on the same page at the same zoom share tiles, and a client that
  already holds an image does not receive it again; two placements reference
  the same id.
- **Rendering**: raster threads share the display lists (`Arc<DisplayList>` is
  `Send + Sync`; today's raster pool already works this way). Requests from all
  panes and clients go into one prioritised queue per document, so visible
  tiles of any pane come first. MuPDF's allocation lock limits useful threads
  to about four (`doc/performance.md`).
- **If rendering moves to separate processes** (crash isolation, or more
  parallelism than MuPDF's lock allows): MuPDF's parsed objects cannot be
  shared between processes, but the expensive parts can: the file bytes are
  shared by the OS page cache (mmap), and rendered tiles go to a shared-memory
  cache that every process and client reads. Each process re-parses the
  document (typically a few MB for LaTeX output). The recommended default is
  one document process per file with internal raster threads, which avoids
  even that duplication; extra processes for the same file only when needed.
- Today's `Viewer` mixes document cache and view (`crates/temzdf/src/viewer.rs`);
  the split into a document part (lists, tiles, links, text) and a view part
  (layout, scroll, zoom, peek window) is the first refactoring step.

The same applies to other pane kinds: one terminal (pty + VT state) can be
shown in several panes or windows, like tmux linked windows; one web page per
pane by default, since web views are stateful per viewport.

## Libraries, revisited for this design

| Need | Choice | Why, in this design |
|---|---|---|
| Session IPC | own protocol on Unix sockets (`std`/`rustix`), serde + a compact binary codec; framing adapted from zellij's `ipc.rs` (MIT, with attribution) | Depending on zellij crates, even via git, brings its whole application (wasm plugins, KDL layouts, web client) and no stable API; the framing is ~50 lines |
| Session discovery | socket directory listing + liveness probe, adapted from zellij's `sessions.rs` | Simple logic, worth copying |
| Shared memory | `rustix::shm` + `memmap2` (already used by termpix) | Images to local clients |
| PTY | `portable-pty` | De facto standard |
| VT emulation | `libghostty-vt` | Full kitty graphics incl. placeholders and shm, SGR-pixel and kitty keyboard encoders, render state with dirty rows; the server holds the model, so its snapshot API matters only for surviving server restarts. Needs Zig in Nix; unstable API |
| PDF | MuPDF via `mupdf` (as today), one document service per file | Shared across panes; AGPL as before |
| Web | headless Chromium over CDP in its own process, screencast frames | Fits the server model: the engine is offscreen anyway, so web panes are pixel surfaces like PDF pages. This removes the Wayland webview embedding problem for the GUI (no native child views needed on wlroots, xfwl4 or macOS). CEF only if Chromium's encode/decode cost dominates; Servo later |
| TTY client | `termpix` (+ `termpix-ui` for pixel widgets where still needed) | Exists; becomes the scene renderer for terminals |
| GUI client | winit + wgpu + parley/swash (or glyphon), egui optional for chrome | A thin scene renderer (cell grids as glyph quads, layers as textures); GPUI or Qt bring their own application models, which a thin client does not need, and the webview argument for Qt disappears |
| Config | Lua via `mlua` in the server | Key bindings and hooks act on server state; generated LuaCATS stubs |

The biggest consequence: with the web engine and the PDF renderer both
offscreen in the server, every pane kind reduces to cell grids plus image
layers, so both clients stay small and identical in behaviour, and the GUI
needs no platform webview at all.

## Open questions

- Which client's size wins by default (`latest` proposed) and how viewports
  pan for the others.
- Whether fit-width zoom follows the logical (session) size or each client's
  pixel size.
- Crash isolation boundaries: MuPDF and the web engine in child processes from
  the start, or threads first.
- The capability set the server advertises to programs in terminal panes, and
  how kitty graphics from panes reach clients (re-emission per client; the
  biggest piece of new work, see `doc/multiplexer.md`).
- Resurrection after a server restart (layout, cwd and commands; PDF view
  state is easy to save).
- Naming and crate layout once temzdf is more than a PDF viewer.

## Incremental plan

1. **Core split**: extract the PDF engine into a library crate; split
   `Viewer` into a shared document part and per-pane views; define the scene
   types (cell grids + image layers). The current viewer keeps working.
2. **Server/client for the PDF viewer alone**: `temzdf --server`, a TTY client
   that attaches and detaches; several panes on one document. Validates the
   protocol, sharing and resync with existing features; testable with the
   mock terminal.
3. **Terminal panes**: `portable-pty` + `libghostty-vt`, text in cell grids,
   pane images re-emitted per client. temzdf becomes a multiplexer.
4. **GUI client**: wgpu renderer of the same scene, local only.
5. **Web panes**: headless Chromium process, frames as image layers.
6. **Lua config** in the server.
