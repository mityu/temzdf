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

## Session socket

Frames go from the session server to each client over its Unix socket:
postcard-encoded into one buffer (`proto::send`), written, read into a buffer
grown as the bytes come and decoded into the images' own buffers
(`proto::recv`). One frame is in flight per client (the next waits for the
acknowledgement), so this time adds directly to the time to show a change.
Shared memory would leave about one copy of the pixels.

Benchmark: `cargo test --release -p temzdf-protocol socket_throughput -- --ignored --nocapture`.
A frame of N 512 px RGBA tiles over a socket pair, by stage; `copy` is one
memcpy into a fresh buffer, about what shared memory would still cost
(median of 7, 2026-10-29):

| Tiles | MB | Encode | Socket | Decode | send + recv | Copy |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 1.0 | 0.04 | 0.22 | 0.09 | 0.44 | 0.08 |
| 4 | 4.2 | 0.41 | 1.59 | 0.46 | 2.41 | 0.52 |
| 12 | 12.6 | 3.68 | 5.17 | 2.03 | 13.83 | 4.76 |
| 30 | 31.5 | 10.55 | 12.59 | 4.32 | 29.56 | 9.85 |

(ms.) Large buffers cost mostly their first touch (page faults): the copy
into a fresh buffer is as slow as the socket per byte.

In use: `TEMZDF_TIMING=1` makes the server and the clients log `timing:`
lines; `tests/e2e/perf.actions` (the first screen, zoom steps, the last page
and back, paging, the wheel, a reload) under `scripts/e2e.sh`, and
`scripts/timing.sh OUTDIR` joins them by frame. Release builds, Xvfb
(lavapipe draws the GUI on the CPU), 2026-10-29; ms:

| Client | Frames with pixels | Heaviest frame | send / recv of it | Its ack | send max | recv max | ack median / max |
|---|---:|---:|---:|---:|---:|---:|---:|
| GUI 900x600 | 23 (46.5 MB) | 7.2 MB | 14.7 / 5.7 | 33.3 | 14.7 | 9.2 | 7.6 / 33.3 |
| GUI 1800x1200 at scale 2 | 30 (162.6 MB) | 22.9 MB | 27.7 / 12.0 | 68.7 | 27.7 | 14.6 | 12.9 / 80.7 |
| kitty 900x600 (`temzdf` client) | 28 (52.5 MB) | 8.2 MB | 2.9 / 8.0 | 13.4 | 13.2 | 10.0 | 2.8 / 19.9 |

`send` includes waiting for the client to read (the socket buffer is
small), so it overlaps `recv`; `ack` is from queueing the frame to the
client's acknowledgement (shown, for a terminal: handed to its writer).
Uploads to the GPU took at most 7.5 ms. The heavy frames are zoom steps
and full redraws (13 to 23 MB at scale 2).

Conclusion: the socket costs more than the threshold set beforehand (a
quarter of a 60 Hz frame, 4 ms, or a fifth of the time to show) on every
heavy frame: 10 to 25 ms of a 30 to 80 ms acknowledgement at scale 2, and
about half of it in the terminal client. Shared memory pays off for local
clients. Cheaper steps that take part of it: write the pixels straight
from the images (vectored writes) instead of encoding them into one more
buffer, and read into a buffer of the announced length (in steps, so a
bogus length cannot allocate much).

### With shared memory

Large images now come in shared memory (`doc/session-server.md`, "Pixels");
`TEMZDF_PIXELS=socket` (`scripts/e2e.sh -P socket`) still sends them inside
the messages, so one build compares both. The benchmark (same command; `send`
is the server's part, `total` until the client has read every byte once):

| Tiles | MB | socket send / total | shared send / total | Copy |
|---:|---:|---:|---:|---:|
| 4 | 4.2 | 6.0 / 7.3 | 3.9 / 4.7 | 1.1 |
| 12 | 12.6 | 7.8 / 10.1 | 7.4 / 8.4 | 3.6 |
| 30 | 31.5 | 26.0 / 29.9 | 6.9 / 8.2 | 3.3 |

`tests/e2e/perf.actions`, release builds, Xvfb with lavapipe, 2026-10-29, ms
(maxima over frames with pixels, and the median acknowledgement):

| Client | Path | send max | recv max | upload / present max | ack median / max |
|---|---|---:|---:|---:|---:|
| GUI 900x600 | socket | 18.8 | 9.7 | 6.8 | 6.8 / 37.3 |
| GUI 900x600 | shared | 4.8 | 0.6 | 6.9 | 6.5 / 21.4 |
| GUI 1800x1200 at scale 2 | socket | 37.7 | 14.5 | 8.1 | 14.0 / 56.7 |
| GUI 1800x1200 at scale 2 | shared | 8.3 to 9.8 | 1.0 to 1.3 | 16.7 to 22.3 | 14.2 / 42.7 to 51.5 |
| kitty 900x600 (`temzdf` client) | socket | 20.9 | 13.9 | 13.9 | 3.2 / 36.2 |
| kitty 900x600 (`temzdf` client) | shared | 4.0 | 0.6 | 7.8 | 2.2 / 11.0 |

The socket's part is gone: sending is the copy into the shared objects,
receiving maps them (prefaulted, `populate`, so the receiving thread takes
the page faults, not the GUI's main thread). The terminal client's worst
acknowledgement went from 36 to 11 ms. At scale 2 the GUI's uploads to
lavapipe became slower (up to about 20 ms, from 8) and took back most of
the gain there; not at scale 1, and not explained yet (page faults are
ruled out by `populate`; reading the mapping is as fast as the heap in the
benchmark). Worth measuring on a real GPU with `TEMZDF_TIMING=1` and
`scripts/timing.sh`, both ways.
