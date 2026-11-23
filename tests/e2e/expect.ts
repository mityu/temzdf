// Key-driven scenario judged automatically: each `expect` compares the
// terminal with the screen `temzdf standalone pdf --render-frame` draws
// after the same keys, so it checks the path from viewer state to pixels on
// screen (placements, crops, updates, stale images), not PDF rendering
// itself (both sides use MuPDF). Expected: every line of OUTDIR/expect.txt
// is PASS (exit 4 otherwise). Also judged: each expect's cells against this
// terminal's own drawing of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

export default scenario(pdf(), async (_t, { pause, expect, keys, type }) => {
  await pause(1);
  await expect("start", "");
  // One key at a time: quick repeats accelerate scrolling (flick), which the
  // headless renderer (it lets each key's animation settle) does not reproduce.
  for (let i = 0; i < 3; i++) {
    await keys("j");
    await pause(0.6);
  }
  await expect("scrolled", "jjj");
  await keys("plus");
  await pause(0.3);
  await keys("plus");
  await pause(1.5);
  await expect("zoomed", "jjj++");
  await keys("0");
  await pause(1);
  await expect("fit", "jjj++0");
  await keys("2", "shift+g");
  await pause(1.5);
  await expect("page2", "jjj++02G");
  await keys("slash");
  await type("needle");
  // The hit shows while the input line is open (the page placed for the pane
  // without it, else, after <CR>: rendering speed would decide).
  await pause(1);
  await keys("Return");
  await pause(1);
  await expect("search", "jjj++02G/needle<CR>");
  await keys("n");
  await pause(1);
  await expect("next", "jjj++02G/needle<CR>n");
  // Apart: with legacy key encoding (tmux) ESC followed quickly by Tab reads as Alt+Tab.
  await keys("Escape");
  await pause(0.3);
  await keys("Tab");
  await pause(1);
  await expect("sidebar", "jjj++02G/needle<CR>n<Esc><Tab>");
});
