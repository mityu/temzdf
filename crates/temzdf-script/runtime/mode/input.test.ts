// The input mode of prompts (runtime/mode/input.ts): <BS> and <C-h> on an
// empty Ex command line (<C-b>:) cancel it, as the Vim mode's `:` does;
// elsewhere they delete the character before the caret (`temzdf test`,
// doc/dev/testing.md).
import { isPrompt, openPrompt } from "temzdf";
import { isCmdline } from "temzdf/command";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

/** The focused pane's cursor line. */
const line = (t: TestContext) => t.action.text.line(t.action.text.cursor().line);

async function keys(t: TestContext, k: string): Promise<void> {
  await t.keys(k);
  await t.settle();
}

for (const bs of ["<BS>", "<C-h>"]) {
  test(`${bs} deletes in the <C-b>: command line, and cancels it once it is empty`, async (assert, t) => {
    t.openText("one\n");
    await keys(t, "<C-b>:ab");
    assert.true(t.run(() => isCmdline()));
    await keys(t, bs);
    assert.equal(line(t), "a");
    await keys(t, bs);
    assert.equal(line(t), "", "the last character goes, the command line stays");
    assert.true(t.run(() => isCmdline()));
    await keys(t, bs);
    assert.false(
      t.run(() => isPrompt()),
      "on the empty line it cancels",
    );
  });
}

test("<BS> on an empty line of a two-line <C-b>: command line deletes, as the Vim mode's", async (assert, t) => {
  t.openText("one\n");
  await keys(t, "<C-b>:a");
  t.action.text.insert("\n");
  await t.settle();
  assert.equal(t.action.text.lineCount(), 2);
  assert.equal(line(t), "", "the cursor on the empty second line");
  await keys(t, "<BS>");
  assert.true(
    t.run(() => isCmdline()),
    "not cancelled: the line break goes",
  );
  assert.equal(t.action.text.lineCount(), 1);
  await keys(t, "<Esc>");
});

test("<BS> on an empty prompt that is not a command line leaves it open", async (assert, t) => {
  t.openText("one\n");
  t.run(() => openPrompt({ prompt: "> " }));
  await t.settle();
  assert.true(t.run(() => isPrompt()));
  assert.false(t.run(() => isCmdline()));
  await keys(t, "<BS>");
  assert.true(
    t.run(() => isPrompt()),
    "still open",
  );
  await keys(t, "<Esc>");
  assert.false(t.run(() => isPrompt()));
});
