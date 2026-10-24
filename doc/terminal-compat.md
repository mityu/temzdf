# Terminal compatibility

Snapshot of research done on 2026-10-25 from sources, changelogs and issues.
`?` means unverified. Update this file whenever behaviour is confirmed or
disproved in practice.

Versions at the time: kitty 0.49.2, Ghostty 1.3.1 (tip has 1.4 features),
WezTerm last stable 20240203 (nightlies active), iTerm2 3.7.6, tmux 3.8,
zellij 0.45.1.

## Kitty graphics

| Feature | kitty | Ghostty 1.3.1 | WezTerm (nightly) | Konsole | iTerm2 3.7 | foot |
|---|---|---|---|---|---|---|
| Basic `a=T/t/p`, ids | yes | yes | yes | yes | yes | no (sixel only) |
| `t=d` | yes | yes | yes | yes (only medium) | yes | – |
| `t=f` / `t=t` | yes | yes | yes / ? | no | yes | – |
| `t=s` shm | yes | yes | Linux yes, macOS broken (#7631) | no | yes | – |
| `o=z` | yes | yes | ? | yes | ? | – |
| Crop + `c,r` | yes | yes | glitchy (#5892) | yes | ? | – |
| z-index below text | yes | yes | **not respected** (#7222) | parsed | ? | – |
| Unicode placeholders | yes | yes | no (PR #7924 open) | no | yes | – |

Known problems:

- WezTerm: placement-move glitches (#5892), delete-by-placement fails after
  resize (#2422), aspect ratio (#5402), crash when repositioning with DECSTBM
  scrolling (#7953), no pixel size on WSL (#6781). ratatui-image and yazi avoid
  kitty graphics on WezTerm entirely and use the iTerm2 protocol. We plan the
  `Composited` backend for it.
- Ghostty: older placement/deletion bugs are closed; require >= 1.3.1. No `a=f`
  frame edits in 1.3.
- tmux: no native kitty graphics. Needs `allow-passthrough on` and DCS wrapping;
  direct placements bleed across panes. Only Unicode placeholders are robust.
  Replies are not routed back, so probes fall back to inline.
- zellij 0.45: native kitty graphics per pane, but rejects `U=1` (#5530), no
  `t=s` (#5626), ignores `x/y` without `c/r` (#5567). Use `t=d` and always pass
  `c,r`.

## Input and output modes

| Feature | kitty | Ghostty | WezTerm | foot | Konsole | iTerm2 |
|---|---|---|---|---|---|---|
| SGR-pixel mouse (1016) | yes | yes | yes | yes (>= 1.11) | master | ? |
| Kitty keyboard protocol | full | yes | yes (`enable_kitty_keyboard`, off by default ?) | yes | master | yes (flags ?) |
| Synchronized output (2026) | yes | yes | yes, but DECRQM reports "off": send it unconditionally | yes | master | yes |
| In-band resize (2048) | yes | yes | no | ? | ? | ? |

- Wheel input is discrete everywhere (buttons 64/65, 66/67 horizontal). No
  terminal forwards pixel deltas or pinch gestures to applications.
- kitty: with mouse tracking on, trackpad deltas are accumulated and emitted as
  one button event per step. Ghostty and Konsole behave similarly.
- Shift+click bypasses application mouse reporting in most terminals.
- tmux: 1016 passthrough unverified; fall back to cell coordinates.

## Verified in practice

- kitty 0.49.2 (Xvfb, `scripts/e2e.sh`): Layered backend; one tall image
  uploaded once and re-placed with `x,y,w,h` crops scrolls pixel-exactly
  (6 wheel steps = 360 px). SGR-pixel (1016) drag deltas are exact (100 px drag
  = 100 px scroll). kitty logs "Pending mode change to already current mode (0)"
  when we send `CSI ? 2026 l` on exit outside a synchronized update; harmless.
- Ghostty 1.3.1 (Linux under Xvfb, and macOS over SSH as reported by the user):
  **crashes with SIGSEGV when inflating zlib streams produced by zlib-rs at
  level 1** (its "deflate_quick" strategy) in `o=z` transmissions. The same
  pixels compressed by Python's zlib, by miniz_oxide, or by zlib-rs at levels
  2, 3 or 6 are fine. termpix therefore compresses at level 2
  (`crates/termpix/src/graphics.rs`). Reproduce with
  `TERMPIX_MEDIUM=direct scripts/e2e.sh -t ghostty tests/e2e/viewer-stress.actions -- target/debug/temzdf target/fixtures/sample.pdf`
  after switching the level back to 1. Not yet reported upstream.
- Ghostty 1.3.1 with kitty keyboard flags 31 reports Shift+letter as the base
  key plus Shift (e.g. `CSI 103;2u` style with alternates); termpix maps it to the
  produced character (`G`) using the alternate key or associated text.
- Environment leaks: a terminal started from inside zellij or tmux inherits
  `ZELLIJ`/`TMUX`, which makes detection think it runs inside a multiplexer.
  `scripts/e2e.sh` clears them; real users may see this when launching a GUI
  terminal from a multiplexer shell.

## Multiplexers (research 2026-10-25, from zellij main ffb334a and tmux master acee541 sources)

### zellij (latest release 0.45.1; kitty graphics since 0.45.0)

zellij does **not** pass graphics through: it decodes images, keeps RGBA per pane,
and re-emits its own placements to the host (`a=t,f=32,t=d` once per image, then
`a=p` with crops and the app's `z`). It answers the app's `a=q` and commands
itself; host replies never reach the app. Config: `support_kitty_graphics_protocol`
(default true; enabled only if the host answers zellij's own probe).

| Item | 0.45.1 | main |
|---|---|---|
| `t=d/f/t`, `o=z`, `f=24/32/100`, chunking | yes | yes |
| `t=s` shm | ENOTSUPPORTED (#5626) | same |
| Source rect `x,y` without `c,r` | ignored (#5567) | still broken (fix PR #5568 open) |
| Explicit placement ids `p=` with `a=p` | broken (#5573) | fixed |
| Unicode placeholders `U=1` | ENOTSUPPORTED (#5530) | same |
| Forwarding to the host | raw RGBA, ~10x bytes (#5596) | compressed (PR #5608) |
| SGR-pixel mouse 1016 | not supported (cells only) | same |
| Kitty keyboard | flag 1 only | same |
| XTVERSION | `Zellij(<ver>)` | same |
| DA1 | `?62;52c` | same |

Likely cause of "nothing displayed" in the user's test (0.45.1 over SSH):
Layered placements hit #5573/#5567, and everything is forwarded uncompressed.

### tmux

- 3.8 (2026-09-09): no kitty graphics; only passthrough (`allow-passthrough on`,
  DCS `ESC P tmux; <ESC doubled> ESC \`). Passthrough images ignore panes/scroll,
  and outer-terminal replies do not reach the app.
- master (after 2026-10-16, build with `--enable-images`): native kitty input
  path. Only `t=d`; `x,y,w,h`, `c,r`, `X,Y` work; **`z` must be 0**; delete only
  `a/A/i/I`; `U=1` and `I=` unsupported; tmux answers queries itself.
- No SGR-pixel mouse and no kitty keyboard protocol in tmux (use
  `extended-keys` with `csi-u`).
- Unicode placeholders through passthrough (as yazi, ratatui-image do) are the
  robust path on released tmux; cell-granular only.

### Plan

Detect `Zellij(`/`tmux` from XTVERSION (plus `$ZELLIJ`/`$TMUX`) and use a
multiplexer profile: no 1016 (cell mouse), reduced keyboard flags, `t=d` only,
probe the features we rely on (`p=`, crops, `z<0`) instead of assuming them.
zellij 0.45.1: Composited full frames; zellij main: Layered with `c,r` on every
crop. tmux 3.8: Unicode placeholders via passthrough; tmux master: native with
`z=0` (Composited, or Layered without z-layering).
- kitty 0.49.2: OSC 52 clipboard writes work (verified with `xclip` in
  `tests/e2e/select.actions`); OSC 22 pointer shapes are accepted.
- SGR-pixel (1016) coordinates are zero-based in kitty 0.49.2 and Ghostty 1.3.1
  (a click at window pixel 300 reports 300), unlike SGR cell coordinates, which
  are one-based. Check with `cargo run -p termpix --example events -- log`.
- WezTerm 0-unstable-2026-10-10 (nightly, software renderer on Xvfb): the
  Composited backend works for every scenario in `tests/e2e/` (scrolling, zoom,
  pan, search highlights, selection + OSC 52, links). Detection reports shm and
  temp files as usable; the kitty keyboard protocol is off by default
  (`keyboard_flags: None`). The Layered backend (`TERMPIX_BACKEND=layered`) is
  unusable: the screen turns into a grid of fragments and stops updating, which
  confirms choosing Composited for WezTerm. Harness notes: WezTerm needs its
  window focused (no window manager on Xvfb) to report mouse events, looks the
  program up only in `PATH`, and pads the text area by default; `scripts/e2e.sh -t
  wezterm` handles all three.
- zellij 0.45.1 inside kitty 0.49.2 (`scripts/e2e.sh -m zellij`): with the
  Composited backend (chosen from `$ZELLIJ` or the `Zellij(...)` XTVERSION
  reply) every scenario works: scrolling, pan, search, selection and copy,
  links. Detection inside zellij: no shm, temp files work, no SGR-pixel mouse
  (cell coordinates), keyboard flags 1. zellij keeps some input for itself:
  Ctrl+wheel never reaches the app (zoom with `+`/`-` instead), and Ctrl-o and
  Alt+arrows are zellij bindings (temzdf also goes back with Backspace).
- zellij 0.45.1 over SSH (user report, 2026-10-25): the first page took minutes to
  appear. Cause: every Composited frame was a full-screen image, the first screen
  was sent several times as tiles arrived, and zellij forwards each image
  uncompressed (#5596); fences do not help because zellij answers them itself.
  Now only changed strips are sent, the first frame waits for the visible tiles,
  and Composited frames are rate-limited; startup inside zellij sends exactly one
  screen of strips (measured: 15 strips of 900x40 at 900x600).
- WezTerm nightly garbles Composited output sent as several strip images (black
  gaps, strips from older frames left behind), while one whole-screen image per
  frame works. Probe therefore picks `Backend::composited()` (whole frame) for
  WezTerm and `Backend::composited_strips(2)` for zellij.
- tmux 3.8 inside kitty 0.49.2 (`scripts/e2e.sh -m tmux`, passthrough on, mouse
  on): works with the Placeholders backend. Frames are composited as strips
  and sent as `U=1` virtual images through passthrough (`ESC P tmux; ...`), and
  the screen is filled with placeholder text once, which tmux keeps and redraws
  like any text. Scrolling stays pixel-smooth because pixels are composited by
  us; only the text-to-image mapping is per cell. Every scenario passes,
  including Ctrl+wheel zoom (tmux forwards it). Notes:
  - tmux never returns the outer terminal's graphics replies, so detection asks
    tmux instead (`show-options allow-passthrough`, `display -p
    '#{client_termtype}'`/`'#{client_termname}'`) and fences are off.
  - OSC 52 from applications is dropped with tmux's default
    `set-clipboard external`; temzdf sends it through passthrough.
  - SGR-pixel mouse and the kitty keyboard protocol do not exist in tmux: cell
    mouse coordinates and legacy keys.
  - The outer terminal must support Unicode placeholders (kitty, Ghostty; not
    WezTerm).
