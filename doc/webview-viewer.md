# Webview previewer research

Question (2026-10-25): the Markdown viewer (`doc/markdown-viewer.md`) grows
into a general **webview-based previewer**: show arbitrary HTML/CSS/JS through
`termpix`, e.g. Markdown rendered by JS toolchains (markdown-it, KaTeX,
mermaid), HTML exported by other tools (Typst, pandoc, Jupyter/Quarto,
reveal.js slides), SVG, or a local dev server. Full JS and modern CSS become
required, pixels must arrive continuously (scrolling, animation, live reload),
and input must reach the page. Linux and macOS, no display server or GPU, over
SSH.

This changes the Markdown conclusion: Blitz has no JS, so it is at most a
no-JS fast path.

Status: research only, no decision. Figures are marked **[M]** measured,
**[C]** cited, **[E]** estimated. Versions and dates were collected by a
research agent on that date.

## Measurements [M]

Chromium 156.0.8078.47 (nixpkgs), `--headless --disable-gpu`, `DISPLAY` and
`WAYLAND_DISPLAY` unset, i7-14700 (28 threads), Linux 6.18; an 1800x1200
viewport on a long page with grid, gradients, tables and code, driven by a
small CDP client:

| Operation | Median | p90 | Size |
|---|---|---|---|
| `Page.captureScreenshot`, full 1800x1200, PNG | 77 ms | 95 ms | 146 KiB |
| same, PNG `optimizeForSpeed` | 66 ms | 77 ms | 265 KiB |
| same, JPEG | 63 ms | 73 ms | 158 KiB |
| same, WebP | 112-125 ms | 134-147 ms | 86 KiB |
| clip 400x100 (any format) | ~50 ms | 51 ms | 2-4 KiB |
| wheel, then screenshot (round trip) | 84 ms | 96 ms | |
| tall full-page capture, 1800x20000 PNG | 354 ms | | 4.9 MiB |

`Page.startScreencast`: an idle page sends no frames; a CSS animation gives
60 fps of full 1800x1200 frames (199 KiB JPEG q90, 270 KiB PNG on average);
wheel to next frame 35 ms median (PNG), 39 ms (JPEG), at most 42/50 ms, with
Chromium's smooth scrolling on. Small clipped screenshots cost as much as big
ones, so screencast is the path for continuous updates. All Chromium processes
together: 1137 MiB RSS (overcounts shared pages). The nixpkgs Chromium closure
is 1.7 GiB including GTK.

## Engines

### Headless Chromium over CDP (recommended)

- Since Chrome 132 `--headless` is the "new" headless mode (full Chrome without
  UI); the old one ships separately as `chrome-headless-shell` [C:
  developer.chrome.com/blog/removing-headless-old-from-chrome]. Rust clients:
  `chromiumoxide` 0.9.1, `headless_chrome` 1.0.22; the CDP subset we need is
  small enough for a thin client of our own.
- Frames: `startScreencast` (JPEG or PNG only, full viewport, acknowledged,
  `maxFramesInFlight`), `captureScreenshot` (JPEG/PNG/WebP, `clip`), and
  `HeadlessExperimental.beginFrame` for deterministic frames (needs
  BeginFrameControl targets; in practice `chrome-headless-shell` only, to
  verify). No dirty rectangles: we diff frames ourselves. Decoding an
  1800x1200 frame costs about 5-15 ms [E].
- Input: `Input.dispatchMouseEvent` (move/hover, press, drag, wheel with pixel
  deltas), `dispatchKeyEvent`, `insertText` (termpix delivers committed text,
  so preedit is not needed).
- Selection: a dispatched drag selects natively; copy with
  `Runtime.evaluate("getSelection().toString()")` and OSC 52.
- Scrolling: let the page scroll (sticky headers, lazy loading, `vh`, JS
  scroll handlers all depend on it). A tall image scrolled by us breaks those
  and is slow (354 ms, 4.9 MiB per 20k px). Our kinetic scroll can drive the
  page offset (wheel deltas or `scrollTo`, with `--disable-smooth-scrolling`)
  [E].
- Reload keeping the position: `Page.reload`, then restore from an element
  anchor (element at the viewport top plus offset) via JS; or patch the DOM
  (e.g. morphdom for Markdown); dev servers keep it themselves (HMR).
- Distribution: the user's Chrome/Chromium, or a downloaded Chrome for Testing
  `chrome-headless-shell` (~100-150 MB [E]). BSD-3 plus third-party notices.

### CEF off-screen rendering (`cef` crate 156.x, tauri-apps/cef-rs)

`OnPaint` delivers raw BGRA plus dirty rectangles (with shared textures off):
no encode/decode. Input through `SendMouseWheelEvent`, `SendKeyEvent`,
`ImeCommitText`. But multi-process, hundreds of MB [E], an `.app` bundle with
helpers on macOS, and headless Linux without X11 is doubtful (reports suggest
Xvfb). Best frame path in theory, heaviest integration. Reconsider if
Chromium's encode/decode cost dominates.

### Servo (`servo` 0.7.0, MPL-2.0)

First crates.io release 2026-04 (LTS every six months, monthly releases may
break the API). In-process Rust with JS (SpiderMonkey) and raw RGBA through
`SoftwareRenderingContext` + `read_to_image(rect)`, but software GL is slow,
web compatibility has gaps, the build is huge and no macOS software path is
confirmed. Re-evaluate at the next LTS.

### Rejected

- **WPE WebKit**: headless WPEPlatform backend and Skia CPU rendering exist,
  but Linux only and no maintained Rust bindings.
- **macOS WKWebView** snapshots: AppKit main thread, misbehaves detached from
  a window, doubtful over SSH, and a second backend.
- **Ladybird/LibWeb**: no stable embedding API (alpha targeted for 2026).
- **Lightpanda**: headless browser without a rendering engine (no pixels).
- **Blitz**: no JS; only a no-JS fast path. Note: the two research passes
  disagreed on its state (0.3.0-beta vs 0.2.4 "pre-alpha"); check before use.

## Architecture

- A separate binary (`temzdf-web`) depending only on `termpix` and
  `termpix-ui`, with an engine trait (`resize`, frames, input, `eval`,
  `reload`); Chromium/CDP first, CEF or Servo later. Our UI chrome (status bar,
  URL/search line, link hints) stays termpix-ui layers above the page layer.
- Frame path: screencast PNG (JPEG q90 blurs text) -> decode on a worker ->
  diff against the previous frame in tiles or strips -> send only changed
  parts (Layered: tiles re-placed; Composited/Placeholders: changed strips).
- SSH bandwidth: a full frame is 8.6 MB raw, likely 0.5-2 MB after our
  zlib+base64 [E]; cap the frame rate when congested (`is_congested`) and use
  `maxFramesInFlight=1` for backpressure. Scrolling changes the whole
  viewport; later, detect pure vertical shifts (screencast metadata reports
  `scrollOffsetY`), re-place existing tiles and send only the exposed band.
- Viewport = terminal pixel size; `deviceScaleFactor` from cell size or user
  zoom. Link hover can go to our status bar via hit testing in JS [E].
- Editor sync, in layers: source positions provided by the content
  (markdown-it `data-source-line`, pandoc `sourcepos` `data-pos`); an opt-in
  hook for previewed pages (`postMessage` or `window.__temzdf.scrollToLine`);
  text-anchor matching (find the cursor line's text with a TreeWalker; send
  clicked text back to the editor); proportional scroll as the last resort.
  Typst HTML export and Jupyter output appear to have no source maps (check).

## Recommendation

1. **Headless Chromium over CDP**, screencast plus our own tile diff: works
   today on Linux and macOS without display or GPU, full fidelity, 60 fps and
   ~35 ms wheel-to-frame locally [M].
2. **CEF OSR** only if encode/decode cost or latency proves limiting.
3. **Servo**: revisit in about six months.

Risks: an external ~1 GiB-RSS browser and its startup time; scroll feel
(engine latency + decode + terminal transfer); SSH bandwidth while scrolling or
animating; no dirty rectangles; Chromium flag churn; running arbitrary JS and
`file://` (keep the browser sandbox on, temporary `--user-data-dir`); missing
fonts on headless servers.

## Suggested first spike (not started)

`temzdf-web URL|FILE`, about 300 lines: CDP client -> screencast PNG -> decode
-> termpix layer; forward wheel, click, drag and keys; copy via OSC 52.
Measure, under `TERMPIX_TRACE` with `TERMPIX_MEDIUM=direct` and `shm`, in kitty
and zellij and once on macOS: cold start to first frame, wheel-to-photon
latency and fps, decode + diff time per frame, bytes per scroll step and per
second of animation (with and without tile diff and shift reuse), idle CPU
(should be ~0), memory, PNG vs JPEG text quality, and position kept across
`Page.reload` vs a DOM patch.
