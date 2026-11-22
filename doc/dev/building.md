# Building and CI

Builds without Nix, the fonts `temzdf-gui` embeds, `nix build` and CI.
The dev shell itself is in `doc/dev/environment.md`.

## Fonts and builds without Nix

The fonts `temzdf-gui` embeds are listed with their URLs and sha256 in
`crates/temzdf-gui/fonts.toml`. Nix fetches them from there and the dev
shell passes them in `TEMZDF_BUNDLED_FONTS`; a build without Nix runs
`cargo xtask fetch-fonts` first (into `target/fonts/`, where
`crates/temzdf-gui/build.rs` finds them).

Such a build also needs Zig 0.15.2 (libghostty-vt is then built from
source), esbuild (on Windows `crates/temzdf-script/build.rs` finds its
executable behind npm's `esbuild.cmd`) and npm, libclang, fontconfig and pkg-config (on Ubuntu:
`libclang-dev libfontconfig-dev pkg-config`, and `autoconf automake
libtool` for the next step), and libtexprintf and the shipped scripts' npm
packages, which `scripts/deps-without-nix.sh PREFIX` installs (it prints
the environment variables to set).

`cargo xtask update-fonts --check` shows newer releases,
`cargo xtask update-fonts` rewrites the list (then run the tests and
restart the dev shell).

## nix build

`nix build` builds the `temzdf` package (`nix run . -- FILE` runs it). It
uses crane: the Rust dependencies, the script host's guest and the guest's
dependencies are derivations of their own (`.#deps`, `.#guest`,
`.#guest-deps`), for a Nix cache to keep between runs. There is none for
now: Magic Nix Cache ran into the cache API's rate limit and made
rust-cache's restores fail with it (`todo.md`).

## CI

CI (`.github/workflows/ci.yml`) runs `npm ci` and
`cargo xtask check --stream` in `nix develop .#ci` (job
`check-with-nix`, without Miri) and `nix build` on Linux and macOS
(`build-with-nix`). Its job `build` builds `temzdf` and `temzdf-gui` on
Ubuntu without Nix and starts them (`scripts/smoke-session.sh`: a session
splits its pane on `<C-b>%`).

The slower checks are a workflow of their own, Advanced CI
(`.github/workflows/advanced-ci.yml`), on the same events:
`scripts/miri.sh` in `.#ci-miri` (job `miri-with-nix`). Both
`check-with-nix` and `miri-with-nix` keep built dependencies with
`Swatinem/rust-cache`.

The Rust caches (of `build`, `check-with-nix` and `miri-with-nix`) are
saved only by pushes to `dev` and `main`, and a job after each
(`prune-rust-cache.yml`) deletes those of the same branch but the one used
last.

The documents' check (`node scripts/check-docs.ts`, Node's standard
library only, so no `npm ci`) runs on every push, documentation-only ones
included (`.github/workflows/docs.yml`).
