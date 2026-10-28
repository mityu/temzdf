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
- A resize comes twice where in-band resize (2048) is on: SIGWINCH (the
  kernel's `winsize`) and the 2048 report, in no fixed order. Their pixel
  sizes may differ (a terminal counting its padding in one of them; not
  measured in any terminal yet), so once a 2048 report came, the frontend
  takes only the grid from the kernel and the cell size from the report
  (`resized` in `crates/temzdf-tty/src/frontend.rs`).
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
  `TERMPIX_MEDIUM=direct scripts/e2e.sh -t ghostty tests/e2e/viewer-stress.actions -- target/debug/temzdf standalone pdf --clean target/fixtures/sample.pdf`
  after switching the level back to 1. Not yet reported upstream.
- Ghostty 1.3.1 (Xvfb, 2026-10-28) blends translucent images in linear
  light, as kitty does (a search highlight over the page matched
  `--render-frame --linear-blending`, not the sRGB blend). Started with
  `--window-decoration=false` and zero padding, its terminal area starts at
  the window's top left, so `scripts/e2e.sh`'s `expect` works there.
- WezTerm 20260117 (software renderer, Xvfb, 2026-10-28): SGR-pixel
  coordinates are one-based (`xdotool mousemove 450 300` arrives as 451,301;
  kitty reports 450,300), and input arrives in bursts while it renders:
  wheel steps sent 30 ms apart came 78, 1, 24 and 91 ms apart. So
  `scripts/e2e.sh` replays what temzdf recorded (`TEMZDF_RECORD_INPUT`),
  not what it sent.
- Ghostty (reported by the user, 2026-10-27, more often over SSH) reports
  SGR-pixel motion past the window's left or top edge with negative
  coordinates (`CSI <35;-3;251M`). The `-` is not a CSI parameter byte
  (0x30-0x3f) but an intermediate one, so a parser following ECMA-48 to the
  letter breaks the report and its rest arrives as typed text; termpix
  takes `-` as part of the parameters after `CSI <`.
- Ghostty 1.3.1 with kitty keyboard flags 31 reports Shift+letter as the base
  key plus Shift (e.g. `CSI 103;2u` style with alternates); termpix maps it to the
  produced character (`G`) using the alternate key or associated text.
- kitty 0.49.2 with the kitty keyboard protocol reports the Linefeed keysym
  (what `xdotool type` sends for a newline) as code point 10, i.e.
  `KeyCode::Char('\n')`, not Enter. Terminal panes map control code points
  to their keys (`crates/temzdf-term/src/keys.rs`).
- Ghostty 1.3.1 (GTK, on Xvfb) ignores that Linefeed keysym altogether: a
  command typed with `xdotool type "...\n"` never ran
  (`tests/e2e/session.actions` reattached nothing). `scripts/e2e.sh`'s
  `type` sends each newline as the Return key instead (2026-10-29).
- In a terminal pane's pty `C-s` is XOFF while the program leaves flow control
  on (`stty ixon`, the default; interactive shells' line editors usually turn
  it off), as in any terminal: sending `C-s` then pauses the pane's output
  until `C-q`.
- Environment leaks: a terminal started from inside zellij or tmux inherits
  `ZELLIJ`/`TMUX`, which makes detection think it runs inside a multiplexer.
  `scripts/e2e.sh` clears them; real users may see this when launching a GUI
  terminal from a multiplexer shell.
- macOS 26 on Apple Silicon (2026-10-25, see `doc/macos.md`):
  - kitty 0.49.2: shm, temp files and direct all work (Layered over shm by
    default); OSC 52 reaches the macOS clipboard. A window covered by other
    windows is not redrawn at all.
  - Ghostty 1.3.1: shm works and the temp-file probe fails (cause not
    investigated), so Layered over shm. Keeps the window title temzdf set after exit, as on Linux.
  - WezTerm 20260117: the probe finds shm unusable (#7631) and picks
    Composited over temp files, which works. At startup it reports 8x16 px
    cells for an instant before the window settles; the resize that follows
    corrects it. Redraws lazily while its window is not focused. Synthesized
    Shift+G arrived as `CSI 103;;71u` (no Shift, text G); termpix now takes
    the associated text whenever only Shift (or nothing) is held.
  - SGR-pixel coordinates are exact in all three (WezTerm's origin is below
    its tab bar).

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
- zellij over SSH (user report, 2026-10-25): `q` did nothing until the screen
  had finished drawing. Writes to `/dev/tty` block while the terminal side
  reads slowly, and the main loop did them itself, so input waited behind each
  frame. Output now goes through a writer thread (`termpix::output`); exiting
  drops what is still queued at a 16 KiB chunk boundary and terminates a cut
  APC/DCS before the reset. Reproduced locally with a pty proxy relaying output
  at 100 KB/s (uncompressed frames, `zlib_level = 0`): `q` took over 15 s before,
  0.12 s after, and the shell came back clean. zellij may still hold input
  back while it is busy itself; that part is out of our hands. Confirmed by the
  user: Ghostty and WezTerm over SSH now quit at once, through zellij `q` still
  waits, and so do zellij's own prefix commands while a frame is drawn.
- Ghostty and WezTerm over SSH (user report, 2026-10-25): after quitting
  mid-frame the shell showed `Gi=8323307;OK`, the terminal's reply to a
  graphics fence that arrived after exit (link latency). Exiting now stops the
  input thread, sends DA1 (`CSI c`) after the reset and swallows input until
  its reply (at most 2 s), then flushes input. The writer uses its own
  non-blocking `/dev/tty` handle, so a stalled write can no longer keep the
  reset back (previously it gave up after 0.5 s and left mouse reporting on).
  Reproduced locally with a pty proxy adding 150 ms each way.
- Ghostty keeps the window title temzdf set after exit; the title is saved and
  restored with XTWINOPS 22/23, which Ghostty seems to ignore.
- kitty 0.49 blends translucent images in linear light: the current search
  hit colour (255,120,0, alpha 150) over white shows as (255,191,172), not the
  sRGB source-over (255,176,105). Scaled images are filtered against a
  transparent border, darkening half a source pixel along each edge, so a 1x1
  image stretched over the screen faded to black over half the screen width;
  the background is now a screen-sized image. (Found by `expect` in e2e.)
- WezTerm (nixpkgs build) uses 10x22 px cells at its default font, so
  `scripts/e2e.sh` sizes its window as height/22 rows.
- WezTerm nightly garbles Composited output sent as several strip images (black
  gaps, strips from older frames left behind), while one whole-screen image per
  frame works. Probe therefore picks `Backend::composited()` (whole frame) for
  WezTerm and `Backend::composited_strips(2)` for zellij.
- WezTerm on Linux (user's machine, 2026-10-26): both the shm and the
  temp-file probes fail, so Composited over direct transfers, which the
  frontend took for a slow link (frames capped at one per 200 ms). The cap
  held text too: keys typed into a shell echoed up to 200 ms late, and up to
  500 ms while a PDF beside it scrolled (user reports). Now direct transfers
  count as slow only over SSH (`SSH_CONNECTION`, `SSH_CLIENT`, `SSH_TTY`;
  33 ms locally), and a frame whose images wait for the cap still shows its
  text at once (and is acknowledged, so the next one comes).
- In the e2e harness (Xvfb) WezTerm's shm probe succeeds, unlike on the
  user's desktop; force `TERMPIX_MEDIUM=direct` to reproduce the latter.
- WezTerm draws images over text whatever their z (#7222), Composited
  frames included: text the session draws over images (floating messages,
  a floating pane's title on its frame) was hidden. The session now cuts
  the images out under such text (`Session::covers`); kitty and Ghostty
  look the same either way (cells with a background hide images below
  text there). Seen in `tests/e2e/floats.actions` (`notice.png`).
- WezTerm 0-unstable-2026-10-10 and wide characters under images (user
  report, 2026-10-29; reproduced on Xvfb with `printf` and kitty graphics
  alone): placing an image (`a=T` or `a=p`) over cells drops the wide
  characters in them, while ASCII stays; the image is drawn twice over a
  cell that held a wide character (in both its halves); and writing text
  into a cell drops the image there. Opening a float sends the screen's
  Composited image again, so every wide character on the screen went
  (the poem left of the float, the lines above it), and a second strip of
  the float's border showed where its frame met wide characters. kitty
  keeps them all. Now the compositor tells the text writer which cells a
  frame placed images on (`Compositor::present_with`): our text that goes
  away is blanked before the images (`TextScreen::clear_gone`), and the
  wide characters under new placements are written again after them
  (`TextScreen::redraw_wide`). `tests/e2e/float-wide.actions`.
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
- Session prefix: `C-s` (until 2026-10-26) reached temzdf in kitty, WezTerm
  and tmux (raw mode clears IXON, so it is no flow control) but not zellij,
  which keeps `C-s` for its scroll mode. The default is now `<C-b>`, which is
  tmux's own prefix: inside tmux map another key (or press it twice).
