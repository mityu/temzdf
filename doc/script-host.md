# Script host

The configuration can be a JavaScript or TypeScript program,
`$XDG_CONFIG_HOME/temzdf/config.ts` (or `config.js`; both at once is an
error). It runs in a separate process, the script host. This document
describes what is implemented; the design, its reasons and the open
questions are in `doc/scripting.md` on the `research/notes` branch (temzdf
is meant to feel like Vim/Neovim without editing buffers, with a vimrc-like
configuration).

Status (2026-10-26): the execution model works end to end with a first set of
commands and hooks. Not yet: modes and real key mappings (`prefixMap` stands
in), the shipped default configuration in JavaScript, removing the TOML
configuration, generated type definitions, the floating message window.

## Pieces

| Where | What |
|---|---|
| `crates/temzdf-protocol/src/script.rs` | Messages between session and host (`ToHost`, `FromHost`, `ScriptValue`), framed like the session protocol (length prefix and postcard), over the host's stdin and stdout. |
| `crates/temzdf-script` | The host: QuickJS through `rquickjs`, TypeScript type stripping (`swc_ts_fast_strip`), module loading, the event loop, the native half of the API (`src/host.rs`) and its JavaScript half (`src/prelude.js`). |
| `crates/temzdf-server/src/session/script.rs` | The session's side: starting the host, holding input while scripts run, the frame stack that runs hooks nested, the command registry. |
| `crates/temzdf` | `temzdf script-host` (hidden subcommand the session starts), `--clean`, finding `config.ts`/`config.js`. |

The server crates never link QuickJS: the host is a child process (the same
`temzdf` binary), so a runaway or crashing script cannot take the session
down.

## Execution model

- **Startup**: the session starts the host and sends it the configuration's
  path and the command list. The configuration's top level should only
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

## API (first version)

```ts
import { cmd, on, prefixMap, dispatch, atomic } from "temzdf";

prefixMap("t", () => {            // C-s t (provisional, until real mappings)
  cmd.split("right");
  cmd.sendText("make\n");
});
on("PaneClosePre", (id) => console.log(`closing ${id}`));
on("PaneOpened", (id, kind) => {}, { nested: true });
dispatch(async (ctx) => {
  await new Promise((r) => setTimeout(r, 1000));
  ctx.cmd.message("a second later");
});
```

Commands (`cmd.*`, from the registry in `session/script.rs`): `split(dir)`,
`newPane(...)` (as `temzdf action new-pane`), `moveFocus(dir)`,
`focusNextPane()`, `closePane()`, `detach()`, `message(text)`,
`sendText(text)` (to the focused terminal pane, like Vim's `chansend()`),
`pdf(command, count?)` (a page-view command such as `"scroll-down"` in the
focused PDF pane), `panes()`, `focusedPane()`, `sessionName()`.

Events (`on(name, fn, { nested })`): `SessionReady`, `PaneOpened(id, kind)`,
`PaneClosePre(id)`, `PaneClosed(id)`, `PaneFocused(id)`,
`DocumentReloaded(path)`.

Imports: `"temzdf"` and paths relative to the importing file (`.ts` files are
type-stripped; only erasable syntax, so no `enum` or `namespace` with values).
No npm packages, no Node APIs. Globals: `setTimeout`, `clearTimeout`,
`console`.

## Tests

- `crates/temzdf-script/tests/host.rs`: the host against a fake session
  (loading, imports, nested hooks, interrupts, tasks, atomic, tokens).
- `crates/temzdf/tests/tui.rs`: the real binary with a `config.ts` in a test
  configuration directory (mappings, hook order, interrupts with held input,
  broken configurations, dispatched tasks, reload, a killed host).
