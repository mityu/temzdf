// The command line (/ ? :): a prompt (openPrompt) one row high below the
// text pane, in the mode vim/cmdline over the input mode (its editing keys
// work); <CR> runs the cursor's line in the text pane, <Esc> cancels.
import { acceptPrompt, action, cancelPrompt, openPrompt } from "temzdf";
import type { Position } from "temzdf";
import { command } from "temzdf/command";
import { rememberEx } from "./macros";
import { recordJump } from "./marks";
import { combine, leaveTransient, operator } from "./normal";
import { convert } from "./pattern";
import { again, find, restoreHighlight, wordUnder } from "./search";
import { options, state } from "./state";
import type { Motion } from "./state";
import { clampNormal, cursor, setCursor, t } from "./util";
import { exitVisual, moveVisual } from "./visual";

type Open = { buf: number; pane: number; kind: string; count: number | null; origin: Position };
let open: null | Open = null;

export function openCmdline(kind: "/" | "?" | ":", count: number | null): void {
  const pane = action.focusedPane();
  if (pane === undefined) return;
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
  if (p === undefined) {
    open = null;
    return;
  }
  o.buf = p.buffer;
  action.switchMode("vim/cmdline");
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
  let r: ReturnType<typeof t.search>;
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
  if (r === undefined) {
    back();
    return;
  }
  t.setCursor(r.range.start, pane);
  t.addDecoration(r.range, { style: "IncSearch", namespace: INC, pane: o.pane });
  if (options.hlsearch) action.highlightPattern("vim-search", rust, "Search");
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
    command.execute(line);
    return;
  }
  const backward = o.kind === "?";
  searchMotion(o.count, (n) => find(line, backward, n));
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

/** A search as a motion: `search` finds where (with the count), and the
 * cursor moves there, or a pending operator acts up to it (exclusive, its
 * count times the search's; `.` searches the same way again), or the
 * visual selection's end goes. A pending operator is used up before the
 * search, so one that fails (an error) cancels it, as in Vim. */
function searchMotion(count: number | null, search: (count: number) => Position | null): void {
  const p = state.pending;
  state.pending = null;
  if (p !== null) {
    leaveTransient();
    const total = combine(p.count, count);
    const to = search(total ?? 1);
    if (to === null) return;
    const again = (c: number | null): Motion | null => {
      const r = search(c ?? total ?? 1);
      return r === null ? null : { to: r, kind: "exclusive" };
    };
    operator(p.op, { to, kind: "exclusive" }, p.register, again, total);
    return;
  }
  const to = search(count ?? 1);
  if (to === null) return;
  const m: Motion = { to, kind: "exclusive" };
  const at = cursor();
  if (to.line !== at.line || to.character !== at.character) recordJump(at);
  if (state.visual !== null) {
    moveVisual(m);
    return;
  }
  setCursor(clampNormal(to));
}

/** n and N: the last search again, the same way or the other; `.` after
 * an operator searches the way this one did. */
export function searchAgain(reverse: boolean, count: number | null): void {
  const last = state.lastSearch;
  searchMotion(count, (n) => {
    if (last === null) return again(reverse, n);
    const r = find(last.pattern, reverse ? !last.backward : last.backward, n);
    state.lastSearch = last;
    return r;
  });
}

export function searchWord(backward: boolean, count: number | null): void {
  searchMotion(count, (n) => wordUnder(backward, n));
}
