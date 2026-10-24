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
| `tests/fixtures/` | LaTeX sources and generated PDFs/SyncTeX files used by tests. |
| `todo.md` | Long-running tasks and deferred work. |

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

The host has no `rustc` on `PATH` outside the dev shell. TeX (`latexmk`,
`pdflatex`, `synctex`) comes from the host profile, not the flake.

## Conventions

- Everything written into the repository (docs, comments, `todo.md`, skills,
  commit messages) is in English.
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
