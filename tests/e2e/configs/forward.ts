// Forward-search highlights stay until the next key, click or wheel step, so a
// screenshot taken at any time after the search shows them, and so does
// the headless replay `expect` compares it with (synctex.actions,
// synctex-reload.actions).
import { o } from "temzdf";

o.forwardflash = "input";
