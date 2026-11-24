// PDF panes' keys (namespace builtin-pane-pdf): the page view, / to
// search (a prompt), the prefix. Defined when enabled: `import pdf from
// "temzdf/pane/pdf"; pdf.enable();` (defaults.ts does).
import { action } from "temzdf";
import { keyset } from "temzdf/keyset";
import { prefix } from "../lib/prefix";
import { openSearch } from "../lib/search";

/** PDF panes' keys (namespace builtin-pane-pdf). */
export default keyset("builtin-pane-pdf", (d) => {
  d.map("pdf", ["q"], () => action.closePane());
  d.map("pdf", ["<Esc>"], () => action.cancel());
  d.map("pdf", ["j", "<Down>"], (c) => action.scrollDown(c.count));
  d.map("pdf", ["k", "<Up>"], (c) => action.scrollUp(c.count));
  d.map("pdf", ["h", "<Left>"], (c) => action.scrollLeft(c.count));
  d.map("pdf", ["l", "<Right>"], (c) => action.scrollRight(c.count));
  d.map("pdf", ["<Space>", "<PageDown>", "<C-f>"], (c) => action.pageDown(c.count));
  d.map("pdf", ["b", "<PageUp>"], (c) => action.pageUp(c.count));
  d.map("pdf", ["d"], (c) => action.halfPageDown(c.count));
  d.map("pdf", ["u"], (c) => action.halfPageUp(c.count));
  d.map("pdf", ["g", "<Home>"], (c) => action.firstPage(c.count));
  d.map("pdf", ["G", "<End>"], (c) => action.lastPage(c.count));
  // Backspace too: multiplexers (zellij) take Ctrl-o and Alt+arrows.
  d.map("pdf", ["<C-o>", "<BS>", "<M-Left>"], (c) => action.back(c.count));
  d.map("pdf", ["<M-Right>"], (c) => action.forward(c.count));
  d.map("pdf", ["+", "="], (c) => action.zoomIn(c.count));
  d.map("pdf", ["-"], (c) => action.zoomOut(c.count));
  d.map("pdf", ["0", "w"], () => action.fitWidth());
  d.map("pdf", ["/"], () => openSearch());
  d.map("pdf", ["n"], (c) => action.nextMatch(c.count));
  d.map("pdf", ["N"], (c) => action.prevMatch(c.count));
  // The copy's Promise is not waited for: the notice says when it is in.
  d.map("pdf", ["y"], () => void action.copy());
  d.map("pdf", ["<Tab>"], () => action.toggleSidebar());
  d.map("pdf", [prefix], () => action.switchMode("normal"));
});
