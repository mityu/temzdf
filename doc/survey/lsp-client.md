# An LSP client on VS Code's LSP libraries

Question (2026-11-16): can a future LSP client reuse the npm libraries VS
Code's LSP support is built from? Which part belongs in the scripts and
which part in Rust?

Status: research only, no decision.

- The research was done elsewhere and handed over. The facts about the npm
  packages (names, entry points, what they import) come from it and were
  not checked again.
- What it says about this project was checked against the code and the
  design documents on that date.
- Crate versions are as the research reported them. Check them before
  relying on them.

## The packages

The names `vscode-languageclient-protocol` and `vscode-languageclient-jsonrpc`
do not exist on npm. The real ones:

| Package | What it is |
|---|---|
| `vscode-languageserver-protocol` | LSP's requests, notifications and types; its root export is `common` |
| `vscode-jsonrpc` | JSON-RPC connections: `MessageReader`/`MessageWriter`, request ids, cancellation, progress |
| `vscode-languageclient` | VS Code's whole client; requires the `vscode` module at run time, so unusable outside VS Code |
| `vscode-languageserver-types` | The types alone |

- `vscode-languageserver-protocol`'s `common` re-exports
  `vscode-jsonrpc`'s `common` (`lib/common/api.js`). Bundling the
  protocol package therefore bundles `vscode-jsonrpc` too. Only
  `vscode-languageserver-types` with method names as strings avoids it.
- Each package has three entry points:
  - the root (`common`), independent of Node;
  - `/node`: Node streams, `child_process`, `net`;
  - `/browser`: the browser RAL, Workers, `MessagePort`.
- Only the root is usable here. There is no `/common` subpath: imports
  are from `"vscode-jsonrpc"` and `"vscode-languageserver-protocol"`.
- `vscode-jsonrpc`'s `common` reaches its platform through a RAL
  ("runtime abstraction layer") that the embedder installs with
  `RAL.install()`:
  - timers (`setTimeout`, `setImmediate`, `setInterval`);
  - `console`;
  - JSON encoders and decoders, and a message buffer. These are only
    used by the stream-based reader and writer.
- `/browser` calls `new TextEncoder()` when its module loads, which
  fails in QuickJS.

## This project's constraints

From `doc/design/script-host.md`, `doc/design/scripting.md` and
`doc/design/text-editor.md`.

### The sandbox

- Scripts run in QuickJS inside a WASI guest. The guest sees:
  - its memory, stderr and clocks;
  - the file system, read-only;
  - four imports (`temzdf.send`, `recv`, `recv_into`, `interrupts`).
- A script cannot start processes, open sockets or write to a pipe. So
  the language server's process and its stdio are Rust's, whichever
  option is chosen.
- The server has no general way to run a process yet. Terminal panes run
  programs on a pty (`crates/temzdf-term`), which is not a pipe carrying
  messages. A new primitive is needed.
- The project uses no async runtime: it uses threads and channels. tokio
  is in `Cargo.lock` only through `wasmtime-wasi`.

### What JavaScript has

- Available:
  - `setTimeout` and `clearTimeout` (the prelude, on the host's timers);
  - `console`, writing to the session log;
  - Promises and `async`/`await` in dispatched tasks;
  - `AbortController` and `AbortSignal` (`runtime/polyfills.ts`, from
    `abortcontroller-polyfill`).
- Missing:
  - `setImmediate` and `setInterval`;
  - `TextEncoder` and `TextDecoder`.
- Globals QuickJS lacks go in `runtime/polyfills.ts`, from a maintained
  npm package where one exists. The RAL's timers can be made from
  `setTimeout`. With a message-oriented reader and writer (below), the RAL
  needs no encoders and no message buffer.

### Asynchrony

The host is synchronous only where the design wants it:

- **Mapped and hook functions are synchronous** on purpose. Input waits
  for them, and `<C-c>` interrupts them.
- **Asynchronous work goes in `dispatch(async (ctx) => ...)`.** A task
  issues commands through `ctx.cmd`. `ctx.atomic(fn)` runs a synchronous
  part with everything else held.
- **Answers that come later already exist.**
  - A command that waits for something (`readClipboard`) gets
    `{ "$promise": n }` at once, and `ToHost::Resolve` settles it later.
  - Settled answers are delivered when nothing runs, as timers fire.
  - An answer that arrives during a nested wait is queued, and the host
    settles queued answers before it waits again.
- Messages from a language server can travel the same path: a new
  `ToHost` message delivered and queued as `Resolve` is. Only the message
  kind and its routing are new.
- **Context tokens** decide what handlers may do.
  - Callbacks that run when nothing else runs (timers, settled Promises)
    have no call context. Global `cmd.*` throws "no context" there.
  - Code that reacts to a server message therefore acts through a
    dispatched task's `ctx`, as any timer callback does.
  - Edits computed from a reply (formatting, code actions) go through
    `ctx.atomic`. That way the buffer version cannot change between
    checking it and applying the edits.

### Document synchronisation

The editor framework gives scripts what LSP's document synchronisation
needs, in order:

- Positions are `{ line, character }` in UTF-16, LSP's names and its
  default encoding (`doc/design/text-editor.md`, section 3).
- **`TextChanged(buffer, { changes, version, cause, invocation })`**
  (`doc/design/text-editor.md`, section 9) provides:
  - `changes` in LSP's form (`{ range, rangeLength, text }`, ranges in
    positions before the change);
  - a version number;
  - one event per editing primitive, undo or redo;
  - synchronous delivery: the event arrives before the editing command
    returns.
- A script can forward `didChange` from this hook in order.
- **Open point:** hooks only read (`doc/design/text-editor.md`,
  section 10). A hook may read, show a `message`, subscribe and
  `schedule`. Sending a message to a language server changes nothing in
  the session, but it is a side effect. It needs a rule:
  - either such a send is allowed in hooks, like `message`;
  - or it is queued and sent at the safe point.

### Shipping the libraries

- npm packages are added to `crates/temzdf-script/runtime/package-lock.json`
  (`npm install --package-lock-only`). Nix builds them, and
  `crates/temzdf-script/build.rs` bundles them with esbuild.
- The bar for adding a package: widely used, maintained, plain ES code.
  `vscode-jsonrpc` and `vscode-languageserver-protocol` qualify.
- A user's configuration imports only `temzdf`, bundled `temzdf/NAME`
  modules and relative paths. So the client would be a bundled module,
  `temzdf/lsp` (`MODULES` in `build.rs`).
- Startup cost:
  - A bundled module is loaded when something imports it.
  - It registers nothing on import: modules are keysets, enabled by
    `enable()`.
  - `defaults.ts` imports only what it enables.
  - So a user without LSP does not pay to parse it.

### The project's split between Rust and scripts

- The Rust part is **a framework of primitives**, as Emacs' C core is to
  Emacs Lisp (`doc/design/text-editor.md`, section 1).
- Features are scripts: the Vim mode is a script, as Evil is in Emacs.
- For questions like this, the first reference is "what does Vim or
  Neovim do" (`doc/design/scripting.md`). Neovim's LSP client (`vim.lsp`)
  is Lua, built on the core's processes and RPC (`vim.system`,
  `vim.lsp.rpc` over libuv). Emacs' eglot is Emacs Lisp, built on
  `make-process` and `jsonrpc.el`.

A client written in JavaScript over a process primitive in Rust matches
this. A client in Rust puts a feature in the core.

## Options

### Option 1: `vscode-jsonrpc` in JavaScript over a Rust transport

```text
language server (child) <--stdio--> Rust: process, Content-Length framing
        |  ToHost: a message (JSON body)        ^ a command: send a message
        v                                       |
JavaScript (temzdf/lsp): vscode-jsonrpc MessageConnection
        over a MessageReader / MessageWriter made on the session link
```

- Rust:
  - starts and stops the process;
  - reads and writes `Content-Length` frames;
  - logs the server's stderr.
- `vscode-jsonrpc` handles the JSON-RPC: request ids, `$/cancelRequest`,
  `$/progress`, errors, and requests from the server.
- The client logic is written in JavaScript on
  `vscode-languageserver-protocol`'s definitions:
  - `initialize`;
  - capabilities;
  - document synchronisation;
  - answering the server's requests.

Pros:

- The canonical LSP types and JSON-RPC behaviour; the hard parts of
  JSON-RPC are not rewritten.
- `MessageReader` and `MessageWriter` are public extension points, so
  Node is not needed.
- The Rust part is small and testable (process and framing).
- It matches the project's split between Rust and scripts, and Neovim's
  and Emacs' clients (above).
- The pieces it needs already exist:
  - delivery of messages that come later (as `Resolve`);
  - tasks and `atomic` for reacting to them;
  - `TextChanged` in LSP's form;
  - bundled modules loaded on import.

Cons:

- A small RAL to write: timers made from `setTimeout`, and `console`.
- `vscode-jsonrpc` assumes `setImmediate` and Promises. How its scheduling
  meets the host's (timers and settled answers run when nothing else
  does) needs testing.
- The rule for sending from hooks (above).
- `vscode-languageserver-protocol` defines messages; it is not a client.
  The client logic is ours to write.

### Option 2: JSON-RPC in Rust, the protocol's types in JavaScript

Rust does the process, the framing and JSON-RPC (ids, cancellation,
progress). JavaScript uses the protocol package only for its types and
method definitions.

Cons:

- JSON-RPC is rewritten in Rust, with every subtlety of cancellation,
  progress and the server's requests.
- The protocol package bundles `vscode-jsonrpc` anyway, so nothing is
  saved in JavaScript.
- The types exist twice (`lsp-types` in Rust, the protocol in
  JavaScript).
- Its behaviour drifts from upstream.

Not recommended: it takes neither option 1's reuse nor option 3's
libraries.

### Option 3: the client in Rust, handlers in JavaScript

Rust owns everything up to the server's requests: the process, the
framing, JSON-RPC, `initialize`, document synchronisation. JavaScript gets
a thin API:

```ts
import { lsp } from "temzdf";
lsp.onNotification("textDocument/publishDiagnostics", (params) => { /* ... */ });
lsp.onRequest("workspace/configuration", (params) => /* ... */);
const hover = await lsp.request("textDocument/hover", params);
```

Rust building blocks (versions as the research reported):

- `async-lsp` 0.2.4: supports clients. It is on `lsp-types` 0.95, older
  than 0.97 and LSP 3.18, and it needs an async runtime, which the project
  does not use.
- `lsp-types` 0.97 with `lsp-server` 0.10's `Message` and framing, on std
  threads. `lsp-server` is meant for servers, but `Message::read` and
  `write` work for a client.

Pros:

- Rust controls order, back-pressure, timeouts and restarts in one
  place.
- A small JavaScript bundle.
- Tested with a fake server in Rust.

Cons:

- Against the project's split (above): a feature in the core, while
  editing features are scripts.
- More Rust.
- Two copies of the types.
- Answering the server's requests still crosses to JavaScript.
- Its main argument, ordering document changes, is already met by
  `TextChanged`.

### Rejected

- **`vscode-languageclient` in QuickJS.** Its `common` and `browser`
  entry points need the `vscode` API; shimming it is not realistic.
- **An outside Node process running the client.** It brings Node, which
  the project avoids (`doc/design/scripting.md`, "Where scripts run").
- **TCP or WebSocket to the server.** Most servers speak only stdio, so a
  Rust bridge is needed anyway.

## Comparison

| | 1: JSON-RPC in JS | 2: JSON-RPC in Rust | 3: client in Rust |
|---|---|---|---|
| Process, stdio, framing | Rust | Rust | Rust |
| JSON-RPC | `vscode-jsonrpc` | written by us | a Rust library |
| Client logic | JavaScript | JavaScript | Rust |
| LSP types | canonical (JS) | twice | `lsp-types` (JS optional) |
| Bundle | larger, loaded only on import | medium | small |
| Rust to write | small | large | medium to large |
| Fits the project's split | yes | partly | no |
| Main risk | scheduling of `vscode-jsonrpc` in the host | JSON-RPC bugs | crate and type versions |

## Leaning

Option 1, as the research also recommended for a client written in
JavaScript. Two refinements from the project's side:

- **Make the Rust part a general primitive, not an LSP one.** Neovim
  offers this as `jobstart` with `rpc`, or `vim.system`.
  - Shape: a process with a message channel, the framing chosen
    (`Content-Length` for LSP; lines, or raw bytes, later).
  - Formatters, linters and other tools reuse it.
  - The names are provisional.
- **Write the client logic as a bundled module (`temzdf/lsp`) on the
  existing pieces:**
  - `TextChanged` for `didChange`;
  - tasks with `ctx.atomic` for applying edits;
  - the `Resolve`-like delivery for server messages.

Decisions to take with the user before planning:

- the process primitive's API: arguments, environment, working
  directory, framing, stderr, exit status;
- whether hooks may send messages (above);
- how much of the client ships (which features, which servers
  configured by default).

## Testing (when built)

- Rust: the process primitive and framing against a fake server (a
  thread and a pipe, or a small test binary).
- JavaScript: the client against a fake transport in the host's tests
  (`crates/temzdf-script/tests/host/cases.rs`).
- End to end: `crates/temzdf/tests/tui.rs` with a small fake language
  server, checking diagnostics or hover on screen.
