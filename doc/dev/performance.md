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
  `min(available_parallelism, 4)` (`DocumentHost` in
  `crates/temzdf-pdfview/src/host.rs`) as a compromise.
- Tiles are rendered into RGBA pixmaps (on opaque white, premultiplied equals
  straight alpha) and copied out. Converting RGB to RGBA pixel by pixel was
  about half the time of a text tile (4146 -> 5759 tiles/s single-threaded).
  Output is byte-identical.

## Display lists

About 70 KiB and 0.2 ms to load per page of a LaTeX text document (201 pages,
measured from RSS). At most 256 are kept (`doc_cache::MAX_LISTS` in `temzdf-pdfcore`), dropping the
ones farthest from the view.

## Terminal transfer

- Opaque images are sent as RGB (`f=24`): 29% fewer bytes without compression,
  about 1% with zlib (it already squeezes the constant alpha).
- Output is written by a background thread, so a slow terminal (SSH, zellij)
  never delays input handling; see `doc/terminals/terminal-compat.md`.

## Session socket

Frames go from the session server to each client over its Unix socket
(`session::send_server` / `recv_server` in `temzdf-protocol`): the message
is postcard-encoded with the small images (UI parts) inside it; large
images go in shared memory, a descriptor each with the message, or, when
the client did not ask for it (`TEMZDF_PIXELS=socket`) or the memory cannot
be made, their bytes follow the message, written from the images and read
into buffers of exactly their size. One frame is in flight per client (the
next waits for the acknowledgement), so this time adds directly to the time
to show a change.

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

Large images now come in shared memory (`doc/design/session-server.md`, "Pixels");
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

### The socket without copies

Without shared memory (`TEMZDF_PIXELS=socket`, and images that cannot go
there) a frame's images of 4 KiB and more no longer go through the encoder:
their bytes follow the message as they are, written from the images in one
vectored write, and the client reads each into a buffer of exactly its size
that becomes the image (two copies, into and out of the kernel, instead of
four). Same procedure before (`2573137`) and after, release builds,
nothing else running, the median of 3 runs of each (ms; the benchmark's
own figures are medians of 7):

| Benchmark (socket rows) | send before | send after | total before | total after |
|---|---:|---:|---:|---:|
| 4 tiles, 4.2 MB | 5.13 | 1.85 | 6.17 | 2.06 |
| 12 tiles, 12.6 MB | 7.94 | 3.49 | 10.11 | 4.12 |
| 30 tiles, 31.5 MB | 25.85 | 6.65 | 29.83 | 7.83 |

The 3 runs of 30 tiles spanned 29.5 to 30.1 ms before and 7.8 to 8.0 after.

| `perf.actions -P socket` | send max | recv max | heaviest 5 frames, send + recv | ack median / max |
|---|---:|---:|---:|---:|
| GUI 900x600, before | 17.5 | 11.2 | 15.2 | 5.7 / 34.6 |
| GUI 900x600, after | 8.7 | 8.6 | 8.4 | 5.4 / 25.0 |
| GUI at scale 2, before | 30.8 | 13.2 | 31.0 | 13.8 / 51.3 |
| GUI at scale 2, after | 12.8 | 12.8 | 9.6 | 12.6 / 47.7 |
| kitty, before | 14.3 | 12.3 | 18.1 | 3.4 / 22.6 |
| kitty, after | 7.4 | 7.5 | 9.5 | 2.8 / 11.9 |

The socket now costs about what shared memory does on heavy frames (the
benchmark's 30 tiles: 7.8 ms against 8.2), as the copies left are the same
size; shared memory still saves the kernel's buffers and the client's
allocation.

## Terminal panes under load

Measured 2026-11-18 (i7-14700, release, the simulated terminal of
`crates/tuitest`), with the ignored tests in `crates/temzdf/tests/tui.rs`:

```sh
cargo test --release -p temzdf --test tui measure_ -- --ignored --nocapture
```

The session parses every pane's output (libghostty-vt's `vt_write`) on its
main loop, the loop that handles keys too, so a pane writing all the time
might delay keys typed into another.

- `measure_echo_while_another_pane_floods`: 60 keys typed into a shell on
  the right half, from sending a key to seeing it, with the left pane idle
  and with it running `yes` (the server at about 150% CPU, `yes` at 100%,
  so the pane kept up). Median 16.1 ms (p90 17.0, max 17.8) idle against
  15.6-16.4 ms (p90 16.7-16.8, max 16.9-17.6) beside `yes`, over two runs:
  `yes` made no difference, but every echo took about one frame interval.
  The cause: the server made at most one frame per `FRAME` (16 ms, in
  `crates/temzdf-server/src/daemon.rs`), and the test types the next key
  right after seeing the last echo, so every echo (back from the shell
  0.4 ms after the key) waited for the interval; with 50 ms between keys
  it took 5.2 ms idle and still 14.3 ms beside `yes`, whose frames keep the
  interval running. Since `Pacing` (two frames right after typing go out at
  once, as kitty does):

  | Between keys | Other pane | Median | p90 | Max |
  |---|---|---:|---:|---:|
  | none | idle | 2.2 ms | 5.1 ms | 5.3 ms |
  | none | `yes` | 1.6 ms | 1.7 ms | 1.7 ms |
  | 50 ms | idle | 5.2 ms | 5.3 ms | 5.4 ms |
  | 50 ms | `yes` | 1.7 ms | 1.7 ms | 4.4 ms |

  (50 ms between keys: a temporary change to the test.) Idle is slower
  than beside `yes` probably because of the CPU's power saving: with four
  other cores kept busy by shell loops, the idle median at 50 ms went from
  5.2 to 2.8 ms.
- `measure_output_throughput`: 100 MB of `yes` through a pane took 12.1 s
  (8 MB/s), and 12.5 s written straight to the simulated terminal (also
  libghostty-vt) without temzdf: the parser's line feeds (two bytes a
  line, a scroll each) set the rate, not the session.

## Text decorations

Measured 2026-11-22 (i7-14700, `temzdf test`) with a scratch script test
(not kept): N lines in a buffer shown in a 60x20 float, N style-only
decorations added one request each (`text.addDecoration`), then 200 of
each cheap request timed (`text.cursor`, `text.setCursor` near the top
and in the middle, `text.edit` inserting a character mid-buffer), then
the decorations deleted one by one. Tree-sitter and LSP highlighting will
make that many. Per request in ms (add and delete: the whole phase in s);
"before" is 6a2f528, "index" the marks indexed, "cache" with text panes'
frames cached too:

| N | build | read before / index / cache | setCursor (mid) before / index / cache | edit before / cache | add all before / cache | delete all before / cache |
|---:|---|---|---|---|---|---|
| 0 | release | 0.05 / 0.06 / 0.05 | 0.05 / 0.05 / 0.05 | 0.08 / 0.09 | 0 / 0 | 0 / 0 |
| 2000 | release | 0.08 / 0.08 / 0.05 | 0.09 / 0.09 / 0.10 | 0.15 / 0.16 | 0.15 / 0.17 | 0.20 / 0.21 |
| 8000 | release | 0.14 / 0.07 / 0.05 | 0.20 / 0.08 / 0.09 | 0.37 / 0.31 | 0.75 / 0.65 | 1.2 / 0.8 |
| 32000 | release | 0.51-0.75 / 0.07 / 0.05 | 0.79-1.19 / 0.08 / 0.11 | 1.5 / 1.2 | 9.2-10.1 / 2.7 | 14.8-18.0 / 3.5 |
| 0 | debug | 0.21 / 0.20 / 0.19 | 0.21 / 0.22 / 0.21 | 0.58 / 0.59 | 0 / 0 | 0 / 0 |
| 2000 | debug | 0.65 / 0.48 / 0.18 | 0.92 / 0.72 / 0.76 | 1.25 / 1.34 | 1.6 / 1.1 | 1.5 / 1.3 |
| 8000 | debug | 0.78 / 0.41 / 0.19 | 1.23 / 0.67 / 0.72 | 2.2 / 2.5 | 5.5 / 3.8 | 6.9 / 5.1 |
| 32000 | debug | 1.70 / 0.40 / 0.19 | 2.84 / 0.64 / 0.69 | 6.0 / 7.6 | 37 / 15 | 48 / 20 |

- Before, every frame made about five passes over all marks (layout's
  inline virtual text, conceal and virtual lines; render's two
  `overlapping`, each sorting what it kept), and `get`, `set` and
  `remove` scanned by id, so deleting them one by one was quadratic.
  Marks are now indexed by id, position and kind
  (`crates/temzdf-texteditor/src/marks.rs`, `doc/design/text-editor.md`
  section 18): a frame costs what it shows, adding or deleting one costs
  the same with 32000 as with 2000. Tests count the marks looked at
  (`a_range_visits_only_the_marks_near_it`,
  `drawing_looks_only_at_the_marks_near_the_rows`).
- What stays linear: an edit (`follow` maps every mark and builds the
  position index again: 1.2 ms per keystroke with 32000 in release, a
  little slower than the plain scan in debug builds, where the
  `BTreeMap` costs more), clearing a namespace, and the
  `text.decorations` request without a range. Neovim's marktree keeps
  positions relative to the tree's nodes so an edit moves only a path;
  worth it only if typing in such a buffer is felt.
- Drawing a 60x20 text pane costs about 90 µs in release and 570 µs in
  debug (cloning the drawn cells 30 and 50 µs), whatever the marks, and
  the session draws every pane for every frame; `temzdf test` builds a
  frame after each batch of requests. So the server keeps each text
  pane's last frame with what it was drawn from (`TextFrameKey`, section
  18 of the design) and clones it while that is unchanged: the read
  column's debug numbers (0.4 to 0.19 ms with N lines) are the pane not
  drawn again. Tests draw every cached frame again and compare
  (`check_text_frames`), and so do debug builds with
  `TEMZDF_CHECK_FRAME_CACHE` set: `TEMZDF_CHECK_FRAME_CACHE=1 cargo xtask
  check` (script tests and the simulated terminal's) passes;
  `text_panes_are_drawn_again_only_when_what_they_show_changed` checks
  that each part of the key draws the pane again.
