// Markdown viewers' keys (namespace builtin-pane-markdown,
// doc/design/markdown-viewer.md). The graphics mode (the mode `markdown`):
// scrolling, zoom, / to search, links back, <Tab> to the text mode, o
// for the outline. The text mode is a text pane (Vim's keys): there
// <CR> follows the link at the cursor, <BS> goes back, <Tab> to the
// graphics mode and gO opens the outline. Defined when enabled: `import
// markdown from "temzdf/pane/markdown"; markdown.enable();` (defaults.ts
// does).
import { action, cmd, defineMode, feedkeys } from "temzdf";
import type { MapContext } from "temzdf";
import { keyset } from "../lib/keyset";
import { prefix } from "../lib/prefix";
import { openSearch } from "../lib/search";

const M = "markdown";
const OUTLINE = "markdown/outline";

/** Open outline pickers: their buffer, the viewer and its headings' anchors. */
const outlines = new Map<number, { viewer: number; anchors: string[]; float: number }>();

function focusedBuffer(): number | null {
  return cmd.panes().find((p) => p.focused)?.buffer ?? null;
}

/** The headings of the focused viewer in a floating list: <CR> goes there. */
function openOutline(): void {
  const viewer = cmd.focusedPane();
  if (viewer === null) return;
  const heads = cmd.markdown.outline(viewer);
  if (heads.length === 0) {
    cmd.message("No headings");
    return;
  }
  const lines = heads.map((h) => "  ".repeat(h.level - 1) + h.text);
  const buf = cmd.newBuffer({ text: lines.join("\n"), basemode: OUTLINE, attrs: ["nofile", "unlisted", "nomodifiable"] });
  // Wide enough for its title too.
  const width = Math.min(Math.max(24, ...lines.map((l) => l.length + 2)), 72);
  const height = Math.min(heads.length, 20);
  const float = cmd.openFloat({ buffer: buf }, { width, height, title: "Outline", focus: true });
  if (float === null) return;
  outlines.set(buf, { viewer, anchors: heads.map((h) => h.anchor), float });
}

function closeOutline(go: boolean): void {
  const buf = focusedBuffer();
  const o = buf === null ? undefined : outlines.get(buf);
  if (buf === null || o === undefined) return;
  const line = cmd.text.cursor().line;
  outlines.delete(buf);
  cmd.closePane(o.float);
  cmd.deleteBuffer(buf);
  const anchor = o.anchors[line];
  if (go && anchor !== undefined) cmd.markdown.goto(anchor, { pane: o.viewer });
}

const inText = () => cmd.markdown.mode() === "text";

export default keyset("builtin-pane-markdown", (d) => {
  // The graphics mode.
  d.map(M, ["q"], action.quit());
  d.map(M, ["j", "<Down>", "<C-e>"], (c: MapContext) => cmd.markdown.scroll(c.count ?? 1));
  d.map(M, ["k", "<Up>", "<C-y>"], (c: MapContext) => cmd.markdown.scroll(-(c.count ?? 1)));
  d.map(M, ["<Space>", "<PageDown>", "<C-f>", "f"], (c: MapContext) => cmd.markdown.page(c.count ?? 1));
  d.map(M, ["b", "<PageUp>"], (c: MapContext) => cmd.markdown.page(-(c.count ?? 1)));
  d.map(M, ["d", "<C-d>"], (c: MapContext) => cmd.markdown.page(0.5 * (c.count ?? 1)));
  d.map(M, ["u", "<C-u>"], (c: MapContext) => cmd.markdown.page(-0.5 * (c.count ?? 1)));
  d.map(M, ["g", "<Home>"], () => cmd.markdown.top());
  d.map(M, ["G", "<End>"], () => cmd.markdown.bottom());
  d.map(M, ["+", "="], () => cmd.markdown.zoom("in"));
  d.map(M, ["-"], () => cmd.markdown.zoom("out"));
  d.map(M, ["0"], () => cmd.markdown.zoom("reset"));
  d.map(M, ["/"], () => openSearch());
  d.map(M, ["n"], () => cmd.markdown.searchNext(1));
  d.map(M, ["N"], () => cmd.markdown.searchNext(-1));
  // Backspace too: multiplexers (zellij) take Ctrl-o and Alt+arrows.
  d.map(M, ["<C-o>", "<BS>", "<M-Left>"], () => cmd.markdown.back());
  d.map(M, ["<Tab>"], () => cmd.markdown.setMode("text"));
  d.map(M, ["o", "gO"], () => openOutline());
  d.map(M, [prefix], action.switchMode("normal"));
  // The text mode: Vim's keys, and these on a viewer only.
  d.map(
    "vim/normal",
    ["<CR>"],
    (c: MapContext) => {
      if (!cmd.markdown.follow()) feedkeys(`${c.count ?? ""}+`, { remap: true });
    },
    { when: inText },
  );
  d.map("vim/normal", ["<BS>"], () => cmd.markdown.back(), { when: inText });
  d.map("vim/normal", ["<Tab>"], () => cmd.markdown.setMode("graphics"), { when: inText });
  d.map("vim/normal", ["gO"], () => openOutline(), { when: inText });
  // The outline's list.
  defineMode(OUTLINE, { fallback: "drop", count: true });
  d.map(OUTLINE, ["j", "<Down>", "<C-n>"], (c: MapContext) => cmd.text.move("line", c.count ?? 1));
  d.map(OUTLINE, ["k", "<Up>", "<C-p>"], (c: MapContext) => cmd.text.move("line", -(c.count ?? 1)));
  d.map(OUTLINE, ["<CR>"], () => closeOutline(true));
  d.map(OUTLINE, ["q", "<Esc>"], () => closeOutline(false));
});
