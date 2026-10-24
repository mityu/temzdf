# Testing

Layers of verification, cheapest first. All of them run without a human.

| Layer | How | What it catches |
|---|---|---|
| Unit tests | `scripts/check.sh` | Input parsing, kitty command encoding, compositor diffing (replayed through `termpix::mock`), layout maths, tile rendering vs full-page rendering |
| Headless frames | `target/debug/temzdf FILE.pdf --render-frame out.png [--size 100x30] [--cell 10x20] [--page N] [--scale S]` | What a screen looks like: runs the real `Viewer` and compositor, replays the bytes through the mock terminal. Read the PNG to check it. |
| Real terminal | `scripts/e2e.sh [-t kitty\|ghostty] ACTIONS -- COMMAND` | Terminal behaviour: probing, transports (shm, inline with `TERMPIX_MEDIUM=direct`), pixel mouse, actual placement semantics, terminal crashes (exit code 3). Screenshots land in `target/e2e/`. |
| User | Ghostty / WezTerm on a real desktop | The feel of scrolling and zooming. |

Action files live in `tests/e2e/`; each starts with a comment saying how to run it
and what the screenshots should show. To look at many screenshots at once:
`nix develop -c montage target/e2e/*.png -tile 2x -geometry 450x300+4+4 target/e2e/montage.png`.

## Fixtures

`tests/fixtures/*.tex` are built into PDFs and SyncTeX files with
`scripts/build-fixtures.sh` (host TeX). Generated files are committed. The
script rewrites the fixture directory in SyncTeX `Input:` lines to `./` so no
machine-specific path is committed.
