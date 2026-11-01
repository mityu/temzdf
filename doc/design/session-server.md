# Session server design

The design of the session server (temzdf as a tmux-like multiplexer of
terminal, PDF and later web panes) and what was decided on the way, with
the reasons. How to use what exists is in `doc/config.md`; the crates are
in `doc/architecture.md` and AGENTS.md; what is left to do is in `todo.md`.
The research it builds on: `doc/survey/multiplexer.md`,
`doc/survey/gui-stack.md`, `doc/survey/webview-viewer.md`,
`doc/survey/config-language.md`.

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
  (in shared memory, see "Pixels" under "Decided while implementing"; inline compressed would be for
  remote clients, which do not exist). The TTY
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
  to about four (`doc/dev/performance.md`).
- **If rendering moves to separate processes** (crash isolation, or more
  parallelism than MuPDF's lock allows): MuPDF's parsed objects cannot be
  shared between processes, but the expensive parts can: the file bytes are
  shared by the OS page cache (mmap), and rendered tiles go to a shared-memory
  cache that every process and client reads. Each process re-parses the
  document (typically a few MB for LaTeX output). The recommended default is
  one document process per file with internal raster threads, which avoids
  even that duplication; extra processes for the same file only when needed.
- `Viewer`, which mixed the document cache and the view, was split into
  `DocCache` (display lists, tiles; `crates/temzdf-pdfcore/src/doc_cache.rs`)
  and the view (layout, scroll, zoom, peek window;
  `crates/temzdf-pdfview/src/viewer.rs`); links and text moved into the
  document part, `DocumentHost` (`crates/temzdf-pdfview/src/host.rs`),
  shared by the panes on a file.

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
| Config | TOML at first, then a language (Lua via `mlua` was the survey's pick); became JavaScript/TypeScript in a script-host process (`doc/design/scripting.md`) | Key bindings and hooks act on server state |

The biggest consequence: with the web engine and the PDF renderer both
offscreen in the server, every pane kind reduces to cell grids plus image
layers, so both clients stay small and identical in behaviour, and the GUI
needs no platform webview at all.

## Crate layout

Two ways to run must share one code path:

- **Session mode**: `temzdf` starts or attaches to the server; panes live in
  the server, clients attach and detach.
- **Standalone mode**: `temzdf standalone pdf FILE` behaves like the
  viewer temzdf started as: one PDF in the current terminal, using the same
  PDF backend and pane logic, but not registered in the session list and
  gone when it exits.

Standalone mode is not a second implementation: it links the same pane logic
and TTY renderer in one process and connects them with an in-process channel
instead of the socket protocol. Both modes therefore exercise the same code,
and standalone stays the easy way to test it.

The crates are listed in AGENTS.md ("Layout"). The rules: `termpix` and
`termpix-ui` stay ignorant of PDF and sessions; backends (`temzdf-pdfcore`,
`temzdf-term`, later `temzdf-web`) know nothing about terminals or clients;
only frontends (`temzdf-tty`, `temzdf-gui`) know how to draw; panes speak
only `temzdf-scene`. SyncTeX forward search works in both modes: standalone
listens on its per-PDF socket; in session mode the server receives the
request and routes it to the panes showing that PDF.

## Decisions (2026-10-25, with the user)

- **Size**: the latest active client's size wins (tmux `latest`). Other
  attached clients may see the screen cut off or padded; that is acceptable.
- **Zoom**: stored relative to the pane width (fit-width = the page width
  equals the pane width; other zooms as a fraction of it), so every client
  shows the same part of the page and renders it at its own pixel density.
- **Isolation**: start with threads where that is simpler, but keep the
  document and web services behind message-passing interfaces so they can move
  into child processes later (the intended end state).
- **No resurrection**: a server restart loses the sessions.
- **Configuration**: stays TOML for now; the programmable language
  (`doc/survey/config-language.md`) is decided later. Update 2026-10-26: JavaScript /
  TypeScript in a separate process replaces TOML; design in
  `doc/design/scripting.md`.
- **Terminal panes**: `libghostty-vt` for VT emulation (temzdf itself behaves
  best in Ghostty among the tested terminals: layered kitty placements work
  there, while WezTerm needed whole-frame compositing); the pty itself comes
  from `portable-pty` (or `rustix`), since libghostty-vt does not spawn
  processes.

## Tab pages (decided 2026-10-27, with the user)

- As in Vim: each tab page has its layout and focus (the focus comes back
  when the tab page is entered); buffers are shared, and a terminal's size
  counts its panes in every tab page (Neovim's rule).
- The current tab page is the session's, the same for every client (tmux;
  zellij gives each client its own).
- A tab page's last tiled pane gives way to a shown float of that tab page;
  without one the tab page closes, and the last one ends the session.
- The tab line follows Vim's `showtabline` (default `"auto"`: with two tab
  pages or more), on the top row: number and focused buffer, or the name
  given with `rename-tab`. A click goes to a tab page, a middle click closes
  it.
- Floats carry Vim's popup `tabpage` (one tab page by default, or all); one
  stack for all, shown or hidden per tab page.
- Commands as Vim and tmux name them (`new-tab`, `close-tab`, `next-tab`,
  `prev-tab`, `goto-tab`, `move-tab` as `:tabmove`, `rename-tab`,
  `break-pane` as `<C-w>T`); default keys after the prefix as Vim's: `gt`,
  `gT`, `{N}gt`, `T`. No default keys for new-tab and close-tab (Vim has
  none: `:tabnew`, `:tabclose`).
- Implementation (`crates/temzdf-server/src/session/tabs.rs`): as Vim keeps
  the current tab page's windows in globals, the session's fields hold the
  current tab page's layout and focus, and the others keep theirs in their
  record, swapped on switching.

## Decided while implementing

Choices made while building steps 1 to 4 (with the user where noted in the
git log), and why:

- **Which client leads.** The latest client to attach or use the session
  (keys, clicks, scrolling, paste) sets its size and frontend hints and runs
  the effects (copy, opening links, inverse search). The pointer merely
  passing over a window does not count, since terminals report it even when
  unfocused (it is ignored from other clients), nor does resizing a window,
  which window managers do in the background. Programs are told they have
  the focus while any attached client's window has it.
- **Pacing.** Frames are paced per client by acknowledgements; image
  changes are queued per client (latest wins); a late attacher gets a
  snapshot. The server makes at most one frame per 16 ms, so floods of
  output cost little (`seq 1 500000` takes 0.14 s in a pane,
  `tests/e2e/terminal.actions`).
- **Pixels.** Clients that say so in their `Hello` (`TEMZDF_PIXELS`,
  default `shared`) take images of 64 KiB and more in shared memory: one
  anonymous object per image (a sealed memfd on Linux, an unlinked POSIX
  object on macOS), its descriptor passed with the frame (`SCM_RIGHTS`),
  mapped read-only and handed on without a copy (`termpix_core::raster::Pixels`
  to the GPU or the terminal's image store). Smaller images, more than 250
  in a frame, objects that cannot be made, and `TEMZDF_PIXELS=socket` go
  through the socket: from 4 KiB on right after the message, read into the
  image's own buffer, smaller ones inside it (`temzdf-protocol`'s
  `send_server`, `recv_server`, `shared.rs`; numbers in
  `doc/dev/performance.md`).
- **Nesting.** A client says which session's pane it runs in (`Hello`), and
  a session lists those of its clients (`SessionInfo::shown_in`); `attach`
  from a pane refuses that session and every session showing it, directly
  or through others, since the session would show itself inside itself.
- **Layout.** A split halves its area and keeps its first half's size, so
  when the screen changes size the last pane takes the difference (as in
  Vim; only that pane re-renders); panes given a size keep it. Borders are
  one cell wide and drawn as text (2026-11-02, so terminals without
  graphics show them too). What a float covers is cut out of the panes below
  it (`crates/temzdf-server/src/clip.rs`), since images cannot be stacked
  over text.
- **Commands** follow zellij's names (`attach`, `list-sessions`/`ls`,
  `kill-session`, `action`); unnamed sessions are named after the NATO
  alphabet (alfa, bravo, ...). One server process per session, its output
  in `NAME.log`. Splitting a PDF pane shows the same document again;
  splitting a terminal starts another shell in the directory the shell
  reported (OSC 7).
- **Terminal panes.** Palette colours stay palette indices, so the outer
  terminal's theme applies. Programs' kitty graphics become the pane's
  image layers (every medium kitty takes); queries are answered by
  libghostty-vt, not forwarded to a client's terminal. Programs get
  `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=temzdf` and
  `TEMZDF_SESSION_NAME`. On the alternate screen (less, man) the wheel
  sends arrow keys (alternate scroll, DECSET 1007).
- **Terminals without graphics** (2026-11-02): a client whose terminal
  cannot show images still attaches and draws the text; the frame carries
  where each pane is and whether it has images (`regions`) and text only
  such clients draw (`fallback_text`), and the client says what it cannot
  show itself, since other clients of the session can.

## The plan (2026-10-25)

The order the work was planned in; where each step stands is in `todo.md`.

1. **Core split**: create `temzdf-scene`, `temzdf-pdfcore`,
   `temzdf-pdfview` and `temzdf-tty` from the viewer's code (splitting
   `Viewer` into the document part and the view part on the way); nothing
   user-visible changes, existing tests and e2e keep passing.
2. **Server/client for the PDF viewer alone**: a session server, a TTY
   client that attaches and detaches; several panes on one document.
   Validates the protocol, sharing and resync with existing features;
   testable with the mock terminal.
3. **Terminal panes**: `portable-pty` + `libghostty-vt`, text in cell
   grids, programs' kitty graphics as the pane's layers. temzdf becomes a
   multiplexer.
4. **GUI client**: wgpu renderer of the same scene, local only.
5. **Web panes**: headless Chromium process, frames as image layers.
6. **Programmable config**: JavaScript/TypeScript in the script host
   (`doc/design/script-host.md`).
