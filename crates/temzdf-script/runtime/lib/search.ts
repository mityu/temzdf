import { action, openPrompt } from "temzdf";

// Searching a PDF pane (/) or a terminal in the visual mode (/ and ?): a
// prompt (openPrompt) one row high below it. As the query is typed the
// pane shows its first match (searchPreview, as Vim's incsearch); <CR>
// searches, <Esc> cancels (back where it began), and either closes the
// prompt, back to the pane (and its visual mode). Closed any other way,
// the search is cancelled.
export function openSearch(backward = false): void {
  const pane = action.focusedPane();
  if (pane === undefined) return;
  const visual = action.currentMode() === "visual";
  const back = () => {
    if (visual) action.switchMode("visual");
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
      if (visual) action.visual.searchPreview(query, pane, backward);
      else action.searchPreview(query, pane);
    },
    onLine: (query) =>
      guard(() => {
        back();
        if (visual) action.visual.searchFor(query, pane, backward);
        else action.searchFor(query, pane);
      }),
    onCancel: () =>
      guard(() => {
        back();
        if (visual) action.visual.searchCancel(pane);
        else action.searchCancel(pane);
      }),
  });
}
