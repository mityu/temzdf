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
each("normal", ["o"], action.focusNextPane());
each("normal", ["x"], action.closePane());
each("normal", ["d"], [action.detach(), action.exitMode()]);
// The prefix twice sends it to the pane.
each("normal", [prefix], [action.exitMode(), action.feedkeys(prefix, false)]);
each("normal", ["<Esc>"], action.exitMode());
