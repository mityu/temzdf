// The Markdown viewer (doc/design/markdown-viewer.md): the text mode (a
// text pane: wrapped paragraphs, the table's borders, math as text by
// libtexprintf, the SVG image below its line), the graphics mode (Blitz:
// strips of the page as images), text selected there with the mouse, a
// search marked there, zoom, and back.
// Judged: expect (the screen against the session's frame) for each step.
// Expected:
//   source.png    the file's source in the text editor
//   text.png      the viewer's text mode: "temzdf sample Markdown" over a
//                 double rule, the paragraph wrapped, the list's bullets
//   graphics.png  the same document as a GitHub-like page in pixels,
//                 "markdown" in the status line
//   selected.png  the paragraph's first line, from "paragraph" into
//                 "strikethrough", selected in blue; "Copied N characters"
//   search.png    "Tables" searched: the page scrolled to it, the match
//                 marked in orange, "[1/2]" in the status line
//   zoom.png      the page larger (zoomed in twice)
//   back.png      the text mode again, at the same heading
// Also judged: each expect's cells against this terminal's own drawing
// of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";

export default scenario(
  {
    command: [
      "target/debug/temzdf",
      "--config",
      "tests/e2e/configs/markdown.ts",
      "-s",
      "e2e-md",
      "target/e2e-markdown/sample.md",
    ],
    shellAfter: true,
    prepare(fs) {
      fs.mkdir("target/e2e-markdown");
      fs.copy("tests/fixtures/sample.md", "target/e2e-markdown/sample.md");
      fs.copy("tests/fixtures/sample-figure.svg", "target/e2e-markdown/sample-figure.svg");
    },
  },
  async (_t, { pause, expect, keys, dragto, type, run }) => {
    await pause(1);
    await expect("source");
    await keys("F5");
    await pause(1);
    await expect("text");
    await keys("Tab");
    await pause(1.5);
    await expect("graphics");
    await dragto(50, 93, 530, 93, 5);
    await pause(0.5);
    await expect("selected");
    await keys("Escape");
    // Until the "Copied" notice is gone (3 s): a notice fading between the
    // screen and the session's frame would fail the next judgement.
    await pause(3);
    await type("/Tables");
    await keys("Return");
    await pause(1);
    await expect("search");
    await type("++");
    await pause(1.5);
    await expect("zoom");
    await keys("Tab");
    await pause(1);
    await expect("back");
    await run(["target/debug/temzdf", "kill-session", "e2e-md"]);
    await pause(0.5);
  },
);
