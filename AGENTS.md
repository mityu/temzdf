# AGENTS.md

Notes for coding agents (and humans) maintaining this repository.

## What this is

`temzdf` is a terminal PDF viewer for live preview while writing a document in a
typesetting system that outputs PDF (LaTeX, Typst, SATySFi, ...). It draws
everything as pixels with the kitty graphics protocol (pages *and* UI chrome) and
aims for GUI-grade mouse interaction (pixel-smooth scrolling, drag, selection).
Key features: outline sidebar, in-document search, links, SyncTeX forward/inverse
search (LaTeX), and auto-reload that keeps the reading position.

The terminal plumbing is a separate, reusable library so that other viewers
(e.g. a future markdown viewer rendering a webview offscreen) can share it.

## Layout

Crates and other directories, each with its purpose: "Crates" in
`doc/architecture.md`. Read it before adding a crate, moving code between
crates or changing dependencies. Start any larger change with
`doc/architecture.md`.

Dependency direction: `termpix-core <- termpix <- temzdf-tty <- temzdf` and
`temzdf-scene <- temzdf-pdfcore <- temzdf-pdfview <- temzdf-server <- temzdf`
(and `temzdf-scene <- temzdf-term <- temzdf-server`). Never make `termpix*`
depend on PDF, SyncTeX or viewer code, and never make `temzdf-pdfcore`,
`temzdf-pdfview`, `temzdf-term` or `temzdf-server` depend on `termpix`
(tests excepted): backends, panes and the session produce scenes, only
frontends draw. The direction the project is heading (a tmux-like session
server with terminal, PDF and web panes) is in
`doc/design/session-server.md`.

## Environment

Everything runs inside the Nix dev shell (`flake.nix`). Agents are started
inside it, so run tools directly, never through `nix develop -c` (the
scripts enter their dev shell by themselves); if a tool the dev shell
should provide is missing, say so instead of working around it. A tool
added to `flake.nix` needs the shell, and the agent in it, restarted.
Never put the WASI clang (`CC_wasm32_wasip1`) on `PATH`. Run `npm ci` in
`crates/temzdf-script/runtime/` once after cloning and whenever its
`package-lock.json` changes. Miri: `env -u IN_NIX_SHELL scripts/miri.sh`.

Run the gate before every commit:

```sh
cargo xtask check            # fmt, Biome, declarations, docs, clippy -D warnings, tests (+ Miri where available)
cargo xtask check --fix      # same, applying rustfmt and Biome's fixes
cargo xtask check --no-miri  # without Miri (about a minute and a half less)
cargo xtask check --all      # the tests even when only Markdown under doc/ changed
```

`scripts/check.sh` takes the same options and runs it inside the dev shell.

Read before you act:

- Before setting up, adding an npm package, touching the scripts
  (TypeScript, Biome) or their declarations, or running Miri:
  `doc/dev/environment.md`.
- Before writing or changing tests, or verifying a change on screen
  (simulated terminal, e2e, frames, mutation checks, Miri, fixtures):
  `doc/dev/testing.md`; on macOS also `doc/terminals/macos.md`.
- When investigating terminal problems: `doc/dev/debugging.md`.
- Before touching builds without Nix, the embedded fonts, `nix build` or
  CI: `doc/dev/building.md`.

## Conventions

- Everything written into the repository (docs, comments, `todo.md`, skills,
  commit messages) is in English.
- Never commit binary files (`.pdf`, `.gz`, images, ...) or anything a tool
  generates (LaTeX/pandoc/typst output, SyncTeX files, logs, screenshots).
  Generated files can embed machine-specific absolute paths without anyone
  noticing. Commit the source plus a script that generates the output into
  `target/` instead, as `scripts/build-fixtures.sh` does. Exceptions: lock files
  (`Cargo.lock`, `flake.lock`), which pin dependencies and contain no paths,
  and `crates/temzdf-script/ts-types/temzdf-all.d.ts`, which editors and
  `temzdf print-types` read, written by `scripts/ts-types.ts` and checked
  by `cargo xtask check` (`--fix` writes it again).
- Paths written in docs, comments and commit messages are relative to the
  repository root. Never write absolute paths (home directories, scratch
  directories, `/nix/store/...`).
- Commit at natural checkpoints (a coherent, self-contained change), only when
  `cargo test` and `cargo clippy` pass. Use Conventional Commits:
  `type(scope): summary`, types `feat|fix|perf|refactor|test|docs|build|chore`,
  scope = crate or area (`termpix`, `termpix-ui`, `temzdf`, `synctex`, `nix`, ...).
  Do not push unless asked. Never put session URLs (e.g. `Claude-Session:`
  trailers) in commit messages; a `Co-Authored-By` trailer is fine.
- Prefer designs that can be verified without a human: deterministic CPU
  rendering, PNG snapshots (inspect them by reading the image), the `termpix`
  mock terminal, the simulated terminal (`crates/tuitest`), and automated tests. Only the "feel" of scrolling in a real
  terminal needs the user.
- When you learn something new about terminal behaviour, record it in
  `doc/terminals/terminal-compat.md`.
- When a task starts repeating, follow `.claude/skills/automate-repeated-work`
  and turn it into a script or skill, then link it from here.

## Documents

`doc/README.md` lists every document, by kind:

- `doc/architecture.md` (start here) and `doc/config.md` (the
  configuration, for users; keep it in sync with
  `crates/temzdf-script/runtime/` (`defaults.ts` and the modules it enables) and the registry in
  `crates/temzdf-server/src/session/commands.rs`) at the top;
- `doc/design/`: how the existing parts are built and what was decided
  with the user, with the reasons (session server, scripting, key maps,
  floating panes, GUI client, reload, SyncTeX). Plans and status go in
  `todo.md`, not here;
- `doc/terminals/`: terminal and platform behaviour (the compatibility
  matrix, the kitty protocol, input modes, macOS);
- `doc/dev/`: the environment, testing, debugging, building and CI,
  performance, known issues;
- `doc/survey/`: research done before deciding (libraries, prior art,
  experiments), as of when it was written.

A new document goes into one of these and into `doc/README.md`;
`scripts/check-docs.ts` (part of `cargo xtask check`) fails otherwise, and
when a path to a document written anywhere no longer exists.
