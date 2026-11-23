// SyncTeX both ways (forward.ts: the forward search's highlight stays until
// the next input). Judged: `temzdf --forward` from another process succeeds
// (forward-sent, check) and shows page 2 with the "Back to Section 1" line
// highlighted in red (forward, expect: the replay sends the same search;
// forward-page, check: the status line, kitty and WezTerm); Ctrl+click on
// the title on page 1 runs the editor on sample.tex at a line of the title,
// from \title to the line after \maketitle (inverse, check). Also judged:
// each expect's cells against this terminal's own drawing of them, once
// temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { lineOf } from "./lib/commands.ts";

export default scenario(
  {
    command: [
      "target/debug/temzdf",
      "standalone",
      "pdf",
      "--config",
      "tests/e2e/configs/forward.ts",
      "--editor",
      "echo %{input}:%{line} > target/e2e-sync/inverse.txt",
      "target/e2e-sync/sample.pdf",
    ],
    shellAfter: true,
    prepare(fs) {
      fs.mkdir("target/e2e-sync");
      fs.copy("target/fixtures/sample.pdf", "target/e2e-sync/sample.pdf");
      fs.copy("target/fixtures/sample.synctex.gz", "target/e2e-sync/sample.synctex.gz");
      fs.remove("target/e2e-sync/inverse.txt");
    },
  },
  async (_t, { fs, pause, check, forward, expect, screenContains, keys, click }) => {
    const tex = fs.read("tests/fixtures/sample.tex");
    const line = lineOf(tex, /Back to Section/);
    await pause(1);
    await check("forward-sent", () => forward(`${line}:sample.tex`, "target/e2e-sync/sample.pdf"));
    await pause(0.8);
    await expect("forward");
    await check("forward-page", () => screenContains("2 / 3"));
    await keys("g");
    await pause(1);
    fs.remove("target/e2e-sync/inverse.txt");
    await click(450, 188, 1, "C");
    await pause(1);
    // The line SyncTeX gives for the title: where \maketitle ends.
    await check("inverse", () => {
      const text = fs.exists("target/e2e-sync/inverse.txt") ? fs.read("target/e2e-sync/inverse.txt") : "";
      const got = text
        .split("\n")
        .map((l) => /.*\/tests\/fixtures\/sample\.tex:([0-9]+)$/.exec(l)?.[1])
        .find((g) => g !== undefined);
      const first = lineOf(tex, /^\\title/);
      const last = lineOf(tex, /^\\maketitle/) + 1;
      return got !== undefined && Number(got) >= first && Number(got) <= last;
    });
  },
);
