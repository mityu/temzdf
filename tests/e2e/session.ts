// A PDF session: splitting, detaching and attaching again, a pane added
// by another process, killing it. Judged: expect for split, reattached and
// new-pane; check for the messages after detaching and killing (their text
// where the terminal tells it; the screens as images, detached-restored and
// killed-restored), and for `temzdf ls` listing the session. Expected:
//   split.png       two panes side by side on the same document, the right
//                   one focused (accent border), scrolled down three steps
//   detached.png    the shell, "[detached from session e2e]"
//   ls.txt          "e2e  (2 panes, detached)  .../sample.pdf"
//   reattached.png  the same two panes as split.png (state kept)
//   new-pane.png    a third pane with sample-short.pdf, added by another process
//   killed.png      the shell, "[session e2e was killed]"
// Also judged: each expect's cells against this terminal's own drawing of
// them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";

export default scenario(
  {
    command: ["target/debug/temzdf", "--clean", "-s", "e2e", "target/fixtures/sample.pdf"],
    shellAfter: true,
    prepare: (fs) => fs.mkdir("target/e2e-session"),
  },
  async (_t, { pause, keys, expect, shot, check, screenContains, type, fs, run, drawLines, screenRestored }) => {
    await pause(1);
    await keys("ctrl+b", "percent");
    await pause(1.5);
    await keys("j", "j", "j");
    await pause(1);
    await expect("split");
    await keys("ctrl+b", "d");
    await pause(1);
    await shot("detached");
    await check("detached-text", () => screenContains("[detached from session e2e]"));
    fs.remove("target/e2e-session/ls.txt");
    await type("target/debug/temzdf ls > target/e2e-session/ls.txt; target/debug/temzdf attach e2e\n");
    await pause(2.5);
    await check("ls", () => /^e2e .*detached/m.test(fs.read("target/e2e-session/ls.txt")));
    await expect("reattached");
    await run(["target/debug/temzdf", "action", "-s", "e2e", "new-pane", "target/fixtures/sample-short.pdf"]);
    await pause(2);
    await expect("new-pane");
    await run(["target/debug/temzdf", "kill-session", "e2e"]);
    await pause(1);
    await shot("killed");
    await check("killed-text", () => screenContains("[session e2e was killed]"));
    // The screens after detaching and killing against this terminal's own
    // drawing of the messages: the screen came back, nothing of temzdf's left.
    await drawLines("reference", "[detached from session e2e]", "[session e2e was killed]");
    await check("detached-restored", () => screenRestored("detached", "reference", 0));
    await check("killed-restored", () => screenRestored("killed", "reference", 1, "detached", 0));
  },
);
