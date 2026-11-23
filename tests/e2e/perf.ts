// What frames cost between the session server and a client (the GUI or a
// terminal): run with TEMZDF_TIMING=1 in the environment, then read the
// `timing:` lines with scripts/timing.sh OUTDIR (doc/dev/performance.md,
// "Session socket").
// HiDPI on X11: WINIT_X11_SCALE_FACTOR=2 and -s 1800x1200 (the GUI).
// Not judged. Covers the heavy moments: the first screen, zooming in and out
// by steps, jumping to the last page and back, paging down, scrolling, and a
// reload of the whole document.

import { scenario } from "../../scripts/e2e/scenario.ts";

export default scenario(
  { command: ["target/release/temzdf-gui", "--clean", "target/fixtures/sample.pdf"] },
  async (_t, { pause, keys, wheel, fs }) => {
    await pause(2);
    for (const k of ["plus", "plus", "plus", "minus", "minus", "minus"]) {
      await keys(k);
      await pause(1);
    }
    await keys("shift+g");
    await pause(1);
    await keys("g", "g");
    await pause(1);
    await keys("space");
    await pause(1);
    await keys("space");
    await pause(1);
    await wheel(10);
    await pause(1);
    await wheel(-10);
    await pause(1);
    fs.touch("target/fixtures/sample.pdf");
    await pause(2);
  },
);
