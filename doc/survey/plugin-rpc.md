# RPC for plugins in other runtimes

Question (2026-11-19): a built-in plugin could define a protocol over
some RPC transport (msgpack-rpc, say) so that plugins can be written in
any runtime, a kind of FFI across processes. Which protocols suit that,
and how do they handle the awkward cases: values that do not serialize
(closures, objects with identity) and a peer that crashes or hangs?

Status: research only, no decision. Mostly from the protocols'
documentation as known before this date; Cap'n Web's was looked up on
the date. **[V]** verified on the date, **[K]** known, not re-checked,
**[?]** not confirmed in what was read.

## A constraint first

The script host runs in a WASI sandbox (`doc/design/script-host.md`): no
sockets and no processes. A bridge to external runtimes cannot be a
JavaScript plugin as things stand; it would live in the server (Rust), or
the host would need that capability given to it. Scripts also have no way
to start a program yet (no such command in the registry).

## Calling by name: functions never cross

- **Neovim's msgpack-rpc** (remote plugins, `nvim --embed`, the API
  clients) **[K]**:
  - Three message kinds: request, response, notification.
  - Functions do not cross. A plugin registers functions by name, and
    Neovim calls them with `rpcrequest(chan, "name", ...)`. Events such as
    `nvim_buf_attach` arrive as notifications.
  - **Reentrancy**: while Neovim waits in `rpcrequest`, it still serves
    the peer's requests to it, so neither side deadlocks.
  - **A dead peer**: the channel closes, and requests waiting on it fail
    with "channel closed".
  - **API metadata**: `nvim_get_api_info` lists the functions and types;
    clients in other languages generate bindings from it. temzdf does the
    same for TypeScript, generating `.d.ts` from the command registry
    (`crates/temzdf-script/src/types.rs`).
- **denops.vim** (Vim and Neovim plugins in Deno) **[K]**:
  - Named dispatcher methods.
  - `lambda.register(fn)` gives a function an id, which Vim script calls
    back through.
  - A crashed Deno server is restarted and the plugins loaded again.
- **JSON-RPC 2.0 and LSP** **[K]**:
  - No closures: a side that needs a call back sends a request of its own
    (server-to-client requests).
  - Worth taking:
    - `initialize` exchanges capabilities;
    - the lifecycle is `initialize`, `shutdown`, `exit`;
    - `$/cancelRequest` cancels;
    - progress is tied to a token.
  - **A dead server**: VS Code restarts a language server a limited number
    of times (five in three minutes), then gives up and tells the user.

## Passing functions and objects by reference

The model for closures:

- **Cap'n Proto RPC** **[K]**:
  - Functions and objects cross as **capabilities**: references to an
    object living in the other process, passed in arguments and results.
  - Lifetimes are reference counted: the holder sends `Release`.
  - **On disconnect, every capability over that connection breaks**;
    calls on them fail with `DISCONNECTED`.
  - Promise pipelining: calls on a result can be sent before it arrives,
    saving round trips.
- **Cap'n Web** (Cloudflare, 2025), the same ideas made JavaScript-native:
  - A function or `RpcTarget` passed over RPC arrives as a stub; calling
    the stub makes an RPC back to its owner **[V]**.
  - Stubs are released with `Symbol.dispose` (`using`); one not disposed
    leaks on the other side. Stubs received as call arguments are disposed
    automatically when the call completes. A target's own `Symbol.dispose`
    runs when every stub to it is gone **[V]** (from an unofficial mirror
    of its docs).
  - `stub.onRpcBroken(cb)` reports a stub made unusable by a dropped
    connection **[?]** (unofficial mirror only).
  - The best model for how this should look from JavaScript.
- **Comlink** (Web Workers) **[K]**: `proxy(fn)` passes a function by
  reference; `releaseProxy` releases it.
- **VS Code's extension host** **[K]**:
  - Extensions run in another process. A function given to
    `registerCommand` and the like stays there; the main process gets a
    numeric handle.
  - Lifetimes follow `Disposable`s (unregistering).
  - **When the extension host crashes**, everything it registered goes,
    "Extension host terminated unexpectedly" is shown, and it can be
    restarted.

## A peer that dies or hangs

- **Erlang's links and monitors** **[K]**:
  - A process's death arrives as a message (`DOWN`).
  - Supervisors hold the restart policy: how often, and how many times.
  - The origin of treating death as an ordinary event.
- **Java RMI's distributed GC** **[K]**: references are leases. One not
  renewed in time is taken as a dead holder and collected. This works
  even where a broken connection cannot be detected.
- **D-Bus** **[K]**: a peer's name vanishing is announced
  (`NameOwnerChanged`).

## What temzdf already has

The script host's protocol (`doc/design/script-host.md`) already settles
several of these questions for one in-process peer:

- **Reentrancy**: while the session waits for the host, it keeps
  answering the host's requests (the deadlock rules), as Neovim does.
- **Context tokens**: commands of finished calls are refused.
- **Generations**: messages from a replaced host are ignored.
- **A host that ends** (a trap, the memory cap) leaves the session
  running without scripts until a reload.
- **Registrations**: mappings and hooks live in the host, and the session
  holds ids for them.

## If temzdf did this

- **Transport**: msgpack-rpc, as Neovim; libraries exist for most
  languages.
- **Closures**: functions stay in their owner's table and cross as **ids**
  (Neovim, VS Code, denops).
  - A function's lifetime is that of the registration it was given to (a
    mapping, a hook, a command), as VS Code's `Disposable`s; simpler than
    counting references one by one.
  - Functions given as arguments for one call are valid only during that
    call, as Cap'n Web's auto-disposed stubs.
- **A dead peer**: as VS Code's extension host.
  - Everything that connection registered goes at once, so no mapping
    calls a function that no longer exists.
  - Requests waiting on it fail with one known error.
  - The user is told, and restarts are limited in number and spaced out.
  - This can share the shape of the script host's "ended" handling.
- **A hung peer**: mapped functions run synchronously
  (`doc/design/scripting.md`, "Execution model"), so a peer that never
  answers holds the session.
  - Requests get a deadline.
  - Alternatively, external plugins may only be called asynchronously
    (notifications).
- **Reentrancy**: the peer's requests are served while waiting for its
  answer, as the script host's deadlock rules already do.
- **API metadata**: a language-neutral list of commands and their types,
  generated from the registry as the `.d.ts` is (Neovim's `api_info`), so
  other languages can generate bindings.
- **Capabilities**: an `initialize`-style exchange (LSP) to version the
  protocol.

## Sources

- Cap'n Web: github.com/cloudflare/capnweb (README), the Cloudflare blog
  post "Cap'n Web: a new RPC system for browsers and web servers", an
  unofficial mirror of its agent-skill docs (mcpservers.org)
- Cap'n Proto: capnproto.org (RPC protocol); the capnproto mailing list
  on detecting disconnects
- Neovim: `:help msgpack-rpc`, `:help api`, `:help remote-plugin`
- denops.vim: github.com/vim-denops/denops.vim
- LSP: the Language Server Protocol specification (lifecycle,
  cancellation, progress); VS Code's `vscode-languageclient` restart
  policy
- Comlink: github.com/GoogleChromeLabs/comlink
- Erlang: the Erlang reference manual (links and monitors), OTP design
  principles (supervisors)
- Java RMI: the RMI specification (distributed garbage collection)
