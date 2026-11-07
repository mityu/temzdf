// The insert mode: one undo step for the whole session, and what was typed
// remembered for `.` (between a mark where it began and the cursor where it
// ends).
import { cmd, dispatch } from "temzdf";
import type { KeyInfo } from "temzdf";
import * as registers from "./registers";
import { setChanged, setMark } from "./marks";
import { state } from "./state";
import type { BlockInsert } from "./state";
import type { Position } from "temzdf";
import { clampNormal, cmp, cursor, lastLine, min, pos, prevChar, setCursor, t, text } from "./util";

/** What `.` runs before typing the text again: it positions the cursor (and
 * may delete, as `c` does); it returns how many times the text goes in. */
export type Replay = (count: number | null) => number;

/** Enter the insert mode at the cursor. `groupStarted`: an undo group is
 * open already (c deleted first). `prefix`: put before each repetition
 * (`3o`: a line each). */
export function startInsert(replay: Replay | null, count: number | null, groupStarted = false, prefix = "", block?: BlockInsert): void {
  if (!groupStarted) t.undoBegin();
  const at = cursor();
  const mark = t.addDecoration({ start: at, end: at }, { namespace: "vim-insert" });
  state.insert = { mark, replay: null, count, block };
  insertReplay = replay;
  insertPrefix = prefix;
  insertPane = cmd.focusedPane();
  cmd.switchMode("vim/insert");
}

let insertReplay: Replay | null = null;
let insertPrefix = "";
let insertPane: number | null = null;

/** Leave the insert mode (<Esc>): repeat the text for a count, remember it
 * for `.`, close the undo step, and the cursor goes back one. */
export function endInsert(): void {
  const ins = state.insert;
  state.insert = null;
  let typed = "";
  if (ins !== null) {
    const d = t.decoration(ins.mark);
    const c = cursor();
    if (d !== null && cmp(d.range.start, c) < 0) typed = text({ start: d.range.start, end: c });
    // ^ where it was left; [ ] and . what was typed.
    setMark("^", c);
    if (d !== null) setChanged(min(d.range.start, c), c, true);
    t.clearNamespace("vim-insert");
    if (ins.block !== undefined && d !== null) {
      endBlockInsert(ins.block, typed, d.range.start);
      return;
    }
    if (typed !== "" && ins.count !== null && ins.count > 1) t.insert((insertPrefix + typed).repeat(ins.count - 1));
    const replay = insertReplay;
    const prefix = insertPrefix;
    if (replay !== null) {
      state.lastChange = (count) => {
        t.undoBegin();
        const times = replay(count);
        if (typed !== "") {
          t.insert(typed + (prefix + typed).repeat(Math.max(0, times - 1)));
        }
        t.undoEnd();
        leaveCursor();
      };
    }
  }
  t.undoEnd();
  cmd.exitMode();
  leaveCursor();
}

/** Put what a block's I, A or c typed on its first line into the other
 * lines (not when it broke the line), as one step with it. */
function blockEdits(b: BlockInsert, typed: string): { range: { start: Position; end: Position }; text: string }[] {
  const edits = [];
  if (typed === "" || typed.includes("\n")) return [];
  for (const l of b.lines) {
    if (l === b.first) continue;
    const len = t.line(l).length;
    let at: Position;
    let text = typed;
    if (b.col === "end") {
      at = pos(l, len);
    } else if (len < b.col) {
      if (!b.pad) continue;
      at = pos(l, len);
      text = " ".repeat(b.col - len) + typed;
    } else {
      at = pos(l, b.col);
    }
    edits.push({ range: { start: at, end: at }, text });
  }
  return edits;
}

function endBlockInsert(b: BlockInsert, typed: string, start: Position): void {
  const edits = blockEdits(b, typed);
  if (edits.length > 0) t.edit(edits);
  t.undoEnd();
  cmd.exitMode();
  // On the block's first line, where the text went in.
  setCursor(clampNormal(start));
  const n = b.last - b.first;
  const col = b.col;
  state.lastChange = () => {
    // Again from the cursor, on as many lines.
    const c = cursor();
    const at = col === "end" ? "end" : c.character + (b.pad ? 1 : 0);
    const lines = [];
    for (let l = c.line; l <= Math.min(lastLine(), c.line + n); l++) lines.push(l);
    const again: BlockInsert = { first: -1, last: c.line + n, col: at, pad: b.pad, lines };
    const es = blockEdits(again, typed);
    if (es.length > 0) t.edit(es);
    setCursor(clampNormal(c));
  };
}

/** <C-r>{reg} (read in vim/insreg): the register's text goes in as typed,
 * so `.` and a count repeat it too; lines with their line break. */
export function insertRegister(key: KeyInfo): void {
  cmd.exitMode();
  const name = key.text;
  if (name === null || !registers.isName(name)) return;
  if (registers.isClipboard(name)) {
    dispatch(async (ctx) => {
      const text = await ctx.cmd.readClipboard({ primary: name === "*" });
      ctx.atomic(() => {
        if (state.insert !== null) t.type(text);
      });
    });
    return;
  }
  const r = registers.get(name);
  if (r !== undefined) t.type(r.text);
}

/** As leaving the insert mode: back one character, kept on the line. */
function leaveCursor(): void {
  const c = cursor();
  setCursor(clampNormal(c.character > 0 ? prevChar(c) : c));
}

/** The insert mode was left another way (the focus moved): close what is
 * open. */
export function abandonInsert(): void {
  if (state.insert === null) return;
  state.insert = null;
  const pane = insertPane;
  try {
    if (pane !== null) {
      t.clearNamespace("vim-insert", { pane });
      t.undoEnd({ pane });
    }
  } catch {
    // The pane is gone.
  }
}
