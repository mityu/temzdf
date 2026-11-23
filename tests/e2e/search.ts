// Search in the standalone viewer. Judged (expect: each screen compared
// with --render-frame replaying the same input): typing shows the input
// line below the page with incremental highlights (typing), Enter + n moves
// to the next hit (next), Esc clears highlights (cleared). Also judged: each
// expect's cells against this terminal's own drawing of them, once temzdf
// ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

export default scenario(pdf(), async (_t, { keys, type, pause, expect }) => {
  await keys("slash");
  await type("needle");
  await pause(0.8);
  await expect("typing");
  await keys("Return", "n");
  await pause(0.8);
  await expect("next");
  await keys("Escape");
  await pause(0.5);
  await expect("cleared");
});
