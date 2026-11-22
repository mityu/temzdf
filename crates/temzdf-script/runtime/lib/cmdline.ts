import { action, openPrompt } from "temzdf";
import { CMDLINE_VAR, command } from "temzdf/command";

// An Ex command line in any pane (<C-b>: from the normal mode): a prompt
// (openPrompt) one row high below the pane, marked as one (CMDLINE_VAR);
// <CR> runs its line with command.execute once the focus is back on the
// pane, so the commands' `when` sees that pane. Errors are shown as the
// mapping's.
export function openCommandLine(): void {
  const p = openPrompt({ prompt: ":", onLine: (line) => command.execute(line) });
  if (p !== undefined) action.setVar("buffer", CMDLINE_VAR, ":", p.buffer);
}
