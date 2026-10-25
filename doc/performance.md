# Performance

Measurements and what they mean for the design. Re-measure when touching the
raster path; numbers are from one machine (28 hardware threads, release build).

## Rasterising tiles

Benchmark: `cargo test --release -p temzdf-pdfcore raster_throughput -- --ignored --nocapture`
(`TEMZDF_BENCH_PDF=doc.pdf` for another document). It renders the 512 px tiles
of up to 8 pages at fit width on a 1800 px wide screen, after one warm-up pass
(the first render of a page decodes its images into MuPDF's store).

Tiles per second by pool size, 2026-10-25:

| Threads | Text (LaTeX, 160 tiles) | Images (3000x3000 noise per page, 160 tiles) |
|---:|---:|---:|
| 1 | 5759 | 744 |
| 2 | 3366 | 1406 |
| 4 | 2873 | 2347 |
| 8 | 2270 | 4428 |
| 12 | 2322 | 5287 |

- Text pages get *slower* with more threads. MuPDF takes a global lock
  (`FZ_LOCK_ALLOC`) around every `fz_malloc`/`fz_free` (`source/fitz/memory.c`)
  and text rendering allocates a lot; the locks are static pthread mutexes
  shared by every context (`mupdf-sys` `wrapper/context.c`).
  Allocating and converting tile buffers in Rust alone scales fine (checked
  separately), so the contention is inside MuPDF.
- Image-heavy pages scale almost linearly (decoding and scaling images dominate).
- Either way a whole screen (about 12 tiles at HiDPI) takes a few milliseconds,
  far below what reaching the terminal costs. The pool stays at
  `min(available_parallelism, 4)` (`app.rs`) as a compromise.
- Tiles are rendered into RGBA pixmaps (on opaque white, premultiplied equals
  straight alpha) and copied out. Converting RGB to RGBA pixel by pixel was
  about half the time of a text tile (4146 -> 5759 tiles/s single-threaded).
  Output is byte-identical.

## Display lists

About 70 KiB and 0.2 ms to load per page of a LaTeX text document (201 pages,
measured from RSS). At most 256 are kept (`viewer::MAX_LISTS`), dropping the
ones farthest from the view.

## Terminal transfer

- Opaque images are sent as RGB (`f=24`): 29% fewer bytes without compression,
  about 1% with zlib (it already squeezes the constant alpha).
- Output is written by a background thread, so a slow terminal (SSH, zellij)
  never delays input handling; see `doc/terminal-compat.md`.
