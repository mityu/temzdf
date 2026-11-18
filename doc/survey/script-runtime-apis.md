# Web and Node APIs for the script host

Question (2026-11-18): the script host offers scripts QuickJS's own
built-ins, a few polyfills (`crates/temzdf-script/runtime/polyfills.ts`)
and `"temzdf"`. Which crates or packages could add Web APIs (`URL`,
`TextEncoder`, streams, `fetch`, ...) or Node's modules, and would they fit
a host that is rquickjs 0.14 (quickjs-ng 0.16.2), synchronous, built for
`wasm32-wasip1` and run in wasmtime (`doc/design/script-host.md`)?

Status: research only, no decision. Read from the projects' READMEs,
manifests and crates.io metadata on that date; nothing was built into the
guest. **[V]** verified there, **[I]** inference.

## What limits the choice

- `doc/design/scripting.md`, decision 26 (2026-10-26): a configuration
  imports relative paths and `"temzdf"`; no npm packages, no Node APIs.
  Offering Node's modules to scripts changes that decision. The shipped
  scripts may bundle npm packages (`runtime/package-lock.json`, bundled by
  esbuild), which is how `AbortController` comes in today.
- The guest is synchronous rquickjs with the host's own event loop, not
  rquickjs's async runtime and tokio.
- The guest is a WASI sandbox: the file system read-only, no processes, no
  network. An API that touches the operating system cannot work inside it;
  it needs a request to the server, as running external programs would.

## rquickjs crates

| Crate | What | rquickjs | Notes |
|---|---|---|---|
| `llrt_modules` (AWS, from the LLRT runtime) | Node and WinterTC APIs, one crate per module: `abort`, `assert`, `buffer`, `child_process`, `console`, `crypto`, `events`, `fetch`, `fs`, `net`, `path`, `process`, `stream`, `string_decoder`, `timers`, `url`, `util`, `zlib`, ... (most marked partial) | 0.14 on `main` [V] | On crates.io as 0.8.1-beta (its README still says it is not); `main` is 0.10.0-beta [V]. Active (pushed that day) [V]. Built for rquickjs's async runtime and tokio; some modules need an `init` call before use [V]. The pure ones (`path`, `url`, `buffer`, `string_decoder`, `events`, `util`, `abort`) may build for `wasm32-wasip1`; those using processes, sockets or writes cannot work in the sandbox [I] |
| `wasm-rquickjs` (Golem) | Web APIs (`fetch`, `Headers`, `Request`, `Response`, `Blob`, `URL`, streams, timers, `TextEncoder`/`TextDecoder`, ...) and many `node:` modules, in async Rust | 0.14, quickjs-ng 0.16.2, wasmtime 50 [V] | The closest to temzdf's stack: QuickJS in WebAssembly offering Web APIs. But it is a generator wrapping JavaScript into a WASI component (component model, wasip2/p3, `fetch` through `wasi:http`), not a library for a wasip1 guest [V]; whether its runtime crate can be used alone is unchecked [I]. Worth reading for how the APIs map onto host imports |
| `ferrijs-std` | WHATWG streams, events, `AbortController`, crypto, compression, file system | 0.14, tokio [V] | Default features pull in the file system, networking and system information [V]; young project, maturity unknown |
| `rquickjs-extra` (the rquickjs organisation) | timers, `URL`, console, os, sqlite | unchecked | Few modules |
| `javy` (Bytecode Alliance) | console, `TextEncoder`/`TextDecoder`, JSON; a plugin API | 0.12 [V] | Older rquickjs; does not fit as is |

## Other engines (reference only)

- `boa_runtime`: console, `URL`, timers and others for boa
  (`doc/survey/script-host-transport.md` compares boa with the host).
- `deno_web`, `deno_url`, `deno_fetch`, ...: tied to V8.

## Plain JavaScript polyfills

npm packages in `crates/temzdf-script/runtime/`, bundled by esbuild as
`abortcontroller-polyfill` is now: `whatwg-url` (`URL`),
`web-streams-polyfill` (streams), `fast-text-encoding` (`TextEncoder`,
`TextDecoder`), `event-target-polyfill` (`EventTarget`), `core-js`
(language built-ins). They run in QuickJS as they are [I], add no Rust
dependency and leave the sandbox as it is.

## Reading

- APIs that need no operating system (`URL`, `TextEncoder`, streams,
  `EventTarget`) are simplest as JavaScript polyfills in the shipped
  scripts.
- APIs that touch the operating system (`fetch`, writing files, processes)
  cannot work inside the guest whatever the crate; they would be server
  commands, decided with the API for external programs.
- The Rust crates are more useful as references than as parts:
  `wasm-rquickjs` for Web APIs over host imports, `llrt_child_process` for
  Node's process API over native code (for temzdf, a request to the
  server).
- Node compatibility matters most if npm libraries are to run as they are:
  `vscode-jsonrpc` (`doc/survey/lsp-client.md`) expects Node's `stream`,
  `child_process` and `events`.
- Next step if wanted: build a pure `llrt_modules` crate (`path`, `buffer`)
  into the guest to see whether it compiles for `wasm32-wasip1` and works
  without the async runtime.
