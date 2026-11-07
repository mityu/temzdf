// The insert mode: one undo step for the whole session, and what was typed
// remembered for `.` (between a mark where it began and the cursor where it
// ends).
import { cmd } from "temzdf";
import { setChanged, setMark } from "./marks";
import { state } from "./state";
import { clampNormal, cmp, cursor, min, prevChar, setCursor, t, text } from "./util";

/** What `.` runs before typing the text again: it positions the cursor (and
 * may delete, as `c` does); it returns how many times the text goes in. */
export type Replay = (count: number | null) => number;

/** Enter the insert mode at the cursor. `groupStarted`: an undo group is
 * open already (c deleted first). `prefix`: put before each repetition
 * (`3o`: a line each). */
export function startInsert(replay: Replay | null, count: number | null, groupStarted = false, prefix = ""): void {
  if (!groupStarted) t.undoBegin();
  const at = cursor();
  const mark = t.addDecoration({ start: at, end: at }, { namespace: "vim-insert" });
  state.insert = { mark, replay: null, count };
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
