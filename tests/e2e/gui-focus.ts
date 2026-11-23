// Keys still held when the GUI's window gets the focus are not typed into
// it (X11 reports them as synthetic presses on focus-in): Ctrl+D ends a
// shell in another window, which closes and gives the focus back. Not run on
// macOS (-d macos): AppKit sends no such presses, and keys held there would
// go to whatever window is in front.
// Judged (check): focus.txt holds "got:" (not "got:d") (focus).

import { scenario } from "../../scripts/e2e/scenario.ts";

export default scenario(
  { command: ["target/debug/temzdf-gui", "--clean"], env: { SHELL: "/bin/sh", PS1: "$ " } },
  async (
    _t,
    { pause, type, outdir, otherWindow, focusWindow, hold, kill, termPid, release, keys, shot, check, fs },
  ) => {
    await pause(1);
    await type(`read l; echo "got:$l" > ${outdir}/focus.txt\n`);
    await pause(0.5);
    // Another window takes the focus; Ctrl+D goes down there.
    const other = await otherWindow();
    await focusWindow(other);
    await pause(0.5);
    await hold("ctrl", "d");
    await pause(0.3);
    // It closes with the keys still down: the GUI gets the focus back.
    kill(other);
    await focusWindow(termPid);
    await pause(0.5);
    await release("d", "ctrl");
    await pause(0.3);
    await keys("Return");
    await pause(1);
    await shot("done");
    await check("focus", () => fs.read(`${outdir}/focus.txt`).split("\n").includes("got:"));
  },
);
