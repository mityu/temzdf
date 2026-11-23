// The terminal visual mode: keys select and copy a shell's output.
// Judged (check): copied-word (C-b [, k0wve, y: the clipboard is "beta"),
// copied-rows (gg V 2j y: "1", "2", "3" on three lines), searched (/alpha,
// v e y: "alpha"), label (the window's status line says VISUAL LINE; kitty and
// WezTerm only). Not judged: rows.png (rows 1 to 3 inverted at the top of
// the scrollback, the mode on the bottom row, the block cursor).

import { scenario } from "../../scripts/e2e/scenario.ts";
import { session } from "./lib/commands.ts";

export default scenario(
  { ...session("e2e-v"), prepare: (fs) => fs.mkdir("target/e2e-visual") },
  async (_t, { pause, type, keys, clipboard, fs, outdir, check, shot, screenContains, run }) => {
    /** The clipboard into OUTDIR/NAME.txt (empty when it cannot be read). */
    const save = async (name: string) => {
      fs.write(`${outdir}/${name}.txt`, (await clipboard()) ?? "");
      return fs.read(`${outdir}/${name}.txt`);
    };
    /** Whether TEXT has a line LINE (`grep -qx`). */
    const hasLine = (text: string, line: string) => text.split("\n").includes(line);
    await pause(1);
    await type("clear; seq 1 100; echo alpha beta\n");
    await pause(1);
    await keys("ctrl+b", "bracketleft");
    await pause(0.3);
    await type("k0wve");
    await pause(0.3);
    await keys("y");
    await pause(0.5);
    const word = await save("word");
    await check("copied-word", () => hasLine(word, "beta"));
    await keys("ctrl+b", "bracketleft");
    await pause(0.3);
    await type("ggV2j");
    await pause(0.5);
    await shot("rows");
    await check("label", () => screenContains(" VISUAL LINE "));
    await keys("y");
    await pause(0.5);
    const rows = await save("rows");
    // (As `test "$(cat F)" = ...`: trailing newlines aside.)
    await check("copied-rows", () => rows.replace(/\n+$/, "") === "1\n2\n3");
    await keys("ctrl+b", "bracketleft");
    await pause(0.3);
    await type("gg/alpha");
    await keys("Return");
    await pause(0.5);
    await type("vey");
    await pause(0.5);
    const search = await save("search");
    await check("searched", () => hasLine(search, "alpha"));
    await run(["target/debug/temzdf", "kill-session", "e2e-v"]);
  },
);
