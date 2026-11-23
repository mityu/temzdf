// Links in the standalone viewer. Coordinates assume the default 900x600
// window (fit width, page 1 at the top). Judged (expect): clicking "3
// Filler" in the table of contents jumps to section 3 on page 2 (clicked);
// Backspace (or Ctrl-o) returns to the table of contents (back); Alt+Right
// goes forward again (forward); from the top, three steps down show the URL
// in section 1 where the click below expects it (url), and clicking it
// hands it to the opener (opened; the harness's stand-in notes it in
// OUTDIR/opened.txt instead of starting a browser). Also judged: each
// expect's cells against this terminal's own drawing of them, once temzdf
// ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

export default scenario(pdf(), async (_t, { pause, click, expect, keys, check, fs, outdir }) => {
  await pause(1);
  await click(160, 412);
  await pause(1);
  await expect("clicked");
  await keys("BackSpace");
  await pause(1);
  await expect("back");
  await keys("alt+Right");
  await pause(1);
  await expect("forward");
  await keys("g", "g", "j", "j", "j");
  await pause(1);
  await expect("url");
  await click(195, 380);
  await pause(1);
  await check("opened", () => fs.read(`${outdir}/opened.txt`).split("\n").includes("https://example.com/"));
});
