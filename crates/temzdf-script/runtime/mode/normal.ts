// The normal mode (namespace builtin-mode-normal), reached with the prefix
// from any pane: panes, floating panes, tab pages, putting the newest copy,
// detaching, ending the session. Defined when enabled: `import normal from
// "temzdf/mode/normal"; normal.enable();`.
import { action } from "temzdf";
import { keyset } from "../lib/keyset";
import { prefix } from "../lib/prefix";

export default keyset("builtin-mode-normal", (d) => {
  // The normal mode: panes. Moving the focus returns to that pane's mode.
  d.map("normal", ["%"], action.split("right"));
  d.map("normal", ['"'], action.split("bottom"));
  d.map("normal", ["h", "<Left>"], action.moveFocus("left"));
  d.map("normal", ["j", "<Down>"], action.moveFocus("down"));
  d.map("normal", ["k", "<Up>"], action.moveFocus("up"));
  d.map("normal", ["l", "<Right>"], action.moveFocus("right"));
  // Sizes as Vim's <C-w>+ - < >: the pane taller, shorter, narrower, wider;
  // the normal mode stays, so the keys repeat (3>: three cells).
  d.map("normal", ["+"], action.resizePane("taller"));
  d.map("normal", ["-"], action.resizePane("shorter"));
  d.map("normal", ["<lt>"], action.resizePane("narrower"));
  d.map("normal", [">"], action.resizePane("wider"));
  // As Vim's <C-w>= | _: every pane the same size; the pane as wide or as
  // high as it can be (with a count: that many cells).
  d.map("normal", ["="], action.equalizePanes());
  d.map("normal", ["<Bar>"], action.maximizePane("width"));
  d.map("normal", ["_"], action.maximizePane("height"));
  // Places as Vim's <C-w>H J K L r R: the pane to an edge of the screen;
  // the panes of its row or column rotated.
  d.map("normal", ["H"], action.movePane("left"));
  d.map("normal", ["J"], action.movePane("bottom"));
  d.map("normal", ["K"], action.movePane("top"));
  d.map("normal", ["L"], action.movePane("right"));
  d.map("normal", ["r"], action.rotatePanes());
  d.map("normal", ["R"], action.rotatePanes(true));
  d.map("normal", ["o"], action.focusNextPane());
  // Floating panes (zellij's keys): show or hide them all (with none, open a
  // floating shell), open a floating shell, turn the focused pane floating or
  // tiled (the focus stays, so back to its mode, as zellij does).
  d.map("normal", ["w"], action.toggleFloats());
  d.map("normal", ["W"], action.openFloat());
  d.map("normal", ["e"], [action.toggleFloating(), action.exitMode()]);
  d.map("normal", ["x"], action.closePane());
  // Tab pages as Vim's gt, gT ({N}gt: tab page N) and <C-w>T.
  d.map("normal", ["gt"], action.nextTab());
  d.map("normal", ["gT"], action.prevTab());
  d.map("normal", ["T"], action.breakPane());
  d.map("normal", ["d"], [action.detach(), action.exitMode()]);
  d.map("normal", ["q"], action.killSession());
  // The prefix twice sends it to the pane.
  d.map("normal", [prefix], [action.exitMode(), action.feedkeys(prefix, false)]);
  d.map("normal", ["<Esc>"], action.exitMode());
  // <C-b>] (tmux's paste-buffer key) or p puts the newest copy (any pane's;
  // doc/design/registers.md) into the focused pane.
  d.map("normal", ["]", "p"], [action.exitMode(), action.put()]);
});
