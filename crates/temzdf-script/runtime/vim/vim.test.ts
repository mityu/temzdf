// The Vim mode driven by keys (`temzdf test`, doc/dev/testing.md).
import { keymap } from "temzdf";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

const text = (t: TestContext, pane: number) => t.action.text.getText(undefined, { pane });

test("dd deletes a line and u brings it back", async (assert, t) => {
  const pane = t.openText("one\ntwo\nthree\n");
  await t.keys("jdd");
  assert.equal(text(t, pane), "one\nthree\n");
  await t.keys("u");
  assert.equal(text(t, pane), "one\ntwo\nthree\n");
});

test("a macro replays the keys recorded", async (assert, t) => {
  const pane = t.openText("1\n2\n3\n4\n5\n");
  await t.keys("qaddq");
  assert.equal(t.action.register("a")?.text, "dd");
  await t.keys("2@a");
  assert.equal(text(t, pane), "4\n5\n");
});

test("a mapping that is a prefix of another runs once no key follows", async (assert, t) => {
  const ran: string[] = [];
  const namespace = "vim.test";
  t.run(() => {
    keymap.add("vim/normal", "<F3>", () => ran.push("short"), { namespace });
    keymap.add("vim/normal", "<F3><F4>", () => ran.push("long"), { namespace });
  });
  t.after(() => t.run(() => keymap.clear({ namespace })));
  const r = await t.keys("<F3>", { flush: false });
  assert.equal(r.pending, "<F3>");
  await t.settle();
  await t.keys("<F3><F4>");
  assert.deepEqual(ran, ["short", "long"]);
});
