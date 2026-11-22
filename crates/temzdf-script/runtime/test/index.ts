// temzdf/test: tests of temzdf's behaviour written as scripts, on QUnit 3
// (doc/design/scripting.md). A test file registers tests at its top level
// (`test(name, async (assert, t) => ...)`); they run in one dispatched task,
// in the session itself: `t.keys` types keys and waits until the session
// settled (the action test.keys), `t.action` and `t.run` act on it.
// Under `temzdf test` (the session collects test reports: test.reporting())
// they start once the session is ready and their TAP goes out through
// test.report and test.end; a configuration importing this module starts
// them with run() (the TAP then goes to the log).
//
// Each test gets the session as the one before left it, after a reset:
// the session settled, test.reset() (pending keys, the base mode), the Vim
// mode's reset() (an insert, a selection, a macro being recorded), a
// prompt closed. What a test changes beyond that it undoes itself
// (t.after; the helpers register their own).
/// <reference path="./qunit.d.ts" />
import QUnit from "qunit";
import type { QUnitAssert, QUnitError } from "qunit";
import { inspect } from "loupe";
import { action, cancelPrompt, dispatch, isPrompt, on } from "temzdf";
import type { Actions, TaskContext, TestSettled } from "temzdf";
import vim from "temzdf/edit/vim";

/** How long a test may take by default, in milliseconds. */
const DEFAULT_TIMEOUT = 10_000;
/** QUnit's own timeout comes after the wrapper's (which stops the test). */
const QUNIT_GRACE = 5_000;

/** QUnit's assertions (https://qunitjs.com/api/assert/). */
export interface Assert {
  ok(state: unknown, message?: string): void;
  notOk(state: unknown, message?: string): void;
  true(state: unknown, message?: string): void;
  false(state: unknown, message?: string): void;
  equal(actual: unknown, expected: unknown, message?: string): void;
  notEqual(actual: unknown, expected: unknown, message?: string): void;
  strictEqual<T>(actual: T, expected: T, message?: string): void;
  notStrictEqual(actual: unknown, expected: unknown, message?: string): void;
  deepEqual<T>(actual: T, expected: T, message?: string): void;
  notDeepEqual(actual: unknown, expected: unknown, message?: string): void;
  propEqual(actual: unknown, expected: unknown, message?: string): void;
  propContains(actual: unknown, expected: unknown, message?: string): void;
  throws(block: () => void, expected?: unknown, message?: string): void;
  rejects(promise: Promise<unknown>, expected?: unknown, message?: string): Promise<void>;
  step(value: string): void;
  verifySteps(steps: string[], message?: string): void;
  expect(amount: number): void;
  pushResult(result: { result: boolean; actual: unknown; expected: unknown; message?: string }): void;
}

/** `t.keys`' options: test.keys' (`flush`, `remap`), and `allowErrors`:
 * error messages shown meanwhile are returned rather than failing the
 * test. */
export interface KeysOptions {
  flush?: boolean;
  remap?: boolean;
  allowErrors?: boolean;
}

/** What a test gets besides `assert`. */
export interface TestContext {
  /** Aborted when the test timed out: the helpers then throw rather than
   * act (and stop waiting). */
  readonly signal: AbortSignal;
  /** Run `fn` after the test, whether it passed or not (the last
   * registered first). */
  after(fn: () => void | Promise<void>): void;
  /** The session's actions (the test runs in a dispatched task: the plain
   * `action` works inside `run` only). */
  readonly action: Actions;
  /** Run synchronous `fn` with everything else held, where the plain
   * `action`, `o` and the modules' functions work (ctx.atomic). */
  run<T>(fn: () => T): T;
  /** Type `keys` (key notation) and wait until the session settled
   * (test.keys); error messages shown meanwhile fail the test unless
   * `allowErrors`. */
  keys(keys: string, opts?: KeysOptions): Promise<TestSettled>;
  /** Wait until the session settled (test.settle). */
  settle(opts?: { flush?: boolean }): Promise<TestSettled>;
  /** A new text buffer holding `text` (no file), in a new pane beside the
   * focused one, focused; both go after the test. Returns the pane. */
  openText(text: string, opts?: { basemode?: string }): number;
}

/** `test`'s options. */
export interface TestOptions {
  /** Milliseconds before the test is stopped and fails (10 s by default). */
  timeout?: number;
}

/** Run around each test of a module, with its `assert` and `t` (inside
 * the test's timeout: a failure fails the test). */
export interface ModuleHooks {
  beforeEach?(assert: Assert, t: TestContext): void | Promise<void>;
  afterEach?(assert: Assert, t: TestContext): void | Promise<void>;
}

type TestFn = (assert: Assert, t: TestContext) => void | Promise<void>;

/** The task running the tests (while they run). */
let runner: TaskContext | null = null;
let started = false;

/** The hooks of the modules being registered (innermost last), and those
 * of a module without a scope (until the next). */
const scopes: ModuleHooks[] = [];
let flat: ModuleHooks | undefined;

QUnit.config.autostart = false;
QUnit.config.testTimeout = DEFAULT_TIMEOUT + QUNIT_GRACE;
// QUnit's own messages (ok's "was: ...") show values as the reports do.
QUnit.dump.parse = (value: unknown) => show(value);

/** A value as the reports show it: Maps, Sets, class names, -0, cycles. */
function show(value: unknown): string {
  return inspect(value, { depth: 6 });
}

function stopped(): Error {
  return new Error("the test was stopped (it timed out)");
}

/** `p`, or a rejection as soon as `signal` is aborted. */
function abortable<T>(p: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(stopped());
  return new Promise<T>((resolve, reject) => {
    const abort = () => reject(stopped());
    signal.addEventListener("abort", abort);
    p.then(
      (v) => {
        signal.removeEventListener("abort", abort);
        resolve(v);
      },
      (e: unknown) => {
        signal.removeEventListener("abort", abort);
        reject(e);
      },
    );
  });
}

function running(): TaskContext {
  if (runner === null) throw new Error("temzdf/test: tests run inside run() (or `temzdf test`)");
  return runner;
}

/** A line (or lines) of TAP: to `temzdf test`, else the log. */
function report(text: string): void {
  for (const l of text.split("\n")) {
    if (runner !== null) runner.action.test.report(l);
    else console.log(l);
  }
}

/** The session as each test starts from (see the top). */
async function reset(ctx: TaskContext): Promise<void> {
  await ctx.action.test.settle();
  ctx.action.test.reset();
  ctx.atomic(() => {
    vim.reset();
    if (isPrompt()) cancelPrompt();
  });
  await ctx.action.test.settle();
}

function context(ctx: TaskContext, assert: Assert, signal: AbortSignal, afters: (() => unknown)[]): TestContext {
  const live = () => {
    if (signal.aborted) throw stopped();
  };
  const t: TestContext = {
    signal,
    after(fn) {
      afters.push(fn);
    },
    action: ctx.action,
    run(fn) {
      live();
      return ctx.atomic(fn);
    },
    async keys(keys, opts = {}) {
      live();
      const { allowErrors = false, ...rest } = opts;
      const r = await abortable(ctx.action.test.keys(keys, rest), signal);
      if (!allowErrors && r.errors.length > 0) {
        assert.pushResult({ result: false, actual: r.errors, expected: [], message: `errors after keys ${keys}` });
      }
      return r;
    },
    settle(opts = {}) {
      live();
      return abortable(ctx.action.test.settle(opts), signal);
    },
    openText(text, opts = {}) {
      live();
      const buffer = ctx.action.newBuffer({
        text,
        ...(opts.basemode === undefined ? {} : { basemode: opts.basemode }),
      });
      t.after(() =>
        ctx.atomic(() => {
          // Its changes go with it (the shipped defaults ask otherwise).
          action.setVar("buffer", "forceDelete", true, buffer);
          action.deleteBuffer(buffer);
        }),
      );
      const pane = ctx.action.split({ buffer });
      if (pane === undefined) throw new Error("openText: no pane");
      ctx.action.focusPane(pane);
      return pane;
    },
  };
  return t;
}

/** Register a test: `fn(assert, t)`, stopped and failed after
 * `opts.timeout` ms. */
export function test(name: string, fn: TestFn, opts: TestOptions = {}): void {
  const ms = opts.timeout ?? DEFAULT_TIMEOUT;
  if (!(ms > 0)) throw new TypeError(`test(${name}): timeout is a number of milliseconds`);
  const hooks = scopes.length > 0 ? [...scopes] : flat === undefined ? [] : [flat];
  QUnit.test(name, async (qassert: QUnitAssert) => {
    const assert = qassert as unknown as Assert;
    qassert.timeout(ms + QUNIT_GRACE);
    const ctx = running();
    await reset(ctx);
    const controller = new AbortController();
    const afters: (() => unknown)[] = [];
    const t = context(ctx, assert, controller.signal, afters);
    let timer: number | undefined;
    const timedOut = new Promise<"timeout">((resolve) => {
      timer = setTimeout(() => resolve("timeout"), ms);
    });
    try {
      const body = (async () => {
        for (const h of hooks) await h.beforeEach?.(assert, t);
        await fn(assert, t);
        for (const h of [...hooks].reverse()) await h.afterEach?.(assert, t);
        return "done" as const;
      })();
      if ((await Promise.race([body, timedOut])) === "timeout") {
        controller.abort();
        // What it does after this fails as stopped (no keys go).
        body.catch(() => {});
        ctx.action.test.reset();
        assert.pushResult({
          result: false,
          actual: undefined,
          expected: undefined,
          message: `timed out after ${ms} ms`,
        });
      }
    } finally {
      if (timer !== undefined) clearTimeout(timer);
      for (const f of afters.reverse()) {
        try {
          await f();
        } catch (e) {
          assert.pushResult({ result: false, actual: e, expected: undefined, message: `t.after: ${String(e)}` });
        }
      }
    }
  });
}

/** Group the tests registered in `scope` (or, without one, those after it
 * until the next module) under `name`, with `hooks` around each. */
export function module(name: string, hooks?: ModuleHooks | (() => void), scope?: () => void): void {
  let h: ModuleHooks | undefined;
  let s = scope;
  if (typeof hooks === "function") s = hooks;
  else h = hooks;
  if (s === undefined) {
    flat = h;
    QUnit.module(name);
    return;
  }
  const body = s;
  QUnit.module(name, () => {
    scopes.push(h ?? {});
    try {
      body();
    } finally {
      scopes.pop();
    }
  });
}

// The reports: TAP, failures with their values shown by loupe.
let count = 0;
QUnit.on("runStart", () => report("TAP version 13"));
QUnit.on("testEnd", (r) => {
  count += 1;
  const name = r.fullName.join(" > ");
  if (r.status === "skipped") return report(`ok ${count} ${name} # SKIP`);
  if (r.status === "todo") return report(`not ok ${count} ${name} # TODO`);
  report(`${r.status === "passed" ? "ok" : "not ok"} ${count} ${name}`);
  for (const e of r.errors) report(failure(e));
});
QUnit.on("runEnd", (r) => {
  const c = r.testCounts;
  report(`1..${c.total}`);
  report(`# pass ${c.passed}\n# skip ${c.skipped}\n# todo ${c.todo}\n# fail ${c.failed}`);
  if (runner !== null) runner.action.test.end({ failed: c.failed, total: c.total });
});

/** A failed assertion as TAP's YAML block. */
function failure(e: QUnitError): string {
  const lines = ["  ---", `  message: ${JSON.stringify(e.message ?? "failed")}`];
  if (e.actual !== undefined || e.expected !== undefined) {
    lines.push(`  actual: ${indent(show(e.actual))}`, `  expected: ${indent(show(e.expected))}`);
  }
  if (e.stack !== undefined && e.stack !== "") {
    lines.push("  stack: |", ...e.stack.split("\n").map((l) => `    ${l.trim()}`));
  }
  lines.push("  ...");
  return lines.join("\n");
}

function indent(s: string): string {
  return s.replaceAll("\n", "\n    ");
}

/** Run the registered tests (once) in a dispatched task. `temzdf test`
 * starts them itself. */
export function run(): void {
  if (started) throw new Error("temzdf/test: the tests ran already");
  started = true;
  dispatch(async (ctx) => {
    runner = ctx;
    const done = new Promise<void>((resolve) => QUnit.on("runEnd", () => resolve()));
    QUnit.start();
    await done;
    runner = null;
  });
}

// Under `temzdf test`, the tests start once the session is ready.
on("SessionReady", () => {
  if (!started && action.test.reporting()) run();
});
