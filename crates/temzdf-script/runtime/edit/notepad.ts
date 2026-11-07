// Editing text panes without modes, as Notepad and most editors do
// (namespace builtin-edit-notepad, in the built-in mode `edit`): typed text
// goes in, arrows move, with Shift they select; <C-s> writes, <C-z>/<C-y>
// undo and redo. temzdf's default is the Vim mode; `import notepad from
// "temzdf/edit/notepad"; vim.disable(); notepad.enable();` edits this way.
import { action, o } from "temzdf";
import { keyset } from "../lib/keyset";
import { prefix } from "../lib/prefix";

/** Text panes in the mode `edit` (not another editor's). */
function useEditMode(): void {
  const b = { ...o.basemode };
  if ("edit" in b) {
    delete b.edit;
    o.basemode = b;
  }
}

export default keyset("builtin-edit-notepad", (d) => {
  useEditMode();
  const sel = { select: true };
  for (const [keys, unit, n] of [
    [["<Left>"], "grapheme", -1],
    [["<Right>"], "grapheme", 1],
    [["<Up>"], "displayLine", -1],
    [["<Down>"], "displayLine", 1],
    [["<Home>"], "lineStart", 1],
    [["<End>"], "lineEnd", 1],
    [["<C-Left>"], "wordStart", -1],
    [["<C-Right>"], "wordEnd", 1],
    [["<C-Home>"], "documentStart", 1],
    [["<C-End>"], "documentEnd", 1],
    [["<PageUp>"], "page", -1],
    [["<PageDown>"], "page", 1],
  ] as const) {
    d.map("edit", [...keys], action.text.move(unit, n));
    d.map("edit", keys.map((k) => k.replace("<", "<S-").replace("<S-C-", "<C-S-")), action.text.move(unit, n, sel));
  }
  d.map("edit", ["<CR>"], action.text.type("\n"));
  d.map("edit", ["<Tab>"], action.text.type("\t"));
  d.map("edit", ["<BS>"], action.text.erase("grapheme", -1));
  d.map("edit", ["<Del>"], action.text.erase("grapheme", 1));
  d.map("edit", ["<C-BS>"], action.text.erase("wordStart", -1));
  d.map("edit", ["<C-Del>"], action.text.erase("wordEnd", 1));
  d.map("edit", ["<Esc>"], action.text.clearSelection());
  d.map("edit", ["<C-s>"], action.write());
  d.map("edit", ["<C-z>"], action.text.undo());
  d.map("edit", ["<C-y>", "<C-S-z>"], action.text.redo());
  d.map("edit", ["<C-S-v>", "<D-v>"], action.paste());
  d.map("edit", [prefix], action.switchMode("normal"));
});
