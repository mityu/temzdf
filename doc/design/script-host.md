# Script host

The configuration can be a JavaScript or TypeScript program,
`$XDG_CONFIG_HOME/temzdf/config.ts` (or `config.js`; both at once is an
error). It runs in the script host: QuickJS compiled to WebAssembly, run
by wasmtime inside the session server, on a thread of its own. This document
describes how the host is built: its pieces, the execution model, the API.
The design and its reasons are in `doc/design/scripting.md` (temzdf is
meant to feel like Vim/Neovim without editing buffers, with a vimrc-like
configuration); how to write a configuration is in `doc/config.md`.

## Pieces

| Where | What |
|---|---|
| `crates/temzdf-protocol/src/script.rs` | Messages between session and host (`ToHost`, `FromHost`, `ScriptValue`), framed like the session protocol (length prefix and postcard). Without its feature `session`, the crate is only these and the framing (what the guest builds with). |
| `crates/temzdf-script` | The host: QuickJS through `rquickjs`, TypeScript type stripping (`swc_ts_fast_strip`), module loading, the event loop, the native half of the API (`src/host.rs`) and its JavaScript half (`src/prelude.js`), all behind its feature `host`. It talks through a `Link` (`src/link.rs`); `StreamLink` runs it natively over a byte stream (its tests). |
| `crates/temzdf-script-guest` | The host built for `wasm32-wasip1`: a `Link` made of imports, and the export `run`. Empty when built for anything else. |
| `crates/temzdf-script-runner` | Runs the guest in the server: `build.rs` builds it (nested cargo, profile `script-guest`) and precompiles it with Cranelift; at run time wasmtime's runtime alone loads it (`ScriptHost`). |
| `crates/temzdf-script/runtime/defaults.ts` | The shipped default configuration (embedded; loaded before the user's). |
| `crates/temzdf-script/src/types.rs` | `temzdf.d.ts` for `temzdf print-types`, from the command registry. |
| `crates/temzdf-server/src/session/script.rs` | The session's side: starting the host, holding input while scripts run, the frame stack that runs hooks nested, options, mapping and mode operations. |
| `crates/temzdf-server/src/session/commands.rs` | The command registry (`cmd.*`, `action.*`, `temzdf action`). |
| `crates/temzdf-server/src/keymap/` | Key notation (Neovim's, with fallbacks for keys legacy terminals cannot tell apart), modes, the sequence matcher, the mode history. |
| `crates/temzdf-server/src/session/notices.rs` | Floating messages; `session.rs` has the escape keymap used while the host is down. |
| `crates/temzdf` | `temzdf print-types` (hidden), `--clean`, `--config`, finding `config.ts`/`config.js`. |

## The sandbox

The host runs in the server's process, but inside WebAssembly (decided by
the user on 2026-11-07, after `doc/survey/script-host-transport.md`; it used
to be a child process, `temzdf script-host`, over pipes). The server never
runs QuickJS's C code natively, so a runaway or crashing script still cannot
take the session down:

- The guest sees its own linear memory, stderr, clocks, the file system
  read-only (WASI, `/` preopened: the configuration and what it imports),
  and four imports (`temzdf.send`, `recv`, `recv_into`, `interrupts`).
  A fault in it (QuickJS's C code included) is a trap: the guest ends, the
  server goes on.
- Its memory is capped at 1 GiB; growing past it traps (and ends it).
- `ScriptHost::stop` ends it whatever it is doing: the engine's epoch is
  advanced, and the guest's deadline callback traps at its next function
  call or loop iteration. Dropping a `ScriptHost` stops it (a reload, the
  session's end).
- What it cannot contain: bugs in wasmtime itself, and panics in the
  server's own import functions.

**Build.** `crates/temzdf-script-runner/build.rs` builds the guest with a
nested `cargo build --target wasm32-wasip1 --profile script-guest` (its own
target directory, `target/script-guest`, kept between builds; QuickJS's C
code needs `CC_wasm32_wasip1`, which the dev shells and `nix build` set to
nixpkgs' WASI clang) and precompiles it with Cranelift for the target's
baseline (not the build machine's CPU), into `OUT_DIR`, embedded with
`include_bytes!`. The binary carries wasmtime's runtime but no compiler;
loading the module takes about 0.3 ms. The profile `script-guest`
(`Cargo.toml`) optimises for size (swc is most of the guest) but for
QuickJS, its bindings and the host. Both sides configure the engine with
`src/engine_config.rs` (a precompiled module only loads into an engine
configured the same way). Under Miri the build script embeds nothing.

## Execution model

- **Startup**: the session always starts the host (the default mappings are
  JavaScript) and sends it the configuration's path (none with `--clean`) and
  the command list; the host evaluates the shipped defaults, then the
  configuration. The configuration's top level should only
  register things; a top-level `await` that does not settle at once is an
  error ("use dispatch"). The session waits up to 5 s for the host to finish,
  then opens its first pane and fires `SessionReady`. A configuration that
  fails (syntax, exception, timeout) is reported in the log and in a message,
  and the session runs without scripts.
- **Mapped and hook functions are synchronous.** One that returns a Promise
  is an error. Their commands are synchronous requests: the host waits for
  the answer. Meanwhile the session holds user input (keys, mouse, paste,
  `temzdf action` requests) and replays it afterwards; documents and
  terminals go on. Frames wait too, for up to a second; after that a message
  says that `<C-c>` interrupts.
- **`<C-c>` interrupts** a running function (QuickJS's interrupt handler
  raises an uncatchable error; it sees the interrupt as soon as the session
  sends it, through the import `interrupts`, a count of those sent) and drops the held typeahead, as Vim's CTRL-C.
  A function blocked in a command is interrupted as well. Commands it already
  ran keep their effect. There is no timeout.
- **Hooks run as Vim's autocmds**: synchronously, at the moment the event
  happens, nested inside the command that caused it (the host runs the hook
  while it waits for that command's answer). Events caused inside a hook only
  reach hooks registered with `nested: true`, up to 10 levels. "Before" events
  (`PaneClosePre`) run ahead of the operation, which waits for them.
- **`dispatch(async (ctx) => ...)`** starts a task without waiting for it. Its
  commands go through `ctx.cmd`; `ctx.atomic(fn)` runs a synchronous `fn` with
  everything else held. Errors of tasks are reported as messages and in the
  log.
- **Context tokens**: every command carries the call or task it belongs to;
  the session refuses commands of calls that have finished and of tasks that
  ended. Global functions (`cmd.split()`) only work inside a mapped or hook
  function (or while the configuration loads); elsewhere they throw "no
  context".
- **Deadlock rules**: while the session waits for the host it keeps answering
  the host's requests; it asks the host to run nothing else except the hooks
  of events the running function caused.
- **Reload**: `temzdf action reload-config` starts a new host (the old one
  is stopped first); mappings, hooks and the script's state start over. A
  host that ends (a trap, the memory cap) leaves the session running
  without scripts until a reload; why it ended is in the log. Messages from a replaced host are
  ignored (each host has a generation number).
- `console.log` and friends write to the guest's stderr (WASI), which is the
  server's, the session log (`temzdf ls -v` shows it). Error positions are those of the source as
  written: type stripping turns types into whitespace.
- `temzdf action` gives up after `--timeout SECONDS` (default 10), e.g. while
  a script holds the session.

## API

`doc/config.md` describes it for users; `temzdf print-types` gives the exact
declarations. In short, `import { ... } from "temzdf"`:

- `cmd.*` runs registry commands now (synchronous requests); `action.*`
  describes them as data for mappings.
- `keymap.add(modes, lhs, rhs, { nowait, desc, when, namespace, priority })`,
  `keymap.remove(modes, lhs, { namespace })`, `keymap.clear({ mode,
  namespace })`, `keymap.get({ mode, namespace, lhs })`, `defineMode(name,
  { fallback, count, transient, keepalive, timeout })`,
  `feedkeys(keys, { remap })`, `currentMode()` (doc/design/keymaps.md).
- `o.<option>` reads and sets options.
- `on(event, fn, { nested })`: `SessionReady`, `PaneOpened(id, kind)`,
  `PaneClosePre(id)` (also for each pane of a tab page `closeTab` or a
  middle click closes; not when `deleteBuffer` closes the pane: the buffer
  goes either way), `PaneClosed(id)`, `PaneFocused(id)`, `TabEntered(n)`,
  `TabClosed(n)`,
  `DocumentReloaded(path)`, `ModeChanged(old, new)`, `BufferCreated(id,
  kind)`, `BufferDeleted(id)`, `BufferHidden(id, kind)` (the last pane
  showing it went: closed, showing another buffer, its tab page closed;
  before `BufferDeleted` when `deleteBuffer` closes its panes),
  `InputChanged(id, text)` (an input buffer's text changed: typed, pasted,
  edited, set; not when only its caret moved), `BufferDeletePre(id)`
  (before `deleteBuffer`), `SessionEndPre()` (before `kill-session` and
  before closing the last pane).
- **"Before" hooks may cancel** (`PaneClosePre`, `BufferDeletePre`,
  `SessionEndPre`; doc/design/text-editor.md section 13): one returning
  `{ cancel: true, reason }` stops the operation, and the reason goes to
  whoever asked: a script's command throws it, `temzdf action` (and
  `temzdf kill-session`) fails with it, a key shows it in a message.
  `temzdf action` answers once the hooks it started ran. `kill-session
  --force` (`killSession({ force: true })`) ends without asking. The
  shipped defaults refuse to delete a text buffer with unsaved changes or
  to end the session while one has them. Every hook returns what it
  likes; only "before" hooks' answers are read.
- `dispatch(async (ctx) => ...)` with `ctx.cmd` and `ctx.atomic(fn)`;
  `atomic(fn)`.
- Globals: `setTimeout`, `clearTimeout`, `console`.

Mapped functions get `{ count, pane }`. A mapping whose actions are data runs
entirely in the session (no RPC); only function mappings and hooks call the
host.

## Tests

- `crates/temzdf-script/tests/host/cases.rs`: the host against a fake
  session (loading, imports, nested hooks, interrupts, tasks, atomic,
  tokens), run twice: natively (`tests/host.rs`) and as the guest in
  wasmtime (`crates/temzdf-script-runner/tests/guest.rs`, which adds
  stopping, the memory cap at 32 MiB, and an ignored timing test).
- `crates/temzdf/tests/tui.rs`: the real binary with a `config.ts` in a test
  configuration directory (mappings, modes, options, hook order, interrupts
  with held input, broken configurations, dispatched tasks, reload, a stopped
  host (`temzdf action debug-stop-script-host`, not in the registry) and the
  escape keys, floating messages).
- `crates/temzdf/tests/types.rs`: the shipped defaults and a sample
  configuration type check with `tsc` against `temzdf print-types`; mistakes
  do not.
- `crates/temzdf-server/src/keymap/`: the matcher and notation, with time
  injected.
