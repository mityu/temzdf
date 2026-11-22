// temzdf/ff's engine (runtime/ff/session.ts): collecting, matching,
// sorting and marking without a UI, on fake sources (`temzdf test`,
// doc/dev/testing.md).
import { keymap } from "temzdf";
import { createSession, defineCurator, defineMatcher, defineSource } from "temzdf/ff";
import type { Matcher, Session, SessionOptions, Snapshot, Source } from "temzdf/ff";
import { byScore, fuzzy, substring } from "temzdf/ff/builtin";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** Wait (polling) until `cond` holds, at most `ms` milliseconds. */
async function until(cond: () => boolean, ms = 3000): Promise<void> {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error("timed out waiting");
    await sleep(2);
  }
}

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

/** A matcher passing `inner`'s matches on, recording what it was given. */
function spy(inner: Matcher = substring()): Matcher & { given: string[]; seen: number; signals: AbortSignal[] } {
  const m = {
    given: [] as string[],
    seen: 0,
    signals: [] as AbortSignal[],
    match: defineMatcher(async function* (p, o) {
      m.signals.push(o.signal);
      m.given.push(...p.items.map((i) => i.value));
      m.seen += p.items.length;
      yield* inner.match(p, o);
    }).match,
  };
  return m;
}

/** A session started, disposed after the test. */
function session(t: TestContext, o: SessionOptions<object>): Session<object> {
  const s = createSession(o);
  t.after(() => s.dispose());
  s.start();
  return s;
}

const values = (snap: Snapshot<object>) => snap.items.map((i) => i.value);
const numbers = (n: number, prefix = "item") => Array.from({ length: n }, (_, i) => `${prefix} ${i}`);

test("collects, matches the query and publishes a snapshot", async (assert, t) => {
  const s = session(t, { sources: [fake(["apple", "banana", "cherry", "grape"])], matcher: substring() });
  await s.whenIdle();
  assert.deepEqual(values(s.snapshot()), ["apple", "banana", "cherry", "grape"]);
  s.setQuery("ap");
  await s.whenIdle();
  const snap = s.snapshot();
  assert.deepEqual(values(snap), ["apple", "grape"]);
  assert.deepEqual([snap.query, snap.matched, snap.collected, snap.state], ["ap", 2, 4, "done"]);
  assert.deepEqual(snap.sources, [{ state: "done", collected: 4 }]);
  assert.deepEqual(values(s.snapshot({ start: 1, end: 2 })), ["grape"]);
});

test("the engine waits for a timer between chunks: a timer set during a match runs before it ends", async (assert, t) => {
  const m = spy();
  const s = session(t, { sources: [fake(numbers(2000))], matcher: m, chunkItems: 100 });
  await s.whenIdle();
  m.seen = 0;
  s.setQuery("item 1");
  let atTimer = -1;
  setTimeout(() => {
    atTimer = m.seen;
  }, 0);
  await s.whenIdle();
  assert.equal(m.seen, 2000, "everything matched again");
  assert.ok(atTimer > 0 && atTimer < 2000, `the timer ran after ${atTimer} items`);
});

test("a mapped key runs between chunks of a long match", async (assert, t) => {
  t.openText("x\n");
  const namespace = "ff.test";
  const m = spy();
  const s = session(t, { sources: [fake(numbers(20000))], matcher: m, chunkItems: 100 });
  await s.whenIdle();
  let atKey = -1;
  t.run(() =>
    keymap.add(
      "vim/normal",
      "<F3>",
      () => {
        atKey = m.seen;
      },
      { namespace },
    ),
  );
  t.after(() => t.run(() => keymap.clear({ namespace })));
  m.seen = 0;
  s.setQuery("item 2");
  t.action.feedkeys("<F3>");
  await s.whenIdle();
  await t.settle();
  assert.equal(m.seen, 20000);
  assert.ok(atKey >= 0 && atKey < 20000, `the key ran after ${atKey} items`);
});

test("a new query aborts the running match: its results never appear", async (assert, t) => {
  // A matcher that does not look at its signal: the engine drops what an
  // older match yields by itself.
  const plain = defineMatcher(async function* ({ query, items }) {
    for (const i of items) if (i.value.includes(query)) yield i;
  });
  const m = spy(plain);
  const s = session(t, { sources: [fake([...numbers(3000, "a"), ...numbers(3000, "b")])], matcher: m, chunkItems: 50 });
  await s.whenIdle();
  const heard: Snapshot<object>[] = [];
  s.onChange(() => heard.push(s.snapshot()));
  m.signals.length = 0;
  s.setQuery("a 1");
  await sleep(0);
  await sleep(0);
  const first = m.signals[0];
  s.setQuery("b 2");
  await s.whenIdle();
  assert.true(first?.aborted, "the first match's signal aborted");
  const ok = (h: Snapshot<object>) => h.query === "b 2" && h.items.every((i) => i.value.includes("b 2"));
  const last = heard[heard.length - 1]!;
  assert.true(ok(last), "only the new query's matches");
  assert.equal(last.matched, numbers(3000, "b").filter((v) => v.includes("b 2")).length);
  const after = heard.findIndex((h) => h.query === "b 2");
  assert.true(heard.slice(after).every(ok), "nothing of the old query after the new one appeared");
});

test("while the query stays only new batches are matched; a new query matches everything", async (assert, t) => {
  const g = gate();
  const m = spy();
  const s = session(t, { sources: [fake(["one", "two"], g.wait, ["three"])], matcher: m, query: "o" });
  await until(() => s.snapshot().matched === 2);
  assert.deepEqual(m.given, ["one", "two"]);
  assert.equal(s.snapshot().state, "collecting");
  g.open();
  await s.whenIdle();
  assert.deepEqual(m.given, ["one", "two", "three"], "the first batch not matched again");
  assert.deepEqual(values(s.snapshot()), ["one", "two"]);
  m.given.length = 0;
  s.setQuery("t");
  await s.whenIdle();
  assert.deepEqual(m.given, ["one", "two", "three"]);
  assert.deepEqual(values(s.snapshot()), ["two", "three"]);
});

test("a new query is published once it has a match or is done: the old matches stay meanwhile", async (assert, t) => {
  // A matcher waiting before its first match: a publication due meanwhile
  // (the first after a quiet spell is due at once) must not empty the list.
  let wait: Promise<void> | undefined;
  const slow = defineMatcher(async function* (p, o) {
    if (wait !== undefined) await wait;
    yield* substring().match(p, o);
  });
  const s = session(t, { sources: [fake(["apple", "banana", "cherry"])], matcher: slow });
  await s.whenIdle();
  const heard: Snapshot<object>[] = [];
  s.onChange(() => heard.push(s.snapshot()));
  await sleep(60);
  const g = gate();
  wait = g.wait;
  s.setQuery("an");
  await sleep(60);
  assert.deepEqual(heard, [], "nothing published while the match waits");
  assert.deepEqual(values(s.snapshot()), ["apple", "banana", "cherry"], "the old matches");
  assert.equal(s.snapshot().state, "matching");
  g.open();
  await s.whenIdle();
  assert.deepEqual(values(s.snapshot()), ["banana"]);
  assert.true(
    heard.every((h) => h.matched > 0),
    `heard ${heard.map((h) => `${h.query}:${h.matched}`).join(" ")}`,
  );

  // No match at all: published once the match is done.
  const g2 = gate();
  wait = g2.wait;
  s.setQuery("zz");
  await sleep(60);
  assert.deepEqual(values(s.snapshot()), ["banana"]);
  g2.open();
  await s.whenIdle();
  const snap = s.snapshot();
  assert.deepEqual([snap.query, snap.matched, snap.state], ["zz", 0, "done"]);
});

test("the matches are sorted, also as batches come", async (assert, t) => {
  const g = gate();
  const s = session(t, {
    sources: [fake(["xxaxxb", "ab"], g.wait, ["a_b", "zzz"])],
    matcher: fuzzy(),
    sorter: byScore(),
    query: "ab",
  });
  await until(() => s.snapshot().matched === 2);
  assert.deepEqual(values(s.snapshot()), ["ab", "xxaxxb"]);
  g.open();
  await s.whenIdle();
  assert.deepEqual(values(s.snapshot()), ["ab", "a_b", "xxaxxb"]);
});

test("marks are kept by source and id across new queries", async (assert, t) => {
  const s = session(t, { sources: [fake(["apple", "apricot", "banana"])], matcher: substring() });
  await s.whenIdle();
  const apricot = s.snapshot().items[1]!;
  assert.true(s.toggleMark(apricot));
  s.setQuery("ap");
  await s.whenIdle();
  const items = s.snapshot().items;
  assert.notStrictEqual(items[1], apricot, "a new item object (decorated)");
  assert.false(s.isMarked(items[0]!));
  assert.true(s.isMarked(items[1]!));
  assert.deepEqual(s.snapshot().marked, [{ source: 0, id: 1 }]);
  assert.deepEqual(
    s.markedItems().map((i) => i.value),
    ["apricot"],
  );
  assert.false(s.toggleMark(items[1]!));
  assert.deepEqual(s.snapshot().marked, []);
  assert.false(s.mark({ id: 0, value: "stranger", detail: {} }), "an item the session does not know");
});

test("a failing source sets its state and error; the others go on", async (assert, t) => {
  const broken = defineSource(async function* () {
    yield { id: 0, value: "first", detail: {} };
    await sleep(5);
    throw new Error("boom");
  });
  const s = session(t, { sources: [broken, fake(["a"], sleep(20), ["b"])], matcher: substring() });
  await s.whenIdle();
  const snap = s.snapshot();
  assert.deepEqual(
    snap.sources.map((x) => x.state),
    ["failed", "done"],
  );
  assert.equal((snap.sources[0]!.error as Error).message, "boom");
  assert.deepEqual(
    snap.errors.map((e) => [e.part, e.source, (e.error as Error).message]),
    [["source", 0, "boom"]],
  );
  assert.deepEqual(values(snap), ["first", "a", "b"]);
});

test("dispose aborts the collection", async (assert, t) => {
  let signal: AbortSignal | undefined;
  let cleaned = false;
  const endless = defineSource(async function* (_, o) {
    signal = o.signal;
    try {
      yield { id: 0, value: "only", detail: {} };
      await new Promise((r) => o.signal.addEventListener("abort", () => r(undefined)));
    } finally {
      cleaned = true;
    }
  });
  const s = session(t, { sources: [endless] });
  await until(() => s.snapshot().collected === 1);
  assert.equal(s.snapshot().state, "collecting");
  const idle = s.whenIdle();
  s.dispose();
  await idle;
  assert.true(signal?.aborted);
  await until(() => cleaned);
  assert.true(s.disposed);
  assert.equal(s.snapshot().sources[0]?.state, "stopped");
});

test("several sources: items told apart by source and id", async (assert, t) => {
  const s = session(t, { sources: [fake(["same", "left"]), fake(["same", "right"])], matcher: substring() });
  await s.whenIdle();
  const snap = s.snapshot();
  assert.equal(snap.collected, 4);
  const keys = snap.items.map((i) => s.keyOf(i));
  assert.deepEqual(keys, [
    { source: 0, id: 0 },
    { source: 0, id: 1 },
    { source: 1, id: 0 },
    { source: 1, id: 1 },
  ]);
  s.mark(snap.items[2]!);
  assert.false(s.isMarked(snap.items[0]!), "the same id in the other source is not marked");
  assert.equal(s.item({ source: 1, id: 1 })?.value, "right");
  s.setQuery("same");
  await s.whenIdle();
  assert.deepEqual(
    s.snapshot().items.map((i) => [s.keyOf(i)?.source, s.isMarked(i)]),
    [
      [0, false],
      [1, true],
    ],
  );
});

test("a curator collects again for each query", async (assert, t) => {
  const asked: string[] = [];
  const curator = defineCurator(async function* ({ query }) {
    asked.push(query);
    for (const [id, value] of [`${query} 1`, `${query} 2`, "other"].entries()) yield { id, value, detail: {} };
  });
  const s = session(t, { sources: [curator, fake(["fixed"])], matcher: substring(), query: "q" });
  await s.whenIdle();
  assert.deepEqual(values(s.snapshot()), ["q 1", "q 2"]);
  s.setQuery("fi");
  await s.whenIdle();
  assert.deepEqual(asked, ["q", "fi"]);
  assert.deepEqual(values(s.snapshot()), ["fixed", "fi 1", "fi 2"]);
  assert.equal(s.snapshot().collected, 4);
});

test("changes are heard at once, then at most every throttle milliseconds", async (assert, t) => {
  const m = spy();
  const s = session(t, { sources: [fake(numbers(20000))], matcher: m, chunkItems: 100, throttle: 200 });
  await s.whenIdle();
  const times: number[] = [];
  s.onChange(() => times.push(Date.now()));
  await sleep(250);
  const asked = Date.now();
  s.setQuery("item 3");
  await s.whenIdle();
  assert.ok(times.length >= 2, `heard ${times.length} times`);
  assert.ok(times[0]! - asked < 100, `first heard after ${times[0]! - asked} ms`);
  const gaps = times.slice(1).map((x, i) => x - times[i]!);
  assert.ok(
    gaps.every((g) => g >= 195),
    `gaps ${gaps.join(" ")}`,
  );
});

// 100k items take a few seconds in a debug build: the engine must keep
// taking the host's turns meanwhile.
test(
  "100k items: the engine stays responsive while matching",
  async (assert, t) => {
    const n = 100_000;
    const many = defineSource(async function* () {
      for (let i = 0; i < n; i++) yield { id: i, value: `src/dir${i % 100}/file${i}.ts`, detail: {} };
    });
    const m = spy(fuzzy());
    const s = session(t, { sources: [many], matcher: m, sorter: byScore() });
    const started = Date.now();
    await s.whenIdle();
    const collected = Date.now() - started;
    assert.equal(s.snapshot().collected, n);
    let atTimer = -1;
    m.seen = 0;
    const asked = Date.now();
    s.setQuery("d9f99");
    setTimeout(() => {
      atTimer = m.seen;
    }, 0);
    await s.whenIdle();
    const matched = Date.now() - asked;
    assert.equal(m.seen, n);
    assert.ok(atTimer > 0 && atTimer < n, `the timer ran after ${atTimer} items`);
    const snap = s.snapshot();
    assert.ok(snap.matched > 0 && snap.matched < n, `${snap.matched} matched`);
    assert.ok(snap.items[0]!.value.includes("dir9"), snap.items[0]!.value);
    console.log(`100k: collected in ${collected} ms, matched in ${matched} ms`);
  },
  { timeout: 25_000 },
);
