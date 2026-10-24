# Testing

Layers of verification, cheapest first. All of them run without a human.

| Layer | How | What it catches |
|---|---|---|
| Unit tests | `scripts/check.sh` | Input parsing, kitty command encoding, compositor diffing (replayed through `termpix::mock`), layout maths, tile rendering vs full-page rendering |
| Headless frames | `target/debug/temzdf FILE.pdf --render-frame out.png [--size 100x30] [--cell 10x20] [--page N] [--scale S]` | What a screen looks like: runs the real `Viewer` and compositor, replays the bytes through the mock terminal. Read the PNG to check it. |
| Real terminal | `scripts/e2e.sh [-t kitty\|ghostty\|wezterm] [-m zellij] ACTIONS -- COMMAND` | Terminal behaviour: probing, transports (shm, inline with `TERMPIX_MEDIUM=direct`), pixel mouse, actual placement semantics, terminal crashes (exit code 3). Screenshots land in `target/e2e/`. |
| User | Ghostty / WezTerm on a real desktop | The feel of scrolling and zooming. |

Action files live in `tests/e2e/`; each starts with a comment saying how to run it
and what the screenshots should show. To look at many screenshots at once:
`nix develop -c montage target/e2e/*.png -tile 2x -geometry 450x300+4+4 target/e2e/montage.png`.

## Fixtures

`tests/fixtures/*.tex` are the sources. `scripts/build-fixtures.sh` builds them
into `target/fixtures/` (PDF and SyncTeX) with a global TeX installation (or
the one in `nix develop .#alldeps`), reproducibly
(`SOURCE_DATE_EPOCH`), and skips fixtures newer than their source. Unit tests
call it on first use (`pdf::fixture`); run it yourself before e2e scenarios.
Generated files are never committed (see AGENTS.md).
