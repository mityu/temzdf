// The JavaScript side of temzdf's scripting API, evaluated once before the
// configuration (as a script: crates/temzdf-script/build.rs wraps it in a
// function with esbuild). It builds `globalThis.__temzdf` (what `import ...
// from "temzdf"` exports) on top of the host's native functions in
// `__host` (crates/temzdf-script/src/host.rs). Values cross into Rust as
// JSON. What it builds is typed as the `temzdf` module of
// ts-types/temzdf-core.d.ts, so the declarations and this agree.
// biome-ignore lint/suspicious/noRedundantUseStrict: evaluated as a script, not a module.
"use strict";

/** The native functions host.rs gives this prelude (`install`), and only
 * it: taken off the globals at once. */
type Host = {
  /** The context token of the synchronous call running now (JSON; "null": none). */
  current(): string;
  /** The actions' names (JSON, a string[]). */
  commands(): string;
  /** A request to the session under `token` (JSON), its payload JSON; the answer (JSON). */
  request(token: string, op: string, payload: string): string;
  /** A function the session may call later, by the number returned. */
  register(fn: (argsJson: string) => unknown): number;
  /** A dispatched task starts: its number. */
  taskStart(): number;
  /** It ended: `error` is "null" or the error (JSON). */
  taskDone(task: number, error: string): void;
  atomicBegin(token: string): void;
  atomicEnd(token: string): void;
  /** Call `fn` after `ms` milliseconds: the timer's id. */
  setTimer(ms: number, fn: () => void): number;
  clearTimer(id: number): void;
  /** Settle a command's later answer (`{ $promise: id }`) with `settle`. */
  awaitPromise(id: number, settle: (ok: boolean, json: string) => void): void;
  /** Write a line to the log. */
  print(level: string, text: string): void;
};
type Temzdf = typeof import("temzdf");
type Actions = Temzdf["action"];
/** The session's context token, as it came in JSON (null: none). */
type Token = unknown;
/** What the session answers (JSON parsed): its shape depends on the request. */
// biome-ignore lint/suspicious/noExplicitAny: the session's answers are not typed here.
type Answer = any;

const g = globalThis as unknown as Record<string, unknown>;
const host = g.__host as Host;
delete g.__host;

// A failed command's error (marked by the host) as the command said it.
const errorJson = (e: unknown): string =>
  e instanceof Error && (e as Error & { temzdfCommandFailed?: boolean }).temzdfCommandFailed === true
    ? JSON.stringify({ message: e.message, stack: null, command: true })
    : JSON.stringify({
        message: e instanceof Error ? `${e.name}: ${e.message}` : String(e),
        stack: e instanceof Error && e.stack ? String(e.stack) : null,
      });

// The context token of the synchronous call running now (a mapped or hook
// function, or the configuration at startup), else the task's inside
// its ctx.atomic(fn); null elsewhere.
let atomicToken: Token = null;
const current = (): Token => JSON.parse(host.current()) ?? atomicToken;

// A command answering later answers `{ $promise: n }`: a Promise, settled
// when the session sends ToHost::Resolve.
function promised(result: Answer): Answer {
  if (result === null || typeof result !== "object" || typeof result.$promise !== "number") return result;
  return new Promise((resolve, reject) =>
    host.awaitPromise(result.$promise, (ok, json) => (ok ? resolve(JSON.parse(json)) : reject(new Error(json)))),
  );
}

function request(token: Token, op: string, payload: unknown): Answer {
  if (token === null) {
    throw new Error(
      "no context: commands outside a mapped or hook function go through the ctx a dispatched task receives",
    );
  }
  return JSON.parse(host.request(JSON.stringify(token), op, JSON.stringify(payload)));
}

const camel = (name: string): string => name.replace(/-([a-z])/g, (_, c: string) => c.toUpperCase());
// `obj.input.deleteWord = value` for the command "input.delete-word": a
// namespace (before a dot) is an object of its own.
function put(obj: Record<string, unknown>, name: string, value: unknown): void {
  const parts = name.split(".").map(camel);
  const last = parts.pop() as string;
  for (const p of parts) {
    obj[p] ??= {};
    obj = obj[p] as Record<string, unknown>;
  }
  obj[last] = value;
}
function deepFreeze<T extends object>(obj: T): Readonly<T> {
  for (const v of Object.values(obj)) if (typeof v === "object" && v !== null) deepFreeze(v);
  return Object.freeze(obj);
}

// The session's actions, as functions issuing them under `tokenOf()`:
// typed as the declarations say (a test runs every one of them).
function commands(tokenOf: () => Token, action: Record<string, unknown> = {}): Actions {
  for (const name of JSON.parse(host.commands()) as string[]) {
    put(action, name, (...args: unknown[]) => promised(request(tokenOf(), "command", { name, args })));
  }
  return deepFreeze(action) as unknown as Actions;
}
// Filled once the session has said which commands it has (before the
// configuration runs).
const globalAction = {} as Actions;
g.__temzdfLoad = () => {
  if (Object.isFrozen(globalAction)) return;
  commands(current, globalAction as unknown as Record<string, unknown>);
};

// Functions by the id they were registered under, for keymap.get.
// biome-ignore lint/suspicious/noExplicitAny: any function scripts register.
type Fn = (...args: any[]) => unknown;
const registered: ({ fn: Fn; condition: boolean } | undefined)[] = [];

// Register `fn` as a mapped or hook function, or (`condition`) as a
// mapping's `when` or a mode's `keepalive`, which gives a boolean:
// synchronous only.
function register(fn: unknown, what: string, condition = false): number {
  if (typeof fn !== "function") throw new TypeError(`${what}: expected a function`);
  const f = fn as Fn;
  const id = host.register((argsJson) => {
    const r = f(...JSON.parse(argsJson));
    if (r !== null && typeof r === "object" && typeof (r as { then?: unknown }).then === "function") {
      throw new TypeError(
        condition
          ? `${what}: conditions must be synchronous`
          : `${what}: mapped and hook functions must be synchronous (use dispatch for asynchronous work)`,
      );
    }
    // A hook's answer (`{ cancel: true, reason }`) goes back as JSON.
    return condition ? !!r : r === undefined ? undefined : JSON.stringify(r);
  });
  registered[id] = { fn: f, condition };
  return id;
}

// A function keymap.get gave back: its id again, not a new one.
const funcId = (f: unknown, what: string, condition: boolean): number => {
  const id = registered.findIndex((r) => r !== undefined && r.fn === f && r.condition === condition);
  return id >= 0 ? id : register(f, what, condition);
};

function atomicWith<T>(token: Token, fn: () => T): T {
  if (typeof fn !== "function") throw new TypeError("atomic: expected a function");
  const t = JSON.stringify(token);
  host.atomicBegin(t);
  // The plain `action` works inside too, under the task's token.
  const outer = atomicToken;
  atomicToken = token;
  try {
    const r = fn();
    if (r !== null && typeof r === "object" && typeof (r as { then?: unknown }).then === "function") {
      throw new TypeError("atomic: the function must be synchronous");
    }
    return r;
  } finally {
    atomicToken = outer;
    host.atomicEnd(t);
  }
}

/** An open prompt (openPrompt), by its buffer. */
type Prompt = {
  pane: number;
  origin: number | null;
  onLine?: (line: string) => void;
  onCancel?: () => void;
  onChange?: (line: string) => void;
};

const temzdf: Temzdf = {
  action: globalAction,
  keymap: Object.freeze({
    // keymap.add(mode | modes, lhs, rhs, { nowait, desc, when, namespace,
    // priority }): rhs and `when` are synchronous functions of the key's
    // context.
    add(modes, lhs, rhs, options = {}) {
      const payload = {
        modes,
        keys: lhs,
        nowait: !!options.nowait,
        desc: options.desc ?? null,
        when: options.when == null ? null : funcId(options.when, `keymap.add(${lhs}) when`, true),
        namespace: options.namespace ?? null,
        priority: options.priority ?? 0,
        func: 0,
      };
      if (typeof rhs !== "function") throw new TypeError(`keymap.add(${lhs}): expected a function`);
      payload.func = funcId(rhs, `keymap.add(${lhs})`, false);
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
      const back = (v: Answer) => (v !== null && typeof v === "object" && "func" in v ? registered[v.func]?.fn : v);
      const payload = { mode: filter.mode ?? null, namespace: filter.namespace ?? null, lhs: filter.lhs ?? null };
      return request(current(), "getMappings", payload).map((m: Answer) => ({
        ...m,
        rhs: back(m.rhs),
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
      throw new TypeError(
        "defineMode: inherit is gone; a transient mode with keepalive takes the keys of the mode below",
      );
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
    return globalAction.feedkeys(keys, options.remap !== false);
  },
  currentMode() {
    return globalAction.currentMode();
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
  ) as Temzdf["o"],
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
      action: commands(() => token),
      atomic: <T>(f: () => T) => atomicWith(token, f),
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
    const c = globalAction;
    const origin = c.focusedPane();
    const buffer = c.newBuffer({
      text: opts.text ?? "",
      basemode: opts.mode ?? "input",
      attrs: ["nofile", "unlisted"],
    });
    let pane: number | null = null;
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
        { virtText: [{ text: opts.prompt as string, style: opts.style ?? "Prompt" }], namespace: "prompt", pane },
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
              if (p?.onChange) p.onChange(promptLine(p));
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
    const b = buffer ?? focusedBuffer();
    return b !== undefined && prompts.has(b);
  },
  // Inside a mapped or hook function everything is atomic already.
  atomic(fn) {
    const token = current();
    if (token === null) throw new Error("atomic: in a dispatched task use ctx.atomic");
    return fn();
  },
};
// Open prompts by buffer (openPrompt), and their hooks while any is.
const prompts = new Map<number, Prompt>();
let promptHooks: (() => void)[] | null = null;
const focusedBuffer = (): number | undefined => globalAction.panes().find((p) => p.focused)?.buffer;
// The cursor's line of a prompt (what is run, and previewed).
const promptLine = (p: Prompt): string => {
  const at = globalAction.text.cursor({ pane: p.pane });
  return globalAction.text.line(at.line, { pane: p.pane });
};
// Close a prompt: its line, back to where it was opened, the buffer gone.
function closePrompt(buffer: number | undefined): { p: Prompt; line: string } | null {
  if (buffer === undefined) return null;
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
    if (p.origin !== null) globalAction.focusPane(p.origin);
  } catch {
    // That pane is gone.
  }
  try {
    globalAction.deleteBuffer(buffer);
  } catch {
    // Gone already.
  }
  return { p, line };
}
g.__temzdf = Object.freeze(temzdf);

// Delays as browsers take them: not a number is 0; at most 2^31 - 1 ms.
const delay = (ms: unknown): number => Math.min(2147483647, Math.max(0, Number(ms) || 0));
const setTimer: typeof setTimeout = (fn, ms = 0, ...args) => host.setTimer(delay(ms), () => fn(...args));
g.setTimeout = setTimer;
// Anything but a timer id is ignored, as browsers do (clearTimeout(undefined)).
const clearTimer: typeof clearTimeout = (id) => {
  if (typeof id === "number") host.clearTimer(id);
};
g.clearTimeout = clearTimer;

const show = (v: unknown): string => {
  if (typeof v === "string") return v;
  if (v instanceof Error) return v.stack ? `${v.name}: ${v.message}\n${v.stack}` : `${v.name}: ${v.message}`;
  try {
    return JSON.stringify(v) ?? String(v);
  } catch {
    return String(v);
  }
};
const print =
  (level: string) =>
  (...args: unknown[]) =>
    host.print(level, args.map(show).join(" "));
const logs: typeof console = Object.freeze({
  log: print("log"),
  info: print("info"),
  warn: print("warn"),
  error: print("error"),
  debug: print("debug"),
});
g.console = logs;
g.__temzdfErrorJson = errorJson;
