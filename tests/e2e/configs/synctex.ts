// SyncTeX both ways (tests/e2e/synctex.ts): forward-search highlights stay
// until the next input, as forward.ts keeps them, and an inverse search
// (Ctrl+click) writes where it found the source to
// target/e2e-sync/inverse.txt, as FILE:LINE.
import { dispatch, o, on } from "temzdf";
import { run } from "temzdf/process";

o.forwardflash = "input";

on("SynctexInverse", (_pane, loc) => {
  dispatch(async () => {
    const write = 'printf "%s:%s\\n" "$1" "$2" > target/e2e-sync/inverse.txt';
    await run(["sh", "-c", write, "sh", loc.file, String(loc.line)]);
  });
});
