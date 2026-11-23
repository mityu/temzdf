// Many compressed tiles sent inline (TERMPIX_MEDIUM=direct, as over SSH):
// scroll the whole document, zoom in and out at several points. Pass = the
// terminal survives (exit status 0, not 3) and, once every tile of the last
// screen has arrived (waitComplete: inline transfers are slow), it is the
// one a headless render of the same input draws (expect stress-end). Also
// judged: each expect's cells against this terminal's own drawing of them,
// once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

export default scenario(
  pdf(undefined, { TERMPIX_MEDIUM: "direct" }),
  async (_t, { wheel, pause, keys, waitComplete, expect }) => {
    await wheel(30);
    await pause(0.5);
    await wheel(-30);
    for (const i of [1, 2, 3]) {
      await wheel(-2, 200 * i, 250, "C");
      await pause(0.7);
    }
    await wheel(15);
    await pause(0.7);
    await wheel(6, 400, 300, "C");
    await pause(1);
    await keys("shift+g");
    await pause(1.5);
    await waitComplete();
    await expect("stress-end");
  },
);
