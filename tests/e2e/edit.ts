// A text pane in a session: editing without modes, drawn as the session's
// frame says (wide characters, the selection, the status line), and written.
// (Without modes: tests/e2e/configs/notepad.ts enables the Notepad editor.)
// Judged: expect (the screen against the session's frame) for start, typed,
// selected, replaced and written; check for the file after <C-s>.
// Expected:
//   start.png     the three lines, the cursor at the top left, "notes.txt"
//                 in the status line
//   typed.png     ">> hello world" (typed at the start), "[+]" shown
//   selected.png  "world" selected (Shift+Left five times from the line's
//                 end; not Shift+Ctrl+Left, kitty's previous_tab)
//   replaced.png  ">> hello there"
//   written.png   the same, "[+]" gone
// Also judged: each expect's cells against this terminal's own drawing
// of them, once temzdf ended (NAME.cells).

import { scenario } from "../../scripts/e2e/scenario.ts";

export default scenario(
  {
    command: [
      "target/debug/temzdf",
      "--config",
      "tests/e2e/configs/notepad.ts",
      "-s",
      "e2e-ed",
      "target/e2e-edit/notes.txt",
    ],
    shellAfter: true,
    prepare(fs) {
      fs.mkdir("target/e2e-edit");
      fs.write("target/e2e-edit/notes.txt", "hello world\n数式 wide\n\tindented\n");
    },
  },
  async (_t, { pause, expect, type, keys, check, fs, run }) => {
    await pause(1);
    await expect("start");
    await type(">> ");
    await pause(0.5);
    await expect("typed");
    await keys("End", "shift+Left", "shift+Left", "shift+Left", "shift+Left", "shift+Left");
    await pause(0.5);
    await expect("selected");
    await type("there");
    await pause(0.5);
    await expect("replaced");
    await keys("ctrl+s");
    await pause(0.5);
    await expect("written");
    await check("saved", () => fs.read("target/e2e-edit/notes.txt") === ">> hello there\n数式 wide\n\tindented\n");
    await run(["target/debug/temzdf", "kill-session", "e2e-ed"]);
    await pause(0.5);
  },
);
