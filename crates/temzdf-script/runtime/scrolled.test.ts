// PaneScrolled: a text pane's lines in sight changed (`temzdf test`,
// doc/dev/testing.md). The mouse wheel is tested in
// crates/temzdf-server/src/session/host_tests.rs.
import { action, on } from "temzdf";
import type { ScrollInfo } from "temzdf";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

const LONG = Array.from({ length: 200 }, (_, i) => `line ${i}\n`).join("");

/** What PaneScrolled hooks hear about `pane` from now on (`heard.length =
 * 0` forgets what came so far). */
function listen(t: TestContext, pane: number): ScrollInfo[] {
  const heard: ScrollInfo[] = [];
  const off = t.run(() =>
    on("PaneScrolled", (p, info) => {
      if (p === pane) heard.push(info);
    }),
  );
  t.after(() => t.run(off));
  return heard;
}

/** The first and last lines `pane` shows, as visibleRange says. */
function lines(t: TestContext, pane: number): { first: number; last: number } {
  const r = t.action.text.visibleRange({ pane });
  return { first: r.start.line, last: r.end.line };
}

/** A long buffer in a new pane, settled, and what its hooks hear. */
async function long(t: TestContext): Promise<{ pane: number; heard: ScrollInfo[]; height: number }> {
  const pane = t.openText(LONG);
  await t.settle();
  const heard = listen(t, pane);
  const { first, last } = lines(t, pane);
  return { pane, heard, height: last - first + 1 };
}

test("a new pane is heard once, without previous lines", async (assert, t) => {
  const heard: [number, ScrollInfo][] = [];
  const off = t.run(() =>
    on("PaneScrolled", (p, info) => {
      heard.push([p, info]);
    }),
  );
  t.after(() => t.run(off));
  const pane = t.openText(LONG);
  await t.settle();
  const buffer = t.action.text.info({ pane }).buffer;
  const mine = heard.filter(([p]) => p === pane).map(([, info]) => info);
  assert.deepEqual(mine, [{ buffer, ...lines(t, pane), previous: undefined }]);
  assert.equal(mine[0]?.first, 0);
  assert.ok((mine[0]?.last ?? 0) > 0, "several lines shown");
});

test("keys that scroll are heard once each, with the lines before", async (assert, t) => {
  const { pane, heard, height } = await long(t);
  const buffer = t.action.text.info({ pane }).buffer;
  await t.keys("3<C-e>");
  assert.deepEqual(heard, [{ buffer, first: 3, last: height + 2, previous: { first: 0, last: height - 1 } }]);
  heard.length = 0;
  await t.keys("<C-y>");
  assert.deepEqual(heard, [{ buffer, first: 2, last: height + 1, previous: { first: 3, last: height + 2 } }]);
});

test("moving the cursor within sight is not heard; past the bottom it is", async (assert, t) => {
  const { pane, heard, height } = await long(t);
  await t.keys("jj");
  assert.deepEqual(heard, [], "within sight");
  await t.keys("G");
  assert.equal(heard.length, 1, "the cursor's line revealed");
  assert.deepEqual(heard[0]?.previous, { first: 0, last: height - 1 });
  assert.equal(heard[0]?.last, 199, "the last line in sight");
  assert.deepEqual({ first: heard[0]?.first, last: heard[0]?.last }, lines(t, pane));
});

test("several scrolls in one go are heard once, as they ended", async (assert, t) => {
  const { pane, heard, height } = await long(t);
  t.run(() => {
    action.text.scroll(5, { pane });
    action.text.reveal({ line: 50, character: 0 }, "top", { pane });
    action.text.scroll(-3, { pane });
  });
  await t.settle();
  assert.equal(heard.length, 1);
  assert.deepEqual(heard[0]?.previous, { first: 0, last: height - 1 });
  assert.deepEqual({ first: heard[0]?.first, last: heard[0]?.last }, lines(t, pane));
  heard.length = 0;
  t.run(() => {
    action.text.scroll(4, { pane });
    action.text.scroll(-4, { pane });
  });
  await t.settle();
  assert.deepEqual(heard, [], "back where it was: nothing to hear");
});

test("a split makes the pane show fewer lines", async (assert, t) => {
  const { pane, heard, height } = await long(t);
  const below = t.action.split({ side: "bottom", pane });
  if (below !== undefined) t.after(() => t.action.closePane(below));
  await t.settle();
  assert.equal(heard.length, 1);
  assert.deepEqual(heard[0]?.previous, { first: 0, last: height - 1 });
  assert.equal(heard[0]?.first, 0);
  assert.ok((heard[0]?.last ?? height) < height - 1, "fewer lines");
  assert.deepEqual({ first: heard[0]?.first, last: heard[0]?.last }, lines(t, pane));
});

test("lines inserted above shift what is shown; edits in sight are not heard", async (assert, t) => {
  const { pane, heard, height } = await long(t);
  t.action.text.scroll(10, { pane });
  await t.settle();
  heard.length = 0;
  t.action.text.insert("a\nb\n", { line: 0, character: 0 }, { pane });
  await t.settle();
  assert.deepEqual(
    heard.map((h) => [h.first, h.last, h.previous]),
    [[12, height + 11, { first: 10, last: height + 9 }]],
    "the same text in sight, two lines further down",
  );
  heard.length = 0;
  t.action.text.insert("more ", { line: 13, character: 0 }, { pane });
  await t.settle();
  assert.deepEqual(heard, []);
});
