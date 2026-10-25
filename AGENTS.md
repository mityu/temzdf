# AGENTS.md

Notes for coding agents (and humans) maintaining this repository.

## What this is

`temzdf` is a terminal PDF viewer aimed at previewing LaTeX documents. It draws
everything as pixels with the kitty graphics protocol (pages *and* UI chrome) and
aims for GUI-grade mouse interaction (pixel-smooth scrolling, drag, selection).
Key features: outline sidebar, in-document search, links, SyncTeX forward/inverse
search, and auto-reload that keeps the reading position.

The terminal plumbing is a separate, reusable library so that other viewers
(e.g. a future markdown viewer rendering a webview offscreen) can share it.

## Layout

| Path | Purpose |
|---|---|
| `crates/termpix` | Library. Terminal I/O: tty setup, capability probing, input parsing (SGR-pixel mouse, kitty keyboard), kitty graphics transport, layered compositor, kinetic scroll helper, mock terminal for tests. **Knows nothing about PDF.** |
| `crates/termpix-ui` | Library. Small widget kit rendering UI layers to RGBA (`vello_cpu` + `parley` + `taffy`). Depends only on `termpix`. |
| `crates/temzdf` | The viewer binary (MuPDF via the `mupdf` crate, SyncTeX, reload). |
| `doc/` | Design notes and research. Start with `doc/architecture.md`. |
| `tests/fixtures/` | LaTeX sources of the test documents. Their PDFs and SyncTeX files are generated into `target/fixtures/` (never committed). |
| `todo.md` | Long-running tasks and deferred work. |
| `experiments/` | Standalone experiment crates outside the workspace (see the matching doc). |

Dependency direction is strictly `temzdf -> termpix-ui -> termpix`. Never make the
libraries depend on PDF, SyncTeX or viewer code.

## Environment

Everything runs inside the Nix dev shell (`flake.nix`):

```sh
scripts/check.sh            # fmt check + clippy -D warnings + tests, terse output
scripts/check.sh --fix      # same, but apply rustfmt
nix develop -c cargo build --workspace
```

Run `scripts/check.sh` before every commit; it enters the dev shell by itself.

End-to-end checks in a real kitty (virtual X server, no display needed):

```sh
nix develop -c cargo build --example scroll_demo
scripts/e2e.sh tests/e2e/scroll_demo.actions -- target/debug/examples/scroll_demo
```

Action files in `tests/e2e/` drive the app (wheel, drag, click, keys) and save
screenshots to `target/e2e/`; read the PNGs or measure pixels to verify. For
key-driven checks prefer `expect NAME KEYS`, which compares the terminal with
a headless render and fails the run on a mismatch (`tests/e2e/expect.actions`). See the
script header for the helpers. `temzdf FILE --render-frame out.png` renders one
screen headlessly. `scripts/build-fixtures.sh` builds `target/fixtures/` from
`tests/fixtures/*.tex` (tests run it automatically; e2e scenarios need it first). The
full testing approach is in `doc/testing.md`.

Debugging terminal problems:

- `cargo run -p termpix --example probe` prints what detection found.
- `cargo run -p termpix --example events -- events.log` logs every parsed input
  event (what the terminal really sends).
- In interactive mode `temzdf` sends stdout/stderr (MuPDF and SyncTeX warnings)
  to `$XDG_STATE_HOME/temzdf/temzdf.log`.
- `TERMPIX_TRACE=out.bin <app>` records every byte written to the terminal;
  replay prefixes of it into a terminal (`cat out.bin`) to bisect crashes.
- `TERMPIX_BACKEND=layered|composited` and `TERMPIX_MEDIUM=direct|shm|file`
  override detection (e.g. `direct` mimics SSH).
- `scripts/e2e.sh -t ghostty ...` runs the same scenarios in Ghostty; it exits
  with 3 when the terminal crashed.

The host has no `rustc` or `python3` on `PATH` outside the dev shell (`python3` is in it for scratch scripts). The default
dev shell leaves out large tools that are usually installed globally: TeX
(`latexmk`, `pdflatex`, `synctex`, needed for the fixtures) and the extra
terminals/multiplexers for e2e (Ghostty, WezTerm, zellij). On this machine they
come from the host profile. `nix develop .#alldeps` provides all of them.
`nix develop .#ci` is the minimal shell CI uses (toolchain, TeX, DejaVu fonts
only); `nix build` builds the `temzdf` package (`nix run . -- FILE` runs it).
CI (`.github/workflows/ci.yml`) runs `scripts/check.sh` in that shell and
`nix build` on Linux and macOS.

## Conventions

- Everything written into the repository (docs, comments, `todo.md`, skills,
  commit messages) is in English.
- Never commit binary files (`.pdf`, `.gz`, images, ...) or anything a tool
  generates (LaTeX/pandoc/typst output, SyncTeX files, logs, screenshots).
  Generated files can embed machine-specific absolute paths without anyone
  noticing. Commit the source plus a script that generates the output into
  `target/` instead, as `scripts/build-fixtures.sh` does. Exceptions: lock files
  (`Cargo.lock`, `flake.lock`), which pin dependencies and contain no paths.
- Paths written in docs, comments and commit messages are relative to the
  repository root. Never write absolute paths (home directories, scratch
  directories, `/nix/store/...`).
- Commit at natural checkpoints (a coherent, self-contained change), only when
  `cargo test` and `cargo clippy` pass. Use Conventional Commits:
  `type(scope): summary`, types `feat|fix|refactor|test|docs|build|chore`,
  scope = crate or area (`termpix`, `termpix-ui`, `temzdf`, `synctex`, `nix`, ...).
  Do not push unless asked. Never put session URLs (e.g. `Claude-Session:`
  trailers) in commit messages; a `Co-Authored-By` trailer is fine.
- Prefer designs that can be verified without a human: deterministic CPU
  rendering, PNG snapshots (inspect them by reading the image), the `termpix`
  mock terminal, and automated tests. Only the "feel" of scrolling in a real
  terminal needs the user.
- When you learn something new about terminal behaviour, record it in
  `doc/terminal-compat.md`.
- When a task starts repeating, follow `.claude/skills/automate-repeated-work`
  and turn it into a script or skill, then link it from here.

## Documents

- `doc/architecture.md`: overall design, threads, data flow, invariants.
- `doc/research.md`: library survey and why each dependency was chosen.
- `doc/kitty-protocol.md`: the parts of the kitty graphics protocol we rely on.
- `doc/terminal-compat.md`: per-terminal support matrix and known bugs.
- `doc/input.md`: mouse/keyboard reporting modes and smooth scrolling.
- `doc/reload.md`: auto-reload design (learned from Skim and Okular).
- `doc/testing.md`: how to verify changes without a human.
- `doc/synctex.md`: forward/inverse search, editor setup, vendored parser.
- `doc/config.md`: the configuration file format (keep in sync with `crates/temzdf/src/config.rs`).
- `doc/performance.md`: measurements (raster pool, display lists, transfer) and the benchmark to rerun.
- `doc/macos.md`: macOS status and the checklist to run on a Mac.
- `doc/gpu.md`: GPU rasterisation experiment (`experiments/gpu-raster/`): why not, and the `vello_cpu` lead.
