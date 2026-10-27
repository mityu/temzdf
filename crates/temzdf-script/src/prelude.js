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
  // function, or the configuration at startup); null elsewhere.
  const current = () => JSON.parse(host.current());

  function request(token, op, payload) {
    if (token === null) {
      throw new Error(
        "no context: commands outside a mapped or hook function go through the ctx a dispatched task receives",
      );
    }
    return JSON.parse(host.request(JSON.stringify(token), op, JSON.stringify(payload)));
  }

  const camel = (name) => name.replace(/-([a-z])/g, (_, c) => c.toUpperCase());

  // The session's commands, as functions issuing them under `tokenOf()`.
  function commands(tokenOf, cmd = {}) {
    for (const name of JSON.parse(host.commands())) {
      cmd[camel(name)] = (...args) => request(tokenOf(), "command", { name, args });
    }
    return Object.freeze(cmd);
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
      globalAction[camel(name)] = (...args) => Object.freeze({ [ACTION]: true, name, args });
    }
    Object.freeze(globalAction);
  };
  const isAction = (a) => a !== null && typeof a === "object" && a[ACTION] === true;

  // Functions by the id they were registered under, for getMappings.
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
      return condition ? !!r : undefined;
    });
    registered[id] = { fn, condition };
    return id;
  }

  // A function getMappings gave back: its id again, not a new one.
  const funcId = (f, what, condition) => {
    const id = registered.findIndex((r) => r !== undefined && r.fn === f && r.condition === condition);
    return id >= 0 ? id : register(f, what, condition);
  };

  function atomicWith(token, fn) {
    if (typeof fn !== "function") throw new TypeError("atomic: expected a function");
    const t = JSON.stringify(token);
    host.atomicBegin(t);
    try {
      const r = fn();
      if (r !== null && typeof r === "object" && typeof r.then === "function") {
        throw new TypeError("atomic: the function must be synchronous");
      }
      return r;
    } finally {
      host.atomicEnd(t);
    }
  }

  const temzdf = {
    cmd: globalCmd,
    action: globalAction,
    // map(mode | modes, lhs, rhs, { nowait, desc, when, namespace,
    // priority }): rhs is a synchronous function, an action, or an array of
    // actions; `when` a synchronous function of the key's context.
    map(modes, lhs, rhs, options = {}) {
      const payload = {
        modes,
        keys: lhs,
        nowait: !!options.nowait,
        desc: options.desc ?? null,
        when: options.when == null ? null : funcId(options.when, `map(${lhs}) when`, true),
        namespace: options.namespace ?? null,
        priority: options.priority ?? 0,
      };
      if (typeof rhs === "function") {
        payload.func = funcId(rhs, `map(${lhs})`, false);
      } else {
        const list = Array.isArray(rhs) ? rhs : [rhs];
        if (!list.every(isAction)) throw new TypeError(`map(${lhs}): expected a function, an action or actions`);
        payload.actions = list.map((a) => ({ name: a.name, args: a.args }));
      }
      request(current(), "map", payload);
    },
    // unmap(mode | modes, lhs, { namespace }): that namespace's mappings,
    // or all of them.
    unmap(modes, lhs, options = {}) {
      request(current(), "unmap", { modes, keys: lhs, namespace: options.namespace ?? null });
    },
    // defineMode(name, { unmapped: "drop" | "passthrough", count, transient,
    // keepalive: "mapped" | "always" | (ctx) => boolean, timeout })
    defineMode(name, props = {}) {
      if ("inherit" in props) {
        throw new TypeError("defineMode: inherit is gone; a transient mode with keepalive takes the keys of the mode below");
      }
      const keepalive =
        typeof props.keepalive === "function"
          ? funcId(props.keepalive, `defineMode(${name}) keepalive`, true)
          : (props.keepalive ?? null);
      request(current(), "defineMode", {
        name,
        unmapped: props.unmapped ?? null,
        count: props.count ?? null,
        transient: props.transient ?? null,
        keepalive,
        timeout: props.timeout ?? null,
      });
    },
    // getMappings({ mode, namespace }): mappings as map() takes them.
    getMappings(filter = {}) {
      const back = (v) => (v !== null && typeof v === "object" && "func" in v ? registered[v.func]?.fn : v);
      return request(current(), "getMappings", { mode: filter.mode ?? null, namespace: filter.namespace ?? null }).map(
        (m) => ({
          ...m,
          rhs: Array.isArray(m.rhs)
            ? m.rhs.map((a) => Object.freeze({ [ACTION]: true, name: a.name, args: a.args }))
            : back(m.rhs),
          when: m.when === null ? undefined : back(m.when),
          desc: m.desc ?? undefined,
          namespace: m.namespace ?? undefined,
        }),
      );
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
    on(event, fn, options = {}) {
      request(current(), "on", { event, func: register(fn, `on(${event})`), nested: !!options.nested });
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
    // Inside a mapped or hook function everything is atomic already.
    atomic(fn) {
      const token = current();
      if (token === null) throw new Error("atomic: in a dispatched task use ctx.atomic");
      return fn();
    },
  };
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
