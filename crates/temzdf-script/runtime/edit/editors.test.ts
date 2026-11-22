// The editors of text panes (temzdf/edit/vim, temzdf/edit/notepad),
// enabled and disabled while the session runs (`temzdf test`,
// doc/dev/testing.md).
import { keymap } from "temzdf";
import notepad from "temzdf/edit/notepad";
import vim from "temzdf/edit/vim";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

/** How many mappings a namespace has, in every mode. */
const mappings = (t: TestContext, namespace: string) => t.run(() => keymap.get({ namespace }).length);

test("editors are enabled and disabled while running", async (assert, t) => {
  const namespace = "editors.test";
  const pane = t.openText("ab\n");
  // <F2> swaps Vim and Notepad editing in the focused pane, at once.
  t.run(() => {
    keymap.add(
      ["vim/normal", "edit"],
      "<F2>",
      () => {
        if (vim.enabled) {
          vim.disable();
          notepad.enable();
        } else {
          notepad.disable();
          vim.enable();
        }
      },
      { namespace },
    );
  });
  t.after(() =>
    t.run(() => {
      keymap.clear({ namespace });
      notepad.disable();
      vim.enable();
    }),
  );
  assert.equal(t.action.currentMode(), "vim/normal");
  const vimMaps = mappings(t, "builtin-edit-vim");
  assert.ok(vimMaps > 100, `${vimMaps} Vim mappings`);
  assert.ok(t.action.test.hooks("ModeChanged") > 0, "Vim hooks ModeChanged");

  // Notepad: the edit mode at once, typed text goes in, Vim's keys gone.
  assert.equal((await t.keys("<F2>")).mode, "edit");
  assert.equal(mappings(t, "builtin-edit-vim"), 0);
  assert.equal(t.action.test.hooks("ModeChanged"), 0, "its hook went");
  await t.keys("x<Right>y");
  assert.equal(t.action.text.getText(undefined, { pane }), "xayb\n");

  // Vim again: once only, however often enabled.
  assert.equal((await t.keys("<F2>")).mode, "vim/normal");
  assert.equal(mappings(t, "builtin-edit-vim"), vimMaps);
  assert.equal(mappings(t, "builtin-edit-notepad"), 0);
});
