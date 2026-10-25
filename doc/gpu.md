# GPU rasterisation experiment

Question (2026-10-25): would rendering pages on the GPU make temzdf faster?

Short answer: no. A GPU renderer has to read every frame back into CPU memory
to send it to the terminal, and the readback alone costs more than MuPDF needs
for a whole text page. The experiment did turn up something worth pursuing on
the CPU: MuPDF's anti-aliased rasteriser is very slow on vector-heavy pages,
where `vello_cpu` is about 20x faster single-threaded.

## Method

`experiments/gpu-raster/` (a standalone crate, not in the workspace) records a
page's display list through a custom MuPDF device (`mupdf::NativeDevice`) as
fills, strokes and glyph outlines, then renders the recording with:

- MuPDF's draw device (what temzdf uses), as the reference;
- `vello_cpu` 0.3 (CPU, sparse strips), 1 and 28 threads;
- `vello` 0.11 (GPU compute via wgpu), plus readback;
- `vello_hybrid` 0.2 (strips prepared on the CPU, rasterised on the GPU), plus readback.

```sh
cd experiments/gpu-raster
nix develop ../.. -c cargo build --release
# wgpu needs the Vulkan loader on NixOS:
LD_LIBRARY_PATH=$(nix build --no-link --print-out-paths nixpkgs#vulkan-loader)/lib:/run/opengl-driver/lib \
  target/release/gpu-raster FILE.pdf [PAGE] [WIDTH_PX] [OUT_DIR]
nix develop ../.. -c python3 vector-pdf.py vector.pdf   # the vector-heavy test page
```

It prints times (best of 5, GPU medians of 10 after a warm-up frame) and the
difference from MuPDF's pixels; with OUT_DIR it saves each renderer's PNG.
Clips, images, shadings, dashes, caps/joins and blend groups are not recorded,
so only pages without them compare fairly (all three below).

## Results

Whole page at 1800 px wide (2546 px tall), Intel i7-14700 (28 threads),
NVIDIA GeForce GT 1030 (a low-end GPU) over Vulkan:

| Page | MuPDF | vello_cpu 1 / 28 threads | vello (GPU) | vello_hybrid CPU + GPU | GPU readback |
|---|---:|---:|---:|---:|---:|
| Text (`sample.pdf` p. 1) | 1.7 ms | 4.6 / 2.9 ms | 5.8 ms | 1.1 + 2.6 ms | 9.6 ms |
| Denser text (fixture repeated, p. 4) | 2.1 ms | 8.7 / 3.8 ms | 7.4 ms | 5.7 + 3.3 ms | 9.6 ms |
| 5000 stroked curves + 3000 filled discs | 4320 ms | 220 / 35 ms | 10.9 ms, **blank** | 167 + 75 ms | 8.9 ms |

- Pixels agree with MuPDF (mean difference 0.4 on text, about 2-5 on thin
  strokes) except `vello` on the vector page, which rendered nothing: its GPU
  buffers have a fixed size and a too-complex scene silently fails.
- Readback of an 18 MB frame takes about 9.6 ms on this GPU (~1.9 GB/s), more
  than MuPDF's whole text page. First frames cost 20-40 ms (pipeline setup).
- MuPDF's time on the vector page is anti-aliasing: `MUPDF_AA=8,4,2,0` gives
  4320, 3388, 3283 and 254 ms. With AA off it is fast but jagged.
- In temzdf's tiled pipeline (4 threads) the vector page takes about 1 s
  (197 ms per 512 px tile single-threaded: every curve crosses every tile).

## Conclusions

- GPU rendering does not pay off here: pixels must reach the terminal through
  CPU memory, the readback alone beats MuPDF's text rendering, `vello` fails on
  heavy scenes and `vello_hybrid` is slower than `vello_cpu` with threads. A
  faster GPU shrinks render times, not the readback or the terminal transfer,
  which already dominate (see `doc/performance.md`).
- MuPDF stays the right default for text: its glyph cache makes text pages
  faster than any vello variant.
- For vector-heavy pages (TikZ/pgfplots figures, plots with many paths),
  rendering MuPDF's display list through `vello_cpu` could be one to two orders
  of magnitude faster. Doing that for real needs a complete device translation
  (clip stack as layers, images, shadings as gradients, dashes, caps and joins,
  blend modes and groups, text render modes) and a rule for when to use it
  (e.g. the number of path commands in a page's display list).

## Next steps (not started)

- Measure real documents with TikZ/pgfplots figures (the test page is a worst
  case: each curve spans the page) before investing in the `vello_cpu` path;
  `texliveBasic` lacks pgf, so add it to the `alldeps` TeX or use the user's documents.
- If it pays off: implement the device translation behind an experimental
  config switch, compare pixels with MuPDF on a corpus, fall back to MuPDF for
  anything unsupported.
