// Selecting text in the standalone viewer. Coordinates assume the default
// 900x600 window (fit width, page 1 at the top). Judged (expect): dragging
// across the title selects it (blue) and the status bar says "Copied 22
// characters" (selected); dragging in the margin pans instead (panned). Not
// judged: the clipboard holds the title (clipboard.txt). Also judged: each
// expect's cells against this terminal's own drawing of them, once temzdf
// ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

export default scenario(pdf(), async (_t, { pause, dragto, expect, clipboard, fs, outdir, drag }) => {
  await pause(1);
  await dragto(318, 188, 582, 190, 5);
  await pause(0.5);
  await expect("selected");
  fs.write(`${outdir}/clipboard.txt`, (await clipboard()) ?? "");
  await drag(50, 400, 300);
  await pause(0.5);
  await expect("panned");
});
