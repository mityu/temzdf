// The commands scenarios run (scripts/e2e-all.sh's `pdf` and `session`).

import type { CommandSpec } from "../../../scripts/e2e/command.ts";

/** A shell's variables for what the scenario types into the shell left
 * after the command (SHELL and PS1 reach temzdf only). */
const shell = { SHELL: "/bin/sh", PS1: "$ " };

/** The viewer on a PDF, and a shell after it: there the harness has the
 * terminal draw the cells of each expect, to compare with its screen
 * during the run. */
export function pdf(file = "target/fixtures/sample.pdf", outerEnv?: Record<string, string>): CommandSpec {
  return {
    command: ["target/debug/temzdf", "standalone", "pdf", "--clean", file],
    env: shell,
    shellAfter: true,
    outerEnv,
  };
}

/** A shell that starts session NAME and stays after. */
export function session(name: string): CommandSpec {
  return { command: ["target/debug/temzdf", "--clean", "-s", name], env: shell, shellAfter: true };
}

/** The number of the first line of TEXT matching RE (as `grep -n RE | cut
 * -d: -f1`), or throws. */
export function lineOf(text: string, re: RegExp): number {
  const i = text.split("\n").findIndex((l) => re.test(l));
  if (i < 0) throw new Error(`no line matches ${re}`);
  return i + 1;
}
