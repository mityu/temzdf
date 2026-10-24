# Kitty graphics protocol: what we rely on

Spec: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>. Commands are APC
sequences: `ESC _G <key=value,...> ; <payload> ESC \`.

## Transmission

| Key | Meaning |
|---|---|
| `a=t` / `a=T` / `a=p` | transmit only / transmit and place / place an existing image |
| `f=24` / `f=32` / `f=100` | RGB / RGBA / PNG. Raw formats need `s` (width) and `v` (height) in pixels. termpix sends fully opaque images as `f=24` (measured on the viewer e2e scenario: 29% fewer bytes uncompressed, about 1% with zlib, which already squeezes the constant alpha) |
| `t=d` | direct: base64 payload, chunked to <= 4096 bytes (multiple of 4 except the last), `m=1` on all but the last chunk |
| `t=f` / `t=t` | file path / temp file deleted by the terminal (must live in a known temp dir) |
| `t=s` | POSIX shared memory name; the terminal maps and unlinks it. Pass `S=` (size). Same host only |
| `o=z` | zlib-compressed payload (before base64) |
| `i=` / `I=` / `p=` | client image id / image number (terminal assigns id) / placement id |
| `q=1` / `q=2` | suppress OK replies / suppress OK and errors |
| `C=1` | do not move the cursor |

File and shm transmission only work when the terminal runs on the same host, so
probe them and fall back to `t=d`. Clean up shm objects that the terminal did not
consume.

## Placement

| Key | Meaning |
|---|---|
| `x,y,w,h` | source rectangle (pixels) inside the stored image |
| `c,r` | target size in cells (scales the source rect) |
| `X,Y` | pixel offset inside the first cell (must be smaller than a cell) |
| `z` | z-index. `z >= 0` above text; `z < 0` below text; `z < -1073741824` also below cells with a non-default background |

Re-placing with the same `i` + `p` moves/replaces the placement without flicker.
This is the basis of pixel-smooth scrolling: crop with `x,y,w,h`, put the crop's
top on a whole cell, use `Y` for the remainder. Keep the crop's pixel size and
`c,r` consistent or the terminal rescales.

## Deletion (`a=d`)

`d=a` all, `d=i` by id (+`p`), `d=z` by z-index, `d=r` id range, and more.
Lowercase keeps the image data; uppercase frees it.

## Replies and detection

- Detection: send `a=q` with a tiny image, then DA1 (`CSI c`). DA1 arriving
  without a `_G` reply first means unsupported. Some terminals only reply when
  `i=` (not `I=`) is used.
- `ENOENT` when placing means the terminal evicted the image (storage quota,
  about 320 MB per screen in kitty and Ghostty): re-upload it.
- Inline transfers can pile up on slow links. Use fences: after a frame, send
  `a=q,i=<n>,t=d;AAAA` and do not render a new frame while more than ~2 fences
  are unanswered (gpuix design).

## Unicode placeholders (`U=1`)

Virtual placement shown wherever cells containing U+10EEEE (with row/column
diacritics, image id in the foreground colour) are printed. Needed under tmux,
cell-granular only, at most ~297 rows/columns. Not used in the main path.

## Other sequences we use

- Synchronized output: `CSI ? 2026 h` ... `CSI ? 2026 l` around every frame.
- Cell size: `TIOCGWINSZ` (`ws_xpixel`/`ws_ypixel`, may be 0), `CSI 16 t`
  (cell size), `CSI 14 t` (window size). In-band resize reports: mode 2048.
- Clipboard: OSC 52. Hyperlinks: OSC 8. Pointer shape: OSC 22.
