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
| `crates/temzdf-script` | The host: QuickJS through `rquickjs`, TypeScript type stripping (`swc_ts_fast_strip`), module loading, the event loop, the native half of the API (`src/host.rs`) and its JavaScript half (`src/prelude.ts`: TypeScript typed as the `temzdf` module of `ts-types/temzdf-core.d.ts`, so it builds what the declarations say; what scripts pass is checked, and its defaults filled in, by zod schemas checked against the declared types, and the session's answers come as `unknown`, their shapes checked by zod too; esbuild turns it into a script in `build.rs`) and the module `import ... from "temzdf"` gets (`src/temzdf.ts`: the prelude's API export by export, checked to export exactly what the declarations do), all behind its feature `host`. It talks through a `Link` (`src/link.rs`); `StreamLink` runs it natively over a byte stream (its tests). |
| `crates/temzdf-script-guest` | The host built for `wasm32-wasip1`: a `Link` made of imports, and the export `run`. Empty when built for anything else. |
| `crates/temzdf-script-runner` | Runs the guest in the server: `build.rs` builds it (nested cargo, profile `script-guest`) and precompiles it with Cranelift; at run time wasmtime's runtime alone loads it (`ScriptHost`). |
| `crates/temzdf-script/runtime/` | The shipped scripts, an npm package bundled by `crates/temzdf-script/build.rs` with esbuild (npm packages from `package-lock.json`: `runtime/node_modules` by `npm ci`, else built by Nix into `$TEMZDF_NODE_MODULES`): `defaults.ts`, the default configuration (loaded before the user's); `polyfills.ts`, globals QuickJS lacks (`AbortController` from `abortcontroller-polyfill`, `URL` and `URLSearchParams` from `core-js-pure`; quickjs-ng has `atob`, `btoa` and `queueMicrotask` itself), evaluated first; `lib/validate.ts`, checking values with zod (`zod/mini`); modules scripts import as `temzdf/NAME` (`MODULES` in `build.rs`: `edit/vim`, `edit/notepad`, `pane/pdf`, `pane/terminal`, `pane/markdown`, `mode/normal`, `mode/input`, `buffers`, each a keyset (`keyset`, `runtime/lib/keyset.ts`: nothing on import, `enable()`/`disable()`); `command`, the Ex command registry every script shares; `keyset` itself, for scripts' own keysets; and `test`, tests of temzdf's behaviour on QUnit (`doc/dev/testing.md`, "Script tests")), kept as imports in every bundle so that one instance of each is shared; `defaults.ts` only enables them. The user's configuration is not bundled, nor are the tests beside the scripts (`runtime/**/*.test.ts`; `build.rs` asks cargo to run it again only for the files esbuild bundled, so editing a test rebuilds nothing). |
| `crates/temzdf-script/src/eval.rs` | `temzdf eval`'s code made a function the host runs (below, "temzdf eval"). |
| `crates/temzdf-script/src/types.rs` | The declarations `temzdf print-types` prints: `ts-types/temzdf-all.d.ts`, embedded as it is (`scripts/ts-types.ts` writes it from the declarations written by hand and the shipped modules). |
| `crates/temzdf-server/src/session/script.rs` | The session's side: starting the host, holding input while scripts run, the frame stack that runs hooks nested, options, mapping and mode operations. |
| `crates/temzdf-server/src/session/commands.rs` | The command registry (`action.*`, `temzdf action`). |
| `crates/temzdf-server/src/keymap/` | Key notation (Neovim's, with fallbacks for keys legacy terminals cannot tell apart), modes, the sequence matcher, the mode history. |
| `crates/temzdf-server/src/session/notices.rs` | Floating messages; `input.rs` has the escape keys used while the host is down (`<C-b>d`/`q`/`r`; other keys reach terminal panes only). |
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
configured the same way). Under Miri the build script embeds nothing. A
guest built already can be handed to it in `TEMZDF_SCRIPT_GUEST_WASM` (a
`.wasm` file): then it is only precompiled. `nix build` builds the guest
as a derivation of its own and passes it so, so that the guest and its
dependencies are cached apart from the server.

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
  terminals go on. Frames wait too, for up to a second, then show what the
  script has done so far; after 8 s a message says that `<C-c>` interrupts.
- **`<C-c>` interrupts** a running function (QuickJS's interrupt handler
  raises an uncatchable error; it sees the interrupt as soon as the session
  sends it, through the import `interrupts`, a count of those sent) and drops the held typeahead, as Vim's CTRL-C.
  A function blocked in a command is interrupted as well. Commands it already
  ran keep their effect. What the session waits for has no timeout; a
  timer or promise job nobody waits for (so nobody can interrupt it) is
  interrupted after 10 s (`UNATTENDED_LIMIT` in
  `crates/temzdf-script/src/lib.rs`), and the log says so: the interrupt
  is uncatchable, so whatever awaited that job never goes on.
- **Hooks run as Vim's autocmds**: synchronously, at the moment the event
  happens, nested inside the command that caused it (the host runs the hook
  while it waits for that command's answer). Events caused inside a hook only
  reach hooks registered with `nested: true`, up to 10 levels. "Before" events
  (`PaneClosePre`) run ahead of the operation, which waits for them.
- **`dispatch(async (ctx) => ...)`** starts a task without waiting for it. Its
  commands go through `ctx.action`; `ctx.atomic(fn)` runs a synchronous `fn` with
  everything else held, and the plain `action` works inside it (under the
  task's token), so code shared with mappings runs there unchanged. Errors
  of tasks are reported as messages and in the log.
- **Commands answering later** (`readClipboard`): the session replies
  `{ "$promise": n }` at once and settles it later with `ToHost::Resolve`;
  the prelude turns the reply into a Promise, settled (as timers fire) when
  nothing runs. Its continuation has no context, so it is awaited in a
  dispatched task (`doc/design/registers.md`).
- **The event loop keeps JavaScript's order** (the HTML event loop's,
  Node's): a task is a message from the session that runs code (a call,
  a condition's evaluation, the configuration's load, an answer settling
  a Promise) or a timer firing. After each one comes a microtask
  checkpoint: the promise jobs it queued run, and those they queue, until
  none is left, before the next task. So a mapped function's
  `Promise.resolve().then(...)` runs before its `setTimeout(..., 0)`.
  Calls run nested in a request's wait (hooks) are part of the task
  waiting; their jobs run after it.
- **Nested waits**: every request waits for its answer, attending calls
  the session runs while answering it (hooks the request caused), whose own
  requests wait inside it. An answer for a request further out that comes
  during such an inner wait is kept for it; a `Resolve` is queued, and the
  host settles queued answers before it waits for a message again (none
  may come to wake it).
- **Calls crossing the host's own code wait for it**: the session runs a
  key's call (or a scheduled function, a condition) when it thinks
  nothing runs, but a timer or a dispatched task may have started
  meanwhile, its `AtomicBegin` and requests on their way. The session
  marks each `ToHost::Call` and `ToHost::Eval` `nested` when it sends it
  while answering a request; the host runs one that is not once nothing
  runs (queued, before `temzdf eval`s), never inside the request it
  waits for. Run there, the key's call would change what the task sees
  between two commands of an atomic section (a finder's pane closed
  between "is it there?" and "set its title").
- **The prelude's payloads**: each operation's JSON payload (`map`,
  `defineMode`, `on`, ...) is read into a struct of its own in `host_op`
  (`src/host.rs`). One that breaks the protocol (a required `func`
  missing, a wrong type, a `fallback` that is neither a name nor a
  function's id) is the prelude's bug: the call throws `internal: OP:
  what is wrong`, as it does for a bad context token or an unknown
  operation. Never a default (function 0, no mode), which would map or
  hook the wrong function silently, and never a panic, which would trap
  the guest and stop every script; the exception shows where the call
  was made, with its stack, and the other mappings and hooks live on.
  What scripts get wrong the prelude refuses before, by its zod schemas,
  with messages naming the call and the option (`keymap.add`'s
  `priority` is a 32-bit integer there); option objects are strict
  (`options()` in `runtime/lib/validate.ts`): a key they do not have is
  refused, and a removed name (`defineMode`'s `inherit`, `unmapped`) says
  what replaced it.
- **Context tokens**: every command carries the call or task it belongs to;
  the session refuses commands of calls that have finished and of tasks that
  ended. Global functions (`action.split()`) only work inside a mapped or hook
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
  a script holds the session; so does `temzdf eval`.

## temzdf eval

`temzdf eval FILE` (`-`: standard input) or `temzdf eval -c CODE [-c
CODE ...]` runs code in the session's host (decided with the user on
2026-11-22, as Neovim's `--remote-expr` and `emacsclient --eval`: the
configuration's language from the command line rather than `temzdf
action`'s rules for words; `doc/config.md` has how to use it).

- **The way it travels.** The client reads the file (or standard input)
  and sends `ClientMsg::Eval(Code)`: the steps (each `-c` named `-c 1`,
  `-c 2`, ...; the file by its absolute path; standard input `[stdin]`),
  the caller's directory, and `-p`. The server takes it in turn with
  `temzdf action` requests (held while a script runs), and the session
  sends `ToHost::RunCode` with a number of its own; the host answers
  `FromHost::CodeDone` (what was printed, then the value or the error),
  and the server answers the client with `ServerMsg::Evaluated`. A host
  that is not running answers at once; one that stops or is replaced
  meanwhile (a reload) answers each eval it had with an error.
- **A dispatched task.** `__temzdfEval` (`prelude.ts`) starts a task as
  `dispatch` does and runs the steps in turn, each awaited, stopping at the
  first that throws. The task's `TaskDone` is always a success: an eval's
  error goes to its caller, not to the session's messages and log. A task
  cut off by an interrupt in its atomic section answers "interrupted"
  (`close_cut_off`); one stopped by the unattended limit outside one
  leaves its caller to its `--timeout` (nothing tells which task a stopped
  job belonged to).
- **One scope per eval, not the globals.** Each step is compiled
  (`host.compile`, `src/eval.rs`) into `(function (scope, view) { with
  (scope) { return async function () { "use strict"; STEP } } })`, `scope`
  an object of that eval's alone holding `temzdf` and `console`. Top-level
  statements of the step are rewritten, text spliced in so that lines
  stay: `const`/`let`/`var` become assignments (the names put in `scope`
  first), functions and classes are copied into `scope`, so later steps see
  them, as a browser console's inputs do; `import` becomes `await
  import(...)` destructured, and `import ... from "temzdf"` takes the
  eval's `temzdf` (so `import { action } from "temzdf"` is the task's
  too); a last expression statement is returned, the value `-p` prints
  (`JSON.stringify`, `undefined` for what it gives nothing for). Nothing
  reaches the globals, so running the same `const x` twice is no
  redeclaration, and the other code's `console` still writes to the
  session log: only the eval's `console` (in its scope) prints to the
  caller. A global eval (QuickJS's `JS_EVAL_FLAG_ASYNC`) would have shared
  declarations too, but in the global lexical scope, for good; a module
  per step shares none.
- **`temzdf` is the module with the task's `action` and `atomic`**
  (`ctx.action`, `ctx.atomic`), so `temzdf.action.*` works after an
  `await`. The rest of the API asks for the running call's context, as in
  a dispatched task: inside `temzdf.atomic(() => ...)` it has the task's.
- **Positions.** Types are stripped first (as the configuration's); a
  syntax error is swc's, `NAME:LINE:COLUMN`, found when the step's turn
  comes (the steps before it ran). Errors at run time carry QuickJS's
  stack; their positions are mapped back through the wrapper on the
  first line and the text inserted (`eval::Positions`), and the
  prelude's frames are left out. A step's relative imports start from
  the caller's directory (`-c`, standard input) or the file's.
- `temzdf eval` and its tests: `crates/temzdf/tests/tui.rs`
  (`eval_*`), the host's in `crates/temzdf-script/tests/host/cases.rs`,
  the rewriting's in `src/eval.rs`.

## API

`doc/config.md` describes it for users; `temzdf print-types` gives the exact
declarations. In short, `import { ... } from "temzdf"`:

- `action.*` runs registry commands now (synchronous requests; until
  2026-11-19 this was `cmd.*`, and `action.*` described commands as data for
  mappings, `doc/design/scripting.md`).
- `keymap.add(modes, lhs, rhs, { nowait, desc, when, namespace, priority })`,
  `keymap.remove(modes, lhs, { namespace })`, `keymap.clear({ mode,
  namespace })`, `keymap.get({ mode, namespace, lhs })`, `defineMode(name,
  { fallback, count, transient, keepalive, timeout })`,
  `feedkeys(keys, { remap })`, `currentMode()` (doc/design/keymaps.md).
- `o.<option>` reads and sets options.
- `on(event, fn, { nested })` (returns a function removing the hook,
  `HostOp::Off`): `SessionReady`, `PaneOpened(id, kind)`,
  `PaneClosePre(id)` (also for each pane of a tab page `closeTab` or a
  middle click closes; not when `deleteBuffer` closes the pane: the buffer
  goes either way), `PaneClosed(id)`, `PaneFocused(id)`, `TabEntered(n)`,
  `TabClosed(n)`,
  `DocumentReloaded(path)`, `ModeChanged(old, new)`, `BufferCreated(id,
  kind)`, `BufferDeleted(id)`, `BufferHidden(id, kind)` (the last pane
  showing it went: closed, showing another buffer, its tab page closed;
  before `BufferDeleted` when `deleteBuffer` closes its panes),
  `BufferDeletePre(id)`
  (before `deleteBuffer`), `SessionEndPre()` (before `kill-session` and
  before closing the last pane), `InvokePre(info)` and `InvokePost(info)`
  (around each invocation), `TextChanged(buffer, change)`,
  `PaneScrolled(pane, lines)`, `KeyTyped(key)`.
- **Invocations** (doc/design/text-editor.md section 9): a key sequence
  that runs a mapping, text typed into a text pane (keys no
  mapping took, input methods, pastes), a mode's fallback function, a
  `temzdf action` request. `InvokePre` hooks hear it first (and may
  cancel it, as "before" hooks); `InvokePost` comes at the safe point
  after it and its hooks, with the cursor and selection. Their `info`:
  `seq`, `target` (a mapped or fallback function's id, a `temzdf action`
  request as `[{ name, args }]`, or `{ textInput }`), `keys`, `count`, `mode`, `pane`, `source` (`"key"`,
  `"cli"`). `invoke(target, { count })` runs a target again, as part of
  what runs now (no events of its own). Commands from hooks, timers and
  tasks are no invocation.
- **`TextChanged(buffer, { changes, version, cause, invocation })`**: once
  per editing primitive, undo or redo; `changes` in positions before them
  (`{ range, rangeLength, text }`, LSP's form), `cause` `"typing"`,
  `"script"`, `"undo"`, `"redo"`; heard before the editing command
  returns (the host serves the hooks while it waits).
- **`PaneScrolled(pane, { buffer, first, last, previous })`**: a text
  pane's lines in sight changed (Vim's `WinScrolled`; for lists that grow
  as they are scrolled, the fuzzy finder's, and highlighting only what
  shows). The session keeps what each text pane showed
  (`Session::scrolled`, `View::visible_lines`) while the event is hooked,
  starting when the first hook comes (what shows then is no news), and
  compares at the safe point: when nothing else is left there (no
  scheduled function, no `InvokePost`), before held keys go on, and in
  every `poll` (a new size). So one event per pane for everything an
  invocation and its hooks did, none when the lines came back to where
  they were; each wheel notch is an input of its own. `previous` is
  undefined for a pane not seen before or showing another buffer.
  Edits need no check of their own: the view follows them, and the
  comparison sees lines inserted above.
- **`FileChangedOnDisk(buffer)`**: a text buffer's file changed elsewhere
  (its modification time differs from what was read or written; once per
  change). `reload(buffer?)` reads it again as one undoable edit (a line
  diff: marks and cursors outside it stay), the result the saved state.
  The shipped defaults reload a buffer without unsaved changes and
  otherwise say so.
- **`KeyTyped({ keys, text, source })`**: a key arrived (`"typed"`, or
  `"feedkeys"` when fed), before it is matched: the key waits for these
  hooks, so a script hooks it only while it needs it (the Vim mode while
  recording a macro) and removes the hook then. Observe only.
- An error in a script function drops the keys `feedkeys` queued and not
  yet matched (typed keys stay), as Vim flushes typeahead; `beep()` does
  the same without a message (a failed motion stops a macro).
- **Hooks only read** (doc/design/text-editor.md section 10): a hook's
  commands may read (queries, options, mappings: `read_only` in
  `crates/temzdf-server/src/session/script.rs`, what conditions may do
  too), show a `message`, subscribe (`on`) and `schedule(fn)`; anything else fails ("change
  things in schedule(fn)"). `schedule(fn)` runs `fn` at the **safe
  point**: after the current invocation and its hooks, before the next
  key, in the order scheduled (named so, not `defer`, by the user: Go's
  `defer` runs last-in first-out). `InvokePost` and `SessionReady` hooks
  and scheduled functions may act themselves.
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
- `dispatch(async (ctx) => ...)` with `ctx.action` and `ctx.atomic(fn)`;
  `atomic(fn)`.
- Globals: `setTimeout`, `clearTimeout`, `console`.

Mapped functions get `{ pane, paneKind, floating, mode, keys, count }`
(`keys`: the keys that ran it). Every mapping is a function, so every
mapped key calls the host (a round trip costs 10-15 µs); keys no mapping
could take call nothing.

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
