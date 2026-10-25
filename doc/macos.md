# macOS support

Status (2026-10-25): prepared on Linux, not yet run on a Mac. Work on macOS
continues on the user's Mac; this page is the starting point there.

## Done on Linux

- Waiting on `/dev/tty` uses `select()` everywhere (`termpix::tty::wait_fd`):
  macOS's `poll()` reports `POLLNVAL` for terminal devices. The same code runs
  on Linux, so it is exercised by every local test and e2e run.
- `termpix` and `termpix-ui` (with tests) pass `cargo check` for
  `aarch64-apple-darwin` and `x86_64-apple-darwin` (cross-checked with a fenix
  toolchain). `temzdf` itself was not cross-checked: MuPDF's C build needs the
  macOS SDK.
- Platform switches already in place: `open` instead of `xdg-open` for links
  (`config.rs`), `ftruncate` instead of `fallocate` for shared memory
  (`graphics.rs`), the Linux-only shm test is `cfg`-gated.
- CI (`.github/workflows/ci.yml`) has a `macos-latest` `nix build` job; it has
  not run yet.

## To check on the Mac, in order

1. Build: `nix develop -c cargo build --workspace` (or `nix build`). Expect
   problems first in `mupdf-sys` (bindgen needs libclang: `LIBCLANG_PATH` is set
   by the dev shell), `libz-sys` and `font-kit`/`yeslogic-fontconfig-sys`
   (pulled in by mupdf's `system-fonts`; on macOS font-kit should use Core Text).
2. `scripts/check.sh`. Fixtures need `latexmk` (MacTeX, or `nix develop .#alldeps`).
3. Run the viewer in kitty, Ghostty and WezTerm on macOS:
   `cargo run -p temzdf -- target/fixtures/sample.pdf`. Check startup (probe),
   wheel scrolling, Ctrl+wheel zoom at the pointer, drag, selection and copy
   (OSC 52), search, links, the sidebar, `q`. `cargo run -p termpix --example probe`
   prints what detection found (shm support, cell size, pixel mouse).
4. Transports: shared memory (`t=s`) is the default where the probe says it
   works; macOS limits shm names to 31 characters (ours: `/termpix-<pid>-<n>`)
   and allows sizing a shm object once. Compare with `TERMPIX_MEDIUM=file` and
   `TERMPIX_MEDIUM=direct` if images do not appear.
5. Auto-reload: notify uses FSEvents on macOS. Rebuild a document with latexmk
   while it is open; events are filtered by file name, so `/private/var` vs
   `/var` path differences should not matter.
6. SyncTeX: `temzdf --forward LINE:FILE doc.pdf` against a running viewer. The
   socket lives under `$TMPDIR/temzdf-<uid>/temzdf/` (no `$XDG_RUNTIME_DIR` on
   macOS); the path must stay under 104 bytes (about 90 expected).
7. Ctrl+click inverse search with an editor command (doc/synctex.md).
8. Quitting mid-frame leaves the shell clean (no `Gi=...;OK`, mouse reporting off).

Record findings in `doc/terminal-compat.md` (per terminal) and here; move what
remains to `todo.md`.

## e2e on macOS

`scripts/e2e.sh` is Linux-only (Xvfb, xdotool, ImageMagick `import`). On macOS
check by hand, or render headless frames (`--render-frame`), which are
platform-independent.
