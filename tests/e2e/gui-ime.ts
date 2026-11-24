// Japanese input in the GUI: fcitx5 with Mozc into a shell, over XIM on X11
// and over input-method-v2 and text-input-v3 on Wayland (add -d wayland).
// Judged: expect done; check: ime.txt holds "got:にほん" (committed text
// reached the shell; ime), pasted.txt holds "got:pasted" (Ctrl+Shift+V
// pasted the clipboard; pasted), fed.txt holds "got:fed" (the same keys
// with Return fed at once by `temzdf eval`: Return waited for the paste;
// fed). Not judged: composing.png shows "にほん" being composed at the
// prompt (Mozc's window at the cursor).

import { scenario } from "../../scripts/e2e/scenario.ts";

export default scenario(
  { command: ["target/debug/temzdf-gui", "--clean"], env: { SHELL: "/bin/sh", PS1: "$ " }, ime: true },
  async (_t, { pause, type, outdir, ime, shot, keys, setClipboard, expect, check, fs, run }) => {
    await pause(1);
    await type(`read l; echo "got:$l" > ${outdir}/ime.txt\n`);
    await pause(0.5);
    await ime("on");
    await pause(0.5);
    await type("nihonn", 80);
    await pause(1);
    await shot("composing");
    await keys("Return");
    await ime("off");
    await pause(0.3);
    await keys("Return");
    await pause(1);
    await type(`read l; echo "got:$l" > ${outdir}/pasted.txt\n`);
    await setClipboard("pasted");
    await pause(0.3);
    await keys("ctrl+shift+v", "Return");
    await pause(1);
    await type(`read l; echo "got:$l" > ${outdir}/fed.txt\n`);
    await setClipboard("fed");
    await pause(0.3);
    await run(["target/debug/temzdf", "eval", "-c", 'temzdf.action.feedkeys("<C-S-v><CR>")']);
    await pause(1);
    await expect("done");
    await check("ime", () => fs.read(`${outdir}/ime.txt`).split("\n").includes("got:にほん"));
    await check("pasted", () => fs.read(`${outdir}/pasted.txt`).split("\n").includes("got:pasted"));
    await check("fed", () => fs.read(`${outdir}/fed.txt`).split("\n").includes("got:fed"));
  },
);
