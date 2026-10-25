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
  globalThis.__temzdfLoad = () => {
    if (!Object.isFrozen(globalCmd)) commands(current, globalCmd);
  };

  // Register `fn` as a mapped or hook function: synchronous only.
  function register(fn, what) {
    if (typeof fn !== "function") throw new TypeError(`${what}: expected a function`);
    return host.register((argsJson) => {
      const r = fn(...JSON.parse(argsJson));
      if (r !== null && typeof r === "object" && typeof r.then === "function") {
        throw new TypeError(
          `${what}: mapped and hook functions must be synchronous (use dispatch for asynchronous work)`,
        );
      }
    });
  }

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
    on(event, fn, options = {}) {
      request(current(), "on", { event, func: register(fn, `on(${event})`), nested: !!options.nested });
    },
    // Provisional: run `fn` for the session prefix followed by `key`.
    prefixMap(key, fn) {
      request(current(), "prefixMap", { key, func: register(fn, `prefixMap(${key})`) });
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

  globalThis.setTimeout = (fn, ms = 0, ...args) => host.setTimer(Math.max(0, Number(ms) || 0), () => fn(...args));
  globalThis.clearTimeout = (id) => host.clearTimer(id);

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
