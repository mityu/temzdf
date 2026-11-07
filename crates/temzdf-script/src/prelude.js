// The JavaScript side of temzdf's scripting API, evaluated once before the
// configuration. It builds `globalThis.__temzdf` (what `import ... from
// "temzdf"` exports) on top of the host's native functions in `__host`
// (crates/temzdf-script/src/host.rs). Values cross into Rust as JSON.
(() => {
  "use strict";
  const host = globalThis.__host;
  delete globalThis.__host;

  const errorJson = (e) =>
    JSON.stringify({
      message: e instanceof Error ? `${e.name}: ${e.message}` : String(e),
      stack: e instanceof Error && e.stack ? String(e.stack) : null,
    });

  // The context token of the synchronous call running now (a mapped or hook
  // function, or the configuration at startup), else the task's inside
  // its ctx.atomic(fn); null elsewhere.
  let atomicToken = null;
  const current = () => JSON.parse(host.current()) ?? atomicToken;

  // A command answering later answers `{ $promise: n }`: a Promise, settled
  // when the session sends ToHost::Resolve.
  function promised(result) {
    if (result === null || typeof result !== "object" || typeof result.$promise !== "number") return result;
    return new Promise((resolve, reject) =>
      host.awaitPromise(result.$promise, (ok, json) => (ok ? resolve(JSON.parse(json)) : reject(new Error(json)))),
    );
  }

  function request(token, op, payload) {
    if (token === null) {
      throw new Error(
        "no context: commands outside a mapped or hook function go through the ctx a dispatched task receives",
      );
    }
    return JSON.parse(host.request(JSON.stringify(token), op, JSON.stringify(payload)));
  }

  const camel = (name) => name.replace(/-([a-z])/g, (_, c) => c.toUpperCase());
  // `obj.input.deleteWord = value` for the command "input.delete-word": a
  // namespace (before a dot) is an object of its own.
  function put(obj, name, value) {
    const parts = name.split(".").map(camel);
    const last = parts.pop();
    for (const p of parts) obj = obj[p] ??= {};
    obj[last] = value;
  }
  function deepFreeze(obj) {
    for (const v of Object.values(obj)) if (typeof v === "object") deepFreeze(v);
    return Object.freeze(obj);
  }

  // The session's commands, as functions issuing them under `tokenOf()`.
  function commands(tokenOf, cmd = {}) {
    for (const name of JSON.parse(host.commands())) {
      put(cmd, name, (...args) => promised(request(tokenOf(), "command", { name, args })));
    }
    return deepFreeze(cmd);
  }
  // Filled once the session has said which commands it has (before the
  // configuration runs).
  const globalCmd = {};
  // `action.split("right")` describes a command as data: mappings to such
  // descriptions (or arrays of them) run in the session without a script.
  const globalAction = {};
  const ACTION = Symbol.for("temzdf.action");
  globalThis.__temzdfLoad = () => {
    if (Object.isFrozen(globalCmd)) return;
    commands(current, globalCmd);
    for (const name of JSON.parse(host.commands())) {
      put(globalAction, name, (...args) => Object.freeze({ [ACTION]: true, name, args }));
    }
    deepFreeze(globalAction);
  };
  const isAction = (a) => a !== null && typeof a === "object" && a[ACTION] === true;

  // Functions by the id they were registered under, for keymap.get.
  const registered = [];

  // Register `fn` as a mapped or hook function, or (`condition`) as a
  // mapping's `when` or a mode's `keepalive`, which gives a boolean:
  // synchronous only.
  function register(fn, what, condition = false) {
    if (typeof fn !== "function") throw new TypeError(`${what}: expected a function`);
    const id = host.register((argsJson) => {
      const r = fn(...JSON.parse(argsJson));
      if (r !== null && typeof r === "object" && typeof r.then === "function") {
        throw new TypeError(
          condition
            ? `${what}: conditions must be synchronous`
            : `${what}: mapped and hook functions must be synchronous (use dispatch for asynchronous work)`,
        );
      }
      // A hook's answer (`{ cancel: true, reason }`) goes back as JSON.
      return condition ? !!r : r === undefined ? undefined : JSON.stringify(r);
    });
    registered[id] = { fn, condition };
    return id;
  }

  // A function keymap.get gave back: its id again, not a new one.
  const funcId = (f, what, condition) => {
    const id = registered.findIndex((r) => r !== undefined && r.fn === f && r.condition === condition);
    return id >= 0 ? id : register(f, what, condition);
  };

  function atomicWith(token, fn) {
    if (typeof fn !== "function") throw new TypeError("atomic: expected a function");
    const t = JSON.stringify(token);
    host.atomicBegin(t);
    // The plain `cmd` works inside too, under the task's token.
    const outer = atomicToken;
    atomicToken = token;
    try {
      const r = fn();
      if (r !== null && typeof r === "object" && typeof r.then === "function") {
        throw new TypeError("atomic: the function must be synchronous");
      }
      return r;
    } finally {
      atomicToken = outer;
      host.atomicEnd(t);
    }
  }

  const temzdf = {
    cmd: globalCmd,
    action: globalAction,
    keymap: Object.freeze({
      // keymap.add(mode | modes, lhs, rhs, { nowait, desc, when, namespace,
      // priority }): rhs is a synchronous function, an action, or an array of
      // actions; `when` a synchronous function of the key's context.
      add(modes, lhs, rhs, options = {}) {
        const payload = {
          modes,
          keys: lhs,
          nowait: !!options.nowait,
          desc: options.desc ?? null,
          when: options.when == null ? null : funcId(options.when, `keymap.add(${lhs}) when`, true),
          namespace: options.namespace ?? null,
          priority: options.priority ?? 0,
        };
        if (typeof rhs === "function") {
          payload.func = funcId(rhs, `keymap.add(${lhs})`, false);
        } else {
          const list = Array.isArray(rhs) ? rhs : [rhs];
          if (!list.every(isAction)) throw new TypeError(`keymap.add(${lhs}): expected a function, an action or actions`);
          payload.actions = list.map((a) => ({ name: a.name, args: a.args }));
        }
        request(current(), "map", payload);
      },
      // keymap.remove(mode | modes, lhs, { namespace }): that namespace's
      // mappings, or all of them.
      remove(modes, lhs, options = {}) {
        request(current(), "unmap", { modes, keys: lhs, namespace: options.namespace ?? null });
      },
      // keymap.clear({ mode, namespace }): every mapping of the mode and the
      // namespace; one left out is not a filter.
      clear(filter = {}) {
        request(current(), "clearMappings", { mode: filter.mode ?? null, namespace: filter.namespace ?? null });
      },
      // keymap.get({ mode, namespace, lhs }): the mappings as keymap.add()
      // takes them; one left out is not a filter.
      get(filter = {}) {
        const back = (v) => (v !== null && typeof v === "object" && "func" in v ? registered[v.func]?.fn : v);
        const payload = { mode: filter.mode ?? null, namespace: filter.namespace ?? null, lhs: filter.lhs ?? null };
        return request(current(), "getMappings", payload).map((m) => ({
          ...m,
          rhs: Array.isArray(m.rhs)
            ? m.rhs.map((a) => Object.freeze({ [ACTION]: true, name: a.name, args: a.args }))
            : back(m.rhs),
          when: m.when === null ? undefined : back(m.when),
          desc: m.desc ?? undefined,
          namespace: m.namespace ?? undefined,
        }));
      },
    }),
    // defineMode(name, { fallback: "drop" | "passthrough" | (ctx, key) => void,
    // count, transient, keepalive: "mapped" | "always" | (ctx) => boolean, timeout })
    defineMode(name, props = {}) {
      if ("inherit" in props) {
        throw new TypeError("defineMode: inherit is gone; a transient mode with keepalive takes the keys of the mode below");
      }
      if ("unmapped" in props) {
        throw new TypeError("defineMode: unmapped is now fallback");
      }
      const fallback =
        typeof props.fallback === "function"
          ? funcId(props.fallback, `defineMode(${name}) fallback`, false)
          : (props.fallback ?? null);
      const keepalive =
        typeof props.keepalive === "function"
          ? funcId(props.keepalive, `defineMode(${name}) keepalive`, true)
          : (props.keepalive ?? null);
      request(current(), "defineMode", {
        name,
        fallback,
        count: props.count ?? null,
        transient: props.transient ?? null,
        keepalive,
        timeout: props.timeout ?? null,
      });
    },
    feedkeys(keys, options = {}) {
      return globalCmd.feedkeys(keys, options.remap !== false);
    },
    currentMode() {
      return globalCmd.currentMode();
    },
    // Options, as Neovim's vim.o: `o.timeoutlen = 500`.
    o: new Proxy(
      {},
      {
        get: (_, name) => (typeof name === "string" ? request(current(), "getOption", { name }) : undefined),
        set: (_, name, value) => {
          request(current(), "setOption", { name, value });
          return true;
        },
      },
    ),
    // Returns a function that stops it (a hook only while it is wanted:
    // KeyTyped hooks hold every key until they ran).
    on(event, fn, options = {}) {
      const func = register(fn, `on(${event})`);
      request(current(), "on", { event, func, nested: !!options.nested });
      return () => request(current(), "off", { event, func });
    },
    // Run `fn` at the safe point (after the invocation and its hooks), in
    // the order scheduled: what a hook does to change anything.
    schedule(fn) {
      request(current(), "schedule", { func: funcId(fn, "schedule", false) });
    },
    // Start an asynchronous task without waiting for it. It issues commands
    // through its `ctx`.
    dispatch(fn) {
      if (typeof fn !== "function") throw new TypeError("dispatch: expected a function");
      const task = host.taskStart();
      const token = { Task: task };
      const ctx = Object.freeze({
        cmd: commands(() => token),
        atomic: (f) => atomicWith(token, f),
      });
      Promise.resolve()
        .then(() => fn(ctx))
        .then(
          () => host.taskDone(task, "null"),
          (e) => host.taskDone(task, errorJson(e)),
        );
    },
    // Prompts: a text buffer (nofile, unlisted) in a pane below, its base
    // mode `input`, the prompt inline virtual text at its start (Vim's
    // command-line window). <CR> (acceptPrompt) hands the cursor's line to
    // onLine, <Esc> (cancelPrompt) or closing it otherwise calls onCancel;
    // onChange hears the cursor's line as it changes. Either way the pane
    // and the buffer go, and the focus goes back.
    openPrompt(opts = {}) {
      const c = globalCmd;
      const origin = c.focusedPane();
      const buffer = c.newBuffer({ text: opts.text ?? "", basemode: opts.mode ?? "input", attrs: ["nofile", "unlisted"] });
      let pane = null;
      try {
        pane = c.split({ side: opts.side ?? "bottom", buffer, height: opts.height ?? 1 });
      } catch (e) {
        c.deleteBuffer(buffer);
        throw e;
      }
      if (pane === null) {
        c.deleteBuffer(buffer);
        return null;
      }
      c.statusLine(false, pane);
      c.text.setOptions({ wrap: false, number: "off", cursorline: false, scrolloff: 0 }, { pane });
      const origin0 = { line: 0, character: 0 };
      if ((opts.prompt ?? "") !== "") {
        c.text.addDecoration(
          { start: origin0, end: origin0 },
          { virtText: [{ text: opts.prompt, style: opts.style ?? "Prompt" }], namespace: "prompt", pane },
        );
      }
      const last = c.text.lineCount({ pane }) - 1;
      c.text.setCursor({ line: last, character: c.text.line(last, { pane }).length }, { pane });
      prompts.set(buffer, { pane, origin, onLine: opts.onLine, onCancel: opts.onCancel, onChange: opts.onChange });
      if (promptHooks === null) {
        // Only while a prompt is open: TextChanged hooks cost every edit.
        promptHooks = [
          temzdf.on("TextChanged", (buf) => {
            if (prompts.has(buf)) {
              temzdf.schedule(() => {
                const p = prompts.get(buf);
                if (p !== undefined && p.onChange) p.onChange(promptLine(p));
              });
            }
          }),
          temzdf.on("BufferHidden", (buf) => {
            if (prompts.has(buf)) temzdf.schedule(() => closePrompt(buf)?.p.onCancel?.());
          }),
        ];
      }
      return { buffer, pane };
    },
    acceptPrompt(buffer) {
      const r = closePrompt(buffer ?? focusedBuffer());
      if (r !== null) r.p.onLine?.(r.line);
    },
    cancelPrompt(buffer) {
      const r = closePrompt(buffer ?? focusedBuffer());
      if (r !== null) r.p.onCancel?.();
    },
    isPrompt(buffer) {
      return prompts.has(buffer ?? focusedBuffer());
    },
    // Inside a mapped or hook function everything is atomic already.
    atomic(fn) {
      const token = current();
      if (token === null) throw new Error("atomic: in a dispatched task use ctx.atomic");
      return fn();
    },
  };
  // Open prompts by buffer (openPrompt), and their hooks while any is.
  const prompts = new Map();
  let promptHooks = null;
  const focusedBuffer = () => globalCmd.panes().find((p) => p.focused)?.buffer;
  // The cursor's line of a prompt (what is run, and previewed).
  const promptLine = (p) => {
    const at = globalCmd.text.cursor({ pane: p.pane });
    return globalCmd.text.line(at.line, { pane: p.pane });
  };
  // Close a prompt: its line, back to where it was opened, the buffer gone.
  function closePrompt(buffer) {
    const p = prompts.get(buffer);
    if (p === undefined) return null;
    prompts.delete(buffer);
    let line = "";
    try {
      line = promptLine(p);
    } catch {
      // Its pane is gone.
    }
    if (prompts.size === 0 && promptHooks !== null) {
      for (const off of promptHooks) off();
      promptHooks = null;
    }
    try {
      if (p.origin !== null) globalCmd.focusPane(p.origin);
    } catch {
      // That pane is gone.
    }
    try {
      globalCmd.deleteBuffer(buffer);
    } catch {
      // Gone already.
    }
    return { p, line };
  }
  globalThis.__temzdf = Object.freeze(temzdf);

  // Delays as browsers take them: not a number is 0; at most 2^31 - 1 ms.
  const delay = (ms) => Math.min(2147483647, Math.max(0, Number(ms) || 0));
  globalThis.setTimeout = (fn, ms = 0, ...args) => host.setTimer(delay(ms), () => fn(...args));
  // Anything but a timer id is ignored, as browsers do (clearTimeout(undefined)).
  globalThis.clearTimeout = (id) => {
    if (typeof id === "number") host.clearTimer(id);
  };

  const show = (v) => {
    if (typeof v === "string") return v;
    if (v instanceof Error) return v.stack ? `${v.name}: ${v.message}\n${v.stack}` : `${v.name}: ${v.message}`;
    try {
      return JSON.stringify(v) ?? String(v);
    } catch {
      return String(v);
    }
  };
  const print = (level) => (...args) => host.print(level, args.map(show).join(" "));
  globalThis.console = Object.freeze({
    log: print("log"),
    info: print("info"),
    warn: print("warn"),
    error: print("error"),
    debug: print("debug"),
  });
  globalThis.__temzdfErrorJson = errorJson;
})();
