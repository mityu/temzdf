// Wide characters beside a floating pane. WezTerm drops the wide characters
// in cells an image is placed on, and opening a float sends the screen's
// image again (the Composited backend): the text left of the float went.
// Run in WezTerm (also in kitty) at 800x600. Judged: expect for before and float, the text too in
// kitty and WezTerm. Expected (80x27 cells in WezTerm):
//   before.png  the iroha poem, seven lines of wide characters, top left
//   float.png   a float over the middle; the poem's first four columns
//               (two characters) still show left of it, its first three
//               lines above it whole
// Also judged: each expect's cells against this terminal's own drawing
// of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { session } from "./lib/commands.ts";

export default scenario(
  { ...session("e2e-w"), size: { width: 800, height: 600 } },
  async (_t, { fs, outdir, pause, type, expect, keys, run }) => {
    fs.write(
      `${outdir}/iroha.txt`,
      [
        "いろはにほへと",
        "ちりぬるをわか",
        "よたれそつねな",
        "らむうゐのおく",
        "やまけふこへて",
        "あさきゆめみし",
        "ゑひもせす",
      ]
        .map((l) => `${l}\n`)
        .join(""),
    );
    await pause(1);
    await type(`clear; cat ${outdir}/iroha.txt\n`);
    await pause(1);
    await expect("before");
    await keys("ctrl+b", "W");
    await pause(1.5);
    await expect("float");
    await run(["target/debug/temzdf", "kill-session", "e2e-w"]);
  },
);
