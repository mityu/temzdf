// The command line (/ ? :): an input pane one row high below the text pane,
// in the mode vim/cmdline over the input mode (its editing keys work);
// <CR> runs it in the text pane, <Esc> cancels.
import { cmd } from "temzdf";
import { ex } from "./ex";
import { recordJump } from "./marks";
import { leaveTransient, operator } from "./normal";
import { again, find, wordUnder } from "./search";
import { state } from "./state";
import type { Motion } from "./state";
import { clampNormal, cursor, setCursor } from "./util";
import { exitVisual, moveVisual } from "./visual";

let open: null | { buf: number; pane: number; kind: string; count: number | null } = null;

export function openCmdline(kind: "/" | "?" | ":", count: number | null): void {
  const pane = cmd.focusedPane();
  if (pane === null) return;
  // From the visual mode, : works on its lines.
  let initial = "";
  if (state.visual !== null && kind === ":") {
    exitVisual();
    initial = "'<,'>";
  }
  const buf = cmd.newInput({ prompt: kind, text: initial });
  open = { buf, pane, kind, count };
  try {
    const p = cmd.split({ side: "bottom", buffer: buf, height: 1 });
    if (p !== null) cmd.statusLine(false, p);
  } catch (e) {
    open = null;
    cmd.deleteBuffer(buf);
    throw e;
  }
  cmd.switchMode("vim/cmdline");
}

/** Back to the text pane; what was typed. */
function close(): { line: string; kind: string; count: number | null } | null {
  const o = open;
  open = null;
  if (o === null) return null;
  const line = cmd.inputText(o.buf);
  leaveTransient();
  cmd.deleteBuffer(o.buf);
  cmd.focusPane(o.pane);
  return { line, kind: o.kind, count: o.count };
}

export function cancelCmdline(): void {
  close();
  state.pending = null;
}

/** <BS> on an empty line cancels, as in Vim. */
export function backspace(): void {
  if (open !== null && cmd.inputText(open.buf) === "") cancelCmdline();
  else cmd.input.backspace();
}

export function runCmdline(): void {
  const c = close();
  if (c === null) return;
  if (c.kind === ":") {
    state.pending = null;
    ex(c.line);
    return;
  }
  goTo(find(c.line, c.kind === "?", c.count ?? 1), c.count);
}

/** A search's result as a motion: the cursor moves there, or a pending
 * operator acts up to it (exclusive), or the visual selection's end goes. */
export function goTo(to: ReturnType<typeof find>, count: number | null): void {
  const p = state.pending;
  state.pending = null;
  if (to === null) return;
  const m: Motion = { to, kind: "exclusive" };
  if (p !== null) {
    const last = state.lastSearch;
    operator(p.op, m, p.register, () => (last === null ? null : { to: find(last.pattern, last.backward, count ?? 1)!, kind: "exclusive" }), count);
    return;
  }
  const at = cursor();
  if (to.line !== at.line || to.character !== at.character) recordJump(at);
  if (state.visual !== null) {
    moveVisual(m);
    return;
  }
  setCursor(clampNormal(to));
}

export function searchAgain(reverse: boolean, count: number | null): void {
  goTo(again(reverse, count ?? 1), count);
}

export function searchWord(backward: boolean, count: number | null): void {
  goTo(wordUnder(backward, count ?? 1), count);
}
