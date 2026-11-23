// Terminal panes in a session.
// Judged: expect (the screen against the session's frame) for shell,
// split-less, pdf, flood and reattached; check for the messages after
// detaching and killing (detached-text, killed-text where the terminal tells
// its text; detached-restored, killed-restored: the screens as images).
// Expected:
//   shell.png       a shell filling the window: the listing, "red" in red,
//                   "green on blue", "数式 wide" (wide characters)
//   split-less.png  the shell left, less showing Cargo.toml on the right (focused)
//   pdf.png         sample.pdf right of that shell, opened from it
//                   with `temzdf action new-pane`
//   flood.png       the middle shell (focused again with C-b h) after `time seq 1 500000`: the last numbers and
//                   a real time of well under a second
//   detached.png    the outer shell, "[detached from session e2e-term]"
//   reattached.png  the same three panes as pdf.png (shell text kept)
//   killed.png      "[session e2e-term was killed]"
// Also judged: each expect's cells against this terminal's own drawing
// of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { session } from "./lib/commands.ts";

export default scenario(
  { ...session("e2e-term"), prepare: (fs) => fs.mkdir("target/e2e-terminal") },
  async (_t, { pause, type, expect, keys, shot, check, screenContains, run, drawLines, screenRestored }) => {
    await pause(1);
    await type(
      String.raw`ls tests/e2e | head -3; printf '\033[31mred\033[0m \033[32;44mgreen on blue\033[0m\n'; printf '\346\225\260\345\274\217 wide\n'` +
        "\n",
    );
    await pause(1);
    await expect("shell");
    await keys("ctrl+b", "percent");
    await pause(1);
    await type("less Cargo.toml\n");
    await pause(1);
    await expect("split-less");
    await keys("q");
    await pause(0.5);
    await type("target/debug/temzdf action new-pane target/fixtures/sample.pdf\n");
    await pause(2);
    await expect("pdf");
    await keys("ctrl+b", "h");
    await type("time seq 1 500000\n");
    await pause(3);
    await expect("flood");
    await keys("ctrl+b", "d");
    await pause(1);
    await shot("detached");
    await check("detached-text", () => screenContains("[detached from session e2e-term]"));
    await type("target/debug/temzdf attach e2e-term\n");
    await pause(2.5);
    await expect("reattached");
    await run(["target/debug/temzdf", "kill-session", "e2e-term"]);
    await pause(1);
    await shot("killed");
    await check("killed-text", () => screenContains("[session e2e-term was killed]"));
    // The screens after detaching and killing against this terminal's own
    // drawing of the messages: the screen came back, nothing of temzdf's left.
    await drawLines("reference", "[detached from session e2e-term]", "[session e2e-term was killed]");
    await check("detached-restored", () => screenRestored("detached", "reference", 0));
    await check("killed-restored", () => screenRestored("killed", "reference", 1, "detached", 0));
  },
);
