// The input mode of prompts (namespace builtin-mode-input; openPrompt):
// readline's keys, <CR> runs the cursor's line, <Esc> cancels, the prefix.
// Defined when enabled: `import input from "temzdf/mode/input";
// input.enable();`.
import { acceptPrompt, action, cancelPrompt, cmd } from "temzdf";
import { keyset } from "../lib/keyset";
import { prefix } from "../lib/prefix";

export default keyset("builtin-mode-input", (d) => {
  // The input mode (prompts): editing with readline's keys, <CR> runs the
  // cursor's line, <Esc> cancels, the prefix. Pasted lines go in as they are.
  d.map("input", ["<BS>", "<C-h>"], action.text.erase("grapheme", -1));
  d.map("input", ["<Del>", "<C-d>"], action.text.erase("grapheme", 1));
  // (Not readline's <C-b> and <C-f>: <C-b> is the prefix.)
  d.map("input", ["<Left>"], action.text.move("grapheme", -1));
  d.map("input", ["<Right>"], action.text.move("grapheme", 1));
  d.map("input", ["<Home>", "<C-a>"], action.text.move("lineStart"));
  d.map("input", ["<End>", "<C-e>"], action.text.move("lineEnd"));
  d.map("input", ["<M-b>", "<C-Left>"], action.text.move("wordStart", -1));
  d.map("input", ["<M-f>", "<C-Right>"], action.text.move("wordEnd", 1));
  d.map("input", ["<Up>"], action.text.move("line", -1));
  d.map("input", ["<Down>"], action.text.move("line", 1));
  d.map("input", ["<C-w>"], action.text.erase("wordStart", -1));
  d.map("input", ["<C-u>"], () => {
    const c = cmd.text.cursor();
    cmd.text.delete({ start: { line: c.line, character: 0 }, end: c });
  });
  d.map("input", ["<C-k>"], () => {
    const c = cmd.text.cursor();
    cmd.text.delete({ start: c, end: { line: c.line, character: cmd.text.line(c.line).length } });
  });
  d.map("input", ["<CR>"], () => acceptPrompt());
  d.map("input", ["<Esc>"], () => cancelPrompt());
  d.map("input", ["<C-S-v>", "<D-v>"], action.paste());
  d.map("input", [prefix], action.switchMode("normal"));
});
