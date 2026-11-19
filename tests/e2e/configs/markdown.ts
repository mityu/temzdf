// <F5> shows the focused text pane's file in the Markdown viewer: for the
// viewer's scenario (markdown.actions).
import { action, keymap } from "temzdf";

keymap.add("vim/normal", "<F5>", () => {
  const path = action.text.info().path;
  if (path !== null) action.markdown.open(path, { pane: action.focusedPane() ?? undefined });
});
