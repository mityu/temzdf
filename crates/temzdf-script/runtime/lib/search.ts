import { cmd, openPrompt } from "temzdf";

// Searching a PDF pane (/) or a terminal in the visual mode (/ and ?): a
// prompt (openPrompt) one row high below it. As the query is typed the
// pane shows its first match (searchPreview, as Vim's incsearch); <CR>
// searches, <Esc> cancels (back where it began), and either closes the
// prompt, back to the pane (and its visual mode). Closed any other way,
// the search is cancelled.
export function openSearch(backward = false): void {
  const pane = cmd.focusedPane();
  if (pane === null) return;
  const visual = cmd.currentMode() === "visual";
  const back = () => {
    if (visual) cmd.switchMode("visual");
  };
  const guard = (f: () => void) => {
    try {
      f();
    } catch {
      // The pane is gone.
    }
  };
  openPrompt({
    prompt: backward ? "?" : "/",
    onChange: (query) => {
      if (visual) cmd.visual.searchPreview(query, pane, backward);
      else cmd.searchPreview(query, pane);
    },
    onLine: (query) =>
      guard(() => {
        back();
        if (visual) cmd.visual.searchFor(query, pane, backward);
        else cmd.searchFor(query, pane);
      }),
    onCancel: () =>
      guard(() => {
        back();
        if (visual) cmd.visual.searchCancel(pane);
        else cmd.searchCancel(pane);
      }),
  });
}
