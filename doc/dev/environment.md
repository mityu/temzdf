# Development environment

The Nix dev shell, the shipped scripts' npm packages, script development
and running Miri. The rules agents must always follow are in `AGENTS.md`;
builds without Nix, the embedded fonts and CI are in
`doc/dev/building.md`.

## The dev shell

Everything runs inside the Nix dev shell (`flake.nix`). Its Rust toolchain
(rust-overlay) has the `wasm32-wasip1` target, and `CC_wasm32_wasip1` names
nixpkgs' WASI clang (for the script host's guest; never put that clang on
`PATH`, it would replace the native `cc`).

Agents are started inside it, so run tools directly, not through
`nix develop -c` (it costs a shell evaluation each time); if a tool the dev
shell should provide is missing, say so instead of working around it. A
tool added to `flake.nix` needs the shell, and the agent in it, restarted.
The scripts enter their dev shell by themselves when started outside it
(`scripts/lib/devshell.sh`), so never use `nix develop -c` to run them.

The host has no `rustc` or `python3` on `PATH` outside the dev shell
(`python3` is in it for scratch scripts). The default dev shell leaves out
large tools that are usually installed globally: TeX (`latexmk`,
`pdflatex`, `synctex`, needed for the fixtures) and the extra
terminals/multiplexers for e2e (Ghostty, WezTerm, zellij). On the
maintainer's machine they come from the host profile.
`nix develop .#alldeps` provides all of them. `nix develop .#ci` is the
minimal shell CI uses (toolchain, TeX, no end-to-end tools), `.#ci-miri`
the same with the nightly toolchain and Miri and without TeX (Miri runs no
test that needs the fixtures); `nix build` builds the `temzdf` package
(`nix run . -- FILE` runs it).

## npm packages of the shipped scripts

The shipped scripts (`crates/temzdf-script/runtime/`, an npm package) are
bundled by `crates/temzdf-script/build.rs` with esbuild; npm manages their
packages in `runtime/node_modules`: run `npm ci` in `runtime/` once after
cloning and whenever `package-lock.json` changes (`build.rs` refuses
node_modules that no longer match it, `cargo xtask check` stops without
them; tsc and editors read the packages' types there).

To add one: `npm install PKG` in `runtime/` (widely used, maintained,
plain ES code: it runs in QuickJS; its install scripts never run,
`runtime/.npmrc`). JSR's packages come through its npm registry
(`runtime/.npmrc` points `@jsr` there): `npm install
@std/NAME@npm:@jsr/std__NAME`, as `@std/path`.

`nix build` has Nix build the same lock file into `$TEMZDF_NODE_MODULES`
(no network in builds; the dev shells set it too, used where
`runtime/node_modules` is missing, as in CI's Nix jobs but
`check-with-nix`).

## Scripts and their declarations

The repository's own Node scripts are TypeScript that Node runs as it is
(type stripping, Node 22.18 or later): `node scripts/ts-types.ts`,
`node scripts/check-docs.ts`, the e2e harness (`scripts/e2e/`, its
scenarios in `tests/e2e/*.ts`); erasable syntax only, `.ts` in imports,
type-checked by the gate with `tsc -p scripts` and `tsc -p tests/e2e`
(`scripts/tsconfig.json`, `tests/e2e/tsconfig.json`; `@types/node` is a
devDependency of `runtime/`), their tests (`scripts/**/*.test.ts`) run
with `node --test`.

The scripts (TypeScript and the script host's `prelude.ts` and
`temzdf.ts`) are formatted and linted by Biome (`biome.jsonc`;
`biome check --write .` alone fixes what it can).

Their TypeScript declarations are in `crates/temzdf-script/ts-types/`:

- Rust's API by hand in `temzdf-core.d.ts` (the server takes its events
  from `interface Events`, and a test holds its options to
  `interface Options`);
- the session's actions by hand in `temzdf-actions.d.ts` (a new action is
  declared there: the server's names come from it, and a test runs each);
- the generated `temzdf-all.d.ts` (written by `node scripts/ts-types.ts`,
  committed, checked by the gate).

Editors use `crates/temzdf-script/runtime/tsconfig.json` (the dev shell's
`tsc --lsp --stdio` is a language server). The devDependencies of
`runtime/package.json` (dts-bundle-generator, TypeScript 6, type-fest's and
Node's types) are tools, never bundled.

A value the API may leave out is `undefined`; the scripts check one with
`isNullish` (`runtime/lib/validate.ts`).

## Running Miri

The default dev shell has no Miri (the gate says it skipped it);
`nix develop .#enable-miri` has it, and the user runs it from time to
time. To run Miri yourself, `scripts/miri.sh` (it enters `.#enable-miri`
by itself when started outside a dev shell; its toolchain is nightly);
from inside the default dev shell, as agents are, run it as
`env -u IN_NIX_SHELL scripts/miri.sh`, or it only says it skipped. A unit
test Miri cannot run (foreign code, ptys, processes, sockets) gets
`#[cfg_attr(miri, ignore = "why")]`; see "Miri" in `doc/dev/testing.md`.
