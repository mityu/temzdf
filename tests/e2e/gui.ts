// The GUI client on a PDF session, under xfwm4 on Xvfb (lavapipe), or on
// Windows (the Windows VM: "Windows in a VM" in doc/dev/testing.md; there
// with COMMAND C:/t/target/release/temzdf-gui.exe --clean
// target/fixtures/sample.pdf). Judged (expect: the window against
// temzdf-gui --render-frame on the same session): the first page (start),
// after a flick of the wheel (wheel), Ctrl and the wheel zooming in at the
// pointer (zoom), page 3 after 3G (page3), the title selected by a drag
// after g (selected). Not judged: the clipboard holds the title
// (clipboard.txt, copied through arboard).

import { scenario } from "../../scripts/e2e/scenario.ts";

export default scenario(
  { command: ["target/debug/temzdf-gui", "--clean", "target/fixtures/sample.pdf"] },
  async (_t, { pause, expect, wheel, keys, dragto, clipboard, fs, outdir }) => {
    await pause(1);
    await expect("start");
    await wheel(5);
    await pause(1.5);
    await expect("wheel");
    await wheel(-3, 300, 200, "C");
    await pause(2);
    await expect("zoom");
    await keys("3", "shift+g");
    await pause(1.5);
    await expect("page3");
    await keys("0", "g");
    await pause(1.5);
    await dragto(318, 188, 582, 190, 5);
    await pause(1);
    await expect("selected");
    fs.write(`${outdir}/clipboard.txt`, (await clipboard()) ?? "");
  },
);
