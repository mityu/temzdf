# Programmable configuration: language survey

Question (2026-10-25): can the TOML config (`crates/temzdf/src/config.rs`,
`crates/temzdf/src/keys.rs`) be complemented or replaced by a programmable
language, where temzdf exposes commands and state and users build key mappings
and small functions from them (like Neovim's or WezTerm's Lua config)?

Wanted, in priority order: easy to embed in Rust; a reasonably rich standard
library; optionally static (gradual) typing; optionally an LSP, with a way to
ship type definitions for our API (or an LSP embedded in temzdf).

Status: superseded (2026-10-26): the configuration is to be JavaScript /
TypeScript run in a separate process, see `doc/scripting.md`. The survey below
is kept for reference. Originally: research only, no decision. Versions and dates were collected by a
research agent from crates.io, GitHub releases and lua.org on that date; sizes
are estimates.

## Candidates

| Option | Embedding / C deps | Stdlib | Static typing | LSP and API stubs | Sandbox | Maturity |
|---|---|---|---|---|---|---|
| Lua 5.4/5.5 via `mlua` 0.12 | Excellent; small C, vendored (~300 KB, <1 ms start) | Medium (Lua patterns, no regex) | LuaCATS annotations (editor only) | LuaLS 3.19 or EmmyLua Analyzer + generated `---@meta` files | Good (memory limit, instruction hook, chosen libs) | Very high (Neovim, WezTerm, Yazi, mpv) |
| Luau via `mlua` (`luau` feature) | Excellent; C++ vendored (~1-2 MB) | Medium | Yes, gradual (`--!strict`) | luau-lsp + definition file (must be configured away from Roblox defaults) | Excellent (`Lua::sandbox`) | High (Roblox, Lune) |
| Teal | Via mlua + `tl.lua` | = Lua | Yes | tl language server + `.d.tl` | = Lua | Low, pre-1.0 |
| Rhai 1.27 | Excellent; pure Rust | Medium | No | Weak (LSP unmaintained since 2023) | Excellent | High |
| Rune 0.14 | Good; pure Rust | Medium | No | Built in: `rune::cli::Entry` gives a `languageserver` that knows our modules | Fair | Medium-low |
| Koto 0.16 | Good; pure Rust | Medium | No | koto-ls | Fair | Low |
| Steel 0.8 (Scheme) | Good; pure Rust | Medium | No | steel-language-server | Fair | Medium-low (Helix plugin PR still a draft) |
| Starlark (`starlark` 0.14, Meta) | Good; pure Rust | Small, hermetic (no IO, no regex, no while/recursion by default) | Yes, gradual | `starlark_lsp` library: embeddable as `temzdf --lsp` (as Buck2 does) | Excellent | High |
| Nickel 1.18 | OK | n/a | Contracts / types | nls | Pure | Pure and side-effect free: data only, no callbacks |
| JavaScript (`rquickjs` 0.14 / `boa_engine` 0.22) | Good; QuickJS-NG in C (~1 MB) or pure Rust (larger, slower) | Rich (RegExp, Map/Set, JSON) | `.d.ts` + `// @ts-check` JSDoc; real `.ts` needs a transpiler | tsserver | Good | High, but unusual for a terminal tool |
| Python (PyO3 0.30 / RustPython 0.6) | Poor (libpython; single binary and macOS painful) / big, slow | Very rich / incomplete | `.pyi` | pyright | None | Ruled out by packaging |
| Roto 0.13 (NLnet Labs) | Good; Cranelift JIT | Tiny | Yes, static | No | Fair | Too early: no first-class functions (callbacks), no maps |
| Wasm plugins (extism, wasmtime) | Heavy (10+ MB) | Any language | Any | Any | Excellent | A later plugin tier, not an editable config |

How existing apps ship Lua API stubs: Neovim keeps `---@meta` files in its
runtime (partly generated); Yazi generates a `types.yazi` package from its docs
and has users point `.luarc.json` `workspace.library` at it; WezTerm relies on
community-maintained types, which drift from the real API.

## Recommendation

1. **Lua 5.4/5.5 via `mlua` (vendored), with generated LuaCATS stubs.** Most
   mature embedding, tiny, trivial on macOS, and the language Neovim, WezTerm
   and Yazi users already write. LuaLS/EmmyLua give completion and gradual
   checking without temzdf shipping an LSP.
2. **Luau through the same `mlua` API** if real gradual types and a strong
   sandbox matter more than familiarity (C++ build, luau-lsp setup). `mlua`
   abstracts both, so this can stay a feature-flag decision.
3. **Rhai** if pure Rust matters most (weak editor tooling), or **Starlark** if
   an embedded `temzdf --lsp` and hermetic typed configs matter most (strong
   language restrictions).

## Sketch for Lua

```lua
-- $XDG_CONFIG_HOME/temzdf/init.lua
local t = require("temzdf")
t.map("J", function() t.cmd.scroll_down(5) end, { desc = "scroll 5" })
t.map("gs", function()
  local s = t.state() -- { page=, page_count=, zoom=, file= }
  t.notify(("page %d / %d"):format(s.page, s.page_count))
end)
t.on("reload", function() t.cmd.fit_width() end)
```

- One `mlua::Lua` on the main thread (`!Send` is fine), standard libraries
  except `io`/`os.execute` unless allowed, a memory limit and an instruction
  hook against runaway loops.
- A single Rust registration list: one `temzdf.cmd.*` function per `Command`
  in `keys.rs`, plus `map`, `state`, `notify`, `spawn` and `on(event)` (reload,
  page change). Callbacks are kept as `mlua::Function`s in the keymap beside
  built-in commands; they run synchronously and queue actions that the viewer
  applies afterwards, so scripts never borrow viewer state. Errors go to the
  status bar and the log.
- The same list generates the stubs: `temzdf --print-stubs DIR` writes
  `temzdf.lua` (`---@meta`, `---@class temzdf.State`, `---@param`) and a
  `.luarc.json`; a unit test diffs the generated stubs against the
  registrations so they cannot drift.
- Open question: keep TOML for simple options beside `init.lua`, or translate
  it into Lua defaults.
