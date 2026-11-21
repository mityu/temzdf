// Markdown viewers' keys (namespace builtin-pane-markdown,
// doc/design/markdown-viewer.md). The graphics mode (the mode `markdown`):
// scrolling, zoom, / to search, links back, <Tab> to the text mode, o
// for the outline. The text mode is a text pane (Vim's keys): there
// <CR> follows the link at the cursor, <BS> goes back, <Tab> to the
// graphics mode and gO opens the outline. Defined when enabled: `import
// markdown from "temzdf/pane/markdown"; markdown.enable();` (defaults.ts
// does).
import { action, defineMode, feedkeys } from "temzdf";
import type { MapContext } from "temzdf";
import { keyset } from "temzdf/keyset";
import { focusedPaneInfo } from "../lib/panes";
import { prefix } from "../lib/prefix";
import { openSearch } from "../lib/search";
import { isNullish } from "../lib/validate";

const M = "markdown";
const OUTLINE = "markdown/outline";

/** Open outline pickers: their buffer, the viewer and its headings' anchors. */
const outlines = new Map<number, { viewer: number; anchors: string[]; float: number }>();

/** The headings of the focused viewer in a floating list: <CR> goes there. */
function openOutline(): void {
  const viewer = action.focusedPane();
  if (isNullish(viewer)) return;
  const heads = action.markdown.outline(viewer);
  if (heads.length === 0) {
    action.message("No headings");
    return;
  }
  const lines = heads.map((h) => "  ".repeat(h.level - 1) + h.text);
  const buf = action.newBuffer({
    text: lines.join("\n"),
    basemode: OUTLINE,
    attrs: ["nofile", "unlisted", "nomodifiable"],
  });
  // Wide enough for its title too.
  const width = Math.min(Math.max(24, ...lines.map((l) => l.length + 2)), 72);
  const height = Math.min(heads.length, 20);
  const float = action.openFloat({ buffer: buf }, { width, height, title: "Outline", focus: true });
  if (isNullish(float)) return;
  outlines.set(buf, { viewer, anchors: heads.map((h) => h.anchor), float });
}

function closeOutline(go: boolean): void {
  const buf = focusedPaneInfo(action)?.buffer;
  const o = buf === undefined ? undefined : outlines.get(buf);
  if (buf === undefined || o === undefined) return;
  const line = action.text.cursor().line;
  outlines.delete(buf);
  action.closePane(o.float);
  action.deleteBuffer(buf);
  const anchor = o.anchors[line];
  if (go && anchor !== undefined) action.markdown.goto(anchor, { pane: o.viewer });
}

const inText = () => action.markdown.mode() === "text";

/** Markdown viewers' keys (namespace builtin-pane-markdown): the graphics mode (the mode `markdown`), links and the outline in the text mode. */
export default keyset("builtin-pane-markdown", (d) => {
  // The graphics mode.
  d.map(M, ["q"], () => action.quit());
  d.map(M, ["j", "<Down>", "<C-e>"], (c: MapContext) => action.markdown.scroll(c.count ?? 1));
  d.map(M, ["k", "<Up>", "<C-y>"], (c: MapContext) => action.markdown.scroll(-(c.count ?? 1)));
  d.map(M, ["<Space>", "<PageDown>", "<C-f>", "f"], (c: MapContext) => action.markdown.page(c.count ?? 1));
  d.map(M, ["b", "<PageUp>"], (c: MapContext) => action.markdown.page(-(c.count ?? 1)));
  d.map(M, ["d", "<C-d>"], (c: MapContext) => action.markdown.page(0.5 * (c.count ?? 1)));
  d.map(M, ["u", "<C-u>"], (c: MapContext) => action.markdown.page(-0.5 * (c.count ?? 1)));
  d.map(M, ["g", "<Home>"], () => action.markdown.top());
  d.map(M, ["G", "<End>"], () => action.markdown.bottom());
  d.map(M, ["+", "="], () => action.markdown.zoom("in"));
  d.map(M, ["-"], () => action.markdown.zoom("out"));
  d.map(M, ["0"], () => action.markdown.zoom("reset"));
  d.map(M, ["/"], () => openSearch());
  d.map(M, ["n"], () => action.markdown.searchNext(1));
  d.map(M, ["N"], () => action.markdown.searchNext(-1));
  // Text selected with the mouse (a drag copies it when released).
  d.map(M, ["y"], () => action.markdown.copy());
  d.map(M, ["<Esc>"], () => action.markdown.clearSelection());
  // Backspace too: multiplexers (zellij) take Ctrl-o and Alt+arrows.
  d.map(M, ["<C-o>", "<BS>", "<M-Left>"], () => action.markdown.back());
  d.map(M, ["<Tab>"], () => action.markdown.setMode("text"));
  d.map(M, ["o", "gO"], () => openOutline());
  d.map(M, [prefix], () => action.switchMode("normal"));
  // The text mode: Vim's keys, and these on a viewer only.
  d.map(
    "vim/normal",
    ["<CR>"],
    (c: MapContext) => {
      if (!action.markdown.follow()) feedkeys(`${c.count ?? ""}+`, { remap: true });
    },
    { when: inText },
  );
  d.map("vim/normal", ["<BS>"], () => action.markdown.back(), { when: inText });
  d.map("vim/normal", ["<Tab>"], () => action.markdown.setMode("graphics"), { when: inText });
  d.map("vim/normal", ["gO"], () => openOutline(), { when: inText });
  // The outline's list.
  defineMode(OUTLINE, { fallback: "drop", count: true });
  d.map(OUTLINE, ["j", "<Down>", "<C-n>"], (c: MapContext) => action.text.move("line", c.count ?? 1));
  d.map(OUTLINE, ["k", "<Up>", "<C-p>"], (c: MapContext) => action.text.move("line", -(c.count ?? 1)));
  d.map(OUTLINE, ["<CR>"], () => closeOutline(true));
  d.map(OUTLINE, ["q", "<Esc>"], () => closeOutline(false));
});
