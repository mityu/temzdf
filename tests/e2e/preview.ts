// Link previews in the standalone viewer. Coordinates assume the default
// 900x600 window (fit width, page 1 at the top). Judged (expect, the timing
// replayed): resting on "3 Filler" in the table of contents opens a preview
// of section 3 above it (no room below) with the page's columns aligned
// (preview); the wheel over the preview scrolls it, not the page
// (preview-scrolled); moving away closes it (closed). Also judged: each
// expect's cells against this terminal's own drawing of them, once temzdf
// ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

export default scenario(pdf(), async (_t, { pause, move, expect, wheel }) => {
  await pause(1);
  await move(150, 412);
  await pause(1);
  await expect("preview");
  await move(150, 380);
  await move(300, 300);
  await pause(0.1);
  await wheel(3, 300, 300);
  await pause(0.5);
  await expect("preview-scrolled");
  await move(800, 60);
  await pause(1);
  await expect("closed");
});
