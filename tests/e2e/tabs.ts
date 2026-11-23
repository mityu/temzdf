// Tab pages: a shell with a program's image in one, a PDF in the other.
// Judged: expect for each. Expected (90x30 cells of 10x20 px):
//   pdf.png    the tab line on the top row (" 1 shell  2 sample.pdf", the
//              second highlighted), the PDF page below it; nothing of the
//              shell, its zeros or the gradient
//   shell.png  after <C-b>gt: the tab line with the first tab highlighted,
//              the shell with its zeros and the red-to-blue gradient; no part
//              of the PDF page anywhere
//   back.png   after a click on the second tab: the PDF page again, no
//              gradient
//   one.png    after a middle click on the first tab: no tab line, the PDF
//              page from the top row
// Also judged: each expect's cells against this terminal's own drawing
// of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { session } from "./lib/commands.ts";

export default scenario(
  { ...session("e2e-t"), prepare: (fs) => fs.mkdir("target/e2e-tabs") },
  async (_t, { magick, outdir, type, pause, run, expect, keys, click }) => {
    await magick(["-size", "300x300", "gradient:red-blue", `${outdir}/img.png`]);
    await type(
      String.raw`clear; for i in $(seq 12); do printf '%044d\n' 0; done; printf '\033_Ga=T,f=100,t=f,c=30,r=10,q=2;%s\033\\' "$(printf %s ${outdir}/img.png | base64 -w0)"; echo` +
        "\n",
    );
    await pause(1);
    await run(["target/debug/temzdf", "action", "-s", "e2e-t", "new-tab", "target/fixtures/sample.pdf"]);
    await pause(2);
    await expect("pdf");
    await keys("ctrl+b", "g", "t");
    await pause(1);
    await expect("shell");
    // The second tab's label starts at column 9 (" 1 shell " is 9 cells).
    await click(120, 10);
    await pause(1);
    await expect("back");
    await click(30, 10, 2);
    await pause(1);
    await expect("one");
    await run(["target/debug/temzdf", "kill-session", "e2e-t"]);
  },
);
