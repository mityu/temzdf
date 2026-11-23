// termpix's scroll_demo example (not temzdf): scrolling by the wheel and by
// dragging, measured on the screenshots.
// Expected: the image's top at the top (start); six wheel notches, each
// once the screen stood still after the last, scroll 360 px (wheel6: 60
// px a step; sent at least 0.6 s apart, far past the 80 ms flick window
// of termpix-core's scroll.rs, so no step should accelerate; WezTerm
// still gave 372 px, one step accelerated, in 2 of 6 runs, with steps
// 0.34 s and 1.1 s apart, but not in 6 later runs logged with
// SCROLL_DEMO_LOG: todo.md); in Ghostty 1296 px (it reports three steps a
// notch, at once: each notch's three accelerate, 60 x (1 + 1.2 + 1.4));
// dragging up by 100 px scrolls 100 px further (drag100).
// Judged (check): how far each scrolled, from where the bands' boundaries
// are in the screenshots (wheel6, drag100; scroll_demo writes no log, so by
// the difference between two screenshots). Not on macOS (SKIP), where
// kitty reports three steps a notch and Ghostty nine (past the image's end).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { Skip } from "../../scripts/e2e/helpers.ts";

export default scenario(
  {
    command: ["target/debug/examples/scroll_demo"],
    env: (outdir) => ({ SCROLL_DEMO_LOG: `${outdir}/scroll_demo.log` }),
  },
  async (_t, { pause, shot, magick, outdir, display, wheel, waitStill, term, check, drag }) => {
    await pause(1);
    await shot("start");
    // The image's offset at the top of screenshot NAME (plus the terminal's
    // padding, the same in every screenshot): from the first boundary between
    // two bands (every 500 px, their colour's hue 0.13 apart) below the top. A
    // strip of columns around the middle averaged into one, the grid's rows
    // (grey) and the dark line every 1000 px skipped: the first band row seen
    // is the image's row 500 k + 2 (below the grid's line), or 500 k + 8 for
    // even k (below the dark line). The hues wrap after the image's row 3500.
    const bandOffset = async (name: string): Promise<number> => {
      const file = `${outdir}/${name}.png`;
      const w = Number(await magick(["identify", "-format", "%w", file]));
      const txt = await magick([
        file,
        "-alpha",
        "off",
        "-crop",
        `200x+${Math.floor(w / 2) - 100}+0`,
        "+repage",
        "-scale",
        "1x!",
        "-depth",
        "8",
        "txt:-",
      ]);
      let seen = false;
      let pk = 0;
      for (const line of txt.split("\n").slice(1)) {
        const f = line.split(/[(),: ]+/);
        if (f.length < 5) continue;
        const [y, r, g, b] = [Number(f[1]), Number(f[2]), Number(f[3]), Number(f[4])];
        const max = Math.max(r, g, b);
        const min = Math.min(r, g, b);
        if (max - min < 30) continue;
        let h: number;
        if (max === r) h = (g - b) / (max - min);
        else if (max === g) h = 2 + (b - r) / (max - min);
        else h = 4 + (r - g) / (max - min);
        h /= 6;
        if (h < 0) h += 1;
        const k = Math.floor(h / 0.13 + 0.5);
        if (seen && k !== pk) return 500 * k + (k % 2 ? 2 : 8) - y;
        pk = k;
        seen = true;
      }
      throw new Error(`no band boundary in ${name}.png`);
    };
    // Whether the image moved N px (within 2) from screenshot A to B.
    const bandMoved = async (a: string, b: string, n: number) => {
      if (display === "macos") throw new Skip();
      const from = await bandOffset(a);
      const d = (await bandOffset(b)) - from;
      console.log(`band_moved ${a} -> ${b}: ${d} px (expected ${n})`);
      return d - n <= 2 && n - d <= 2;
    };
    for (let i = 0; i < 6; i++) {
      await wheel(1);
      await waitStill();
    }
    await pause(1);
    await shot("wheel6");
    const wheeled = term === "ghostty" ? 1296 : 360;
    await check("wheel6", () => bandMoved("start", "wheel6", wheeled));
    await drag(450, 300, 200);
    await pause(0.5);
    await shot("drag100");
    await check("drag100", () => bandMoved("wheel6", "drag100", 100));
  },
);
