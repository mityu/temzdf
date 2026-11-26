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
`libclang-dev libfontconfig-dev pkg-config`), and the shipped scripts' npm
packages, which `scripts/deps-without-nix.sh PREFIX` installs (it prints
the environment variables to set, none at the moment). libtexprintf is
vendored and built by `cc` (`crates/texprintf/vendor/libtexprintf/README.md`);
it needs a C compiler only.

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

On Linux, `nix build .#windows` cross-builds `temzdf.exe` and
`temzdf-gui.exe` for `x86_64-pc-windows-gnu` (`.#windows-deps`: its
dependencies). It uses nixpkgs' MinGW-w64 with the UCRT
(`pkgsCross.ucrt64`), as the MSYS2 build on Windows does, and links with
lld as `.cargo/config.toml` says (with `-no-pie`: GCC's default `-pie`
is no option of lld's MinGW driver). libghostty-vt-sys builds
libghostty-vt for the target itself, from the source and Zig packages of
`nix/libghostty-vt.nix` (`GHOSTTY_SOURCE_DIR`, `GHOSTTY_ZIG_SYSTEM_DIR`).
The executables import only Windows' own DLLs, the UCRT included.

## CI

CI (`.github/workflows/ci.yml`) runs `npm ci` and
`cargo xtask check --stream` in `nix develop .#ci` (job
`check-with-nix`, without Miri) and `nix build` on Linux and macOS
(`build-with-nix`). Its job `build` builds `temzdf` and `temzdf-gui` on
Ubuntu without Nix and starts them (`scripts/smoke-session.sh`: a session
splits its pane on `<C-b>%`).

Its job `check-windows` runs the same gate on `windows-latest`, without
Nix, for the crates that build and pass on Windows so far
(`WINDOWS_CRATES` and `WINDOWS_ONLY_CRATES` in `xtask/src/check.rs`:
clippy and the tests of those packages instead of the workspace). Windows builds with MSYS2's UCRT64, not MSVC
(`doc/survey/windows-port.md`, "The toolchain"): the job installs MSYS2
with `msys2/setup-msys2` and its packages `mingw-w64-ucrt-x86_64-rust`
(clippy and rustfmt included), `-rust-wasm`, `-gcc` and `-clang`
(bindgen's libclang, `LIBCLANG_PATH`), `-pkgconf` and `make`, and the
WASI sysroot, builtins, `lld` and `llvm` that QuickJS's C in the script
guest is compiled with (`WASI_SDK` points rquickjs-sys at UCRT64, instead
of the wasi-sdk it downloads, whose clang needs the MSVC runtime). It
builds libghostty-vt with Zig 0.15.2 for `x86_64-windows-gnu` at
`nix/libghostty-vt.nix`'s commit into UCRT64, where pkgconf finds it
(Ghostty's build picks the MSVC ABI unless told), and links with lld
(`.cargo/config.toml`; GNU ld cannot link that static library). It puts
UCRT64's `bin` on `PATH`, prints the tools it found, runs the gate in
UCRT64's shell (`shell: msys2`, which also has MSYS2's `usr/bin` with
`make`) and keeps the build in `Swatinem/rust-cache`. MSYS2's packages
are not pinned: each run takes the current ones. It also installs
Node 24, runs `npm ci` and installs Biome 2.6.8 and esbuild 0.27.2
globally. The Linux gate
also checks the crates of `WINDOWS_CRATES` for `x86_64-pc-windows-gnu`
(step `windows`, a clippy run with `-D warnings`; those of
`WINDOWS_ONLY_CRATES` build C for the target, which the dev shell has no
compiler for), where the Nix dev shells' toolchain
carries that standard library (`flake.nix`), so `check-with-nix` runs it
too; it prints `SKIP windows` without it.

The slower checks are a workflow of their own, Advanced CI
(`.github/workflows/advanced-ci.yml`), on the same events:
`scripts/miri.sh` in `.#ci-miri` (job `miri-with-nix`). Both
`check-with-nix` and `miri-with-nix` keep built dependencies with
`Swatinem/rust-cache`.

The Rust caches (of `build`, `check-with-nix`, `check-windows` and
`miri-with-nix`) are saved only by pushes to `dev` and `main`, and a job after each
(`prune-rust-cache.yml`) deletes those of the same branch but the one used
last.

The documents' check (`node scripts/check-docs.ts`, Node's standard
library only, so no `npm ci`) runs on every push, documentation-only ones
included (`.github/workflows/docs.yml`).
