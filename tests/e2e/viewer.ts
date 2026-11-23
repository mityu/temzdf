// The standalone viewer on a PDF. Judged (expect, mouse input replayed with
// its timing): start (page 1, fit width), after a flick of the wheel,
// zoomed in at the pointer with sharp text once tiles arrive, panned by a
// drag in the left margin (a drag over text selects instead), and page 3
// after `3G`. zoom-early (not judged: the replay has its tiles at once)
// shows the old tiles scaled right after zooming. Also judged: each
// expect's cells against this terminal's own drawing of them, once temzdf
// ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

export default scenario(pdf(), async (_t, { pause, expect, wheel, shot, drag, keys }) => {
  await pause(1);
  await expect("start");
  await wheel(5);
  await pause(1);
  await expect("wheel5");
  await wheel(-3, 300, 200, "C");
  await pause(0.05);
  await shot("zoom-early");
  await pause(1.5);
  await expect("zoom");
  await drag(30, 400, 250);
  await pause(0.5);
  await expect("drag");
  await keys("3", "shift+g");
  await pause(1.5);
  await expect("page3");
});
