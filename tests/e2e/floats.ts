// Floating panes over a shell with a program's image and over a PDF page.
// Judged: expect for each. Expected (90x30 cells of 10x20 px):
//   float.png  a framed float (columns 9-80, rows 3-26, title "shell" on its
//              top edge, accent-coloured frame) showing "inside the float";
//              inside it no zeros, no part of the red-to-blue gradient (left
//              pane) and no part of the PDF page (right pane); both show
//              around it
//   moved.png  the float dragged by its title edge to columns 18-89, rows 6-29
//              (kept on screen): the zeros and the gradient show again at the
//              top left
//   notice.png a floating message at the top right ("a notice in a frame"),
//              readable over the PDF page; the float's title readable on
//              its frame (in WezTerm too, which draws images over text)
// Also judged: each expect's cells against this terminal's own drawing
// of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { session } from "./lib/commands.ts";

export default scenario(
  { ...session("e2e-f"), prepare: (fs) => fs.mkdir("target/e2e-floats") },
  async (_t, { magick, outdir, type, pause, run, keys, expect, down, move, up }) => {
    await magick(["-size", "300x300", "gradient:red-blue", `${outdir}/img.png`]);
    await type(
      String.raw`clear; for i in $(seq 12); do printf '%044d\n' 0; done; printf '\033_Ga=T,f=100,t=f,c=30,r=10,q=2;%s\033\\' "$(printf %s ${outdir}/img.png | base64 -w0)"; echo` +
        "\n",
    );
    await pause(1);
    await run(["target/debug/temzdf", "action", "-s", "e2e-f", "new-pane", "target/fixtures/sample.pdf"]);
    await pause(2);
    await keys("ctrl+b", "W");
    await pause(1);
    await type("echo inside the float\n");
    await pause(1);
    await expect("float");
    // The title edge is row 3 (y 60-79); drag it 20 columns right, 4 rows down.
    await down(300, 70);
    await move(400, 110);
    await move(500, 150);
    await pause(0.3);
    await up();
    await pause(1);
    await expect("moved");
    await run(["target/debug/temzdf", "action", "-s", "e2e-f", "message", "a notice in a frame"]);
    await pause(0.5);
    await expect("notice");
    await run(["target/debug/temzdf", "kill-session", "e2e-f"]);
  },
);
