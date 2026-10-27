# GUI client: groundwork and plan

Status (2026-10-28): implemented for X11 (the user's Xfce) and checked on
Xvfb with xfwm4; Wayland, macOS and HiDPI are not checked yet. See
"Implemented" below. Every **(decide)** item was decided as recommended
(the user, 2026-10-27; see "Decisions"). Step 4 of the session server's plan
(`doc/session-server.md`): a native window that attaches to a session as the
TTY client does. Toolkit research is in `doc/gui-stack.md`; this document
narrows it to the client the current code needs, lists what is in the way,
and plans what can be built now.

## Implemented (2026-10-28)

`temzdf-gui [-s NAME] [FILE]` (`crates/temzdf-gui`) attaches to session
NAME (started with FILE if it does not exist), a new session for FILE, the
only session, or a new shell; it starts servers with the `temzdf` next to
it (else on `PATH`). Closing the window detaches. `--render-frame PNG
[--size WxH] [--scale S]` draws the session offscreen instead.

- **Protocol and server** (G0): `MouseEventKind::ScrollBy` (amounts in
  hundredths of a pixel or a notch, with the source: wheel, fingers,
  inertia; PDF pages follow fingers exactly, terminals add rows up),
  `ClientMsg::Text` (committed input-method text: typed in pass-through
  modes, keys elsewhere), `ClientMsg::Focus`, the `paste` command with
  `Effect::Paste`, and `ServerMsg::Appearance` (options `foreground`,
  `background`, `palette`, `guifont`, `guifontwide`; terminal panes answer
  OSC 4/10/11 from them). Discovery and spawning moved to
  `crates/temzdf-client` (and `temzdf-protocol`'s `discovery`).
- **Drawing** (G1): `drawlist.rs` turns a frame into ordered draws (kitty's
  z bands, cell backgrounds, glyphs, decorations, cursor, preedit), pure and
  tested under Miri; `render.rs` executes it with wgpu (textured quads,
  glyphon in "web" colour mode on non-sRGB targets, so blending matches the
  mock terminal) into the window or offscreen. Cells come from the font:
  the advance of `0`, ascent + descent + line gap, rounded up.
- **Input** (G2): keys as the kitty protocol reports them (`input.rs`,
  `translate`, tested), pointer events in pixels, wheel and trackpad as
  `ScrollBy`, focus, IME (preedit drawn at the cursor, the candidate window
  placed there with `request_ime_update`, commits as `Text`), the clipboard
  through arboard (primary selection on Linux), links and inverse search
  through `temzdf-client`. `<C-S-v>` and `<D-v>` paste in terminal panes.
- **Packaging**: `nix build` builds both binaries and wraps `temzdf-gui`
  with the libraries winit and wgpu load (`guiLibsFor` in `flake.nix`).

Differences from the plan below:

- No input area in `Frame`: PDF prompts already put `Frame.cursor` at their
  caret, so the GUI places the input method at the cursor cell.
- winit's X11 backend loads libXfixes, libXrender, libXext, libXinerama,
  libxcb and libXss besides the libraries listed first; the dev shell and
  the package carry them.
- With fcitx5 over XIM, Mozc draws the composing text in its own window at
  the cursor (winit's X11 input style); the GUI's own preedit drawing shows
  where the input method sends preedit (Wayland's text-input, macOS).
- Default colours (no `foreground` / `background` set): foreground
  `#d8d8d8`, background `#1e1e1e`, xterm's palette.

Tests: `crates/temzdf-gui/tests/gpu.rs` (offscreen on lavapipe: layers
against the CPU reference, text in its cells), unit tests of the draw list,
palette, fonts and key translation, `scripts/e2e.sh -t gui` with
`tests/e2e/gui.actions` (judged against `--render-frame`) and
`tests/e2e/gui-ime.actions` (`-I`: fcitx5 with Mozc, committed Japanese and
a paste reach a shell).

Not done yet: Wayland (sway headless is in the dev shell) and macOS checks,
HiDPI and fractional scales (`--scale` and `ScaleFactorChanged` exist,
untested), Option as Alt on macOS, cursor blinking, underline colours
(SGR 58: the scene's cells do not carry one, so underlines take the text
colour, in the terminal frontend too), shared memory for images. Curly,
dotted and dashed underlines are drawn as fills in `drawlist.rs`, their
pattern following the window's x so neighbouring cells join up.

## What the GUI client is

A second frontend of the session protocol (`crates/temzdf-protocol`), next
to the TTY client. The server stays as it is: it composes one scene per
frame (`temzdf_scene::Frame`: image layers, text grids, cursor, pointer,
title) and sends image changes, frames and effects; the client draws and
sends input back. Local only (Unix socket now, shared memory later).

What the TTY client does today and what the GUI does instead:

| Concern | TTY client (`temzdf-tty`, `crates/temzdf/src/client.rs`) | GUI client |
|---|---|---|
| Surface | the terminal; probed capabilities; cell size from the terminal | a window; cell size from the font and the scale factor |
| `Hello { geometry, hints }` | cells and cell pixels of the terminal; hints from backend and medium | cells and cell pixels of the window's grid; `exact_scaling` and `fast_uploads` both true |
| Image store | mirrored into termpix's compositor | mirrored into GPU textures |
| Layers | kitty placements or composited strips | textured quads: z order, clipped to their rectangle, scaled |
| Text grids | escape sequences and characters (only what changed) | glyphs from a font atlas, cell backgrounds, decorations |
| Cursor, title, pointer | escape sequences | drawn cursor (shape, blink), window title, cursor icon |
| Effects | OSC 52 copy, `xdg-open`/`open`, `sh -c` | the native clipboard, the same commands |
| Input | parsed escape sequences into `KeyEvent`/`MouseEvent` | window events translated into the same types |
| Pacing | `FrameAck` after the terminal took the bytes | `FrameAck` after presenting |

The GUI reuses: the protocol, session discovery and server spawning,
`temzdf-scene` and `termpix-core`'s event types. It needs no `termpix`.
Discovery and spawning (`name_for`, `first_pane`, `spawn_server`,
`resolve` in `crates/temzdf/src/client.rs`) live in the binary crate today,
so they move to a library first (the attach loop itself is TTY-specific).

Since this plan was written the session gained tab pages: the current tab
is the session's, the same for every client, and the session composes the
tab line (a text grid on the top row, clicks handled by the session). The
GUI draws it like any text and needs no tabs of its own.

## Gaps in the current protocol and server

Found by reading the code against what a window delivers; each needs a
change on the server side, testable without a GUI.

1. **Precise scrolling.** `MouseEventKind::Scroll(direction)` carries no
   amount: a wheel step is one event and PDF panes animate it
   (`KineticScroll`). Trackpads deliver pixel deltas with phases, and macOS
   adds its own momentum. Needed: a scroll event with `dx`, `dy`, a unit
   (pixels or lines) and a phase; PDF panes then follow pixel deltas
   directly (no easing of their own when the OS supplies momentum), terminal
   panes turn them into rows (accumulating fractions). This is what makes
   "GUI-grade" scrolling real; the TTY path keeps the discrete steps.
2. **Text input (IME).** Committed text (Japanese input, dead keys, the
   macOS character palette) is not a key; sending it as `Paste` would
   bracket it in terminals. Needed: `ClientMsg::Text(String)`, typed into
   the focused pane (terminals: written as typed; a PDF prompt: inserted).
   **(decide)** whether text goes through the key mappings: recommended no
   in pass-through modes (`terminal`), and in other modes each character as
   a key (as Vim treats IME input in Normal mode), so `normal`-mode
   mappings stay reachable with an IME on.
3. **Where the text cursor is.** The IME needs the caret position to place
   its candidate window, and the client draws the preedit there. For
   terminal panes `Frame.cursor` (cells) suffices. Prompts drawn as pixels
   by PDF panes (search) have no cell cursor. Needed: an optional input area
   (pixel rectangle) in `Frame`, set by whatever has the text focus.
4. **Colours.** `Color::Default` and `Color::Indexed` are left to the outer
   terminal's theme today. A window must resolve them itself: a palette
   (16 + 216 + 24 grey, or all 256), default foreground and background,
   cursor and selection colours. **(decide)** where the theme lives:
   recommended as session options (`o.palette`, `o.foreground`,
   `o.background`) sent to clients with frames (the TTY client ignores
   them), so programs in terminal panes can be answered consistently when
   they query colours (OSC 4/10/11, which libghostty can answer once it
   is given the palette).
5. **Fonts.** The client decides the cell size, so fonts are client-side.
   **(decide)** where they are configured: recommended Neovim's model, a
   session option (`o.guifont = "JetBrains Mono:h13"`, plus
   `o.guifontwide` for CJK) read by GUI clients, so one configuration
   serves every GUI client; the alternative is a GUI-only file
   (`client.ts`, see decision 22 in `doc/scripting.md`).
6. **Pasting.** In a terminal the outer terminal pastes (Ctrl-Shift-V,
   Cmd-V) and the client sees bracketed paste. A GUI must pick the key and
   read the clipboard itself. The server cannot read a client's clipboard.
   Recommended: the key is a mapping like any other (`map("terminal",
   "<D-v>", action.paste())`, `<C-S-v>` on Linux), and `paste` is an effect
   the client answers with `ClientMsg::Paste` (the primary selection too,
   on X11 and Wayland, for middle click).
7. **Focus.** The TTY client parses focus reports (`termpix::input::Event::
   Focus`) but does not forward them; terminal panes report focus only on
   pane changes. Needed: `ClientMsg::Focus(bool)` for both clients (programs
   like Vim refresh on focus-in).
8. **Images over the socket.** Every image travels inside postcard frames.
   A HiDPI window full of tiles is about 30 MB per full redraw; a local
   socket moves that in a few tens of milliseconds, acceptable at first.
   Shared memory (`Hints.fast_uploads` already exists) is the later
   optimisation, measured first (`doc/performance.md` has the method).
9. **One geometry for all clients.** Frames are rendered for the latest
   client's geometry, cell size included (decided: tmux's `latest`). A GUI
   at 2x and a terminal over SSH alternate; each switch re-renders tiles at
   the new density. Per-client rendering stays out of the first version.
10. **Pixel chrome.** A PDF pane's status bar, outline, prompts and link
    previews are pixels drawn by `termpix-ui` at the cell size the client
    reports, so they are sharp at any scale but use termpix-ui's font, not
    the GUI's. The session's own chrome is already text grids (float
    titles, floating messages, the tab line), so the GUI needs its text
    renderer from the start. Moving the PDF chrome to text grids (planned)
    fixes the rest for both clients; not a blocker.

## Facts checked for this plan (2026-10-26, rechecked 2026-10-27)

Rechecked on crates.io on 2026-10-27: winit 0.31.0-beta.4 is still the
newest (0.30.13 the stable one), wgpu is 30.0.1, glyphon 0.12.0 depends on
wgpu ^30 and cosmic-text ^0.19, arboard is 3.7.0.


Collected by a research agent; **[V]** verified from the source named, **[I]**
inferred.

- **winit**: stable 0.30.13 (2026-03); 0.31 is not released, betas up to
  0.31.0-beta.4 (2026-10-24), split into backend crates (`winit-wayland`)
  [V, crates.io API, the v0.31.0-beta.4 `Cargo.toml`]. In 0.31:
  - `Ime` has `Preedit(text, cursor in UTF-8 bytes)`, `Commit`,
    `DeleteSurrounding`. `request_ime_update` applies IME changes at once.
    Betas 3 and 4 fix macOS IME locking, preedit with emoji, and Wayland
    text input dropped on seat changes. Since beta.4 no empty `Preedit`
    comes before `Commit` [V, winit releases, docs.rs].
  - `MouseWheel` carries a `ScrollSource` that tells finger scrolling from
    system momentum (macOS, Wayland) since beta.4; `PixelDelta` and
    `LineDelta` as before [V]. That is the phase information gap 1 needs.
  - `ModifiersKey` tells which modifier key is pressed (left/right) [V].
  - sctk-adwaita client-side decorations stay a default feature [V];
    fractional-scale-v1 and cursor-shape-v1 are presumably still supported
    as in 0.30 [I].
- **Prior art**:
  - Neovide re-enabled IME preedit in 2026-02 by sending preedit and
    commit to Neovim (a Lua API plus a plugin), so the server draws it in
    its grid [V, neovide PR #3221].
  - WezTerm draws the preedit itself by default
    (`ime_preedit_rendering = "Builtin"`), or lets the OS do it [V].
  - Alacritty draws the preedit over the cells, accounting for wide
    characters [V]. Candidate windows go astray when the IME cursor area is
    not the real caret (a report with zellij inside Alacritty) [V]. So the
    input area (gap 3) must always be the focused caret's rectangle.
- **Clipboard**: `arboard` 3.7.0 (2026-10-15); Wayland through the
  `wayland-data-control` feature (`wl-clipboard-rs`), X11 otherwise [V].
  The primary selection is available through `GetExtLinux`/`SetExtLinux` [I].
- **softbuffer** 0.4.8 (2025-12) [V]: CPU presentation on Wayland, X11 and
  macOS in physical pixels [I]. It remains a fallback option.
- **Headless wgpu**:
  - wgpu's own CI runs on Mesa's lavapipe, which shows up as a CPU adapter
    [V, wgpu's `ci.yml` and `docs/testing.md`].
  - On NixOS: `mesa` and `vulkan-loader`, with `VK_DRIVER_FILES` pointing
    at lavapipe's ICD file [I].
  - Output is stable for a pinned Mesa (the flake pins it) and differs from
    real GPUs in anti-aliasing [I].

## Decisions (the user, 2026-10-27: every recommendation)

- Text input (gap 2) goes through the key mappings as keys, one per
  character, except in pass-through modes (`terminal`), where it is typed.
- The theme (gap 4) is session options (`o.palette`, `o.foreground`,
  `o.background`) sent to clients with frames; libghostty answers colour
  queries from them.
- Fonts (gap 5) are session options as Neovim's `guifont` (`o.guifont`,
  `o.guifontwide`), read by GUI clients.
- Pasting (gap 6) is a mapping to a `paste` command; the client answers the
  `paste` effect with `ClientMsg::Paste` (the primary selection too).
- winit 0.31 beta, pinned exactly, updated as betas come.
- A separate crate and binary, `temzdf-gui`, spawning sessions with
  `temzdf` (next to itself, else on `PATH`).
- wgpu from the start, the scene first turned into a plain draw list.
- No standalone mode: the GUI always attaches to a session (starting one as
  `attach -c` does).
- One window per session; the session's tab pages show through its tab
  line. Several windows per session or a session switcher: later.
- macOS: Option as Alt by default, a setting for composing characters
  (kitty's `macos_option_as_alt`); Cmd is `<D-...>`.
- Closing the window detaches; a configurable action ends the session.
- Decorations: server-side where the compositor offers them, client-side
  otherwise; no custom title bar at first.

Also decided (the user, 2026-10-27):

- First target: the user's Xfce (X11, xfwm4). Wayland and macOS follow;
  e2e tests run xfwm4 on Xvfb first.
- Default font "UDEV Gothic NF" at 14 (points); no colours given, so the
  theme defaults are ours (the default foreground and background of the
  session's chrome, xterm's 256-colour palette).
- The dev shell already has the tools (`flake.nix`: Vulkan loader,
  lavapipe as `TEMZDF_LAVAPIPE_ICD`, the font as `TEMZDF_TEST_FONTS`,
  xfwm4, sway, grim, wtype, fcitx5 with Mozc).

The items below were the questions and their recommendations.

## Things to decide (decided as recommended, see above)

- **winit version.** **(decide)** 0.30.13 (stable) or 0.31 beta (pinned
  exactly). 0.31 has the IME fixes and the scroll source this client wants,
  and the GUI will not ship before it settles. Recommended: 0.31 beta,
  pinned, updated as betas come.

Besides the (decide) items above:

- **Binary.** **(decide)** `temzdf gui [-s NAME] [FILE]` in the same binary,
  or a separate `temzdf-gui`. The same binary is simpler for users (one
  install, the server spawned by the same executable); a separate one keeps
  wgpu and winit out of the terminal binary (build time, size, and SSH hosts
  that need no GUI). Recommended: a separate crate and binary, `temzdf-gui`,
  that spawns sessions with `temzdf` (found next to itself or on `PATH`).
- **Renderer.** **(decide)** wgpu from the start, or a CPU renderer first
  (softbuffer + `vello_cpu`/`parley`, both already dependencies of
  `termpix-ui`) with wgpu later. Recommended: wgpu, with the scene turned
  into a plain draw list first (testable without a GPU, see Testing), since
  a full-window CPU blit at 4K every frame costs too much for smooth
  scrolling.
- **Standalone mode.** The TTY binary can run without a server
  (`temzdf FILE`). Recommended: the GUI always attaches to a session
  (starting one, as `attach -c` does); closing the window detaches, a
  configurable action ends the session.
- **Window per session.** One window shows one session (as a terminal does),
  with the session's tab pages in its tab line. Several windows per session
  or a session switcher are later.
- **macOS modifiers.** `<D-...>` (Cmd) already exists in the notation.
  **(decide)** Option as Alt (`<M-...>`) or as a character composer:
  recommended a setting, default "Alt" (what terminal users expect), as
  kitty's `macos_option_as_alt`.
- **Quitting and closing.** **(decide)** what closing the window does:
  recommended detach (the session survives, as a terminal window closing
  under tmux).
- **Decorations.** Server-side where the compositor offers them
  (xdg-decoration: wlroots, KWin, xfwl4), client-side otherwise (GNOME);
  no custom title bar in the first version.

## Problems and risks

- **IME on Wayland** depends on text-input-v3 support in the compositor and
  the IME (fcitx5, ibus); xfwl4 advertises it, sway since 1.9 has it with
  input-method-v2. Preedit drawing, cursor area updates and commit timing
  are where GUI terminals have had bugs; the user types Japanese, so this
  must be tested early on each platform.
- **Fonts**: CJK fallback, wide cells (a CJK glyph spans two cells, the
  scene already marks width), emoji colour glyphs, bold and italic faces,
  and keeping glyphs exactly on the cell grid (no subpixel drift between
  rows). Ligatures are out of scope.
- **Text and image ordering**: layers have z, text grids are drawn above
  all layers today (the TTY can do nothing else). The GUI must honour the
  same rule: tiles (z below the cell background) under text, images from
  programs with kitty's z (text above negative z, below positive z), and
  floating messages on top.
- **Resizing**: the window changes size in pixels continuously; the grid
  changes in whole cells. The remainder is padding (centred or at the
  right/bottom). Live-resize on macOS blocks the event loop.
- **Fractional scales** (1.25, 1.5): cell sizes round to whole physical
  pixels; layers are already in pixels, so nothing else changes.
- **Large textures**: PDF tiles are 512 px squares at most today, well under
  GPU limits; a whole-page thumbnail at 4K may approach 8192 px on old GPUs.
  Upload cost per frame needs a budget (staging belt).
- **Testing on GPUs we do not have**: CI has no GPU; pixel tests need a
  software adapter (Mesa lavapipe/llvmpipe), and its output differs from a
  real GPU in anti-aliasing, so comparisons need tolerance or must avoid
  filtering.
- **The dev shell** has the GUI's test tools since 2026-10-27 (the Vulkan
  loader and lavapipe, xfwm4, headless sway, `grim`, `wtype`, `fcitx5` with
  Mozc); the `ci` shell has none of them yet, so GPU and window tests skip
  there.
- **Miri**: the draw list and input translation are plain Rust and run
  under `scripts/miri.sh`; tests that open a window or a GPU device get
  `#[cfg_attr(miri, ignore = "...")]`.
- **NixOS**: wgpu needs the Vulkan loader and drivers at runtime
  (`LD_LIBRARY_PATH` with `vulkan-loader` and `/run/opengl-driver/lib`,
  as `doc/gpu.md` notes); a `nix run` wrapper must set that up.
- **Licences**: MuPDF's AGPL covers the server; the GUI client links no
  MuPDF, so it could be licensed separately if ever wanted.

## Testing approach

As for the rest of temzdf, everything but the "feel" should be checked
without a human:

- **Draw list**: the scene (frame + images) becomes a list of draw
  operations (quads with texture, source, destination, z; glyph runs with
  cell positions and colours; cursor) in a pure function. Unit-tested with
  the existing scene fixtures; the GPU stage only executes the list.
- **Offscreen GPU rendering**: render a frame into a texture and read it
  back (wgpu with a software adapter in CI), compare with a CPU reference
  for the image layers (`termpix::mock::MockTerminal` already composes
  layers) and with stored expectations for text (tolerances for
  anti-aliasing). `--render-frame` gets a GUI twin.
- **Input translation**: winit's event types cannot be built in tests, so
  translation goes through our own intermediate struct (key, text,
  modifiers, location), unit-tested for the kitty-equivalent output
  (`<S-a>`, `<C-i>`, Option/Cmd, keypad, dead keys).
- **End to end**: under Xvfb on Linux (X11) and headless sway (Wayland,
  `WLR_BACKENDS=headless`, screenshots with `grim`, keys with `wtype`), the
  GUI driven like `scripts/e2e.sh` drives terminals; on macOS through
  `scripts/macos-gui.sh` (virtual display). IME needs fcitx5 or ibus in the
  test session; at least one scripted Japanese commit per platform.

## Plan

Steps that can start now, each committed separately with tests. G0 needs no
window at all.

**G0: protocol and server (no GUI yet)**

1. Precise scrolling: a scroll event with deltas, unit and phase; PDF panes
   follow pixel deltas, terminals accumulate rows. Tests: pane unit tests
   (same final position for one 120 px delta as for its wheel steps, no
   easing with OS momentum).
2. `ClientMsg::Text`, `ClientMsg::Focus`, the input area in `Frame`, the
   `paste` effect and its answer. Tests: session unit tests; the TTY client
   forwards focus reports (tuitest can send them).
3. Theme options (palette, foreground, background) in frames, and libghostty
   given the palette so colour queries are answered. Tests: a program in a
   pane queries OSC 11 and gets the configured background.

**G1: a window that shows a session**

0. Move session discovery and spawning (`name_for`, `first_pane`,
   `spawn_server`, `resolve`) from `crates/temzdf/src/client.rs` into a
   library both clients use. (The dev shell's test tools are in place.)
4. Crate `temzdf-gui` (library + binary): connect or spawn a session, Hello
   with the window's geometry, mirror images into textures, draw layers
   (z, clip, scale) and acknowledge frames. Tests: draw-list unit tests,
   offscreen render of a PDF frame compared with the MockTerminal render.
5. Text grids: font loading (one family plus fallback), a glyph atlas
   (glyphon/cosmic-text, or swash with our own atlas), backgrounds,
   attributes (bold, italic, faint, inverse, underline styles,
   strikethrough), wide cells, the cursor (shapes, blink), the title, the
   pointer shape. Tests: offscreen renders of terminal frames.

**G2: input**

6. Keys (the kitty-equivalent translation), mouse with pixel coordinates
   (minus padding), scrolling with deltas, focus, effects (clipboard,
   links, inverse search), paste. Tests: translation unit tests; e2e under
   Xvfb (type into a shell, scroll a PDF, copy a selection).
7. IME: preedit drawn at the input area, the candidate window placed there,
   commit as `Text`. Tests: e2e with fcitx5 under Xvfb (X11 XIM) and sway
   (text-input-v3), plus a manual check by the user on macOS.

**G3: polish and platforms**

8. HiDPI and fractional scales, padding, decorations, live resize; macOS
   specifics (Option as Alt, Cmd mappings, app bundle if IME or focus need
   it). Tests: e2e at scale 2 (`WAYLAND_DISPLAY` headless output scale).
9. Performance: shared memory for images, texture reuse for tiles, partial
   redraw when only text changed; measure first.

Rough size: G0 a few days of work; G1 and G2 the bulk (the renderer and IME);
G3 open-ended.
