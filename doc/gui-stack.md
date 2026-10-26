# GUI stack research

Question (2026-10-25): if we built a GUI application combining a terminal
emulator, a PDF viewer and a webview in one window (panes and tabs),
GPU-rendered, which toolkits and rendering libraries fit? It must run on
macOS, X11 and Wayland; on Wayland at least wlroots-based compositors (sway,
labwc, river) and Xfce's xfwl4. The pty would come from `portable-pty`, the VT
emulation from `libghostty-vt` (see `doc/multiplexer.md`), web
content from an engine (see `doc/webview-viewer.md`). Also: how much of
temzdf's PDF code a GUI viewer could reuse.

Status: research only, no decision. Collected by a research agent from
crates.io, GitHub, docs and sources on that date. **[V]** verified, **[I]**
inference or background knowledge not re-checked. Nothing was run on xfwl4.

## Reusing temzdf's PDF code in a GUI

The PDF code is not a separate library yet; it lives in `crates/temzdf`.
Judged from module dependencies (no extraction attempted):

| Group | Modules | Reuse |
|---|---|---|
| Terminal-independent | `pdf.rs` (MuPDF wrapper: tiles to RGBA, links, outline, text, search), `layout.rs`, `search.rs`, `selection.rs`, `synctex.rs` (+ vendored parser), `ipc.rs` | as is |
| Light `termpix` coupling | `viewer.rs` (tile cache, prioritised render needs, peek window: registers images with the termpix compositor), `workers.rs` (document thread, raster pool, watcher, SyncTeX: one terminal event variant), `outline.rs`, `keys.rs` | small changes |
| Terminal-specific | `session.rs`, `app.rs`, `headless.rs`, `main.rs` | rewrite; much of `session.rs`'s logic (history, link preview, search flow, reload anchoring) ports, since termpix's `KeyEvent`/`MouseEvent` are plain data a GUI can convert into |

Roughly 60-70% reusable [I]. Steps: move the first two groups into a
`temzdf-core` crate and put an image-sink trait in front of `viewer.rs`'s
compositor calls (GPU textures in a GUI); both can be done while the terminal
viewer keeps working. A GUI drops the terminal workarounds (stand-ins, cell
snapping, congestion control, transport selection). `termpix-ui` renders RGBA
with vello_cpu, usable as textures; drawing on the GPU means vello or the
toolkit's widgets. MuPDF's AGPL applies to a GUI too.

## Wayland baseline

- **xfwl4** [V]: written from scratch in Rust on Smithay (git-pinned),
  GPL-3.0-or-later, release 4.21.1 (2026-08-10), active; Xfce calls it
  experimental and not for daily use. Its source sets up xdg-decoration,
  fractional-scale, text-input-v3 and input-method, cursor-shape,
  linux-dmabuf, viewporter, primary-selection and data-control,
  presentation-time, xdg-activation, layer-shell, XWayland; subsurfaces come
  with Smithay's compositor core.
- **wlroots** compositors provide the same protocols since wlroots 0.17-0.18
  [I]. xdg-decoration is optional (GNOME lacks it), so toolkits need
  client-side decorations as a fallback.
- **Webviews on Wayland**: there is no X11-style foreign child window. An
  embedded webview works only if (a) the toolkit is GTK (WebKitGTK is a GTK
  widget composited in GTK's scene graph), (b) the engine renders offscreen to
  a texture or DMA-BUF that our renderer draws (CEF accelerated OSR, Servo,
  WPE), or (c) we build a `wl_subsurface` per pane ourselves, which no Rust
  toolkit does. `wry`'s `build_as_child` is documented for macOS, Windows and
  Linux X11 only; on Wayland it needs a GTK container [V]. None of this is
  compositor-specific: what works on sway works on xfwl4 given the same
  protocols [I].
- **macOS**: WKWebView as an NSView subview over the Metal layer clips to a
  pane, but always sits above GPU content (our menus and popovers cannot cover
  it) and resizes a frame late [I].

## Toolkits

- **Zed GPUI**: crates.io `gpui` 0.2.2 (2025-10-22), Apache-2.0, nothing newer
  published; Zed now splits it into `gpui_platform`, `gpui_linux`,
  `gpui_macos`, `gpui_wgpu`, so in practice a git dependency; "pre-1.0,
  breaking changes common" [V]. Metal on macOS; on Linux Blade was replaced by
  wgpu (Zed PR #46758, 2026-02; freezes on NVIDIA and Smithay compositors were
  a motivation) [V]. Text: cosmic-text + swash on Linux, CoreText on macOS.
  `gpui_linux` speaks text-input-v3, fractional-scale, cursor-shape,
  xdg-decoration (CSD otherwise), primary-selection, linux-dmabuf,
  viewporter, xdg-activation, layer-shell; no `wl_subsurface` [V]. AccessKit
  is in Zed's workspace. Images go through `RenderImage` into the sprite atlas;
  no public hook for our own wgpu textures or shaders [V] (fine for PDF tiles,
  atlas churn when scrolling many [I]). Zed's terminal uses a fork of
  `alacritty_terminal` and paints text runs and quads; `gpui-terminal` 0.1.0 is
  a small standalone one. `gpui-component` 0.8.0 (2026-10-19, now
  longbridge/gpui-kit): docks, editor, virtual lists; no webview component.
  Verdict: excellent text and a proven terminal, no webview path on Wayland
  short of offscreen bitmaps, API churn.
- **wxWidgets**: 3.3.4 (2026-10-10); Rust binding **wxDragon** 0.9.23
  (bundles 3.3.4, webview and GL canvas examples) [V]. wxWebView uses WKWebView
  on macOS and WebKitGTK on GTK (works on Wayland) [I]. GPU drawing only via
  wxGLCanvas, an EGL subsurface on Wayland with a bug history (#22142;
  distributions forcing GLX) [V]; no wgpu or Metal integration; no terminal
  widget. Weakest fit for a GPU-rendered app.
- **Qt 6** (+ `cxx-qt` 0.10.0; `qmetaobject` dormant since 2023) [V]:
  QtWebEngine composites into the Qt Quick scene graph everywhere, so z-order
  and clipping in split panes just work on Wayland and macOS [I]; QtPdf
  (PDFium); qtermwidget / QMLTermWidget (no kitty graphics); RHI (Vulkan,
  Metal, GL) through `QQuickRhiItem` in C++; LGPLv3/GPL. Cost: terminal and
  PDF rendering in C++/QML, heavy build, Rust-Qt boundary.
- **GTK4** (`gtk4` 0.11.5, `vte4` 0.10.0, `webkit6` 0.6.1) [V]: GSK on Vulkan
  (Wayland) or GL; cheap textures including zero-copy DMA-BUF; WebKitGTK
  composed inside GSK, the best webview story on Wayland; Ghostty's Linux app
  is GTK4 [I]. But Homebrew's `webkitgtk` is Linux-only [V], so macOS would
  need WKWebView in a native view, and GTK's macOS IME and accessibility are
  second-class [I]. Best if Linux-only were acceptable.
- **Tauri 2 / wry** (`tauri` 2.12.5, `wry` 0.57.1) [V]: webview-first; terminal
  as xterm.js + `portable-pty` (no kitty graphics), PDF via pdf.js or MuPDF
  tiles as images; WebKitGTK performance and DMA-BUF renderer bugs on Linux
  [I]; the CEF runtime still has gaps.
- **egui** 0.37.0 [V]: wgpu or glow on winit 0.30 (no cursor-shape-v1 there;
  0.31 in beta); easy custom wgpu callbacks and textures; AccessKit; terminal
  grid would be ours (`egui_term` 0.1.0 exists); wry child webviews X11 only.
- **iced** 0.14.0 [V]: wgpu, input method support, custom shader pipelines,
  concurrent image upload; webview crate dead.
- **Slint** 1.18.1 (GPL-3 / royalty-free / commercial): declarative widgets, no
  terminal or webview [V/I].
- **Others**: Makepad 1.0, Xilem/Masonry 0.4 (vello, experimental), Floem 0.2,
  Freya 0.4.3 (Skia; webview X11 only), Dioxus native / Blitz (no JS). None has
  a mature terminal or solves webviews on Wayland [I].

## Rendering libraries for a custom stack

`wgpu` 30.0.1; `vello` 0.11.0 (GPU), `vello_cpu` 0.3.0, `femtovg` 0.27.0,
`skia-safe` 0.153.3; text: `parley` 0.12.0, `cosmic-text` 0.19.0, `swash`
0.2.10, `glyphon` 0.12.0 (cosmic-text to a wgpu atlas); `accesskit` 0.25.1 [V].

- GPU terminals [I]: Ghostty shapes runs (HarfBuzz/CoreText, ligatures), packs
  glyphs into grayscale and colour atlases and draws instanced quads (cell
  backgrounds, glyphs) with kitty images as textured quads in z layers;
  Alacritty: GL atlas + instanced quads, no ligatures; WezTerm: wgpu or GL,
  HarfBuzz, atlas; Zed: GPUI's sprite atlas.
- libghostty's render state [V, "not yet stable"]: dirty-tracked rows and cells
  with styles and cursor after a locked update, and kitty placements with z
  and crop resolved; feeds an instanced-quad renderer directly (rebuild dirty
  rows' instance buffers, keep image textures by id). The Rust binding is
  `!Send`: keep terminal and render state on one thread and hand instance data
  to the render thread.
- PDF on GPU: MuPDF CPU tiles uploaded as wgpu textures through a tile cache
  (as temzdf does with terminal images) is the proven, fastest path [I];
  `hayro` 0.8.0 renders through `vello_cpu` and its author calls performance
  its weak spot [V] (a GPU path would need a vello device for it);
  `pdfium-render` 0.9.4 also rasterises on the CPU.
- Offscreen webviews: CEF accelerated OSR gives DMA-BUF frames on Linux and
  IOSurface on macOS [V/I]; Servo 0.7.0 has an offscreen rendering context and
  `wgpu-native-texture-interop` [V]; WPE WebKit 2.54 is DMA-BUF capable but
  Linux-only without Rust bindings [V].

## Comparison

| Option | Linux X11 / Wayland (wlroots, xfwl4) | macOS | GPU | Terminal text / IME | Large textures | Webview | Licence |
|---|---|---|---|---|---|---|---|
| GPUI (git) | good; text-input-v3, fractional-scale, cursor-shape, xdg-decoration/CSD; no subsurfaces | excellent | wgpu / Metal | excellent (Zed terminal) | atlas only | offscreen bitmaps only | Apache-2.0 |
| wxWidgets / wxDragon | GTK3; GL subsurface fragile | good | GL only | none | via GL | wxWebView native (works on Wayland) | wxWindows / MIT |
| Qt 6 + cxx-qt | very good | very good | RHI | qtermwidget (C++) | good (C++) | QtWebEngine composited everywhere | LGPL / GPL |
| GTK4 + vte4 + webkit6 | best | mediocre; no WebKitGTK | Vulkan / GL | VTE, no kitty graphics | DMA-BUF / GL textures | WebKitGTK composited (Linux only) | LGPL / MIT |
| Tauri / wry | good, WebKitGTK quirks | good | browser | xterm.js | via DOM / canvas | it is the webview | MIT / Apache |
| egui / iced + wgpu | good via winit | good | wgpu | ours; IME ok | excellent | wry child X11 only; offscreen CEF/Servo | MIT / Apache |
| Custom winit + wgpu + parley/swash | as winit | good | wgpu | ours, with libghostty | excellent | offscreen CEF/Servo, or WKWebView on macOS | - |

## Recommended stacks

1. **Custom renderer**: winit + wgpu + swash/parley (or glyphon) +
   `libghostty-vt` + `portable-pty` + MuPDF tiles as textures + offscreen CEF
   (optionally WKWebView on macOS); egui can draw chrome on the same wgpu
   device. Keeps temzdf's architecture; kitty images, PDF tiles and web frames
   are all textured quads with per-pane z-order and clipping; behaves the same
   on wlroots and xfwl4 because everything is our own surface. Risks: the most
   work (IME preedit, accessibility, decorations), CEF's size and its Linux
   DMA-BUF OSR maturity and HiDPI quirks, libghostty's unstable API.
2. **GPUI (git pin) + gpui-component** + `alacritty_terminal` (proven in Zed)
   or libghostty + MuPDF tiles via `RenderImage` + offscreen CEF/Servo frames.
   Fastest route to an editor-grade UI. Risks: no crates.io releases since
   2025-10, no custom texture hook (tiles and web frames churn the atlas, no
   zero-copy), untested on xfwl4.
3. **Qt 6** (QtWebEngine + RHI) with the Rust core through `cxx-qt`: the only
   option where a real browser composes correctly in split panes on Wayland
   (wlroots, xfwl4) and macOS without extra work. Risks: terminal and PDF
   rendering in C++/QML, heavy build and distribution, LGPL, the Rust-Qt
   boundary.

Avoid wxWidgets (weak GPU path, fragile on Wayland) and wry child webviews in
winit-based toolkits (X11 only). GTK4 would be first choice for a Linux-only
application.

Open: CEF's real distribution size (estimates range from hundreds of MB to
about 1 GB); nothing here was run on xfwl4.
