// A terminal without the kitty graphics protocol, as TERMPIX_GRAPHICS=off
// makes any terminal look: the viewer runs as text only. Judged (check): no
// page is drawn (the middle of the screen is the terminal's dark
// background, not a white page; blank); the PDF pane's line and the message
// saying why show (line, why; kitty and WezTerm), and a key takes the
// message away (dismissed).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

export default scenario(
  pdf(undefined, { TERMPIX_GRAPHICS: "off" }),
  async (_t, { pause, shot, check, readGeometry, magick, outdir, origin, screenContains, keys }) => {
    await pause(1.5);
    await shot("start");
    // Mostly dark where a page would be white (the line crosses it): the
    // middle of the terminal's cells (on macOS the screenshot is the whole
    // display, with an alpha channel the mean would count).
    await check("blank", async () => {
      const g = readGeometry();
      if (!g) return false;
      const mean = await magick([
        `${outdir}/start.png`,
        "-alpha",
        "off",
        "-crop",
        `${g.cols * g.cw}x${g.rows * g.ch}+${origin.x}+${origin.y}`,
        "+repage",
        "-gravity",
        "center",
        "-crop",
        "50%x50%+0+0",
        "+repage",
        "-colorspace",
        "gray",
        "-format",
        "%[fx:mean]",
        "info:",
      ]);
      return Number(mean) < 0.2;
    });
    await check("line", () => screenContains("PDF viewer disabled (no kitty graphics protocol support)"));
    await check("why", () => screenContains("TERMPIX_GRAPHICS=off"));
    await keys("j");
    await pause(0.5);
    await check("dismissed", async () => {
      const shown = await screenContains("TERMPIX_GRAPHICS=off");
      return shown === "skip" ? "skip" : !shown;
    });
  },
);
