# Script host

The configuration can be a JavaScript or TypeScript program,
`$XDG_CONFIG_HOME/temzdf/config.ts` (or `config.js`; both at once is an
error). It runs in a separate process, the script host. This document
describes what is implemented; the design, its reasons and the open
questions are in `doc/scripting.md` (temzdf
is meant to feel like Vim/Neovim without editing buffers, with a vimrc-like
configuration).

Status (2026-10-26): implemented: the execution model, modes and key
mappings, the shipped defaults, options, generated type definitions,
floating messages and the escape keys; the TOML configuration is gone. How
to write a configuration is in `doc/config.md`, buffers included. Not yet:
pane sizes kept on resize (see `todo.md`).

## Pieces

| Where | What |
|---|---|
| `crates/temzdf-protocol/src/script.rs` | Messages between session and host (`ToHost`, `FromHost`, `ScriptValue`), framed like the session protocol (length prefix and postcard), over the host's stdin and stdout. |
| `crates/temzdf-script` | The host: QuickJS through `rquickjs`, TypeScript type stripping (`swc_ts_fast_strip`), module loading, the event loop, the native half of the API (`src/host.rs`) and its JavaScript half (`src/prelude.js`). |
| `crates/temzdf-script/runtime/defaults.ts` | The shipped default configuration (embedded; loaded before the user's). |
| `crates/temzdf-script/src/types.rs` | `temzdf.d.ts` for `temzdf print-types`, from the command registry. |
| `crates/temzdf-server/src/session/script.rs` | The session's side: starting the host, holding input while scripts run, the frame stack that runs hooks nested, options, mapping and mode operations. |
| `crates/temzdf-server/src/session/commands.rs` | The command registry (`cmd.*`, `action.*`, `temzdf action`). |
| `crates/temzdf-server/src/keymap/` | Key notation (Neovim's, with fallbacks for keys legacy terminals cannot tell apart), modes, the sequence matcher, the mode history. |
| `crates/temzdf-server/src/session/notices.rs` | Floating messages; `session.rs` has the escape keymap used while the host is down. |
| `crates/temzdf` | `temzdf script-host` and `temzdf print-types` (hidden), `--clean`, `--config`, finding `config.ts`/`config.js`. |

The server crates never link QuickJS: the host is a child process (the same
`temzdf` binary), so a runaway or crashing script cannot take the session
down.

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
  raises an uncatchable error) and drops the held typeahead, as Vim's CTRL-C.
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
- **Reload**: `temzdf action reload-config` starts a new host; mappings, hooks
  and the script's state start over. A host that exits leaves the session
  running without scripts until a reload. Messages from a replaced host are
  ignored (each host has a generation number).
- `console.log` and friends write to the host's stderr, which is the session
  log (`temzdf ls -v` shows it). Error positions are those of the source as
  written: type stripping turns types into whitespace.
- `temzdf action` gives up after `--timeout SECONDS` (default 10), e.g. while
  a script holds the session.

## API

`doc/config.md` describes it for users; `temzdf print-types` gives the exact
declarations. In short, `import { ... } from "temzdf"`:

- `cmd.*` runs registry commands now (synchronous requests); `action.*`
  describes them as data for mappings.
- `map(modes, lhs, rhs, { nowait, desc })`, `unmap`, `defineMode(name, {
  inherit, unmapped, count })`, `feedkeys(keys, { remap })`, `currentMode()`.
- `o.<option>` reads and sets options.
- `on(event, fn, { nested })`: `SessionReady`, `PaneOpened(id, kind)`,
  `PaneClosePre(id)` (not when `deleteBuffer` closes the pane: the buffer
  goes either way), `PaneClosed(id)`, `PaneFocused(id)`, `TabEntered(n)`,
  `TabClosed(n)`,
  `DocumentReloaded(path)`, `ModeChanged(old, new)`, `BufferCreated(id,
  kind)`, `BufferDeleted(id)`.
- `dispatch(async (ctx) => ...)` with `ctx.cmd` and `ctx.atomic(fn)`;
  `atomic(fn)`.
- Globals: `setTimeout`, `clearTimeout`, `console`.

Mapped functions get `{ count, pane }`. A mapping whose actions are data runs
entirely in the session (no RPC); only function mappings and hooks call the
host.

## Tests

- `crates/temzdf-script/tests/host.rs`: the host against a fake session
  (loading, imports, nested hooks, interrupts, tasks, atomic, tokens).
- `crates/temzdf/tests/tui.rs`: the real binary with a `config.ts` in a test
  configuration directory (mappings, modes, options, hook order, interrupts
  with held input, broken configurations, dispatched tasks, reload, a killed
  host and the escape keys, floating messages).
- `crates/temzdf/tests/types.rs`: the shipped defaults and a sample
  configuration type check with `tsc` against `temzdf print-types`; mistakes
  do not.
- `crates/temzdf-server/src/keymap/`: the matcher and notation, with time
  injected.
