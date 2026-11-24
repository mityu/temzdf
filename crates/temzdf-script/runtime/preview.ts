// :preview FILE (namespace builtin-preview): a PDF or a Markdown file in
// its viewer, in a pane split off the focused one as :vsplit does (to the
// side o.splitright says). A Markdown file opens in the viewer
// (markdown.open, in o.markdownmode), not in the text editor as :vsplit
// FILE would. Defined when enabled: `import preview from
// "temzdf/preview"; preview.enable();` (defaults.ts does).
import { action } from "temzdf";
import { keyset } from "temzdf/keyset";
import { focusedPaneInfo } from "./lib/panes";
import { isNullish } from "./lib/validate";

/** The viewer FILE opens in, by its extension; undefined for neither. */
export function previewKind(file: string): "pdf" | "markdown" | undefined {
  const ext = /\.([^./\\]+)$/.exec(file)?.[1]?.toLowerCase();
  if (ext === "pdf") return "pdf";
  if (ext === "md" || ext === "markdown") return "markdown";
  return undefined;
}

/** FILE in its viewer, in a new pane beside the focused one. */
function preview(file: string): void {
  if (file === "") throw new Error("E32: No file name");
  const kind = previewKind(file);
  if (kind === undefined) throw new Error(`preview: ${file} is neither a PDF nor Markdown`);
  const from = focusedPaneInfo(action);
  if (from === undefined) throw new Error("preview: no pane");
  // The focused buffer, as :vsplit: no new shell split off a terminal.
  const pane = action.split({ buffer: from.buffer });
  if (isNullish(pane)) throw new Error("preview: the pane could not be split");
  try {
    if (kind === "markdown") {
      action.markdown.open(file, { pane });
    } else {
      // The split took the focus; edit replaces what the focused pane shows.
      action.focusPane(pane);
      action.edit(file);
    }
  } catch (e) {
    action.closePane(pane);
    throw e;
  }
}

/** `:preview FILE`: a PDF or Markdown file in its viewer, split off beside as :vsplit (namespace builtin-preview). */
export default keyset("builtin-preview", (d) => {
  d.command("preview", (c) => preview(c.args.trim()), { desc: "open a PDF or Markdown file in its viewer beside" });
});
