// PDF panes' keys (namespace builtin-pane-pdf): the page view, / to
// search (a prompt), the prefix. Defined when enabled: `import pdf from
// "temzdf/pane/pdf"; pdf.enable();` (defaults.ts does).
import { action } from "temzdf";
import { keyset } from "../lib/keyset";
import { prefix } from "../lib/prefix";
import { openSearch } from "../lib/search";

export default keyset("builtin-pane-pdf", (d) => {
  d.map("pdf", ["q"], action.quit());
  d.map("pdf", ["<Esc>"], action.cancel());
  d.map("pdf", ["j", "<Down>"], action.scrollDown());
  d.map("pdf", ["k", "<Up>"], action.scrollUp());
  d.map("pdf", ["h", "<Left>"], action.scrollLeft());
  d.map("pdf", ["l", "<Right>"], action.scrollRight());
  d.map("pdf", ["<Space>", "<PageDown>", "<C-f>"], action.pageDown());
  d.map("pdf", ["b", "<PageUp>"], action.pageUp());
  d.map("pdf", ["d"], action.halfPageDown());
  d.map("pdf", ["u"], action.halfPageUp());
  d.map("pdf", ["g", "<Home>"], action.firstPage());
  d.map("pdf", ["G", "<End>"], action.lastPage());
  // Backspace too: multiplexers (zellij) take Ctrl-o and Alt+arrows.
  d.map("pdf", ["<C-o>", "<BS>", "<M-Left>"], action.back());
  d.map("pdf", ["<M-Right>"], action.forward());
  d.map("pdf", ["+", "="], action.zoomIn());
  d.map("pdf", ["-"], action.zoomOut());
  d.map("pdf", ["0", "w"], action.fitWidth());
  d.map("pdf", ["/"], () => openSearch());
  d.map("pdf", ["n"], action.nextMatch());
  d.map("pdf", ["N"], action.prevMatch());
  d.map("pdf", ["y"], action.copy());
  d.map("pdf", ["<Tab>"], action.toggleSidebar());
  d.map("pdf", [prefix], action.switchMode("normal"));
});
