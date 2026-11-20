// What happens to buffers (namespace builtin-buffers): unsaved changes are
// not lost by accident, a file changed elsewhere is read again, and a swap
// file left by a session that is gone is asked about as Neovim does
// (ATTENTION: open read-only, edit anyway, recover, delete it, quit,
// abort). Defined when enabled: `import buffers from "temzdf/buffers";
// buffers.enable();`.
import { action, defineMode, schedule } from "temzdf";
import type { KeyInfo, SwapInfo } from "temzdf";
import { keyset } from "./lib/keyset";

/** The swap file being asked about: its buffer and the pane showing it. */
let asking: null | { buf: number; pane: number | undefined; info: SwapInfo } = null;

const CHOICE = "swap-choice";

/** Neovim's E325, for a swap file of a session that is gone. */
function attention(buf: number, info: SwapInfo): void {
  const b = action.buffers({ all: true }).find((x) => x.id === buf);
  if (b === undefined) return;
  const path = action.text.info({ pane: b.panes[0] }).path ?? b.name;
  const dated = new Date(info.time).toLocaleString();
  const lines = [
    "E325: ATTENTION",
    `Found a swap file by the name "${info.swap}"`,
    `          owned by: session ${info.session}${info.host === "" ? "" : ` on ${info.host}`}   dated: ${dated}`,
    `         file name: ${path}`,
    `          modified: ${info.modified ? "YES" : "no"}`,
    `        process ID: ${info.pid}`,
    `While opening file "${path}"`,
    ...(info.newer ? ["      NEWER than swap file!"] : []),
    "",
    "(1) Another program may be editing the same file. If this is the case,",
    "    be careful not to end up with two different instances of the same",
    "    file when making changes.",
    "(2) An edit session for this file crashed.",
    '    If this is the case, use "(R)ecover" to recover the changes.',
    `    If you did this already, delete the swap file "${info.swap}"`,
    "    to avoid this message.",
    "",
    "[O]pen Read-Only, (E)dit anyway, (R)ecover, (D)elete it, (Q)uit, (A)bort:",
  ];
  asking = { buf, pane: b.panes[0], info };
  // Until the answer (the next key), as Neovim's.
  action.message(lines.join("\n"), { error: true });
  action.switchMode(CHOICE);
}

/** The answer to E325 (one key). */
function answer(key: KeyInfo): void {
  const a = asking;
  const c = (key.keys === "<Esc>" ? "a" : (key.text ?? "")).toLowerCase();
  if (a === null) {
    action.exitMode();
    return;
  }
  if (!["o", "e", "r", "d", "q", "a"].includes(c)) {
    // Asked again (Neovim waits for one of them).
    action.message("[O]pen Read-Only, (E)dit anyway, (R)ecover, (D)elete it, (Q)uit, (A)bort:", { error: true });
    return;
  }
  asking = null;
  action.exitMode();
  switch (c) {
    case "o": {
      const attrs = action.text.info({ pane: a.pane }).attrs;
      action.setBufferAttrs(a.buf, [...attrs.filter((x) => x !== "readonly"), "readonly"]);
      action.useSwap(a.buf);
      break;
    }
    case "e":
      action.useSwap(a.buf);
      break;
    case "r":
      action.text.recover({ swap: a.info.swap, pane: a.pane });
      action.useSwap(a.buf);
      action.message(
        `Recovery completed. You should check if everything is OK; then write the file and delete the swap file "${a.info.swap}" (deleteSwap).`,
      );
      break;
    case "d":
      action.deleteSwap(a.info.swap);
      action.useSwap(a.buf);
      break;
    default:
      // Quit, abort: the file is not opened after all.
      action.deleteBuffer(a.buf);
  }
}

export default keyset("builtin-buffers", (d) => {
  defineMode(CHOICE, { count: false, fallback: (_ctx, key) => answer(key) });
  // A swap file of a session that is gone (SwapExists): asked about.
  d.hook("SwapExists", (buf: number, info: SwapInfo) => {
    schedule(() => attention(buf, info));
  });

  // Unsaved changes are not lost by accident: deleting a text buffer, or
  // ending the session (kill-session, closing the last pane), is refused
  // while one has them (`kill-session --force` ends anyway). Closing a pane
  // is fine: its buffer stays, hidden.
  // A buffer with unsaved changes is not deleted, unless its variable
  // forceDelete says so (Vim's :bd!).
  d.hook("BufferDeletePre", (buf: number) => {
    const b = action.buffers().find((b) => b.id === buf);
    if (b?.modified && action.getVar("buffer", "forceDelete", buf) !== true)
      return { cancel: true, reason: `${b.name} has unsaved changes (write it first)` };
  });
  d.hook("SessionEndPre", () => {
    const names = action
      .buffers()
      .filter((b) => b.modified)
      .map((b) => b.name);
    if (names.length > 0) {
      return { cancel: true, reason: `unsaved changes in ${names.join(", ")} (write them, or kill-session --force)` };
    }
  });

  // A text buffer's file changed elsewhere: read again when it has no
  // unsaved changes (Vim's autoread); otherwise say so (writing then needs
  // write({ force: true }), or reload() to drop the changes).
  d.hook("FileChangedOnDisk", (buf: number) => {
    const b = action.buffers().find((b) => b.id === buf);
    if (b === undefined) return;
    if (b.modified) action.message(`${b.name} changed on disk (reload() drops your changes)`);
    else schedule(() => action.reload(buf));
  });
});
