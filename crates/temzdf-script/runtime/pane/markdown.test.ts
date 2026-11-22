// The Markdown viewer's keys in the graphics mode (`temzdf test`, doc/dev/testing.md).
import { test } from "temzdf/test";

test("q closes a Markdown viewer in the graphics mode", async (assert, t) => {
  t.openText("beside\n");
  const viewer = t.action.markdown.open("tests/fixtures/sample.md");
  t.action.markdown.setMode("graphics", { pane: viewer });
  assert.equal(t.action.markdown.mode(viewer), "graphics");
  t.action.focusPane(viewer);
  const r = await t.keys("q");
  assert.deepEqual(r.errors, []);
  assert.false(
    t.action.panes().some((p) => p.id === viewer),
    "the viewer is gone",
  );
});
