// Floating panes' options `border` and `focusable` (`temzdf test`,
// doc/dev/testing.md, doc/design/floating-panes.md). Frames drawn and
// clicks are tested in crates/temzdf-server/src/session/tests.rs.
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

type FloatOpts = NonNullable<Parameters<TestContext["action"]["openFloat"]>[1]>;

/** A float on a new text buffer holding `text`, with `opts`; both go
 * after the test. */
function float(t: TestContext, text: string, opts: FloatOpts): number {
  const buffer = t.action.newBuffer({ text, attrs: ["nofile"] });
  t.after(() => t.action.deleteBuffer(buffer));
  const pane = t.action.openFloat({ buffer }, opts);
  if (pane === undefined) throw new Error("no float");
  t.after(() => {
    if (t.action.panes().some((p) => p.id === pane)) t.action.closePane(pane);
  });
  return pane;
}

function pane(t: TestContext, id: number) {
  const p = t.action.panes().find((p) => p.id === id);
  if (p === undefined) throw new Error(`no pane ${id}`);
  return p;
}

test("without a border, the float is its content at col and row, one cell at least", (assert, t) => {
  const framed = float(t, "a", { col: 5, row: 3, width: 12, height: 5, focus: false });
  assert.deepEqual(pane(t, framed).rect, { col: 6, row: 4, width: 12, height: 5 }, "inside its frame");
  const bare = float(t, "b", { col: 5, row: 3, width: 10, height: 2, border: false, focus: false });
  assert.deepEqual(pane(t, bare).rect, { col: 5, row: 3, width: 10, height: 2 });
  t.action.floatConfig(bare, { width: 1, height: 1 });
  assert.deepEqual(pane(t, bare).rect, { col: 5, row: 3, width: 1, height: 1 }, "one cell");
  t.action.floatConfig(framed, { width: 1, height: 1 });
  assert.deepEqual(pane(t, framed).rect, { col: 6, row: 4, width: 10, height: 4 }, "a frame keeps the minimum");
  t.action.floatConfig(bare, { border: true });
  assert.deepEqual(pane(t, bare).rect, { col: 6, row: 4, width: 10, height: 4 }, "framed again");
});

test("a float without a border at a text position starts in that cell", (assert, t) => {
  const text = t.openText("one\ntwo three\n");
  const at = t.action.text.screenPosition({ line: 1, character: 4 }, { pane: text });
  if (at === undefined) throw new Error("not shown");
  const popup = float(t, "x", { ...at, width: 5, height: 1, border: false, focus: false });
  const r = pane(t, popup).rect;
  const tr = pane(t, text).rect;
  assert.deepEqual({ col: r.col, row: r.row }, { col: tr.col + 4, row: tr.row + 1 });
});

test("a float that never takes the focus", (assert, t) => {
  const text = t.openText("main\n");
  const popup = float(t, "list\n", { width: 10, height: 3, focusable: false });
  assert.equal(t.action.focusedPane(), text, "not focused when opened");
  assert.deepEqual([pane(t, popup).focusable, pane(t, popup).floating, pane(t, text).focusable], [false, true, true]);
  // Cycling never stops on it.
  const seen = new Set<number>();
  for (let i = 0; i < t.action.panes().length + 1; i++) {
    t.action.focusNextPane();
    seen.add(t.action.focusedPane() ?? -1);
  }
  assert.ok(seen.has(text), "the others take turns");
  assert.notOk(seen.has(popup), "skipped");
  t.action.focusPane(text);
  assert.throws(() => t.action.focusPane(popup), /never takes the focus/);
  assert.equal(t.action.focusedPane(), text);
  // A float that takes the focus stops being one; the focus leaves it.
  const other = float(t, "other\n", { width: 10, height: 3 });
  assert.equal(t.action.focusedPane(), other);
  t.action.floatConfig(other, { focusable: false });
  assert.notEqual(t.action.focusedPane(), other);
  assert.equal(pane(t, other).focusable, false);
});

test("toggleFloats neither hides nor counts a float that never takes the focus", async (assert, t) => {
  const text = t.openText("main\n");
  const popup = float(t, "list\n", { width: 10, height: 3, focusable: false });
  const shown = () => t.action.text.screenPosition({ line: 0, character: 0 }, { pane: popup }) !== undefined;
  assert.ok(shown());
  // The only float: as if there were none, a floating shell opens.
  const before = t.action.panes().map((p) => p.id);
  t.action.toggleFloats();
  const shells = t.action.panes().filter((p) => !before.includes(p.id));
  assert.deepEqual(
    shells.map((p) => [p.kind, p.floating]),
    [["terminal", true]],
  );
  const shell = shells[0]?.id ?? -1;
  t.after(() => {
    if (t.action.panes().some((p) => p.id === shell)) t.action.closePane(shell);
  });
  assert.equal(t.action.focusedPane(), shell);
  // Hidden with the others, it stays.
  const tile = float(t, "tile\n", { width: 10, height: 3 });
  t.action.toggleFloats();
  assert.equal(t.action.focusedPane(), text, "back to the tiled pane");
  assert.equal(t.action.text.screenPosition({ line: 0, character: 0 }, { pane: tile }), undefined, "hidden");
  assert.ok(shown(), "still shown");
  t.action.toggleFloats();
  assert.notEqual(t.action.focusedPane(), popup, "showing them focuses the top one that takes the focus");
  await t.settle();
});
