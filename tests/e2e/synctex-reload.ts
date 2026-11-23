// Forward search sent right after a rebuild, as an editor's compile
// callback (vimtex) does: it must land in the new version (forward.ts: the
// forward search's highlight stays until the next input). Judged: the
// forward search after the rebuild succeeds (forward-sent, check) and shows
// page 1 of the new version (six "Inserted paragraph" lines) with the line
// starting "This document exercises" highlighted in red, not the place that
// line had before the rebuild (forward, expect: the replay opens the new
// version and sends both searches, the first one, for the old version, to
// the new SyncTeX data). Also judged: each expect's cells against this
// terminal's own drawing of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { lineOf } from "./lib/commands.ts";

const dir = "target/e2e-sync-reload";

export default scenario(
  {
    command: [
      "target/debug/temzdf",
      "standalone",
      "pdf",
      "--config",
      "tests/e2e/configs/forward.ts",
      `${dir}/sample.pdf`,
    ],
    shellAfter: true,
    prepare(fs) {
      fs.mkdir(dir);
      fs.copy("target/fixtures/sample.pdf", `${dir}/sample.pdf`);
      fs.copy("target/fixtures/sample.synctex.gz", `${dir}/sample.synctex.gz`);
    },
  },
  async (_t, { fs, pause, forward, keys, check, expect }) => {
    const line = lineOf(fs.read("tests/fixtures/sample-insert.tex"), /^This document exercises/);
    await pause(1);
    // A first forward search loads the old SyncTeX data into the viewer.
    await forward(
      `${lineOf(fs.read("tests/fixtures/sample.tex"), /^Back to Section/)}:sample.tex`,
      `${dir}/sample.pdf`,
    );
    await pause(1);
    await keys("g");
    await pause(0.5);
    // Replace by rename, like latexmk; the PDF first, then the SyncTeX data.
    fs.copy("target/fixtures/sample-insert.pdf", `${dir}/new.pdf`);
    fs.rename(`${dir}/new.pdf`, `${dir}/sample.pdf`);
    fs.copy("target/fixtures/sample-insert.synctex.gz", `${dir}/sample.synctex.gz`);
    await check("forward-sent", () => forward(`${line}:sample-insert.tex`, `${dir}/sample.pdf`));
    await pause(0.8);
    await expect("forward");
  },
);
