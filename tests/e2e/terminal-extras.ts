// Terminal panes: images from programs, selection, links, the scrollback
// position.
// Judged: expect for image, selected and scrolled. Expected:
//   image.png     the right pane (C-b %): a red-to-blue gradient of 16x4 cells
//                 below an empty line, "select me please" and "a link" (a PNG sent as a file
//                 path, t=f)
//   selected.png  "select" inverted (dragged)
//   scrolled.png  the right pane scrolled back one wheel step: "[3/N]" at its
//                 top right
// Also judged: each expect's cells against this terminal's own drawing
// of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { session } from "./lib/commands.ts";

export default scenario(
  { ...session("e2e-x"), prepare: (fs) => fs.mkdir("target/e2e-term-extras") },
  async (
    _t,
    { magick, outdir, keys, pause, type, expect, down, move, up, wheel, run, mark, waitChange, waitStill },
  ) => {
    await magick(["-size", "160x80", "gradient:red-blue", `${outdir}/img.png`]);
    await keys("ctrl+b", "percent");
    await pause(1);
    await type(
      String.raw`clear; printf '\nselect me please\n\033]8;;https://example.org\033\\a link\033]8;;\033\\\n'; printf '\033_Ga=T,f=100,t=f,c=16,r=4,q=2;%s\033\\' "$(printf %s ${outdir}/img.png | base64 -w0)"; echo` +
        "\n",
    );
    await pause(1.5);
    await expect("image");
    // "select" on the second row: y 25 is in it with cells 14 to 20 px
    // tall (kitty's 14, Ghostty's 17 and WezTerm's 16 on macOS, 20 on
    // X11). Not the first row: Ghostty (1.3) takes a press at the top
    // middle for its handle that drags the split. A pause after the
    // press: kitty reports a press followed at once by motion at the
    // motion's position.
    await down(451, 25);
    await pause(0.1);
    await move(480, 25);
    await move(508, 25);
    await pause(0.3);
    await up();
    await pause(0.5);
    await mark("copied");
    await expect("selected");
    // Until the "Copied" notice is gone (it covers the top right, where
    // scrolled's "[3/N]" is): one fading between the screen and the
    // session's frame would fail the judgement.
    await waitChange("copied");
    await waitStill();
    await type("seq 1 100\n");
    await pause(1);
    await wheel(-1, 700, 300);
    await pause(0.5);
    await expect("scrolled");
    await run(["target/debug/temzdf", "kill-session", "e2e-x"]);
  },
);
