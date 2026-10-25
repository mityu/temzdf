# macOS support

Status (2026-10-25): verified on an Apple Silicon Mac (macOS 26, M1) in kitty
0.49.2, Ghostty 1.3.1 and WezTerm 20260117. Everything in the checklist below
works; what is left is listed under "Open" and in `todo.md`.

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
- Auto-reload: notify uses FSEvents. Opening the PDF through a symlinked path
  (`/tmp` -> `/private/tmp`) still reloads, since events are matched by name.

## Checklist and results

1. Build: `nix develop -c cargo build --workspace` works. The temzdf build
   script prints a harmless cc-wrapper warning about `--target
   arm64-apple-macosx`.
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

## Open

- Opening an external URL (`open`) was not clicked in the test run.
- IME preedit placement on macOS.

## Checking GUI terminals on a Mac without a human

`scripts/e2e.sh` is Linux-only (Xvfb, xdotool). On macOS the same checks were
driven with small Swift helpers using public and private system APIs; they are
not in the repository yet (see `todo.md`). What worked:

- Screenshots: `screencapture -l <window id>` (needs the screen recording
  permission for the terminal Claude runs in). Window ids, owners and titles
  come from `CGWindowListCopyWindowInfo`; the title shows temzdf's page number.
- kitty redraws nothing while its window is covered, so a window behind others
  screenshots stale. A virtual display (`CGVirtualDisplay`, a private API also
  used by DeskPad) next to the real screen solves it: launch the terminal in the
  background (`open -g -na <App> --args ...`), move its window there with the
  accessibility API (`kAXPositionAttribute`), and it renders without focus.
- Keys: kitty takes `kitty @ send-text`/`send-key` over its remote-control
  socket (keep the socket path under 104 bytes). Ghostty accepts key events
  posted to its process (`CGEvent.postToPid`) even in the background, but only
  with real key codes; events that carry just a Unicode string are ignored.
  WezTerm ignores per-process events: activate it and post globally, checking
  first that it is the frontmost app so nothing reaches another window.
- Mouse: CGEvents at global coordinates go to the topmost window there, so the
  target must be active. A click on an inactive Ghostty window only focuses it.
  Wait for kinetic scrolling and WezTerm's lazy redraw (over 1.5 s while the
  window is not focused) before measuring a screenshot.
- `kitty @ send-key shift+g` sends `CSI 103;2u` without the shifted key, which
  real typing includes; use `send-text G`.
