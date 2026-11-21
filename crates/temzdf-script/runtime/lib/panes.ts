// The focused pane, for src/prelude.ts too: it takes the actions to ask
// (the prelude builds `temzdf` itself, so this imports only its types).
import type { action } from "temzdf";

type Actions = typeof action;
export type PaneInfo = ReturnType<Actions["panes"]>[number];

/** The focused pane's entry of `panes()`; `undefined` when none is. */
export function focusedPaneInfo(actions: Pick<Actions, "panes">): PaneInfo | undefined {
  return actions.panes().find((p) => p.focused);
}
