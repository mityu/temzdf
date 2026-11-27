// Forward-search highlights stay until the next key, click or wheel step, so a
// screenshot taken at any time after the search shows them, and so does
// the headless replay `expect` compares it with (tests/e2e/synctex.ts,
// tests/e2e/synctex-reload.ts).
import { o } from "temzdf";

o.forwardflash = "input";
