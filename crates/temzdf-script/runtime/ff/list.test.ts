// temzdf/ff's list (runtime/ff/list.ts): a session's matches written into
// a float's text buffer, on fake sources (`temzdf test`,
// doc/dev/testing.md). Colours on screen are crates/temzdf/tests/tui.rs'
// business.
import { on } from "temzdf";
import {
  chosenItems,
  createList,
  createSession,
  defineAction,
  defineRenderer,
  defineSorter,
  defineSource,
} from "temzdf/ff";
import type { Action, InvokeParams, List, ListOptions, Session, SessionOptions, Source } from "temzdf/ff";
import { substring } from "temzdf/ff/builtin";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

/** A promise and what resolves it. */
function gate(): { wait: Promise<void>; open: () => void } {
  let open = () => {};
  const wait = new Promise<void>((r) => {
    open = r;
  });
  return { wait, open };
}

/** A source yielding `parts` in turn: strings as items (ids counted from
 * 0), promises waited for. */
function fake(...parts: (readonly string[] | Promise<void>)[]): Source {
  return defineSource(async function* () {
    let id = 0;
    for (const p of parts) {
      if (p instanceof Promise) await p;
      else for (const value of p) yield { id: id++, value, detail: {} };
    }
  });
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** Wait (polling) until `cond` holds, at most `ms` milliseconds. */
async function until(cond: () => boolean, ms = 3000): Promise<void> {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error("timed out waiting");
    await sleep(2);
  }
}

const numbers = (n: number, prefix = "item") => Array.from({ length: n }, (_, i) => `${prefix} ${i}`);

/** Values in alphabetical order (a sorter that moves later items up). */
const alphabetical = defineSorter(({ items }) => {
  items.sort((a, b) => (a.value < b.value ? -1 : a.value > b.value ? 1 : 0));
});

/** A session started, and a list over it in a float without a border or
 * the focus, `height` rows high; both go after the test. */
function setup(
  t: TestContext,
  so: SessionOptions<object>,
  lo: Partial<ListOptions<object>> = {},
  height = 5,
): { s: Session<object>; list: List<object>; pane: number } {
  const s = createSession({ throttle: 0, ...so });
  t.after(() => s.dispose());
  const list = t.run(() =>
    createList({
      session: s,
      open: (buffer) =>
        t.action.openFloat({ buffer }, { width: 30, height, border: false, focus: false, focusable: false }),
      ...lo,
    }),
  );
  t.after(() => t.run(() => list.dispose()));
  s.start();
  const pane = list.pane;
  if (pane === undefined) throw new Error("no pane");
  return { s, list, pane };
}

/** The lines of the list's buffer ([] when empty). */
function lines(t: TestContext, pane: number): string[] {
  const text = t.action.text.getText(undefined, { pane });
  return text === "" ? [] : text.split("\n");
}

/** list.invoke(name) (t.run takes synchronous functions only). */
function invoke(t: TestContext, list: List<object>, name?: string): Promise<boolean> {
  let p: Promise<boolean> | undefined;
  t.run(() => {
    p = list.invoke(name);
  });
  return p!;
}

const cursor = (t: TestContext, pane: number) => t.action.text.cursor({ pane }).line;

test("the lines are the session's matches in its order; the cursor starts at the top", async (assert, t) => {
  const { s, list, pane } = setup(t, { sources: [fake(["cherry", "apple", "banana"])], sorter: alphabetical });
  await list.whenIdle();
  assert.deepEqual(lines(t, pane), ["apple", "banana", "cherry"]);
  assert.deepEqual(
    lines(t, pane),
    s.snapshot().items.map((i) => i.value),
  );
  assert.equal(cursor(t, pane), 0);
  assert.equal(t.run(() => list.selected())?.value, "apple");
  const info = t.action.text.info({ pane });
  assert.deepEqual(info.attrs.slice().sort(), ["nofile", "nomodifiable", "unlisted"]);
  const view = t.run(() => list.view());
  assert.deepEqual(
    { ...view, errors: view.errors.length },
    { query: "", collected: 3, matched: 3, written: 3, cursor: 0, marked: 0, state: "done", errors: 0 },
  );
});

test("lines are added as results come, the ones written before not written again", async (assert, t) => {
  const g = gate();
  const { list, pane } = setup(t, { sources: [fake(["one", "two"], g.wait, ["three", "four"])] });
  const buffer = t.action.text.info({ pane }).buffer;
  // (whenIdle would wait for the source.)
  await until(() => lines(t, pane).length === 2);
  assert.deepEqual(lines(t, pane), ["one", "two"]);
  const changed: number[] = [];
  const off = t.run(() =>
    on("TextChanged", (b, c) => {
      if (b === buffer) for (const ch of c.changes) changed.push(ch.range.start.line);
    }),
  );
  t.after(() => t.run(off));
  g.open();
  await list.whenIdle();
  assert.deepEqual(lines(t, pane), ["one", "two", "three", "four"]);
  assert.deepEqual(changed, [1], "appended after the last line");
});

test("a new query writes the list again and puts the cursor at the top", async (assert, t) => {
  const { s, list, pane } = setup(t, {
    sources: [fake(["apple", "apricot", "banana", "grape"])],
    matcher: substring(),
  });
  await list.whenIdle();
  t.run(() => list.next(2));
  assert.equal(cursor(t, pane), 2);
  s.setQuery("ap");
  await list.whenIdle();
  assert.deepEqual(lines(t, pane), ["apple", "apricot", "grape"]);
  assert.equal(cursor(t, pane), 0);
  assert.equal(t.run(() => list.view()).query, "ap");
  s.setQuery("zzz");
  await list.whenIdle();
  assert.deepEqual(lines(t, pane), []);
  assert.equal(
    t.run(() => list.selected()),
    undefined,
  );
});

test("view().query is the query of the lines in the buffer, not of those being rendered", async (assert, t) => {
  let hold: { wait: Promise<void>; open: () => void } | undefined;
  const renderer = defineRenderer(async () => {
    if (hold !== undefined) await hold.wait;
  });
  const { s, list, pane } = setup(t, { sources: [fake(["apple", "banana"])], matcher: substring() }, { renderer });
  await list.whenIdle();
  hold = gate();
  s.setQuery("ban");
  await s.whenIdle();
  await sleep(20);
  assert.deepEqual(lines(t, pane), ["apple", "banana"], "the old lines while the renderer runs");
  assert.equal(t.run(() => list.view()).query, "", "their query");
  hold.open();
  await list.whenIdle();
  assert.deepEqual(lines(t, pane), ["banana"]);
  assert.equal(t.run(() => list.view()).query, "ban");
  hold = undefined;
  s.setQuery("zzz");
  await list.whenIdle();
  s.setQuery("zzzz");
  await list.whenIdle();
  assert.equal(t.run(() => list.view()).query, "zzzz", "an empty list shows every query that matched nothing");
});

test("writing again as the order changes keeps the selected candidate on the cursor line", async (assert, t) => {
  const g = gate();
  const { list, pane } = setup(t, { sources: [fake(["d", "f", "h"], g.wait, ["a", "e", "b"])], sorter: alphabetical });
  await until(() => lines(t, pane).length === 3);
  assert.deepEqual(lines(t, pane), ["d", "f", "h"]);
  t.run(() => list.next());
  assert.equal(t.run(() => list.selected())?.value, "f");
  g.open();
  await list.whenIdle();
  assert.deepEqual(lines(t, pane), ["a", "b", "d", "e", "f", "h"]);
  assert.equal(cursor(t, pane), 4, "f moved down with the cursor");
  assert.equal(t.run(() => list.selected())?.value, "f");
});

test("until the cursor moved, it stays at the top as the order changes", async (assert, t) => {
  const g = gate();
  const { list, pane } = setup(t, { sources: [fake(["m", "n"], g.wait, ["a"])], sorter: alphabetical });
  await until(() => lines(t, pane).length === 2);
  g.open();
  await list.whenIdle();
  assert.deepEqual(lines(t, pane), ["a", "m", "n"]);
  assert.equal(cursor(t, pane), 0);
  assert.equal(t.run(() => list.selected())?.value, "a");
});

test("moving: by count, pages, first and last, round with wrap", async (assert, t) => {
  const { list, pane } = setup(t, { sources: [fake(numbers(8))] }, {}, 3);
  await list.whenIdle();
  t.run(() => list.next(3));
  assert.equal(cursor(t, pane), 3);
  t.run(() => list.prev());
  assert.equal(cursor(t, pane), 2);
  t.run(() => list.page());
  assert.equal(cursor(t, pane), 5, "a page is the pane's height");
  t.run(() => list.next(10));
  assert.equal(cursor(t, pane), 7, "stops at the end");
  t.run(() => list.next());
  assert.equal(cursor(t, pane), 7, "no wrap by default");
  t.run(() => list.next(1, { wrap: true }));
  assert.equal(cursor(t, pane), 0, "round to the top");
  t.run(() => list.prev(1, { wrap: true }));
  await list.whenIdle();
  assert.equal(cursor(t, pane), 7, "round to the bottom");
  t.run(() => list.first());
  assert.equal(cursor(t, pane), 0);
  t.run(() => list.prev(5));
  assert.equal(cursor(t, pane), 0);
});

test("the cursor nearing the end writes more", async (assert, t) => {
  const { list, pane } = setup(t, { sources: [fake(numbers(100))] }, { initial: 10, chunk: 10 }, 4);
  await list.whenIdle();
  assert.equal(lines(t, pane).length, 10, "the first lines only");
  t.run(() => list.next(4));
  await list.whenIdle();
  assert.equal(lines(t, pane).length, 10, "not near the end yet");
  t.run(() => list.next(2));
  await list.whenIdle();
  assert.equal(lines(t, pane).length, 20, "a chunk more");
  assert.deepEqual(lines(t, pane), numbers(20));
  assert.equal(cursor(t, pane), 6, "the cursor stayed");
  t.run(() => list.next(100));
  await list.whenIdle();
  assert.equal(cursor(t, pane), 19, "stopped at the last line written");
  assert.equal(lines(t, pane).length, 30);
});

test("last() writes everything and selects the last match", async (assert, t) => {
  const { list, pane } = setup(t, { sources: [fake(numbers(55))] }, { initial: 10, chunk: 10 }, 4);
  await list.whenIdle();
  t.run(() => list.last());
  await list.whenIdle();
  assert.equal(lines(t, pane).length, 55);
  assert.equal(cursor(t, pane), 54);
  assert.equal(t.run(() => list.selected())?.value, "item 54");
});

test("scrolling the pane near the end writes more (PaneScrolled)", async (assert, t) => {
  const { list, pane } = setup(t, { sources: [fake(numbers(100))] }, { initial: 12, chunk: 10 }, 4);
  await list.whenIdle();
  await t.settle();
  assert.equal(lines(t, pane).length, 12);
  t.action.text.scroll(3, { pane });
  await t.settle();
  await list.whenIdle();
  assert.equal(lines(t, pane).length, 12, "lines 3-6 of 12: not near the end");
  t.action.text.scroll(3, { pane });
  await t.settle();
  await list.whenIdle();
  assert.equal(lines(t, pane).length, 22, "lines 6-9: a chunk more");
  assert.deepEqual(lines(t, pane), numbers(22));
});

test("matched parts become decorations styled FFMatch", async (assert, t) => {
  const { s, list, pane } = setup(t, { sources: [fake(["xabx", "nope", "ab"])], matcher: substring(), query: "ab" });
  await list.whenIdle();
  assert.deepEqual(lines(t, pane), ["xabx", "ab"]);
  const decos = t.action.text.decorations({ namespace: list.namespace, pane });
  assert.deepEqual(
    decos.map((d) => [d.range, d.style]),
    [
      [{ start: { line: 0, character: 1 }, end: { line: 0, character: 3 } }, "FFMatch"],
      [{ start: { line: 1, character: 0 }, end: { line: 1, character: 2 } }, "FFMatch"],
    ],
  );
  s.setQuery("x");
  await list.whenIdle();
  assert.deepEqual(
    t.action.text.decorations({ namespace: list.namespace, pane }).map((d) => [d.range, d.deleted]),
    [[{ start: { line: 0, character: 0 }, end: { line: 0, character: 1 } }, false]],
    "the old ones gone",
  );
});

test("renderers run on the lines written only, and their labels and decorations are shown", async (assert, t) => {
  let rendered = 0;
  const renderer = defineRenderer(({ items }) => {
    rendered += items.length;
    for (const i of items) {
      i.label = `> ${i.label}\n`;
      i.decorations = [
        ...i.decorations.map((d) => ({ ...d, start: d.start + 2 })),
        { start: 0, length: 1, style: "X" },
      ];
    }
  });
  const { list, pane } = setup(
    t,
    { sources: [fake(numbers(50, "a"))], matcher: substring(), query: "a" },
    // No margin: the five lines fill the pane, and its PaneScrolled
    // (whenever the hook runs, maybe after whenIdle) would want a chunk more.
    { renderer, initial: 5, chunk: 5, margin: 0 },
  );
  await list.whenIdle();
  assert.deepEqual(lines(t, pane), ["> a 0 ", "> a 1 ", "> a 2 ", "> a 3 ", "> a 4 "], "line breaks become spaces");
  assert.equal(rendered, 5);
  const first = t.action.text
    .decorations({
      namespace: list.namespace,
      pane,
      range: { start: { line: 0, character: 0 }, end: { line: 0, character: 6 } },
    })
    .map((d) => [d.range.start.character, d.range.end.character, d.style]);
  assert.deepEqual(first, [
    [0, 1, "X"],
    [2, 3, "FFMatch"],
  ]);
});

test("marks: toggled on the selected line, shown, kept across queries", async (assert, t) => {
  const { s, list, pane } = setup(t, { sources: [fake(["apple", "apricot", "banana"])], matcher: substring() });
  await list.whenIdle();
  t.run(() => {
    list.next();
    assert.true(list.toggleMark());
  });
  const marks = () =>
    t.action.text
      .decorations({ namespace: list.namespace, pane })
      .filter((d) => d.style === "FFMarked" && !d.deleted)
      .map((d) => d.range.start.line);
  assert.deepEqual(marks(), [1]);
  assert.deepEqual(
    s.markedItems().map((i) => i.value),
    ["apricot"],
  );
  s.setQuery("ap");
  await list.whenIdle();
  assert.deepEqual(marks(), [1], "apricot still marked");
  s.setQuery("ric");
  await list.whenIdle();
  assert.deepEqual(marks(), [0]);
  t.run(() => assert.false(list.toggleMark()));
  assert.deepEqual(marks(), []);
  assert.equal(t.run(() => list.view()).marked, 0);
});

test("dispose closes the pane and deletes the buffer and its decorations", async (assert, t) => {
  const closed: string[] = [];
  const { s, list, pane } = setup(
    t,
    { sources: [fake(["ab", "cab"])], matcher: substring(), query: "ab" },
    { onClose: () => closed.push("closed") },
  );
  await list.whenIdle();
  const buffer = list.buffer;
  assert.true(t.action.buffers({ all: true }).some((b) => b.id === buffer));
  t.run(() => list.dispose());
  assert.false(
    t.action.panes().some((p) => p.id === pane),
    "the pane closed",
  );
  assert.false(
    t.action.buffers({ all: true }).some((b) => b.id === buffer),
    "the buffer went",
  );
  assert.deepEqual(closed, ["closed"]);
  assert.true(list.disposed);
  assert.false(s.disposed, "the session is its owner's");
  t.run(() => list.dispose());
  assert.deepEqual(closed, ["closed"], "once");
});

test("actions: by name or the default, with the cursor's and the marked items; true keeps the list", async (assert, t) => {
  const heard: string[] = [];
  const record = (name: string, keep: boolean): Action<object> =>
    defineAction((p: InvokeParams<object>, o) => {
      heard.push(
        `${name}: ${p.item?.value} [${p.marked.map((i) => i.value)}] ${p.matched.length} -> ${chosenItems(p).map((i) => i.value)}`,
      );
      assert.equal(typeof o.action.focusedPane, "function", "the task's actions");
      assert.false(o.signal.aborted);
      return keep ? true : undefined;
    });
  const { list, pane } = setup(
    t,
    { sources: [fake(["one", "two", "three"])] },
    { actions: { keep: record("keep", true), close: record("close", false) }, defaultAction: "close" },
  );
  await list.whenIdle();
  t.run(() => list.next());
  assert.true(await invoke(t, list, "keep"));
  t.run(() => {
    list.toggleMark();
    list.next();
    list.toggleMark();
  });
  assert.true(await invoke(t, list, "keep"));
  assert.true(
    t.action.panes().some((p) => p.id === pane),
    "still open",
  );
  assert.throws(() => t.run(() => list.invoke("missing")), /no action missing/);
  assert.false(await invoke(t, list));
  assert.deepEqual(heard, [
    "keep: two [] 3 -> two",
    "keep: three [two,three] 3 -> two,three",
    "close: three [two,three] 3 -> two,three",
  ]);
  assert.true(list.disposed, "the default action closed it");
  assert.false(t.action.panes().some((p) => p.id === pane));
});

test("an action's signal is aborted when the list goes", async (assert, t) => {
  let signal: AbortSignal | undefined;
  const g = gate();
  const slow = defineAction(async (_p, o): Promise<true> => {
    signal = o.signal;
    await g.wait;
    return true;
  });
  const { list } = setup(t, { sources: [fake(["a"])] }, { actions: { slow } });
  await list.whenIdle();
  const done = invoke(t, list, "slow");
  // (t.settle would wait for the action's task.)
  await until(() => signal !== undefined);
  t.run(() => list.dispose());
  assert.true(signal?.aborted);
  g.open();
  assert.false(await done, "disposed meanwhile");
});
