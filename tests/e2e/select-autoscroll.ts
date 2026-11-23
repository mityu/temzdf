// Selecting past the bottom edge in the standalone viewer. Coordinates
// assume the default 900x600 window (fit width, page 1 at the top). Judged:
// dragging from the title into the bottom edge and holding scrolls the
// document by itself while the selection grows, until the end of the two
// pages (held, expect: the bottom of page 2, its page number selected; taken
// once the screen stood still, so it does not depend on how many frames the
// viewer drew, nor on how long that took); releasing copies the selection,
// from the title to the document's end, the last line and page 2's number
// (clipboard.txt; copied and to-end, check). The pointer stays right of the
// page number (x 480; at x 450 kitty left it out and WezTerm, one pixel to
// the right, took it in), so where the selection ends is in view at held.
// Also judged: each expect's cells against this terminal's own drawing of
// them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

/** A file's lines (without the empty one after its last newline). */
const lines = (text: string) => text.replace(/\n$/, "").split("\n");

export default scenario(
  pdf("target/fixtures/sample-short.pdf"),
  async (_t, { pause, down, move, waitStill, expect, up, clipboard, fs, outdir, check }) => {
    await pause(1);
    await down(318, 188);
    // kitty on macOS reports a press followed at once by motion at the
    // motion's position (the selection then started below the title).
    await pause(0.1);
    for (const y of [260, 360, 460, 560, 575]) {
      await move(480, y);
      await pause(0.05);
    }
    // Until the autoscroll stopped at the end.
    await waitStill();
    await expect("held");
    await up();
    await pause(0.5);
    const file = `${outdir}/clipboard.txt`;
    fs.write(file, (await clipboard()) ?? "");
    await check("copied", () => /^temzdf sample document/m.test(fs.read(file)));
    await check("to-end", () => {
      const l = lines(fs.read(file));
      return l.at(-2) === "The end, with a last needle." && l.at(-1) === "2";
    });
  },
);
