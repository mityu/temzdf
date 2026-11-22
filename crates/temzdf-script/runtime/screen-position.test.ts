// text.screenPosition and screenSize: where a text pane shows a position,
// counted as openFloat's col and row are (`temzdf test`,
// doc/dev/testing.md). The layout math itself is tested against the drawn
// cursor in crates/temzdf-texteditor/src/view.rs.
import { o } from "temzdf";
import type { Position } from "temzdf";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

/** Pane `pane`'s rectangle, as panes() says. */
function rect(t: TestContext, pane: number): { col: number; row: number; width: number; height: number } {
  const p = t.action.panes().find((p) => p.id === pane);
  if (p === undefined) throw new Error(`no pane ${pane}`);
  return p.rect;
}

/** The cell `x` columns and `y` rows into `pane` (no tab line above). */
function cell(t: TestContext, pane: number, x: number, y: number): { col: number; row: number } {
  const r = rect(t, pane);
  return { col: r.col + x, row: r.row + y };
}

function at(line: number, character: number): Position {
  return { line, character };
}

test("the cursor and other positions of plain text", (assert, t) => {
  const pane = t.openText("hello\nworld\n");
  assert.deepEqual(t.action.text.screenPosition(undefined, { pane }), cell(t, pane, 0, 0), "the cursor");
  assert.deepEqual(t.action.text.screenPosition(at(1, 3), { pane }), cell(t, pane, 3, 1));
  t.action.text.setCursor(at(1, 5), { pane });
  assert.deepEqual(t.action.text.screenPosition(undefined, { pane }), cell(t, pane, 5, 1), "a line's end");
  assert.deepEqual(t.action.text.screenPosition(at(1, 2)), cell(t, pane, 2, 1), "the focused pane's");
});

test("scrolled: lines above and below the pane are not on screen", (assert, t) => {
  const pane = t.openText(Array.from({ length: 200 }, (_, i) => `line ${i}\n`).join(""));
  t.action.text.scroll(10, { pane });
  assert.equal(t.action.text.screenPosition(at(0, 0), { pane }), undefined);
  assert.deepEqual(t.action.text.screenPosition(at(12, 2), { pane }), cell(t, pane, 2, 2));
  assert.equal(t.action.text.screenPosition(at(150, 0), { pane }), undefined);
});

test("line numbers and wrapped lines", (assert, t) => {
  const pane = t.openText("");
  t.action.text.setOptions({ number: "absolute", wrap: true }, { pane });
  // Three digits at least, and a space: the text from the fifth column.
  const room = rect(t, pane).width - 4;
  t.action.text.insert(`${"x".repeat(room + 5)}\nnext`, at(0, 0), { pane });
  assert.deepEqual(t.action.text.screenPosition(at(0, 0), { pane }), cell(t, pane, 4, 0));
  assert.deepEqual(t.action.text.screenPosition(at(0, room + 2), { pane }), cell(t, pane, 6, 1), "its second row");
  assert.deepEqual(t.action.text.screenPosition(at(1, 1), { pane }), cell(t, pane, 5, 2), "the next line below");
});

test("without wrapping, scrolled sideways", (assert, t) => {
  const pane = t.openText(`${"y".repeat(300)}\n`);
  t.action.text.setOptions({ wrap: false }, { pane });
  t.action.text.setCursor(at(0, 200), { pane });
  const width = rect(t, pane).width;
  assert.deepEqual(t.action.text.screenPosition(undefined, { pane }), cell(t, pane, width - 1, 0), "at the right edge");
  assert.equal(t.action.text.screenPosition(at(0, 0), { pane }), undefined, "left of the edge");
  assert.equal(t.action.text.screenPosition(at(0, 250), { pane }), undefined, "right of it");
  assert.deepEqual(t.action.text.screenPosition(at(0, 201 - width), { pane }), cell(t, pane, 0, 0));
});

test("tabs and wide characters", (assert, t) => {
  const tabstop = t.run(() => o.tabstop);
  t.run(() => {
    o.tabstop = 4;
  });
  t.after(() =>
    t.run(() => {
      o.tabstop = tabstop;
    }),
  );
  const pane = t.openText("\tあいx\n");
  assert.deepEqual(t.action.text.screenPosition(at(0, 1), { pane }), cell(t, pane, 4, 0), "after the tab");
  assert.deepEqual(t.action.text.screenPosition(at(0, 2), { pane }), cell(t, pane, 6, 0), "after あ");
  assert.deepEqual(t.action.text.screenPosition(at(0, 3), { pane }), cell(t, pane, 8, 0));
});

test("inline virtual text and concealed text", (assert, t) => {
  const pane = t.openText("abc\nhello world\n");
  t.action.text.addDecoration({ start: at(0, 1), end: at(0, 1) }, { virtText: [">> "], virtTextPos: "inline", pane });
  assert.deepEqual(t.action.text.screenPosition(at(0, 1), { pane }), cell(t, pane, 4, 0), "after the inline text");
  assert.deepEqual(t.action.text.screenPosition(at(0, 2), { pane }), cell(t, pane, 5, 0));
  // Concealed off the cursor's line: "hello " shows as "*".
  t.action.text.addDecoration({ start: at(1, 0), end: at(1, 6) }, { conceal: { text: "*" }, pane });
  assert.deepEqual(t.action.text.screenPosition(at(1, 6), { pane }), cell(t, pane, 1, 1));
  assert.deepEqual(t.action.text.screenPosition(at(1, 3), { pane }), cell(t, pane, 0, 1), "inside: where it shows");
});

test("a pane not on screen, and the tab line above", (assert, t) => {
  const pane = t.openText("one\ntwo\n");
  const before = t.action.screenSize();
  const buffer = t.action.text.info({ pane }).buffer;
  const other = t.action.newTab({ buffer });
  if (other === undefined) throw new Error("no pane");
  t.after(() => t.action.closeTab());
  assert.equal(t.action.text.screenPosition(at(1, 0), { pane }), undefined, "on another tab page");
  // The tab line shows: the room below it is a row smaller, and counts
  // from below it.
  const size = t.action.screenSize();
  assert.deepEqual(size, { width: before.width, height: before.height - 1 });
  const r = rect(t, other);
  const top = r.row - 1;
  assert.deepEqual(t.action.text.screenPosition(at(1, 2), { pane: other }), { col: r.col + 2, row: top + 1 });
});
