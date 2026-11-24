// :preview FILE (temzdf/preview), typed on the Vim mode's command line
// (`temzdf test`, doc/dev/testing.md). PDFs: host_tests.rs (the fixture
// needs TeX).
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";
import { previewKind } from "temzdf/preview";

/** Run an Ex command line; the errors it showed. */
async function ex(t: TestContext, line: string): Promise<string[]> {
  return (await t.keys(`:${line}<CR>`, { allowErrors: true })).errors;
}

/** The focused pane, and the panes there are, put back after the test. */
function start(t: TestContext): { first: number; count: number } {
  const a = t.action;
  const first = a.focusedPane()!;
  const before = new Set(a.panes().map((p) => p.id));
  t.after(() =>
    t.run(() => {
      for (const p of a.panes()) if (!before.has(p.id)) a.closePane(p.id);
      a.focusPane(first);
    }),
  );
  return { first, count: before.size };
}

test("the viewer by the extension", (assert) => {
  assert.equal(previewKind("a.pdf"), "pdf");
  assert.equal(previewKind("dir/A.PDF"), "pdf");
  assert.equal(previewKind("notes.md"), "markdown");
  assert.equal(previewKind("notes.Markdown"), "markdown");
  assert.equal(previewKind("notes.txt"), undefined);
  assert.equal(previewKind("md"), undefined);
  assert.equal(previewKind("dir.md/file"), undefined);
});

test(":preview opens a Markdown file in the viewer, split off to the right", async (assert, t) => {
  const a = t.action;
  const { first, count } = start(t);
  assert.deepEqual(await ex(t, "preview tests/fixtures/sample.md"), []);
  const panes = a.panes();
  assert.equal(panes.length, count + 1);
  const viewer = panes.find((p) => p.focused)!;
  assert.notEqual(viewer.id, first, "a new pane");
  assert.equal(viewer.kind, "markdown");
  const left = panes.find((p) => p.id === first)!;
  assert.true(viewer.rect.col > left.rect.col, "to the right (o.splitright)");
  assert.equal(viewer.rect.row, left.rect.row, "side by side");
});

test(":preview refuses other files, a missing one and none, splitting nothing", async (assert, t) => {
  const a = t.action;
  const { first, count } = start(t);
  const errs = await ex(t, "preview notes.txt");
  assert.true(
    errs.some((e) => e.includes("neither a PDF nor Markdown")),
    errs.join("\n"),
  );
  assert.equal(a.panes().length, count);
  assert.true((await ex(t, "preview tests/fixtures/no-such-file.md")).length > 0, "an error");
  assert.equal(a.panes().length, count, "the pane split off is closed again");
  assert.equal(a.focusedPane(), first);
  assert.true(
    (await ex(t, "preview")).some((e) => e.includes("E32")),
    "no file name",
  );
  assert.equal(a.panes().length, count);
});
