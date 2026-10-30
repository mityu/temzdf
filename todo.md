# TODO

Long-running and deferred work. Milestones M0–M6 are the current plan; the items
below them are deferred.

## Milestones

- [x] M0: scaffolding (flake, workspace, docs, this file)
- [x] M1: `termpix` (tty, probe, input, graphics, compositor Layered/Composited, scroll, mock, `scroll_demo` example)
- [x] M2: minimal viewer (open PDF, continuous layout, tile cache, smooth scroll, zoom, page navigation); the window title stands in for the status bar
- [x] M3: `termpix-ui` and viewer features (status bar, outline sidebar, search, links + history, text selection + OSC 52 copy, config file)
- [x] M4: auto-reload (see `doc/reload.md`)
- [x] M5: SyncTeX (vendored parser, IPC socket, forward/inverse search). SyncTeX anchoring on reload was dropped, see `doc/reload.md`
- [x] M6: compatibility (Composited backend verified on WezTerm and in zellij; zellij/tmux recognised from XTVERSION; inline frames fenced for slow links)

## Session server (`doc/session-server.md`)

Steps 1-3 are done (PDF and terminal panes, standalone viewer and session
server on one `Session`). Next and open:

- [ ] UI chrome as cell grids in the scene instead of pixels.
- [ ] Per-client pixel density (every client now gets frames for the latest
  client's geometry and cell size, programs' images included) and zoom
  stored relative to the pane width.
- [ ] Shared memory for local clients' pixels: done (`doc/session-server.md`,
  "Pixels"; `TEMZDF_PIXELS=socket` for the socket, which no longer copies
  pixels through the encoder either). Left: why the GUI's
  uploads at scale 2 got slower on lavapipe (`doc/performance.md`, "With
  shared memory"); measure on a real GPU and on macOS.
- [ ] A Vim-like command line on the `Action` text form (tab pages are done).
- [ ] The PDF pane's search prompt (`/`), removed with the move to pane
  status lines (user, 2026-10-31): write it again, on the status line, and
  with it its tests (incremental search while typing, the cursor in the
  prompt for IME, Enter before results), the `/` mapping, and the e2e
  searches (`tests/e2e/search.actions`: typing, next, cleared;
  `expect.actions`' `search` and `next` before `sidebar`).
- [ ] A log level (or verbosity) for temzdf's logs: today some lines are
  there for scripts/e2e.sh (`complete frame written`, `incomplete frame
  shown`, geometry) and others only with variables (`TEMZDF_TIMING`); the
  frame lines could go below the default level (user, 2026-10-30).
- [ ] User report (2026-10-26, deferred): after resizing the terminal window
  with split panes, the bottom-right pane's status bar (file name) stayed where
  it was. Not reproduced in kitty or WezTerm (standalone and session) nor
  through the mock terminal (`resizing_split_panes_redraws_like_a_fresh_start`).
  Suspect: the cell size derived from the kernel's pixel size after a resize
  (`Winsize::geometry` divides by rows/cols; padding would skew it, worst at the
  bottom). Needs the terminal, mode and the `resized to geometry` log line.
- [ ] Terminal panes, what is left: kitty graphics through Unicode
  placeholders (tmux inside a pane) and animation frames.
- [ ] Scripting and configuration (JavaScript/TypeScript in a script-host
  process; `doc/scripting.md`, `doc/script-host.md`, `doc/config.md`): the
  host, modes and mappings, options, hooks, buffers, floating panes and tab
  pages are implemented, and every question in `doc/scripting.md` is
  decided. Decided but not built: `temzdf eval` (27 there) and the
  escape-hatch menu on non-focusable panes (13). Next: default mappings for
  switching buffers; status and title bars on panes and the window
  (direction in `doc/floating-panes.md`).
- [ ] Step 4, the GUI client (`temzdf-gui`, `doc/gui-client.md`): works on
  X11 (checked on Xvfb with xfwm4, Japanese input through fcitx5 and Mozc).
  Left: Wayland and macOS checks, HiDPI and fractional scales, Option as
  Alt, cursor blinking, underline colours (SGR 58, not in the scene's
  cells yet). Step 5: web panes (headless Chromium).
  (Step 6, programmable configuration, is the script host.)

## Deferred

- zellij delays keyboard input (even its own prefix commands) while it
  processes a frame, so `q` waits there; sending less data per frame is the
  only lever we have (user agreed to defer, 2026-10-25).
- Multiplexers: zellij (Composited strips) and tmux 3.8 (Unicode placeholders
  through passthrough) work; see `doc/terminal-compat.md`. Left: zellij over SSH
  is still slow (zellij 0.45.1 forwards images uncompressed; a half-resolution
  mode could help), and tmux master's native kitty support (z=0 only) is untested.
- e2e beyond X11 (Wayland, macOS, maybe Windows) without rewriting the
  action files (discussed 2026-10-29). Done: 1. action files use helpers
  only, the display server behind `be_*` functions in `scripts/e2e.sh`;
  2. `-d wayland` on headless sway with `crates/wlinput` (wtype and
  wlrctl did not do: a virtual device per call), kitty and the GUI
  (`doc/testing.md`, "X11, Wayland and macOS"), every terminal and the
  GUI, `-I` (fcitx5) too; 3. `-d macos` (`termctl serve` and a virtual
  display at scale 1; `doc/macos.md`), every terminal and the GUI. Left:
  kitty's `viewer` zoom on macOS (three wheel reports per notch make it
  deeper than its pause allows), and:
  4. Windows: no bash, so then one small Rust driver for every platform
     (`crates/wlinput` is its Wayland part), `enigo` (input; Wayland and
     libei still experimental behind feature flags) with `xcap` (screen and
     window capture on X11, Wayland, macOS, Windows), and the action files
     as one action per line read by a Rust runner.
  Considered and set aside: VNC as the one layer (vncdotool; Xvnc, wayvnc,
  macOS screen sharing, a Windows server; tends to need a logged-in
  desktop on macOS and Windows), PyAutoGUI and nut.js (no Wayland), openQA
  and Appium (whole VMs or accessibility APIs, a poor fit for pixel-drawn
  terminals). The `expect` comparisons themselves are platform-neutral.
- Vector-heavy pages: MuPDF's anti-aliased rasteriser is slow there and
  `vello_cpu` about 20x faster (`doc/gpu.md`; GPU rendering does not pay off).
  Measure real TikZ/pgfplots documents first; then maybe render such pages
  through a MuPDF-device-to-`vello_cpu` translation.
- Two-page (spread) layout.
- Markdown viewer: a web pane (step 5 above) showing the rendered document
  (`doc/markdown-viewer.md`).
- Showing PDF annotations.
- Check crate names (`termpix`, `termpix-ui`, `temzdf`) for collisions before publishing.
- Judged e2e (`expect` in `scripts/e2e.sh`, `doc/testing.md`) covers keys and
  mouse input in kitty, WezTerm and Ghostty (keys also in tmux and zellij);
  sessions are judged against `temzdf render-session` (the text too in kitty
  and WezTerm; all pass in both, 2026-10-29), reload against fresh renders,
  SyncTeX with `check`, the GUI's focus and Japanese input with `check`
  on what the shell wrote, the screens after detaching and killing
  against the terminal's own drawing of the messages, the text
  `select-autoscroll` copies. Left unjudged: the truncated moment of
  `reload`, SyncTeX's highlight and the page it lands on, IME composing,
  how far `select-autoscroll` scrolls (it follows the frames drawn, which
  a replay does not; judging it needs autoscroll driven by time alone),
  sessions inside multiplexers, and Ghostty's text.
- MuPDF's allocation lock makes text rendering slower with more threads (see
  `doc/performance.md`). `fz_malloc`/`fz_free` take `FZ_LOCK_ALLOC` around the
  allocator itself, and `mupdf-sys` backs every context with the same static
  mutexes, so neither a custom allocator nor separate base contexts help. Only
  matters if rasterising ever becomes the bottleneck; then: patch the
  `mupdf-sys` lock wrapper, or render text-heavy tiles on fewer threads.
- Measure frame pacing over a real slow SSH link (fences and the writer thread
  are in place but were only tested locally, with a throttling pty proxy).
- The page thumbnail is misplaced by up to half a cell with the Layered
  backend (terminals scale images to whole cells). It only shows briefly; the
  `StandIn::Composed` path could compose it too.
- Long text input through `$EDITOR` (do this last): run it in a floating
  terminal pane and read the file when it exits (terminal panes and floats
  exist now). Short inputs (search, page number, commands) use the built-in
  one-line input.
