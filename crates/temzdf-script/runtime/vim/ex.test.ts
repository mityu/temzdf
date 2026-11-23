// The Vim mode's Ex commands on panes, buffers and tab pages, typed on
// its command line (`temzdf test`, doc/dev/testing.md).
import { dirname, join } from "temzdf/path";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

/** The name of the buffer the focused pane shows. */
const shown = (t: TestContext) => t.action.text.info().name;
const paneCount = (t: TestContext) => t.action.panes().length;
const bufferCount = (t: TestContext) => t.action.buffers().length;

/** Run an Ex command line; the errors it showed. */
async function ex(t: TestContext, line: string): Promise<string[]> {
  return (await t.keys(`:${line}<CR>`, { allowErrors: true })).errors;
}

/** The session back to one tab page and one pane showing `buffer` (its
 * `attrs` again), the buffers made since `before` gone. */
function restore(t: TestContext, buffer: number, attrs: string[], before: Set<number>): void {
  const a = t.action;
  while (a.tabs().length > 1) a.closeTab(a.currentTab() === 1 ? 2 : 1);
  const focused = a.focusedPane();
  for (const p of a.panes()) if (p.id !== focused) a.closePane(p.id);
  a.showBuffer(buffer);
  a.setBufferAttrs(buffer, attrs as Parameters<typeof a.setBufferAttrs>[1]);
  for (const b of a.buffers({ all: true })) {
    if (before.has(b.id)) continue;
    a.setVar("buffer", "forceDelete", true, b.id);
    a.deleteBuffer(b.id);
  }
}

test("Ex commands split, switch and close panes, buffers and tab pages", async (assert, t) => {
  // One pane showing case.txt, a file beside the session's first one,
  // which is left out of the buffer list meanwhile.
  const a = t.action;
  const first = a.text.info();
  assert.ok(first.path !== undefined, "the session starts on a file");
  const dir = dirname(first.path ?? "");
  const before = new Set(a.buffers({ all: true }).map((b) => b.id));
  t.after(() => t.run(() => restore(t, first.buffer, first.attrs, before)));
  const file = t.run(() => {
    a.setBufferAttrs(first.buffer, [...first.attrs, "unlisted"]);
    const b = a.newBuffer({ path: join(dir, "case.txt"), text: "one\n" });
    a.write({ buffer: b });
    a.showBuffer(b);
    return b;
  });
  assert.equal(paneCount(t), 1);
  assert.equal(shown(t), "case.txt");
  assert.equal(bufferCount(t), 1);

  const pane = a.focusedPane();
  await ex(t, "sp");
  assert.equal(paneCount(t), 2);
  assert.notEqual(a.focusedPane(), pane, "the new pane has the focus");
  assert.equal(shown(t), "case.txt", "showing the same buffer");
  await ex(t, "vnew");
  assert.equal(paneCount(t), 3);
  assert.notEqual(shown(t), "case.txt", "a new buffer");
  assert.equal(bufferCount(t), 2);
  await ex(t, "only");
  assert.equal(paneCount(t), 1);
  assert.ok(
    (await ex(t, "close")).some((e) => e.includes("E444")),
    "the last pane stays",
  );
  await ex(t, "bn");
  assert.equal(shown(t), "case.txt", "round to the first buffer");
  await ex(t, "bp");
  assert.notEqual(shown(t), "case.txt");
  await ex(t, "enew");
  assert.equal(bufferCount(t), 3);

  // Changes keep a buffer, unless :bd!; its pane shows another then.
  await t.keys("ix<Esc>");
  assert.ok((await ex(t, "bd")).some((e) => e.includes("E89")));
  await ex(t, "bd!");
  assert.equal(bufferCount(t), 2);
  assert.equal(paneCount(t), 1, "the pane stays");
  const ls = await t.keys(":ls<CR>");
  assert.ok(
    ls.messages.some((m) => m.includes('"case.txt"')),
    `:ls lists case.txt: ${ls.messages.join(" / ")}`,
  );

  // By name; :wa writes what changed.
  await ex(t, "enew");
  await ex(t, "b case");
  assert.equal(shown(t), "case.txt");
  await t.keys("Atwo<Esc>");
  assert.deepEqual(await ex(t, "wa"), []);
  assert.false(a.buffers().find((b) => b.id === file)?.modified, "written");
  // What the file holds: read back over a change.
  await t.keys("ojunk<Esc>");
  t.run(() => a.reload(file));
  assert.equal(a.text.getText(), "onetwo\n", "the file");

  // Tab pages.
  await ex(t, "tabnew");
  assert.equal(a.tabs().length, 2);
  await ex(t, "tabnew");
  await ex(t, "tabo");
  assert.equal(a.tabs().length, 1);
  assert.ok((await ex(t, "tabc")).some((e) => e.includes("E784")));
});
