// The JavaScript side of temzdf's scripting API, evaluated once before the
// configuration (as a script: crates/temzdf-script/build.rs wraps it in a
// function with esbuild). It builds `globalThis.__temzdf` (what `import ...
// from "temzdf"` exports) on top of the host's native functions in
// `__host` (crates/temzdf-script/src/host.rs). Values cross into Rust as
// JSON. What it builds is typed as the `temzdf` module of
// ts-types/temzdf-core.d.ts, so the declarations and this agree. What
// scripts pass and the session answers is checked with zod
// (runtime/lib/validate.ts): the answers come as unknown.
// biome-ignore lint/suspicious/noRedundantUseStrict: evaluated as a script, not a module.
"use strict";

import type { Condition, KeyContext, KeyInfo, MapOptions, ModeProperties, PromptOptions, Rhs } from "temzdf";
import type { Assert } from "../runtime/lib/types";
import type { Takes } from "../runtime/lib/validate";
import * as z from "../runtime/lib/zod";
import { focusedPaneInfo } from "../runtime/lib/panes";
import { check, func, guard, isNullish, options, parse } from "../runtime/lib/validate";

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
  /** Start a file operation (`temzdf/fs`: readFile, readTextFile, stat,
   * lstat, readDir, realPath) on `path`; `settle` gets its result once
   * nothing runs: the bytes (readFile), the text (readTextFile), else JSON
   * (an error: `{ kind, message }`). */
  fsStart(op: string, path: string, settle: (ok: boolean, value: string | ArrayBuffer) => void): void;
  /** Write a line to the log. */
  print(level: string, text: string): void;
  /** A line `temzdf eval`'s code printed (task `task`), to stderr when `err`. */
  evalPrint(task: number, err: boolean, text: string): void;
  /** A step of `temzdf eval` as a function of its scope and its view of
   * `temzdf` (src/eval.rs); throws a SyntaxError saying where. */
  compile(task: number, name: string, code: string): (scope: object, view: object) => () => Promise<unknown>;
  /** `temzdf eval`'s code ended: `{ value }` or `{ error }` (JSON). */
  evalDone(task: number, outcome: string): void;
};
type Temzdf = typeof import("temzdf");
type Actions = Temzdf["action"];
/** The session's context token, as it came in JSON (null: none). */
type Token = unknown;
/** What the session answers (JSON parsed): its shape depends on the request. */
type Answer = unknown;

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

// What the session sends scripts, with its nulls as undefined: absent
// values are undefined in the script API (doc/design/scripting.md). In
// place (fresh from JSON.parse), keys kept, arrays without holes.
function noNull(v: unknown): unknown {
  if (v === null) return undefined;
  if (typeof v === "object") {
    const o = v as Record<string, unknown>;
    for (const k of Object.keys(o)) o[k] = noNull(o[k]);
  }
  return v;
}
const fromSession = (json: string): unknown => noNull(JSON.parse(json));

// File operations for runtime/fs.ts (`temzdf/fs`), which turns the
// rejections' `{ kind, message }` into its error classes.
g.__temzdfFs = (op: string, path: string): Promise<string | ArrayBuffer> =>
  new Promise((resolve, reject) => host.fsStart(op, path, (ok, value) => (ok ? resolve(value) : reject(value))));

// A command answering later answers `{ $promise: n }`: a Promise, settled
// when the session sends ToHost::Resolve.
const isLater = guard(z.object({ $promise: z.number() }));
function promised(result: Answer): Answer {
  if (!isLater(result)) return result;
  return new Promise((resolve, reject) =>
    host.awaitPromise(result.$promise, (ok, json) => (ok ? resolve(fromSession(json)) : reject(new Error(json)))),
  );
}

function request(token: Token, op: string, payload: unknown): Answer {
  if (token === null) {
    throw new Error(
      "no context: commands outside a mapped or hook function go through the ctx a dispatched task receives",
    );
  }
  return fromSession(host.request(JSON.stringify(token), op, JSON.stringify(payload)));
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

// A Promise, or what awaits as one.
// biome-ignore lint/suspicious/noThenProperty: the schema of a thenable, not one.
const isThenable = guard(z.object({ then: func() }));
const isFunc = guard(func());

// Register `fn` as a mapped or hook function, or (`condition`) as a
// mapping's `when` or a mode's `keepalive`, which gives a boolean:
// synchronous only.
function register(fn: unknown, what: string, condition = false): number {
  check(what, func<Fn>(), fn);
  const f = fn;
  const id = host.register((argsJson) => {
    const r = f(...(fromSession(argsJson) as unknown[]));
    if (isThenable(r)) {
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
  check("atomic", func(), fn);
  const t = JSON.stringify(token);
  host.atomicBegin(t);
  // The plain `action` works inside too, under the task's token.
  const outer = atomicToken;
  atomicToken = token;
  try {
    const r = fn();
    if (isThenable(r)) {
      throw new TypeError("atomic: the function must be synchronous");
    }
    return r;
  } finally {
    atomicToken = outer;
    host.atomicEnd(t);
  }
}

// A new dispatched task: its number and its `ctx`.
function taskContext() {
  const task = host.taskStart();
  const token = { Task: task };
  const ctx = Object.freeze({
    action: commands(() => token),
    atomic: <T>(f: () => T) => atomicWith(token, f),
  });
  return { task, ctx };
}

/** An open prompt (openPrompt), by its buffer. */
type Prompt = {
  pane: number;
  origin: number | undefined;
  onLine?: (line: string) => void;
  onCancel?: () => void;
  onChange?: (line: string) => void;
};

// What the API takes from scripts, with the defaults the session gets
// for what is left out; each checked against its declared type.
const nullable = <T extends z.ZodMiniType>(t: T) => z._default(z.nullable(t), null);
const modeNames = z.union([z.string(), z.readonly(z.array(z.string()))]);
const mapOptions = options({
  nowait: z._default(z.boolean(), false),
  desc: nullable(z.string()),
  when: nullable(func<Condition>()),
  namespace: nullable(z.string()),
  priority: z._default(z.int32(), 0),
});
const namespaceOption = options({ namespace: nullable(z.string()) });
const clearFilter = options({ mode: nullable(z.string()), namespace: nullable(z.string()) });
const getFilter = options({ mode: nullable(z.string()), namespace: nullable(z.string()), lhs: nullable(z.string()) });
const modeProperties = options(
  {
    fallback: nullable(z.union([z.enum(["drop", "passthrough"]), func<(ctx: KeyContext, key: KeyInfo) => void>()])),
    count: nullable(z.boolean()),
    transient: nullable(z.boolean()),
    keepalive: nullable(z.union([z.enum(["mapped", "always"]), func<Condition>()])),
    timeout: nullable(z.number()),
  },
  // Removed names (2026-10-28, 2026-11-07).
  {
    inherit: "inherit is gone; a transient mode with keepalive takes the keys of the mode below",
    unmapped: "unmapped is now fallback",
  },
);
const feedOptions = options({ remap: z._default(z.boolean(), true) });
const hookOptions = options({ nested: z._default(z.boolean(), false) });
const promptOptions = options({
  prompt: z._default(z.string(), ""),
  text: z._default(z.string(), ""),
  side: z._default(z.enum(["bottom", "top", "left", "right"]), "bottom"),
  height: z._default(z.number(), 1),
  mode: z._default(z.string(), "input"),
  style: z._default(z.string(), "Prompt"),
  onLine: z.optional(func<(line: string) => void>()),
  onCancel: z.optional(func<() => void>()),
  onChange: z.optional(func<(line: string) => void>()),
});
const promptBuffer = z.optional(z.number());
type _Options = Assert<
  | Takes<typeof mapOptions, MapOptions>
  | Takes<typeof namespaceOption, { namespace?: string }>
  | Takes<typeof clearFilter, { mode?: string; namespace?: string }>
  | Takes<typeof getFilter, { mode?: string; namespace?: string; lhs?: string }>
  | Takes<typeof modeProperties, ModeProperties>
  | Takes<typeof feedOptions, { remap?: boolean }>
  | Takes<typeof hookOptions, { nested?: boolean }>
  | Takes<typeof promptOptions, PromptOptions>
>;

// What getMappings answers: `rhs` and `when` are functions' ids (test
// builds' mappings of actions list them instead).
const funcRef = guard(z.object({ func: z.number() }));
const mappingAnswers = z.array(
  z.object({
    mode: z.string(),
    lhs: z.string(),
    rhs: z.unknown(),
    when: z.unknown(),
    nowait: z.boolean(),
    desc: z.optional(z.string()),
    namespace: z.optional(z.string()),
    priority: z.number(),
  }),
);

const temzdf: Temzdf = {
  action: globalAction,
  keymap: Object.freeze({
    // keymap.add(mode | modes, lhs, rhs, { nowait, desc, when, namespace,
    // priority }): rhs and `when` are synchronous functions of the key's
    // context.
    add(modes, lhs, rhs, opts) {
      const what = `keymap.add(${String(lhs)})`;
      check(what, modeNames, modes, "mode");
      check(what, z.string(), lhs, "lhs");
      const o = parse(what, mapOptions, opts, "options");
      const when = o.when === null ? null : funcId(o.when, `${what} when`, true);
      request(current(), "map", { modes, keys: lhs, ...o, when, func: funcId(rhs, what, false) });
    },
    // keymap.remove(mode | modes, lhs, { namespace }): that namespace's
    // mappings, or all of them.
    remove(modes, lhs, opts) {
      const what = `keymap.remove(${String(lhs)})`;
      check(what, modeNames, modes, "mode");
      check(what, z.string(), lhs, "lhs");
      request(current(), "unmap", { modes, keys: lhs, ...parse(what, namespaceOption, opts, "options") });
    },
    // keymap.clear({ mode, namespace }): every mapping of the mode and the
    // namespace; one left out is not a filter.
    clear(filter) {
      request(current(), "clearMappings", parse("keymap.clear", clearFilter, filter, "filter"));
    },
    // keymap.get({ mode, namespace, lhs }): the mappings as keymap.add()
    // takes them; one left out is not a filter.
    get(filter) {
      // The host names the keys `keys`, as map and unmap do.
      const { lhs, ...rest } = parse("keymap.get", getFilter, filter, "filter");
      const answer = request(current(), "getMappings", { ...rest, keys: lhs });
      check("keymap.get", mappingAnswers, answer, "answer");
      const back = (v: unknown) => (funcRef(v) ? registered[v.func]?.fn : v) as Rhs;
      return answer.map((m) => ({
        ...m,
        rhs: back(m.rhs),
        when: isNullish(m.when) ? undefined : (back(m.when) as Condition),
      }));
    },
  }),
  // defineMode(name, { fallback: "drop" | "passthrough" | (ctx, key) => void,
  // count, transient, keepalive: "mapped" | "always" | (ctx) => boolean, timeout })
  defineMode(name, props) {
    const what = `defineMode(${String(name)})`;
    check(what, z.string(), name, "name");
    const p = parse(what, modeProperties, props, "properties");
    request(current(), "defineMode", {
      name,
      ...p,
      fallback: isFunc(p.fallback) ? funcId(p.fallback, `${what} fallback`, false) : p.fallback,
      keepalive: isFunc(p.keepalive) ? funcId(p.keepalive, `${what} keepalive`, true) : p.keepalive,
    });
  },
  feedkeys(keys, opts) {
    check("feedkeys", z.string(), keys, "keys");
    return globalAction.feedkeys(keys, parse("feedkeys", feedOptions, opts, "options").remap);
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
        // undefined: unset (JSON drops it; the host reads it as null).
        request(current(), "setOption", { name, value });
        return true;
      },
    },
  ) as Temzdf["o"],
  // Returns a function that stops it (a hook only while it is wanted:
  // KeyTyped hooks hold every key until they ran).
  on(event, fn, opts) {
    const what = `on(${String(event)})`;
    check(what, z.string(), event, "event");
    const { nested } = parse(what, hookOptions, opts, "options");
    const func = register(fn, what);
    request(current(), "on", { event, func, nested });
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
    check("dispatch", func(), fn);
    const { task, ctx } = taskContext();
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
  openPrompt(given) {
    const opts = parse("openPrompt", promptOptions, given, "options");
    const c = globalAction;
    const origin = c.focusedPane();
    const buffer = c.newBuffer({ text: opts.text, basemode: opts.mode, attrs: ["nofile", "unlisted"] });
    let pane: number | undefined;
    try {
      pane = c.split({ side: opts.side, buffer, height: opts.height });
    } catch (e) {
      c.deleteBuffer(buffer);
      throw e;
    }
    if (isNullish(pane)) {
      c.deleteBuffer(buffer);
      return undefined;
    }
    c.statusLine(false, pane);
    c.text.setOptions({ wrap: false, number: "off", cursorline: false, scrolloff: 0 }, { pane });
    const origin0 = { line: 0, character: 0 };
    if (opts.prompt !== "") {
      c.text.addDecoration(
        { start: origin0, end: origin0 },
        { virtText: [{ text: opts.prompt, style: opts.style }], namespace: "prompt", pane },
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
    check("acceptPrompt", promptBuffer, buffer, "buffer");
    const r = closePrompt(buffer ?? focusedBuffer());
    if (r !== null) r.p.onLine?.(r.line);
  },
  cancelPrompt(buffer) {
    check("cancelPrompt", promptBuffer, buffer, "buffer");
    const r = closePrompt(buffer ?? focusedBuffer());
    if (r !== null) r.p.onCancel?.();
  },
  isPrompt(buffer) {
    check("isPrompt", promptBuffer, buffer, "buffer");
    const b = buffer ?? focusedBuffer();
    return b !== undefined && prompts.has(b);
  },
  promptOrigin(buffer) {
    check("promptOrigin", promptBuffer, buffer, "buffer");
    const b = buffer ?? focusedBuffer();
    const origin = b === undefined ? undefined : prompts.get(b)?.origin;
    return isNullish(origin) ? undefined : origin;
  },
  // Inside a mapped or hook function everything is atomic already.
  atomic(fn) {
    check("atomic", func(), fn);
    const token = current();
    if (token === null) throw new Error("atomic: in a dispatched task use ctx.atomic");
    return fn();
  },
};
// Open prompts by buffer (openPrompt), and their hooks while any is.
const prompts = new Map<number, Prompt>();
let promptHooks: (() => void)[] | null = null;
const focusedBuffer = (): number | undefined => focusedPaneInfo(globalAction)?.buffer;
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
    if (!isNullish(p.origin)) globalAction.focusPane(p.origin);
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

// `temzdf eval` (host.rs `run_code`): its steps, in order, as a dispatched
// task, stopping at the first that fails. The code sees `temzdf` (the
// module, its `action` and `atomic` the task's) and a `console` printing
// to the caller, both in the steps' shared scope, not globals: other
// code goes on logging to the session log. Returns the task's number.
g.__temzdfEval = (stepsJson: string, print: boolean): number => {
  const steps = JSON.parse(stepsJson) as { name: string; code: string }[];
  const { task, ctx } = taskContext();
  const view = Object.freeze({ ...temzdf, action: ctx.action, atomic: ctx.atomic });
  const printTo =
    (err: boolean) =>
    (...args: unknown[]) =>
      host.evalPrint(task, err, args.map(show).join(" "));
  const evalConsole: typeof console = Object.freeze({
    log: printTo(false),
    info: printTo(false),
    debug: printTo(false),
    warn: printTo(true),
    error: printTo(true),
  });
  const scope: Record<string, unknown> = Object.create(null);
  scope.temzdf = view;
  scope.console = evalConsole;
  let step = "";
  const run = async (): Promise<unknown> => {
    let value: unknown;
    for (const s of steps) {
      step = s.name;
      value = await host.compile(task, s.name, s.code)(scope, view)();
    }
    return value;
  };
  // undefined (and functions) as Node's -p shows it.
  const asJson = (v: unknown): string => JSON.stringify(v) ?? "undefined";
  Promise.resolve()
    .then(run)
    .then((v) => {
      step = "the value";
      return { value: print ? asJson(v) : null };
    })
    .catch((e: unknown) => ({ error: { step, ...(JSON.parse(errorJson(e)) as object) } }))
    .then((outcome) => {
      host.evalDone(task, JSON.stringify(outcome));
      // Its error went to the caller, not to the session.
      host.taskDone(task, "null");
    });
  return task;
};
