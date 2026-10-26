# Markdown viewer research

Question (2026-10-25): how should a Markdown previewer that reuses `termpix`
be built? Target: preview Markdown beside Vim/Neovim, GitHub-flavoured
(tables, task lists, footnotes, alerts), math, code highlighting, images
(including SVG), ideally mermaid, live update keeping the position, and
editor <-> preview position sync like SyncTeX. CPU only, Linux and macOS,
usable over SSH.

Status: research only, no decision yet. The scope later grew to a general
webview-based previewer (arbitrary HTML/CSS/JS); see `doc/webview-viewer.md`,
which supersedes the engine choice below for that case. Versions and dates were collected by
research agents from crates.io and GitHub on that date; latency figures are
estimates or secondary sources unless marked as measured (none are).

## Approaches

| Approach | Pros | Cons |
|---|---|---|
| 1. Markdown -> HTML -> **Blitz** (offscreen HTML/CSS engine) | Same `parley` 0.12 / `vello_cpu` 0.3 as `termpix-ui`; pure CPU; `paint_scene` takes x/y offsets, so tiles or the viewport of a tall document can be painted; hit testing and text selection built in; an existing app renders READMEs with `github-markdown-css` | Beta, API churn; git-pinned forks of taffy and stylo (dependency unification); heavy build (stylo); no JS, so math and mermaid must become SVG beforehand |
| 2. **Own layout**: Markdown AST -> parley + taffy + vello_cpu | Full control; each block keeps its source range, so scroll sync is exact and simple; reflow on resize is cheap; grows `termpix-ui` | Everything is ours: block flow, inline rich text, tables, nested lists, code blocks; estimated 3-5k lines (`termpix-ui` is ~620 lines today and does not use taffy yet) |
| 3. Markdown -> **Typst** -> `typst` crate | Excellent math; `mlux` already does this in a terminal | Print-like look; a width change re-lays out everything (likely 100+ ms for 20 pages); source mapping takes two hops (Markdown -> generated `.typ` -> frame, via `typst-ide`); ~200 crates; Typst API changes every 0.x release |
| 4. **Headless Chromium** over CDP (`chromiumoxide`, `headless_chrome`) | Highest fidelity; KaTeX and mermaid run unchanged | External 100+ MB browser; screenshot round trips of tens to 100+ ms; hit testing needs JS round trips |

Rejected:

- **Servo** (`servo` 0.7, MPL-2.0): full JS, but the software path
  (`SoftwareRenderingContext`, software GL) is documented as slow, no clear
  macOS software path, huge build.
- **CEF** (`cef` crate): multi-process, hundreds of MB, app bundling on
  macOS, full-frame repaints in software OSR; heaviest integration.
- **System webviews** (what Tauri/wry use): `wry` shows a webview in a native
  window and has no pixel readback. Using the platform APIs directly means two
  backends with display caveats: WebKitGTK needs GTK and a display server
  (Xvfb over SSH); WPE WebKit has a headless CPU backend but is Linux-only,
  rarely packaged, without maintained Rust bindings; macOS `WKWebView`
  `takeSnapshot` works but must run on the AppKit main thread and may lack a
  WindowServer session over SSH. Reconsider only for a local-macOS-only tool.
- **Ultralight**: proprietary licence.
- **litehtml** (C++, `litehtml` crate 0.2.6, MIT): light and has a CPU
  backend, but older CSS (no grid, limited flexbox) and a second text stack.
  Fallback if Blitz fails.

## Building blocks (any approach)

| Need | Candidates |
|---|---|
| Parser | `comrak` 0.57 (BSD-2; full GFM, alerts, math, footnotes; line/column `sourcepos` on every node) or `pulldown-cmark` 0.13 (MIT; streaming, byte offsets, `ENABLE_GFM` alerts, `ENABLE_MATH`) |
| Math without JS | `RaTeX` 0.1.x (MIT; pure-Rust KaTeX port, emits a display list that could be painted with vello_cpu; young); fallback: per-formula Typst via `mitex`. Not usable: `pulldown-latex` (MathML, no layout engine), ReX (dormant), `katex` crate (HTML needing CSS), `mathjax_svg` (embeds V8) |
| Mermaid without a browser | `merman` 0.9 (MIT/Apache; reimplements all 37 diagram types of Mermaid 12.1, SVG out; disable the EPL-2.0 ELK layout feature); `mermaid-rs-renderer` 0.3 (MIT; 23 types, early) |
| Code highlighting | `syntect` 5.3 (easiest styled spans); later `arborium` or `lumis` (tree-sitter, permissive). Avoid `giallo` (EUPL-1.2) |
| Images | `image`; `resvg` 0.49 for SVG |

`resvg` (and `typst-render`) rasterise with tiny-skia while the UI uses
vello_cpu; shipping both is acceptable.

## Prior art

- Terminal viewers mostly draw text cells plus images: `mdcat` (archived
  2026-06), `glow`, `md-tui`, `frogmouth`, `mdfried` (headers as images),
  `presenterm` (math and diagrams through external typst/LaTeX/mermaid-cli).
- `mlux` (Rust) is the closest: Markdown -> Typst -> PNG tiles over the kitty
  protocol, lazily per viewport; kitty and Ghostty only, `--watch`, no editor
  sync.
- `inlyne` (GUI, wgpu): comrak -> HTML -> its own small renderer; keeps the
  position on reload.
- None combine whole-page pixels, tmux/zellij support, two-way editor sync and
  live updates of unsaved buffers.

Editor integration in previews:

- `markdown-preview.nvim`: the server pulls the in-memory buffer over Neovim
  RPC and pushes content plus `cursor`/`winline`/`winheight` to the browser;
  sync modes `middle`, `top`, `relative`; editor -> preview only.
- `peek.nvim`: Lua writes length-prefixed frames (`show` + buffer text) to the
  previewer's stdin on save or TextChanged (throttled), and `scroll(line)` on
  CursorMoved.
- `mdp`: buffer content as NDJSON on stdin, debounced; cursor sent separately.
- VS Code: block elements carry `data-line=N`; editor -> preview finds the
  anchors around a line and interpolates linearly between their pixel
  positions (per line inside fenced code); preview -> editor binary-searches
  the anchors and interpolates back.
- typst-preview/tinymist: incremental SVG over WebSocket; jumps both ways via
  `typst-ide` (`jump_from_click`, `jump_from_cursor`); Typst spans stay stable
  across edits.

Expected features: GFM tables, task lists, strikethrough, highlighted code,
images, GitHub alerts, footnotes, `$`/`$$`/```` ```math ```` math, mermaid,
front matter hidden or shown. Common complaints: Node/Deno installs, switching
to a browser window, no live update, lag on large files, math as Unicode
approximations, silent failures of external renderers.

## Design lessons

1. Receive the unsaved buffer from the editor through a small Vim/Neovim
   plugin (length-prefixed frames or NDJSON over stdin or a Unix socket),
   debounced around 50-150 ms; send cursor/top-line updates as separate cheap
   messages. Keep file watching as the fallback without a plugin.
2. Scroll sync as VS Code does: source line anchors per block, linear
   interpolation both ways; default to matching the editor's top line.
3. Preview -> editor jumps reuse the SyncTeX inverse-search command template.
4. Re-render incrementally and keep the viewport anchored to the same source
   block across updates.
5. When a math/mermaid renderer fails, show the source block visibly; cache
   rendered images by content hash.
6. Single binary plus a tiny plugin; render only the visible range.

## Suggested next steps (not started)

1. One-day Blitz spike: render a fixture `.md` to PNG the way `--render-frame`
   does, measure layout and raster times, and check that its dependencies
   coexist with ours in the workspace.
2. Write the preprocessing (parse, math and mermaid to SVG, highlighted code)
   so it survives a switch to approach 2.
3. Switch to approach 2 if Blitz's quality or dependencies become a problem.

Open questions for the user: approach (start with the Blitz spike?), look
(GitHub-like vs print-like), and whether the viewer lives in this repository.
