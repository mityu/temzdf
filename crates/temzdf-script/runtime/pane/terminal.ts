// Terminal panes' keys (namespace builtin-pane-terminal): the prefix,
// pasting, the visual mode (<C-b>[ or v in the normal mode; its keys, / and
// ? to search), and status lines off for terminal panes. Defined when
// enabled: `import terminal from "temzdf/pane/terminal"; terminal.enable();`.
import { action, cmd, schedule } from "temzdf";
import { keyset } from "../lib/keyset";
import { prefix } from "../lib/prefix";
import { openSearch } from "../lib/search";

export default keyset("builtin-pane-terminal", (d) => {
  // From a pane to the normal mode.
  d.map("terminal", [prefix], action.switchMode("normal"));
  // GUI clients paste the clipboard with these; a terminal client's terminal
  // pastes with its own keys before they reach temzdf.
  d.map("terminal", ["<C-S-v>", "<D-v>"], action.paste());

  // Status lines: terminal panes go without (PDF panes show their file, page
  // and zoom there).
  // (Hooks only read: what they change waits for the safe point, schedule.)
  d.hook("PaneOpened", (id: number, kind: string) => {
    if (kind === "terminal") schedule(() => cmd.statusLine(false, id));
  });

  // The visual mode (a terminal's output, selected and copied by keys): <C-b>[
  // (tmux's copy-mode key) or v in the normal mode enter it, at the program's
  // cursor with nothing selected; v, V, <C-v> select characters, rows or a
  // block, y copies and leaves, <Esc> drops the selection, then leaves. The
  // moves are Vim's (counts too); <C-b> stays the prefix (<PageUp> for a
  // screen up).
  d.map("normal", ["[", "v"], [action.exitMode(), action.switchMode("visual")]);
  d.map("visual", ["h", "<Left>"], action.visual.left());
  d.map("visual", ["l", "<Right>"], action.visual.right());
  d.map("visual", ["k", "<Up>"], action.visual.up());
  d.map("visual", ["j", "<Down>"], action.visual.down());
  d.map("visual", ["0", "<Home>"], action.visual.lineStart());
  d.map("visual", ["^"], action.visual.firstNonBlank());
  d.map("visual", ["$", "<End>"], action.visual.lineEnd());
  d.map("visual", ["w"], action.visual.wordForward());
  d.map("visual", ["b"], action.visual.wordBackward());
  d.map("visual", ["e"], action.visual.wordEnd());
  d.map("visual", ["W"], action.visual.bigWordForward());
  d.map("visual", ["B"], action.visual.bigWordBackward());
  d.map("visual", ["E"], action.visual.bigWordEnd());
  d.map("visual", ["gg"], action.visual.top());
  d.map("visual", ["G"], action.visual.bottom());
  d.map("visual", ["H"], action.visual.screenTop());
  d.map("visual", ["M"], action.visual.screenMiddle());
  d.map("visual", ["L"], action.visual.screenBottom());
  d.map("visual", ["<C-u>"], action.visual.halfPageUp());
  d.map("visual", ["<C-d>"], action.visual.halfPageDown());
  d.map("visual", ["<PageUp>"], action.visual.pageUp());
  d.map("visual", ["<C-f>", "<PageDown>"], action.visual.pageDown());
  d.map("visual", ["<C-y>"], action.visual.scrollUp());
  d.map("visual", ["<C-e>"], action.visual.scrollDown());
  d.map("visual", ["[["], action.visual.prevPrompt());
  d.map("visual", ["]]"], action.visual.nextPrompt());
  d.map("visual", ["v"], action.visual.charwise());
  d.map("visual", ["V"], action.visual.linewise());
  d.map("visual", ["<C-v>"], action.visual.blockwise());
  d.map("visual", ["o"], action.visual.swapEnds());
  d.map("visual", ["y"], action.visual.yank());
  d.map("visual", ["Y"], action.visual.yankLines());
  d.map("visual", ["<Esc>"], action.visual.escape());
  d.map("visual", ["q"], action.exitMode());
  d.map("visual", ["/"], () => openSearch());
  d.map("visual", ["?"], () => openSearch(true));
  d.map("visual", ["n"], action.visual.nextMatch());
  d.map("visual", ["N"], action.visual.prevMatch());
  d.map("visual", [prefix], action.switchMode("normal"));
});
