// temzdf's default configuration. The script host loads it before the user's
// configuration (`--clean` loads only this); the user's can map over or
// unmap anything here. Built-in modes: `normal` (operating), `terminal`
// (pass-through, terminal panes) and `pdf` (PDF panes).
import { action, map } from "temzdf";
import type { Action } from "temzdf";

/** The prefix: enters the normal mode from a pane. */
const prefix = "<C-b>";

function each(mode: string, keys: string[], rhs: Action | Action[]): void {
  for (const k of keys) map(mode, k, rhs);
}

// Page view.
each("pdf", ["q"], action.quit());
each("pdf", ["<Esc>"], action.cancel());
each("pdf", ["j", "<Down>"], action.scrollDown());
each("pdf", ["k", "<Up>"], action.scrollUp());
each("pdf", ["h", "<Left>"], action.scrollLeft());
each("pdf", ["l", "<Right>"], action.scrollRight());
each("pdf", ["<Space>", "<PageDown>", "<C-f>"], action.pageDown());
each("pdf", ["b", "<PageUp>"], action.pageUp());
each("pdf", ["d"], action.halfPageDown());
each("pdf", ["u"], action.halfPageUp());
each("pdf", ["g", "<Home>"], action.firstPage());
each("pdf", ["G", "<End>"], action.lastPage());
// Backspace too: multiplexers (zellij) take Ctrl-o and Alt+arrows.
each("pdf", ["<C-o>", "<BS>", "<M-Left>"], action.back());
each("pdf", ["<M-Right>"], action.forward());
each("pdf", ["+", "="], action.zoomIn());
each("pdf", ["-"], action.zoomOut());
each("pdf", ["0", "w"], action.fitWidth());
each("pdf", ["/"], action.search());
each("pdf", ["n"], action.nextMatch());
each("pdf", ["N"], action.prevMatch());
each("pdf", ["y"], action.copy());
each("pdf", ["<Tab>"], action.toggleSidebar());

// From a pane to the normal mode.
each("terminal", [prefix], action.switchMode("normal"));
each("pdf", [prefix], action.switchMode("normal"));

// The normal mode: panes. Moving the focus returns to that pane's mode.
each("normal", ["%"], action.split("right"));
each("normal", ['"'], action.split("bottom"));
each("normal", ["h", "<Left>"], action.moveFocus("left"));
each("normal", ["j", "<Down>"], action.moveFocus("down"));
each("normal", ["k", "<Up>"], action.moveFocus("up"));
each("normal", ["l", "<Right>"], action.moveFocus("right"));
// Sizes as Vim's <C-w>+ - < >: the pane taller, shorter, narrower, wider;
// the normal mode stays, so the keys repeat (3>: three cells).
each("normal", ["+"], action.resizePane("taller"));
each("normal", ["-"], action.resizePane("shorter"));
each("normal", ["<lt>"], action.resizePane("narrower"));
each("normal", [">"], action.resizePane("wider"));
// Places as Vim's <C-w>H J K L r R: the pane to an edge of the screen;
// the panes of its row or column rotated.
each("normal", ["H"], action.movePane("left"));
each("normal", ["J"], action.movePane("bottom"));
each("normal", ["K"], action.movePane("top"));
each("normal", ["L"], action.movePane("right"));
each("normal", ["r"], action.rotatePanes());
each("normal", ["R"], action.rotatePanes(true));
each("normal", ["o"], action.focusNextPane());
// Floating panes (zellij's keys): show or hide them all (with none, open a
// floating shell), open a floating shell, turn the focused pane floating or
// tiled (the focus stays, so back to its mode, as zellij does).
each("normal", ["w"], action.toggleFloats());
each("normal", ["W"], action.openFloat());
each("normal", ["e"], [action.toggleFloating(), action.exitMode()]);
each("normal", ["x"], action.closePane());
// Tab pages as Vim's gt, gT ({N}gt: tab page N) and <C-w>T.
each("normal", ["gt"], action.nextTab());
each("normal", ["gT"], action.prevTab());
each("normal", ["T"], action.breakPane());
each("normal", ["d"], [action.detach(), action.exitMode()]);
each("normal", ["q"], action.killSession());
// The prefix twice sends it to the pane.
each("normal", [prefix], [action.exitMode(), action.feedkeys(prefix, false)]);
each("normal", ["<Esc>"], action.exitMode());
