// <F5> shows the focused text pane's file in the Markdown viewer: for the
// viewer's scenario (markdown.actions).
import { cmd, keymap } from "temzdf";

keymap.add("vim/normal", "<F5>", () => {
  const path = cmd.text.info().path;
  if (path !== null) cmd.markdown.open(path, { pane: cmd.focusedPane() ?? undefined });
});
