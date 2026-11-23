// Resizing panes: dragging a border with the mouse (pixel coordinates from
// the terminal), then the shipped keys.
// Judged: expect for each. Expected (90x30 cells of 10x20 px):
//   dragged.png  the border at column 60: the left pane 60 columns wide, the
//                right one printing "30 29" (`stty size`)
//   keys.png     after C-b 5 > (five columns wider): the border at column 55, the right pane
//                printing "30 34"
// Also judged: each expect's cells against this terminal's own drawing
// of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { session } from "./lib/commands.ts";

export default scenario(
  { ...session("e2e-r"), prepare: (fs) => fs.mkdir("target/e2e-resize") },
  async (_t, { keys, pause, down, move, up, type, expect, run }) => {
    await keys("ctrl+b", "percent");
    await pause(1);
    // The border is column 44 (x 440-449).
    await down(445, 200);
    await move(500, 200);
    await move(605, 200);
    await pause(0.3);
    await up();
    await pause(0.5);
    await type("clear; stty size\n");
    await pause(1);
    await expect("dragged");
    await keys("ctrl+b", "5", "greater", "Escape");
    await type("clear; stty size\n");
    await pause(1);
    await expect("keys");
    await run(["target/debug/temzdf", "kill-session", "e2e-r"]);
  },
);
