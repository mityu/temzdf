// Reloading as LaTeX tools write the PDF. Judged: expect for before, short
// and recreated, against the file on disk at that moment after "2G" (the
// reload kept the place); while the file is cut short, the screen is
// before's (truncated, the status line aside) and the status line says
// "reloading…" (reloading; kitty and WezTerm). Expected:
//   before.png     page 2 ("3 Filler"), status 2 / 3
//   truncated.png  same view; "reloading…" in the status bar, old document still shown
//   short.png      after rename-over with the 2-page version: still page 2 (now
//                  "3 Conclusion"), status 2 / 2, no "reloading…"
//   recreated.png  after delete + recreate with the 3-page version: page 2 again, 2 / 3
// Also judged: each expect's cells against this terminal's own drawing of
// them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { pdf } from "./lib/commands.ts";

const doc = "target/e2e-reload/doc.pdf";

export default scenario(
  {
    ...pdf(doc),
    prepare(fs) {
      fs.mkdir("target/e2e-reload");
      fs.copy("target/fixtures/sample.pdf", doc);
    },
  },
  async (_t, { keys, pause, expect, fs, shot, check, sameScreen, screenContains }) => {
    await keys("2", "shift+g");
    await pause(1.2);
    await expect("before", "2G");
    fs.head("target/fixtures/sample.pdf", 60000, doc);
    await pause(1.2);
    await shot("truncated");
    await check("truncated", () => sameScreen("truncated", "before"));
    await check("reloading", () => screenContains("reloading…"));
    fs.copy("target/fixtures/sample-short.pdf", "target/e2e-reload/.doc.pdf.tmp");
    fs.rename("target/e2e-reload/.doc.pdf.tmp", doc);
    await pause(1.5);
    await expect("short", "2G");
    fs.remove(doc);
    await pause(0.5);
    fs.copy("target/fixtures/sample.pdf", doc);
    await pause(1.5);
    await expect("recreated", "2G");
  },
);
