# Terminal multiplexer research

Question (2026-10-25): if we wrote our own terminal multiplexer (persistent
sessions with detach/attach, a server process, panes and tabs: a tmux/zellij
clone) in which temzdf runs comfortably, what could be reused? "Comfortably"
means the kitty graphics protocol (placements with pixel offsets, z, `a=q`,
shm/file/direct transfer, Unicode placeholders), SGR-pixel mouse (1016), the
kitty keyboard protocol, synchronized output (2026), OSC 52 and OSC 22 work in
a pane, unlike today's fallbacks inside tmux (placeholders) and zellij
(composited strips), see `doc/terminal-compat.md`.

Status: research only, no decision. Collected by a research agent from the
crates.io API, docs.rs, GitHub and the `libghostty-vt` 0.2.2 sources on that
date. **[V]** verified in source or docs, **[I]** inference.

## Session layer

- **zellij crates** (`zellij-server`, `zellij-client`, `zellij-utils`,
  `zellij-tile` 0.45.1, MIT) are published, but only zellij itself depends on
  them [V]. They are entry points into the application (`start_server`,
  `start_client`, `ServerInstruction`), not a toolkit: `zellij-server` pulls in
  the wasm plugin runtime (`wasmi`), `kdl`, `prost`, `interprocess`,
  `daemonix`; `zellij-utils` pulls in `rusqlite`, `openssl-sys`/`curl-sys`;
  `zellij-client` the web client (`axum`, `tokio-tungstenite`) [V]. IPC is
  zellij's own protobuf/serde messages; resurrection serialises KDL layouts.
  No stability promise. Not reusable for session management alone [I].
- **`portable-pty`** 0.9.0 (WezTerm, MIT): the de facto pty crate (679
  reverse dependencies) [V]. `pty-process` 0.5.4 is a smaller Unix-only option;
  `rustix`/`nix` pty functions suffice for Unix only [I].
- **shpool** (Apache-2.0): persistent sessions without panes; `libshpool`
  (app as a library), `shpool-protocol` (client/daemon protocol),
  `shpool_pty`, `shpool_vt100` (screen restore) [V]. The most reusable
  detach/attach daemon in Rust, mainly as a reference [I].
- **zmx** (Zig, MIT): one daemon and socket per session, multiple clients;
  tees pty output to clients and to `libghostty-vt`, and sends a VT snapshot on
  re-attach [V]. Closest to the wanted architecture, but Zig only.
- **WezTerm `mux`**: `publish = false`, `wezterm-mux-server` not on crates.io,
  coupled to the WezTerm workspace [V].
- **abduco/dtach**: C tools, not libraries; their protocols are a few hundred
  lines to reimplement [I].
- **tmux control mode** as a backend: tmux still parses pane output (answers
  kitty queries itself, forces `z=0`) and cannot pass pixel mouse or the kitty
  keyboard protocol [V, `doc/terminal-compat.md`]. Not viable.

Recommendation: write the session layer ourselves in zmx/abduco style: a
server per user (or per session) on a Unix socket, `portable-pty`, and a small
protocol for attach/detach, resize, input and output frames, with
`shpool-protocol` and zmx as references.

## Per-pane VT emulation

| Crate | Version (date) | Licence | Kitty graphics | Notes |
|---|---|---|---|---|
| `vte` | 0.15.0 (2025-02) | Apache/MIT | parser only (APC hooks) | no screen state |
| `alacritty_terminal` | 0.26.0 (2026-04) | Apache-2.0 | no | the `ayosec/alacritty` fork adds Sixel and iTerm2 only [V] |
| `vt100` | 0.18.1 (2026-10) | MIT | no | simple grid (`tui-term` renders it) |
| `avt` (asciinema) | 0.18.0 | Apache | no | |
| `termwiz` | 0.23.3 (2025-03) | MIT | parses the kitty image APC | no terminal state machine |
| `wezterm-term` | not on crates.io (git only) | MIT | yes (store, place, query, partial deletes); sixel; iTerm2 | no Unicode placeholders (wezterm #986, PR #7924) [V] |
| zellij `Grid` | 0.45.1 | MIT | yes, with gaps: no `U=1` (#5530), no `t=s` (#5626), `x/y` without `c/r` (#5567), uncompressed forwarding (#5596) | internal API |
| **`libghostty-vt`** | 0.2.2 (2026-09-28) | MIT/Apache | **yes** | see below |

### libghostty-vt

- Ghostty's C VT library (`include/ghostty/vt.h`: "incomplete,
  work-in-progress API … not yet stable") for Linux, macOS and Windows [V].
  Rust bindings `libghostty-vt`/`libghostty-vt-sys` (repo uzaaft/libghostty-rs):
  the sys crate builds Ghostty at a pinned commit **with Zig (>= 0.17 for
  current main)**; `GHOSTTY_SOURCE_DIR`, `GHOSTTY_ZIG_SYSTEM_DIR` and
  pkg-config support sandboxed (Nix) builds [V]. Handles are `!Send`/`!Sync`:
  one thread or actor per pane.
- Rust 0.2.2 exposes [V]: `vt_write`, `resize`, scrollback and viewport
  scrolling, modes (including DEC 2026), kitty keyboard flags, mouse tracking
  state; callbacks for query replies (`on_pty_write`), XTVERSION, DA, size,
  OSC 52, title, cwd, bell; kitty graphics with per-medium switches (file, temp
  file, **shared memory**), a storage limit, a placement iterator (image and
  placement ids, `is_virtual` for Unicode placeholders, pixel offsets, source
  rect, columns/rows, z, layer, viewport position that can be negative when
  scrolled partly off) and image data with generation counters; PNG decoding
  through a callback; input encoders for mouse (including **SGR-Pixels**) and
  the kitty keyboard protocol per pane mode.
- Newer upstream C headers not yet in the Rust crate [V]: a kitty graphics
  render-state list with positions, sizes, crops, layer and z resolved
  (placeholders included), and `snapshot.h` (2026-10-21) serialising terminal
  state including image storage, placements and pixel data: usable for
  detach/re-attach and resurrection.
- Rust projects on it are appearing but young: phux, hyprterm, and others [V].

## What a multiplexer must do for kitty graphics

1. Per-pane image store: decode `t=d/f/t/s` and `o=z`, quotas, ids and
   numbers per pane (libghostty-vt, wezterm-term).
2. Answer the app locally: `a=q`, OK/error replies, `I=` mapping, plus 14t/16t
   cell size (libghostty-vt via `on_pty_write` [I]).
3. Placement model: cursor-anchored cell + pixel offset, `c,r` scaling, crops,
   z and the three layers, relative placements (`P=`), virtual placements via
   U+10EEEE (libghostty-vt covers all; wezterm-term lacks placeholders; zellij
   lacks `U=1`).
4. Scrolling: placements move with text and clip at the pane top.
5. Re-emission to the outer terminal, which no library does: upload each image
   once per host (shm or file locally, direct over SSH), namespace ids per
   pane, emit `a=p` clipped to the pane, map z, delete on close or scroll-out,
   rewrite placeholders, and fan out to clients with different cell sizes and
   capabilities. This is close to what `termpix`'s layered compositor already
   does [I].
6. Input: parse host SGR-pixel and kitty keyboard events (termpix does),
   hit-test panes, convert to pane-local pixels, re-encode per pane mode
   (libghostty-vt encoders).
7. Synchronized output: buffer a pane's 2026 frame and flush it inside a host
   2026 bracket.

## Comparison

| Layer | Option | Kitty graphics | Placeholders | Pixel mouse / kitty keyboard | Snapshot / restore | Embeddability |
|---|---|---|---|---|---|---|
| Session | zellij crates | via its Grid | no | partial | KDL layouts | poor (application internals, wasm plugins) |
| Session | libshpool / shpool-protocol | n/a | n/a | n/a | vt100 text | medium |
| Session | WezTerm `mux` | via wezterm-term | no | yes | no | poor (unpublished) |
| Session | zmx | via libghostty | via libghostty | n/a | libghostty snapshot | Zig only |
| PTY | portable-pty / pty-process | n/a | n/a | n/a | n/a | excellent |
| VT | libghostty-vt | full, shm/file | yes | yes | yes (C API) | good; needs Zig; unstable API |
| VT | wezterm-term | yes (partial deletes) | no | yes | no | git dependency only |
| VT | alacritty_terminal / vt100 | no | no | no | no | excellent, but no graphics |

## Recommended stack

- Session: our own server and protocol on `portable-pty` (zmx/abduco style).
- VT: `libghostty-vt`, one terminal per pane on its own thread; pin the Ghostty
  commit and provide Zig in the Nix dev shell. `wezterm-term` is the fallback
  to avoid Zig, at the cost of placeholders.
- Client rendering: `termpix` (probing, layered/composited kitty transport,
  input parsing, mock terminal); pane text as cells, pane images re-emitted as
  host placements clipped per pane, strips only when the host cannot layer.
  temzdf's own `termpix` path keeps working inside it.

Biggest gaps to build: the kitty graphics re-emitter (item 5 above; zellij is
the only working reference); the session protocol, daemon lifecycle and
attach/detach state handoff (wrapping `snapshot.h` until the Rust crate has
it); tracking a pre-1.0 libghostty API and a Zig toolchain in Nix and CI; pane
layout, focus, prefix keys, and a damage-tracked cell renderer; carrying
shm/file transfers across app -> server -> host (re-upload over the host's best
medium).
