// Marks and the jump list (doc/design/vim-mode.md). A mark is a mark of the
// framework (an empty decoration without a style: it follows edits and is
// flagged deleted when its text goes); the buffer variable `vim.marks`
// names them, so they outlive a restarted script host and go with their
// buffer:
//   a-z     the buffer's own
//   A-Z     one in the session (kept in the buffer it was set in)
//   ' `     where the last jump began
//   [ ]     the start and end of the last change or yank
//   .       the last change
//   ^       where the insert mode was left last
//   < >     the last visual selection (state.lastVisual)
// The jump list is per pane (the pane variable `vim.jumps`): what G, gg,
// searches, marks and such left, <C-o> back and <C-i> forward.
import { cmd } from "temzdf";
import type { Position } from "temzdf";
import { state } from "./state";
import { clampNormal, cmp, cursor, lastLine, pos, setCursor, t } from "./util";

const NS = "vim-marks";
const MARKS = "vim.marks";
const JUMPS = "vim.jumps";
const MAX_JUMPS = 100;

type Marks = Record<string, number>;
interface Loc {
  buffer: number;
  id: number;
}
interface Jumps {
  list: Loc[];
  /** Where <C-o> and <C-i> are in the list; its length: past the end. */
  index: number;
}

function buffer(): number {
  return t.info().buffer;
}

function marksOf(buf?: number): Marks {
  const v = cmd.getVar("buffer", MARKS, buf);
  return v !== null && typeof v === "object" ? (v as Marks) : {};
}

/** Drop mark `id` of the shown buffer (gone already: nothing). */
function forget(id: number): void {
  try {
    t.deleteDecoration(id);
  } catch {
    // Not there.
  }
}

function newMark(p: Position): number {
  return t.addDecoration({ start: p, end: p }, { namespace: NS });
}

/** Where mark `id` of the shown buffer is (null: gone, or its text deleted). */
function where(id: number): Position | null {
  const d = t.decoration(id);
  return d === null || d.deleted ? null : d.range.start;
}

/** Set mark `name` at `p` (the cursor). */
export function setMark(name: string, p: Position = cursor()): void {
  if (!isMarkName(name) || name === "<" || name === ">") throw new Error("E191: Argument must be a letter or forward/backward quote");
  const key = name === "`" ? "'" : name;
  const buf = buffer();
  if (/^[A-Z]$/.test(key)) {
    // One in the session: out of the buffer that had it.
    for (const b of cmd.buffers()) {
      if (b.id === buf) continue;
      const m = marksOf(b.id);
      if (key in m) {
        delete m[key];
        cmd.setVar("buffer", MARKS, m, b.id);
      }
    }
  }
  const m = marksOf(buf);
  if (key in m) forget(m[key]);
  m[key] = newMark(p);
  cmd.setVar("buffer", MARKS, m, buf);
}

/** The marks the editing commands set: the last change's ([ ] .) or a
 * yank's ([ ]). */
export function setChanged(start: Position, end: Position, changed: boolean): void {
  setMark("[", start);
  setMark("]", end);
  if (changed) setMark(".", start);
}

/** Whether `c` names a mark. */
export function isMarkName(c: string): boolean {
  return /^[a-zA-Z'`[\].^<>]$/.test(c);
}

/** Mark `name`: its buffer and position. A mark of another buffer is read
 * by showing that buffer first (`show`); without it, null. */
export function markPos(name: string, show = false): Position | null {
  if (name === "<" || name === ">") {
    const v = state.lastVisual;
    if (v === null) return null;
    const [a, b] = cmp(v.anchor, v.cursor) <= 0 ? [v.anchor, v.cursor] : [v.cursor, v.anchor];
    if (v.kind === "line") return name === "<" ? pos(a.line, 0) : pos(b.line, Math.max(0, t.line(b.line).length - 1));
    return name === "<" ? a : b;
  }
  const key = name === "`" ? "'" : name;
  const here = marksOf();
  if (key in here) return where(here[key]);
  if (!/^[A-Z]$/.test(key)) return null;
  for (const b of cmd.buffers()) {
    const m = marksOf(b.id);
    if (!(key in m)) continue;
    if (!show) return null;
    // A jump to another buffer: the jump list keeps where it left.
    recordJump(cursor());
    cmd.showBuffer(b.id, cmd.focusedPane() ?? undefined);
    return where(m[key]);
  }
  return null;
}

/** The line of mark `name` in this buffer (Ex ranges), or E20. */
export function markLine(name: string): number {
  const p = markPos(name);
  if (p === null) throw new Error("E20: Mark not set");
  return Math.min(p.line, lastLine());
}

/** :delm {marks} (a-z, ranges such as a-d) and :delm! (every a-z). */
export function deleteMarks(arg: string, all: boolean): void {
  const m = marksOf();
  const names: string[] = [];
  if (all) names.push(...Object.keys(m).filter((k) => /^[a-z]$/.test(k)));
  for (const r of arg.replace(/\s+/g, "").matchAll(/([a-zA-Z0-9])-([a-zA-Z0-9])|./g)) {
    if (r[1] !== undefined) {
      for (let c = r[1].charCodeAt(0); c <= r[2].charCodeAt(0); c++) names.push(String.fromCharCode(c));
    } else {
      names.push(r[0]);
    }
  }
  for (const n of names) {
    const key = n === "`" ? "'" : n;
    if (key in m) {
      forget(m[key]);
      delete m[key];
    } else if (/^[A-Z]$/.test(key)) {
      for (const b of cmd.buffers()) {
        const other = marksOf(b.id);
        if (key in other) {
          delete other[key];
          cmd.setVar("buffer", MARKS, other, b.id);
        }
      }
    }
  }
  cmd.setVar("buffer", MARKS, m, buffer());
}

/** :marks: the marks set, as Vim lists them. */
export function listMarks(): string {
  const m = marksOf();
  const rows = ["mark line  col text"];
  const add = (name: string, p: Position | null, file = "") => {
    if (p === null) return;
    const text = file === "" ? t.line(p.line).trim() : file;
    rows.push(` ${name} ${String(p.line + 1).padStart(6)} ${String(p.character).padStart(4)} ${text}`);
  };
  const order = "'abcdefghijklmnopqrstuvwxyz";
  for (const k of order) if (k in m) add(k, where(m[k]));
  const here = buffer();
  for (const b of cmd.buffers()) {
    const other = marksOf(b.id);
    for (const k of Object.keys(other).filter((x) => /^[A-Z]$/.test(x)).sort()) {
      if (b.id === here) add(k, where(other[k]));
      else rows.push(` ${k}      -    - ${b.name}`);
    }
  }
  for (const k of "[].^") if (k in m) add(k, where(m[k]));
  for (const k of "<>") add(k, markPos(k));
  return rows.join("\n");
}

// The jump list.

function jumpsOf(): Jumps {
  const v = cmd.getVar("pane", JUMPS);
  if (v !== null && typeof v === "object" && Array.isArray((v as Jumps).list)) return v as Jumps;
  return { list: [], index: 0 };
}

function saveJumps(j: Jumps): void {
  cmd.setVar("pane", JUMPS, j);
}

/** Put position `p` of this buffer at the end of the jump list (others on
 * its line go), and make it the ' mark. */
function push(j: Jumps, p: Position): void {
  const buf = buffer();
  j.list = j.list.filter((e) => {
    if (e.buffer !== buf) return true;
    const at = where(e.id);
    if (at !== null && at.line !== p.line) return true;
    forget(e.id);
    return false;
  });
  j.list.push({ buffer: buf, id: newMark(p) });
  while (j.list.length > MAX_JUMPS) {
    const e = j.list.shift()!;
    if (e.buffer === buf) forget(e.id);
  }
  j.index = j.list.length;
}

/** A jump leaves `from` (the cursor before it): into the jump list, and
 * the ' mark. */
export function recordJump(from: Position): void {
  setMark("'", from);
  const j = jumpsOf();
  push(j, from);
  saveJumps(j);
}

/** <C-o> (`delta` < 0) and <C-i>: through the jump list, `count` entries;
 * entries whose text went are skipped. */
export function jump(delta: number): void {
  const j = jumpsOf();
  let i = j.index;
  if (delta < 0 && i >= j.list.length) {
    // Leaving the end: where the cursor is now comes back with <C-i>.
    push(j, cursor());
    i = j.list.length - 1;
  }
  const step = delta < 0 ? -1 : 1;
  let left = Math.abs(delta);
  while (left > 0) {
    i += step;
    if (i < 0 || i >= j.list.length) {
      saveJumps(j);
      return;
    }
    const e = j.list[i];
    if (e.buffer !== buffer()) {
      if (!cmd.buffers().some((b) => b.id === e.buffer)) continue;
      cmd.showBuffer(e.buffer, cmd.focusedPane() ?? undefined);
    }
    const p = where(e.id);
    if (p === null) continue;
    left--;
    if (left === 0) {
      j.index = i;
      saveJumps(j);
      setCursor(clampNormal(p));
      return;
    }
  }
}
