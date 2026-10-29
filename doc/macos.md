# macOS support

Status (2026-10-25): verified on an Apple Silicon Mac (macOS 26, M1) in kitty
0.49.2, Ghostty 1.3.1 and WezTerm 20260117. Everything in the checklist below
works.

## Platform specifics

- Waiting on `/dev/tty` uses `select()` everywhere (`termpix::tty::wait_fd`):
  macOS's `poll()` reports `POLLNVAL` for terminal devices.
- Shared memory: macOS rounds a shm object up to whole pages (16 KiB), so the
  mapping is longer than the image; we write only the image's bytes and `S=`
  gives the terminal the real length. Names are limited to 31 characters (ours:
  `/termpix-<pid>-<n>`), and an object can be sized only once (`ftruncate`, not
  `fallocate`).
- SyncTeX socket: there is no `$XDG_RUNTIME_DIR`, and `$TMPDIR` is not stable
  (`nix develop` sets a per-shell directory), so the socket lives in the
  per-user directory from `confstr(_CS_DARWIN_USER_TEMP_DIR)`:
  `/var/folders/<..>/T/temzdf/<hash>.sock`, 77 bytes (the limit is 104).
- UI font: `sans-serif` resolves to Helvetica, which lacks some symbols (▸, ▾)
  that font fallback does not find either. Draw such marks as shapes. CJK
  fallback works.
- Links open with `open` instead of `xdg-open` (`config.rs`).
- Terminal panes: on macOS libghostty-vt's key encoder takes Alt for an
  Option key that composes characters (Alt+x sent as `x`) unless
  `macos_option_as_alt` is set, and `set_options_from_terminal` resets it.
  Our key events' Alt is already Alt (the outer terminal or the GUI decided),
  so `crates/temzdf-term/src/keys.rs` sets it to true.
- Ptys: closing the last descriptor of a pty's slave side drops the output
  the master has not read yet (Linux keeps it), so tests keep one open.
- Miri: `confstr` returns nothing and the user id differs from the owner of
  the files Miri creates; the tests relying on them are ignored under Miri on
  macOS.
- Auto-reload: notify uses FSEvents. Opening the PDF through a symlinked path
  (`/tmp` -> `/private/tmp`) still reloads, since events are matched by name.
- GUI: while the window is not visible at all (fully covered, minimised, on
  another Space, or opened behind another window), wgpu's Metal backend
  checks `NSWindow.occlusionState` and hands out no surface texture
  (`Occluded`); winit reports the change as `WindowEvent::Occluded`. A redraw
  then draws nothing. Before the fix the frame stayed unacknowledged, so the
  session sent the window no more frames, and nothing redrew when it showed
  again: the window stayed stale, in front and focused too, until a resize.
  Traced on macOS 26 (2026-10-27): textures are always handed out while any
  part of the window is visible, also with a terminal client attached; it
  was the earlier covering that stopped it. Now frames are acknowledged
  undrawn while hidden (`Acks` in `crates/temzdf-gui/src/app.rs`),
  `Occluded(false)` redraws, and a `Timeout` (unlikely: wgpu disables
  `allowsNextDrawableTimeout`) retries. `tests/macos/gui-redraw.sh` checks
  it; it fails without the redraw on `Occluded(false)`.
- GUI font sizes: on macOS a point is a logical pixel (72 dpi, as in
  Ghostty and kitty), elsewhere points are at 96 dpi (`PIXELS_PER_POINT`
  in `crates/temzdf-gui/src/fonts.rs`). With 96 dpi everywhere, text came
  out 4/3 as large as Ghostty's at the same size (2026-10-28; not yet
  compared on a Mac).

## Checklist and results

1. Build: `nix develop -c cargo build --workspace` works. Nix's cc-wrapper
   warns about the `--target arm64-apple-macosx` the cc crate passes for the
   SyncTeX parser (`temzdf-pdfcore`'s build script); it is harmless and
   silenced in `.cargo/config.toml`.
2. `scripts/check.sh` passes (fixtures need `latexmk`: MacTeX, or `nix develop .#alldeps`).
3. Viewer in kitty, Ghostty and WezTerm: startup, wheel scrolling, Ctrl+wheel
   zoom at the pointer, drag (pan and selection), OSC 52 copy to the macOS
   clipboard, search, sidebar, internal links and history, `q`: all work.
   Detection picks Layered + shm for kitty and Ghostty, Composited + temp files
   for WezTerm (its shm is broken on macOS and the probe notices).
4. Transports: `TERMPIX_MEDIUM=file` and `direct` also work in kitty; kitty
   deletes the temp files.
5. Auto-reload after a latexmk rebuild keeps the position.
6. Forward search finds the viewer even when the editor side has a different
   `$TMPDIR` or spells the path through a symlink.
7. Ctrl+click inverse search runs the editor command with the right line.
8. Quitting mid-frame (direct and shm) leaves the shell clean in kitty,
   Ghostty and WezTerm.

Also confirmed by the user: external URLs open in the browser, and IME input
is fine for now.

## Checking GUI terminals on a Mac without a human

Two tools share the Swift helpers in `scripts/macos/` (built into
`target/macos-gui/`): `scripts/e2e.sh` runs the action files of
`tests/e2e/` with their judgements, as on Linux; `scripts/macos-gui.sh`
drives one terminal step by step, for looking into something by hand.
Both need the screen recording and accessibility permissions for the
terminal they run in, and the screen unlocked (a locked screen gives no
screenshots).

### scripts/e2e.sh -d macos

The default on macOS. The same command lines as on Linux (each action
file's "Used with:"), e.g.

```sh
scripts/e2e.sh -t ghostty tests/e2e/expect.actions -- target/debug/temzdf standalone pdf --clean target/fixtures/sample.pdf
```

It takes the front and the pointer while it runs (pointer events go to the
window under the pointer, so the terminal must be in front), and gives both
back at the end; do not use the Mac meanwhile. It keeps the display awake
(`caffeinate`) and puts the user's text clipboard back. How it works, and
why:

- A virtual display (`vdisp W H 1`) at scale 1, beside the real screen, a
  little larger than the window: screenshots of it are the terminal's
  pixels, so `expect` crops and compares them as on Xvfb, and action
  coordinates mean what they mean there. A menu bar shows on every display,
  so windows go below it; the drawing area's place comes from `termctl
  content` (the window without its title bar, whose height the close
  button's position gives).
- Terminals start through `open -g -n -a APP --args ...` (Ghostty cannot
  start otherwise, and none should come to the front), with launchd's
  environment and `/` as the directory: a script in the run's runtime
  directory sets this run's environment and directory (not the variables
  naming the harness's own terminal, which would override the new one's)
  and waits until the window is on the virtual display before starting the
  command, so temzdf starts at scale 1 (it would otherwise log the main
  screen's geometry first). Window sizes follow the macOS cell sizes
  (Ghostty 8x17 px, WezTerm 7x16 px), so the window is about the size asked
  for and the coordinates of the action files hit what they aim at. The GUI
  runs directly.
- Input: `termctl serve PID` reads one command per line (as `crates/wlinput`
  on Wayland): pointer moves, button presses, wheel notches (without the
  system's acceleration), XKB key names (`ctrl+b`, `shift+g`, `percent`),
  modifiers held for later clicks, typed text. Pointer events are posted
  globally with the terminal in front; keys go to the terminal's process
  (Ghostty, the GUI), globally (WezTerm ignores posted keys), or through
  kitty's remote control (kitty ignores both: its key and text commands).
  The terminal is brought to the front at the start, as it has the focus on
  Linux (unfocused, Ghostty draws a hollow cursor over images).
- Screenshots: `screencapture -R` of the virtual display. macOS draws a
  1 px border over the edge of every window, rounds its corners, and draws
  a title bar's separator over the first row below it; `expect` blacks those
  pixels out on both sides (`NAME.window-mask.png`), since Ghostty's, WezTerm's
  and the GUI's cells reach the edges.
- Links are not opened: `open` is replaced for the command by a script that
  appends what it would open to `OUTDIR/opened.txt` (a browser would open
  them on the desktop; `xdg-open` the same on Linux). A URL in `opened.txt`
  that the scenario did not click (only `links` clicks one) is a sign of
  wrong coordinates.
- What differs per terminal (kitty's wheel, Ghostty's colours and keys,
  WezTerm's title bar) is in `doc/terminal-compat.md`.

### scripts/macos-gui.sh

Its header lists the commands. A session:

```sh
scripts/macos-gui.sh display start
read -r pid status < <(scripts/macos-gui.sh launch kitty -- target/debug/temzdf standalone pdf --clean target/fixtures/sample.pdf)
scripts/macos-gui.sh key "$pid" G          # or: type "$pid" /needle
scripts/macos-gui.sh title "$pid"          # sample.pdf — 3 / 3
scripts/macos-gui.sh click "$pid" 60 72    # window points, as in the screenshot
scripts/macos-gui.sh shot "$pid" target/macos-gui/shot.png
scripts/macos-gui.sh close "$pid"
scripts/macos-gui.sh display stop
```

`launch app -- target/debug/temzdf-gui ...` runs the GUI client the same way
(started directly, its window moved to the virtual display, the front given
back); `tests/macos/gui-redraw.sh` is a whole scenario built on it.

How it works, and why:

- Windows live on a virtual display (`vdisp`, private `CGVirtualDisplay` API as
  used by DeskPad) next to the real screen. kitty redraws nothing while its
  window is covered, so a window behind others would screenshot stale; on the
  virtual display it renders without focus and without covering anything.
  Terminals start in the background (`open -g -n`) and are moved there with
  the accessibility API.
- Screenshots: `screencapture -l <window id>`, scaled to points. Window ids,
  bounds and titles come from `CGWindowListCopyWindowInfo`; the title shows
  temzdf's page number.
- Keys never go to whatever window is in front. kitty gets them through its
  remote-control socket (`kitty @ send-text`/`send-key`; `send-key shift+g`
  would lack the shifted key that real typing reports, so single characters go
  through `send-text`). Ghostty accepts key events posted to its process
  (`CGEvent.postToPid`) in the background, but only with real key codes, hence
  the US-layout map in `termctl`. kitty and WezTerm ignore such events; WezTerm
  is activated and gets global events, sent only while it is frontmost.
- Mouse events go to the window under the pointer, so mouse commands activate
  the terminal and give the front back afterwards. A click on an inactive
  window would only focus it.
- In a command-line tool `NSWorkspace.frontmostApplication` only updates while
  the run loop runs; `termctl` waits with the run loop for that reason.
- Wait for kinetic scrolling to stop, and give WezTerm time: it redraws lazily
  while its window is not focused (over 1.5 s).
