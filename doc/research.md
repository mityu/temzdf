# Library research (snapshot: 2026-10-25)

Why each dependency was chosen, and what was rejected. Versions are as of the
date above; re-check before relying on details.

## Overall approach

Two architectures were compared:

- **A. TUI + images**: ratatui draws chrome in terminal text cells; only pages are
  kitty images. Cheap and robust, but chrome is cell-granular.
- **B. Everything is pixels** (chosen): we render pages and chrome ourselves and
  send images. GUI-like smoothness and freedom; needs our own widget kit.

Full-frame streaming (as gpuix does) costs bandwidth on every frame. We instead
upload page images once and re-place them per frame, uploading only chrome
layers that changed (see `architecture.md`).

## PDF engine

| Option | License | Notes | Verdict |
|---|---|---|---|
| `mupdf` 0.8 (mupdf-rs, vendors MuPDF ~1.27) | AGPL-3.0 | Best quality and speed. Per-char quads (`TextPage`), `Page::search` / `TextPage::search` returning quads, `copy_selection`, `outlines()`, `links()`, page labels, passwords, custom `NativeDevice`. `DisplayList` is `Send + Sync`; `Document` is not `Send`. Builds MuPDF from C (needs clang/libclang). Default features are large (js, html, epub, tesseract...); trim them. | **Chosen** |
| `pdfium-render` 0.9 | MIT/Apache (PDFium BSD) | Nearly the same features; needs a prebuilt `libpdfium` (bblanchon/pdfium-binaries). No device API, so no GPU path. Not thread-safe (calls serialised). | Fallback if a permissive license is ever needed |
| `hayro` 0.8 | MIT/Apache | Pure Rust, renders via `vello_cpu`, `Device` trait. No text-layout/search/outline/links/page-label APIs (DIY). Non-embedded CID fonts unsupported (hurts some CJK PDFs). `gpui-pdf` shows search on top of it. | Rejected for now |
| poppler-rs | GPL | Per-char boxes not in the safe API; heavy glib/cairo stack. | Rejected |
| pdf-rs `pdf_render` | MIT | Unpublished, dormant. | Rejected |

GPU rasterisation (`vello_gpu`, `vello`, Skia) was judged not worth it: every frame
would pay GPU->CPU readback plus terminal transfer, which dominates CPU raster
cost for page-sized images. Kept as a long-term experiment in `todo.md`.

## UI stack

| Layer | Choice | Alternatives considered |
|---|---|---|
| 2D raster | `vello_cpu` 0.3 (pure Rust, SIMD, used by hayro/Masonry/Blitz) | `tiny-skia` 0.12 (no text), `vello_gpu` (GPU), `skia-safe` (heavy C++) |
| Text | `parley` 0.12 + `fontique` (bidi, line breaking, selection, CJK fallback) | `cosmic-text` 0.19 |
| Layout | `taffy` 0.15 | — |
| Widgets | Our own (`termpix-ui`) | egui 0.37 (easy offscreen, weak CJK shaping), Slint 1.18 (software renderer, GPL/royalty-free/commercial), iced 0.14 (headless only via test APIs), Masonry/Xilem 0.4 (alpha), Blitz (HTML/CSS, beta, heavy), GPUI (git-only, unstable) |

## Terminal I/O

- No existing crate covers what we need: crossterm 0.29 does not parse SGR-pixel
  mouse (1016) and mishandles APC replies; `termina` 0.4 has a 1016 report type but
  is unverified for APC. We write the parser and the kitty encoder ourselves.
- Reference implementations worth reading:
  - gpuix `packages/gpui-terminal/src/{kitty.rs,tty.rs}` (Apache-2.0): shm/inline
    probe, fences, 2026 wrapping, mode setup, macOS `select()` workaround.
  - `kittage` 0.4 (itsjunetime): kitty protocol client used by tdf.
  - tdf (Rust + mupdf + ratatui): render thread, prefetch window of 20 pages,
    re-render on `ENOENT`, z-index flip for flicker-free swaps.
  - yazi (`yazi-adapter`, `yazi-emulator`): per-terminal protocol driver table.
  - termpdf.py: feature set (TOC, link hints, page labels) and using the terminal
    as an image cache.
- Helper crates: `rustix` (`shm::open`) + `memmap2` for `t=s` (avoid the
  unmaintained `shared_memory`), `flate2` with the `zlib-rs` backend for `o=z`,
  `base64`.

## gpuix (remorses/gpuix) as an alternative

Node/Bun bindings for Zed's GPUI with a terminal mode that renders offscreen and
streams full frames via kitty graphics (shm locally, zlib inline over SSH,
SGR-pixel mouse, kitty keyboard). Rejected as a foundation because it pins a stale
GPUI fork, end-to-end Linux support is unverified by its author, it always sends
full frames, and it does no smooth scrolling. Its transport and input code are
still the best reference (see above).
