// Another program destroys the GUI's window without asking (`xdotool
// windowclose`, as a key bound to it does): the GUI detaches and ends as
// when the window is closed, not running on with no window.
// Judged (check): the GUI's process ended (ended). X11 only (SKIP
// elsewhere): on Windows and macOS no other process can destroy a window,
// and Wayland compositors only ask the window to close.

import { pidAlive } from "../../scripts/e2e/run.ts";
import { scenario } from "../../scripts/e2e/scenario.ts";

export default scenario(
  { command: ["target/debug/temzdf-gui", "--clean", "target/fixtures/sample.pdf"], ends: true },
  async (_t, { pause, run, check, termPid, display }) => {
    await pause(1);
    if (display !== "x11") {
      await check("ended", () => "skip");
      return;
    }
    // Two commands: `search --pid N windowclose` reads windowclose as the
    // pattern to search for.
    const window = (await run(["xdotool", "search", "--pid", String(termPid)])).trim().split("\n")[0]!;
    await run(["xdotool", "windowclose", window]);
    for (let i = 0; i < 50 && pidAlive(termPid); i++) await pause(0.1);
    await check("ended", () => !pidAlive(termPid));
  },
);
