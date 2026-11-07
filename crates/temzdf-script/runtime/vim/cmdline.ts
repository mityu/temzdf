// The command line (/ ? :): a prompt (openPrompt) one row high below the
// text pane, in the mode vim/cmdline over the input mode (its editing keys
// work); <CR> runs the cursor's line in the text pane, <Esc> cancels.
import { acceptPrompt, cancelPrompt, cmd, openPrompt } from "temzdf";
import type { Position } from "temzdf";
import { ex } from "./ex";
import { rememberEx } from "./macros";
import { recordJump } from "./marks";
import { leaveTransient, operator } from "./normal";
import { convert } from "./pattern";
import { again, find, restoreHighlight, wordUnder } from "./search";
import { options, state } from "./state";
import type { Motion } from "./state";
import { clampNormal, cursor, setCursor, t } from "./util";
import { exitVisual, moveVisual } from "./visual";

type Open = { buf: number; pane: number; kind: string; count: number | null; origin: Position };
let open: null | Open = null;

export function openCmdline(kind: "/" | "?" | ":", count: number | null): void {
  const pane = cmd.focusedPane();
  if (pane === null) return;
  // From the visual mode, : works on its lines.
  let initial = "";
  if (state.visual !== null && kind === ":") {
    exitVisual();
    initial = "'<,'>";
  }
  const o: Open = { buf: -1, pane, kind, count, origin: cursor() };
  open = o;
  const p = openPrompt({
    prompt: kind,
    text: initial,
    onChange: (line) => {
      if (open === o && kind !== ":" && options.incsearch) preview(o, line);
    },
    onLine: (line) => finish(o, line, true),
    onCancel: () => finish(o, "", false),
  });
  if (p === null) {
    open = null;
    return;
  }
  o.buf = p.buffer;
  cmd.switchMode("vim/cmdline");
}

// incsearch: while a search is typed, the text pane's cursor goes to the
// first match from where it began (IncSearch), every match highlighted
// with hlsearch; back there when nothing matches, or on <Esc>.
const INC = "vim-incsearch";

function preview(o: NonNullable<typeof open>, text: string): void {
  const pane = { pane: o.pane };
  t.clearNamespace(INC, pane);
  const back = () => {
    t.setCursor(o.origin, pane);
    restoreHighlight();
  };
  if (text === "") {
    back();
    return;
  }
  let r = null;
  let rust = "";
  try {
    const conv = convert(text, options.ignorecase, options.smartcase);
    rust = conv.rust;
    const backward = o.kind === "?";
    const len = t.line(o.origin.line, pane).length;
    const from = backward || o.origin.character >= len ? o.origin : t.positionOf(o.origin, "grapheme", 1, pane);
    r = t.search(rust, { from, backward, wrap: options.wrapscan, group: conv.group, pane: o.pane });
  } catch {
    // A pattern still being typed.
  }
  if (r === null) {
    back();
    return;
  }
  t.setCursor(r.range.start, pane);
  t.addDecoration(r.range, { style: "IncSearch", namespace: INC, pane: o.pane });
  if (options.hlsearch) cmd.highlightPattern("vim-search", rust, "Search");
}

/** The prompt closed (back in the text pane): run the line (`run`) or
 * not. The cursor is where the command line opened (incsearch moved it). */
function finish(o: Open, line: string, run: boolean): void {
  if (open !== o) return;
  open = null;
  leaveTransient();
  if (o.kind !== ":") {
    t.clearNamespace(INC);
    t.setCursor(o.origin);
  }
  if (!run) {
    if (o.kind !== ":") restoreHighlight();
    state.pending = null;
    return;
  }
  if (o.kind === ":") {
    state.pending = null;
    rememberEx(line);
    ex(line);
    return;
  }
  goTo(find(line, o.kind === "?", o.count ?? 1), o.count);
}

export function cancelCmdline(): void {
  leaveTransient();
  cancelPrompt();
}

/** <BS> on an empty line cancels, as in Vim. */
export function backspace(): void {
  if (open !== null && t.lineCount() === 1 && t.line(0) === "") cancelCmdline();
  else t.erase("grapheme", -1);
}

export function runCmdline(): void {
  leaveTransient();
  acceptPrompt();
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
