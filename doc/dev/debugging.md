# Debugging terminal problems

Tools for finding out what a terminal does and where temzdf goes wrong.
What you learn about terminal behaviour goes into
`doc/terminals/terminal-compat.md`.

- `cargo run -p termpix --example probe` prints what detection found.
- `cargo run -p termpix --example events -- events.log` logs every parsed input
  event (what the terminal really sends).
- `SCROLL_DEMO_LOG=FILE` makes the `scroll_demo` example write each event it
  receives and each frame it draws with the time (Unix ms), for telling a
  terminal's late input from a slow loop.
- In interactive mode `temzdf` sends stdout/stderr (MuPDF and SyncTeX warnings)
  to `temzdf.log`, and session servers theirs to `NAME.log`, in the log
  directory: `$TEMZDF_LOG_DIR`, else `$XDG_STATE_HOME/temzdf`. `temzdf ls -v`
  shows each session's log file.
- `TERMPIX_TRACE=out.bin <app>` records every byte written to the terminal;
  replay prefixes of it into a terminal (`cat out.bin`) to bisect crashes.
- `TERMPIX_BACKEND=layered|composited` and `TERMPIX_MEDIUM=direct|shm|file`
  override detection (e.g. `direct` mimics SSH); `TERMPIX_GRAPHICS=off` runs
  as in a terminal without the kitty graphics protocol (text only).
- `scripts/e2e.sh -t ghostty ...` runs the same scenarios in Ghostty; it exits
  with 3 when the terminal crashed (`doc/dev/testing.md`).
