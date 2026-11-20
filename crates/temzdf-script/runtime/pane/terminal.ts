// Terminal panes' keys (namespace builtin-pane-terminal): the prefix,
// pasting, the visual mode (<C-b>[ or v in the normal mode; its keys, / and
// ? to search), and status lines off for terminal panes. Defined when
// enabled: `import terminal from "temzdf/pane/terminal"; terminal.enable();`.
import { action, schedule } from "temzdf";
import { keyset } from "temzdf/keyset";
import { prefix } from "../lib/prefix";
import { openSearch } from "../lib/search";

/** Terminal panes' keys and their visual mode (namespace builtin-pane-terminal). */
export default keyset("builtin-pane-terminal", (d) => {
  // From a pane to the normal mode.
  d.map("terminal", [prefix], () => action.switchMode("normal"));
  // GUI clients paste the clipboard with these; a terminal client's terminal
  // pastes with its own keys before they reach temzdf.
  d.map("terminal", ["<C-S-v>", "<D-v>"], () => action.paste());

  // Status lines: terminal panes go without (PDF panes show their file, page
  // and zoom there).
  // (Hooks only read: what they change waits for the safe point, schedule.)
  d.hook("PaneOpened", (id: number, kind: string) => {
    if (kind === "terminal") schedule(() => action.statusLine(false, id));
  });

  // The visual mode (a terminal's output, selected and copied by keys): <C-b>[
  // (tmux's copy-mode key) or v in the normal mode enter it, at the program's
  // cursor with nothing selected; v, V, <C-v> select characters, rows or a
  // block, y copies and leaves, <Esc> drops the selection, then leaves. The
  // moves are Vim's (counts too); <C-b> stays the prefix (<PageUp> for a
  // screen up).
  d.map("normal", ["[", "v"], () => {
    action.exitMode();
    action.switchMode("visual");
  });
  d.map("visual", ["h", "<Left>"], (c) => action.visual.left(c.count));
  d.map("visual", ["l", "<Right>"], (c) => action.visual.right(c.count));
  d.map("visual", ["k", "<Up>"], (c) => action.visual.up(c.count));
  d.map("visual", ["j", "<Down>"], (c) => action.visual.down(c.count));
  d.map("visual", ["0", "<Home>"], (c) => action.visual.lineStart(c.count));
  d.map("visual", ["^"], (c) => action.visual.firstNonBlank(c.count));
  d.map("visual", ["$", "<End>"], (c) => action.visual.lineEnd(c.count));
  d.map("visual", ["w"], (c) => action.visual.wordForward(c.count));
  d.map("visual", ["b"], (c) => action.visual.wordBackward(c.count));
  d.map("visual", ["e"], (c) => action.visual.wordEnd(c.count));
  d.map("visual", ["W"], (c) => action.visual.bigWordForward(c.count));
  d.map("visual", ["B"], (c) => action.visual.bigWordBackward(c.count));
  d.map("visual", ["E"], (c) => action.visual.bigWordEnd(c.count));
  d.map("visual", ["gg"], (c) => action.visual.top(c.count));
  d.map("visual", ["G"], (c) => action.visual.bottom(c.count));
  d.map("visual", ["H"], (c) => action.visual.screenTop(c.count));
  d.map("visual", ["M"], () => action.visual.screenMiddle());
  d.map("visual", ["L"], (c) => action.visual.screenBottom(c.count));
  d.map("visual", ["<C-u>"], (c) => action.visual.halfPageUp(c.count));
  d.map("visual", ["<C-d>"], (c) => action.visual.halfPageDown(c.count));
  d.map("visual", ["<PageUp>"], (c) => action.visual.pageUp(c.count));
  d.map("visual", ["<C-f>", "<PageDown>"], (c) => action.visual.pageDown(c.count));
  d.map("visual", ["<C-y>"], (c) => action.visual.scrollUp(c.count));
  d.map("visual", ["<C-e>"], (c) => action.visual.scrollDown(c.count));
  d.map("visual", ["[["], (c) => action.visual.prevPrompt(c.count));
  d.map("visual", ["]]"], (c) => action.visual.nextPrompt(c.count));
  d.map("visual", ["v"], () => action.visual.charwise());
  d.map("visual", ["V"], () => action.visual.linewise());
  d.map("visual", ["<C-v>"], () => action.visual.blockwise());
  d.map("visual", ["o"], () => action.visual.swapEnds());
  d.map("visual", ["y"], () => action.visual.yank());
  d.map("visual", ["Y"], (c) => action.visual.yankLines(c.count));
  d.map("visual", ["<Esc>"], () => action.visual.escape());
  d.map("visual", ["q"], () => action.exitMode());
  d.map("visual", ["/"], () => openSearch());
  d.map("visual", ["?"], () => openSearch(true));
  d.map("visual", ["n"], (c) => action.visual.nextMatch(c.count));
  d.map("visual", ["N"], (c) => action.visual.prevMatch(c.count));
  d.map("visual", [prefix], () => action.switchMode("normal"));
});
