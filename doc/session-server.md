# Session server design

Status (2026-10-26): steps 1 to 3 of the incremental plan below are
implemented (PDF and terminal panes; terminal panes show text only); see
"Implementation status". The research it
builds on is on the `research/notes` branch: `doc/multiplexer.md`,
`doc/gui-stack.md`, `doc/webview-viewer.md`, `doc/config-language.md`.

## Implementation status

What exists:

- Crates as in "Crate layout" below, with these differences: the PDF backend
  is `temzdf-pdfcore`; `termpix-core` holds the terminal-independent parts of
  termpix (geometry, raster helpers, kinetic scroll, input events) so the scene
  and panes need no terminal code; `temzdf-protocol` holds the messages,
  framing and socket locations.
- `Session` (`crates/temzdf-server/src/session.rs`): documents (one
  `DocumentHost` per file, `crates/temzdf-pdfview/src/host.rs`, shared by every
  pane on it: one tile cache, links, text, workers, reload), PDF panes in a
  terminal panes (`crates/temzdf-term`: a pty from `portable-pty`, Ghostty's
  terminal emulation from `libghostty-vt`, the screen as scene text cells),
  all in a binary layout of splits with one-cell borders (a split halves its
  area; when the screen changes size the last pane takes the difference), focus, modes and key mappings (`doc/script-host.md`; the prefix `<C-b>`),
  buffers as in Vim (what panes show; closing a pane hides its buffer,
  see `doc/config.md`), `Action`s with a text form, and one composed frame (pane layers moved into
  their rectangles, clipped, placement keys made unique and stable).
- Three ways to run share it: `temzdf FILE` (standalone: the session and the
  terminal frontend in one process, nothing registered), `--render-frame`
  (headless, waits for the workers after each key) and the session server
  (`crates/temzdf-server/src/daemon.rs`) with TTY clients
  (`crates/temzdf/src/client.rs`).
- Commands follow zellij: `temzdf -s NAME [FILE]` (a shell without FILE),
  `attach [-c] [NAME [FILE]]` (`a`), `list-sessions` (`ls`), `kill-session`
  (`k`), `kill-all-sessions` (`ka`), `action [-s NAME] new-pane [--cwd DIR]
  [FILE | -- COMMAND...] | split right|down | move-focus DIR |
  focus-next-pane | close-pane | detach` (`new-pane` alone opens a shell; the
  CLI adds `--cwd` with its own directory). Splitting a PDF pane shows the same
  document again; splitting a terminal starts another shell in the directory
  the shell reported (OSC 7). `<C-b><C-b>` sends `<C-b>` to the pane.
  Borders drag with the mouse, and `resize-pane` (`H` `J` `K` `L` in the
  `normal` mode) moves them from the keyboard; a split keeps its first
  half's size, so the last pane takes a change of the screen size. One server process per session; its
  output goes to `NAME.log` in the log directory (`$TEMZDF_LOG_DIR`, or
  `$XDG_STATE_HOME/temzdf`); `temzdf ls -v` shows it. Any command of the
  registry runs from the CLI too (`temzdf action NAME ARGS...`).
  Sessions whose server speaks another protocol version are listed as such.
- Terminal panes: palette colours stay palette indices (the outer terminal's
  theme applies), wide characters, SGR attributes, cursor shape, keys through
  libghostty's encoder (legacy or kitty keyboard protocol, as the program
  asks), mouse reports when the program tracks the mouse (otherwise the wheel
  scrolls back through 10000 lines of history), bracketed paste, focus
  reports, OSC 52 copies, resizing (to the largest pane showing the
  terminal), mouse selection copied on release, OSC 8 links opened on click,
  `[rows above/history]` while scrolled back, and kitty graphics from
  programs (placements become the pane's layers; PNG and every medium kitty
  takes). A shown terminal's panes close when its program exits.
  Programs get `TERM=xterm-256color`, `COLORTERM=truecolor`,
  `TERM_PROGRAM=temzdf` and `TEMZDF_SESSION_NAME`. The server makes at most
  one frame per 16 ms, so floods of output cost little (`seq 1 500000` takes
  0.14 s in a pane, `tests/e2e/terminal.actions`).
- Clients: the latest to attach or type sets the session's size and frontend
  hints; frames are paced per client by acknowledgements, image changes are
  queued per client (latest wins), late attachers get a snapshot; effects
  (copy, opening links, inverse search) run in the latest client.

Not yet (in order of the plan, or found on the way):

- Frames carry only image layers: UI chrome (status bar, outline, prompts) is
  still drawn as pixels by the pane, not as cell grids.
- Every client gets frames rendered for the latest client's geometry, cell
  size included; per-client pixel density, and zoom stored relative to the
  pane width, are not done (fit-width does follow the pane width).
- Pixels go through the socket; shared memory for local clients would save
  copies (measure first).
- One window per session (no tabs).
- Terminal panes: kitty graphics through Unicode placeholders (tmux inside a
  pane) and animation frames.
- Steps 4 to 6: GUI client, web panes, programmable config.

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
- `Viewer` used to mix document cache and view; it is now split into
  `DocCache` (display lists, tiles; `crates/temzdf-pdfcore/src/doc_cache.rs`)
  and the view (layout, scroll, zoom, peek window;
  `crates/temzdf-pdfview/src/viewer.rs`). Page links and text are still kept
  per pane and move into the document part when panes share documents.

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
| Config | TOML for now; later a language running in the server (Lua via `mlua` was the survey's pick) | Key bindings and hooks act on server state |

The biggest consequence: with the web engine and the PDF renderer both
offscreen in the server, every pane kind reduces to cell grids plus image
layers, so both clients stay small and identical in behaviour, and the GUI
needs no platform webview at all.

## Crate layout

Two ways to run must share one code path:

- **Session mode**: `temzdf` starts or attaches to the server; panes live in
  the server, clients attach and detach.
- **Standalone mode**: `temzdf standalone-pdf-viewer FILE` (name to be
  decided) behaves like today's temzdf: one PDF in the current terminal, using
  the same PDF backend and pane logic, but not registered in the session list
  and gone when it exits.

Standalone mode is not a second implementation: it links the same pane logic
and TTY renderer in one process and connects them with an in-process channel
instead of the socket protocol. Both modes therefore exercise the same code,
and standalone stays the easy way to test it.

| Crate | Contents | Depends on |
|---|---|---|
| `termpix-core` | geometry, raster helpers, kinetic scroll, key/mouse events (no terminal code) | - |
| `termpix` | terminal I/O, probing, input parsing, kitty transport, compositor, mock terminal | `termpix-core` |
| `termpix-ui` | pixel widgets (cell widgets may follow) | - |
| `temzdf-scene` | what a pane shows: image store and its updates, layers, frames (cursor, pointer, title), frontend hints, effects; serde behind a feature | `termpix-core` |
| `temzdf-protocol` | client/server messages, framing, socket locations | `temzdf-scene` |
| `temzdf-pdfcore` | the PDF backend as a document service: MuPDF wrapper, display lists, prioritised raster pool and per-document tile cache, text, links, outline, search, SyncTeX, file watching and reload; message-passing API so it can later run in a child process | (no termpix) |
| `temzdf-pdfview` | a PDF pane: per-view state (layout, scroll, zoom, peek window, selection, history, link preview, search UI, key bindings) that consumes input events and produces a scene; `Document` and `DocumentHost` shared by the panes on a file | `temzdf-pdfcore`, `temzdf-scene` |
| `temzdf-tty` | TTY frontend: renders a scene with `termpix` (frame pacing, effects); used by the TTY client and by standalone mode | `temzdf-scene`, `termpix` |
| `temzdf-server` | `Session` (layout tree, panes, focus, prefix keys, actions, composition), the server process (clients, pacing), session discovery, forward-search sockets | `temzdf-protocol`, pane crates |
| `temzdf-term` | terminal pane: `portable-pty` + `libghostty-vt` | `temzdf-scene`, `termpix-core` |
| `temzdf-web` (later) | web pane: headless Chromium over CDP | `temzdf-scene` |
| `temzdf-gui` (later) | GUI client: winit + wgpu renderer of the scene | `temzdf-scene` |
| `temzdf` (binary) | standalone viewer, session subcommands and client, `--forward` for SyncTeX, `--render-frame` | all of the above |

Rules, extending today's dependency direction (`temzdf -> termpix-ui ->
termpix`): `termpix` and `termpix-ui` stay ignorant of PDF and sessions;
backends (`temzdf-pdfcore`, later `temzdf-term`, `temzdf-web`) know nothing about
terminals or clients; only frontends (`temzdf-tty`, `temzdf-gui`) know how to
draw; panes speak only `temzdf-scene`. SyncTeX forward search keeps working in
both modes: standalone listens on its per-PDF socket as today; in session mode
the server receives the request and routes it to the panes showing that PDF.

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
  (`doc/config-language.md`) is decided later.
- **Terminal panes**: `libghostty-vt` for VT emulation (temzdf itself behaves
  best in Ghostty among the tested terminals: layered kitty placements work
  there, while WezTerm needed whole-frame compositing); the pty itself comes
  from `portable-pty` (or `rustix`), since libghostty-vt does not spawn
  processes.

## Open questions

- The capability set the server advertises to programs in terminal panes, and
  how kitty graphics from panes reach clients (re-emission per client; the
  biggest piece of new work, see `doc/multiplexer.md`).
- Naming and crate layout once temzdf is more than a PDF viewer.

## Incremental plan

1. (done) **Core split**: create `temzdf-scene`, `temzdf-pdfcore`, `temzdf-pdfview` and
   `temzdf-tty` from today's code (splitting `Viewer` into the document part
   and the view part on the way); the binary's current behaviour becomes
   `standalone-pdf-viewer` (and stays the default until session mode exists).
   Nothing user-visible changes; existing tests and e2e keep passing.
2. (done) **Server/client for the PDF viewer alone**: a session server, a TTY client
   that attaches and detaches; several panes on one document. Validates the
   protocol, sharing and resync with existing features; testable with the
   mock terminal.
3. (done, text only) **Terminal panes**: `portable-pty` + `libghostty-vt`,
   text in cell grids. temzdf becomes a multiplexer. Images from programs in
   the panes (re-emitted per client) are still to come.
4. **GUI client**: wgpu renderer of the same scene, local only.
5. **Web panes**: headless Chromium process, frames as image layers.
6. **Programmable config** in the server (language to be decided).
