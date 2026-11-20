import { openPrompt } from "temzdf";
import { command } from "temzdf/command";

// An Ex command line in any pane (<C-b>: from the normal mode): a prompt
// (openPrompt) one row high below the pane; <CR> runs its line with
// command.execute once the focus is back on the pane, so the commands'
// `when` sees that pane. Errors are shown as the mapping's.
export function openCommandLine(): void {
  openPrompt({ prompt: ":", onLine: (line) => command.execute(line) });
}
