// The normal mode's commands, and how motions run: moving the cursor, or
// the range of a pending operator, or the visual selection's end.
import { cmd, dispatch } from "temzdf";
import type { KeyInfo, Position } from "temzdf";
import { endInsert, startInsert } from "./insert";
import { recordJump } from "./marks";
import { motions } from "./motions";
import { operate, put, span } from "./operators";
import type { Span } from "./operators";
import * as registers from "./registers";
import { state } from "./state";
import type { Motion, MotionFn, Operator } from "./state";
import { textObject } from "./textobjects";
import { block, draw, exitVisual, moveVisual, selection } from "./visual";
import {
  charAt,
  clampNormal,
  cursor,
  firstNonBlank,
  lastLine,
  lineEnd,
  lineLength,
  nextChar,
  pos,
  setCursor,
  t,
} from "./util";

/** The transient modes the Vim mode pushes (left after their key). */
const TRANSIENT = new Set(["vim/pending", "vim/char", "vim/register", "vim/cmdline"]);

/** Leave the transient modes back to the mode below them. */
export function leaveTransient(): void {
  for (let i = 0; i < 4 && TRANSIENT.has(cmd.currentMode()); i++) cmd.exitMode();
}

/** Two counts given around an operator multiply; none given: null. */
function combine(a: number | null, b: number | null): number | null {
  return a === null && b === null ? null : (a ?? 1) * (b ?? 1);
}

/** The register the command uses, used up. */
function takeRegister(): string | null {
  const r = state.register;
  state.register = null;
  return r;
}

/** The normal mode's cursor: on a character, kept there by vertical moves'
 * column. */
export function clampKeep(): void {
  const c = cursor();
  const k = clampNormal(c);
  if (k.line !== c.line || k.character !== c.character) t.setCursor(k, { keepColumn: true });
}

/** Run an operator over a motion's range, remembering it for `.`. */
export function operator(op: Operator, m: Motion | null, register: string | null, again: (count: number | null) => Motion | null, count: number | null): void {
  if (m === null) return;
  const at = cursor();
  const s = span(m, at);
  runOperator(op, s, register, (c) => {
    const m2 = again(c ?? count);
    return m2 === null ? null : span(m2, cursor());
  });
}

/** Do `op` on `s`; `.` does it again on what `respan` gives then (with
 * its count, if given). */
function runOperator(op: Operator, s: Span, register: string | null, respan: (count: number | null) => Span | null): void {
  if (op === "c") {
    t.undoBegin();
    operate("c", s, register);
    startInsert(
      (count) => {
        const s2 = respan(count);
        if (s2 !== null) operate("c", s2, null);
        return 1;
      },
      null,
      true,
    );
    return;
  }
  operate(op, s, register);
  if (op !== "y") {
    state.lastChange = (count) => {
      const s2 = respan(count);
      if (s2 !== null) operate(op, s2, null);
    };
  }
}

/** The motions that are jumps (Vim's :help jump-motions; searches in
 * cmdline.ts). */
const JUMPS = new Set(["G", "gg", "%", "(", ")", "{", "}", "H", "M", "L", "'", "`"]);

/** A motion key: move, or finish a pending operator, or move the visual
 * selection's end. */
export function runMotion(key: string, fn: MotionFn, count: number | null): void {
  const p = state.pending;
  if (p !== null) {
    state.pending = null;
    leaveTransient();
    const total = combine(p.count, count);
    let f = fn;
    // cw is ce (on a non-blank): the blanks after the word stay.
    if (p.op === "c" && (key === "w" || key === "W") && !/^\s?$/.test(charAt(cursor()))) f = motions[key === "w" ? "e" : "E"];
    // With an operator, w stops at the end of the line its last word is on
    // (dw on a line's last word leaves the line break).
    else if (key === "w" || key === "W") f = wordToLineEnd(fn);
    const m = f(cursor(), total);
    if (m === null) {
      // A motion that fails stops a macro (Vim's beep).
      cmd.beep();
      return;
    }
    operator(p.op, m, p.register, (c) => f(cursor(), c ?? total), total);
    return;
  }
  const before = cursor();
  const buf = JUMPS.has(key) ? t.info().buffer : null;
  const m = fn(before, count);
  if (m === null) {
    cmd.beep();
    return;
  }
  // A jump (G, gg, %, a mark, ...) within the buffer goes into the jump
  // list (one to another buffer did, as it left).
  if (buf !== null && t.info().buffer === buf && (m.to.line !== before.line || m.to.character !== before.character)) {
    recordJump(before);
  }
  if (state.visual !== null) {
    moveVisual(m, key);
    return;
  }
  if ("vertical" in m) {
    const v = (m as Motion & { vertical: { unit: "line"; count: number } }).vertical;
    t.move(v.unit, v.count);
    clampKeep();
  } else {
    setCursor(clampNormal(m.to));
  }
}

/** w and W after an operator: a word motion that crossed into a later
 * line ends at the end of the line before (Vim's special case). */
function wordToLineEnd(fn: MotionFn): MotionFn {
  return (from, count) => {
    const m = fn(from, count);
    if (m === null || m.to.line <= from.line) return m;
    if (m.to.character > firstNonBlank(m.to.line).character) return m;
    return { to: lineEnd(m.to.line - 1), kind: "exclusive" };
  };
}

/** A text object key (after an operator, or in the visual mode). */
export function runTextObject(key: string, count: number | null): void {
  const p = state.pending;
  if (p !== null) {
    state.pending = null;
    leaveTransient();
    const total = combine(p.count, count);
    operator(p.op, textObject(key, cursor(), total), p.register, (c) => textObject(key, cursor(), c ?? total), total);
    return;
  }
  const v = state.visual;
  if (v === null) return;
  const m = textObject(key, cursor(), count);
  if (m === null) return;
  if (m.kind === "linewise" && v.kind === "char") v.kind = "line";
  v.anchor = m.from ?? cursor();
  t.setCursor(m.kind === "exclusive" && m.to.character > 0 ? t.positionOf(m.to, "grapheme", -1) : m.to);
  draw();
}

/** An operator key: in the normal mode it waits for a motion; in the
 * visual mode it acts on the selection. */
export function startOperator(op: Operator, count: number | null): void {
  if (state.visual !== null) {
    visualOperator(op);
    return;
  }
  state.pending = { op, count, register: takeRegister() };
  cmd.switchMode("vim/pending");
}

/** The operator's key again (dd, cc, yy, >>, g~~): the cursor's line and
 * `count - 1` more. */
export function lineOperator(op: Operator, count: number | null): void {
  const p = state.pending;
  if (p === null || p.op !== op) {
    cancelPending();
    return;
  }
  state.pending = null;
  leaveTransient();
  const total = combine(p.count, count) ?? 1;
  const lines = (n: number): Span => ({ kind: "line", first: cursor().line, last: Math.min(lastLine(), cursor().line + n - 1) });
  runOperator(op, lines(total), p.register, (c) => lines(c ?? total));
}

export function cancelPending(): void {
  state.pending = null;
  leaveTransient();
}

function visualOperator(op: Operator): void {
  const v = state.visual!;
  const register = takeRegister();
  if (v.kind === "block") {
    blockOperator(op, register);
    return;
  }
  const s = selection();
  exitVisual();
  // `.` acts on as many lines (or characters on one line) from the cursor.
  const size: { lines: number } | { chars: number } | null =
    s.kind === "line"
      ? { lines: s.last - s.first + 1 }
      : s.start.line === s.end.line
        ? { chars: s.end.character - s.start.character }
        : null;
  runOperator(op, s, register, () => {
    if (size === null) return null;
    // (A count after a visual change is not Vim's size multiplier here.)
    const c = cursor();
    if ("lines" in size) return { kind: "line", first: c.line, last: Math.min(lastLine(), c.line + size.lines - 1) };
    return { kind: "char", start: c, end: pos(c.line, Math.min(lineLength(c.line), c.character + size.chars)) };
  });
}

/** d, y, c (on the first line only) and case on a block selection. */
function blockOperator(op: Operator, register: string | null): void {
  const b = block();
  exitVisual();
  const pieces: string[] = [];
  const edits = [];
  for (let l = b.first; l <= b.last; l++) {
    const line = t.line(l);
    const end = Math.min(line.length, b.c1 + 1);
    const piece = b.c0 < line.length ? line.slice(b.c0, end) : "";
    pieces.push(piece);
    if (piece === "") continue;
    const range = { start: pos(l, b.c0), end: pos(l, end) };
    if (op === "d" || op === "c") edits.push({ range, text: "" });
    else if (op === "gu") edits.push({ range, text: piece.toLowerCase() });
    else if (op === "gU") edits.push({ range, text: piece.toUpperCase() });
    else if (op === "g~") edits.push({ range, text: [...piece].map((c) => (c === c.toUpperCase() ? c.toLowerCase() : c.toUpperCase())).join("") });
  }
  if (op === "y" || op === "d" || op === "c") registers.store(register, { text: pieces.join("\n"), kind: "block" }, op === "y" ? "yank" : "delete");
  if (op === "c") t.undoBegin();
  if (edits.length > 0) t.edit(edits);
  if (op === "c") {
    // What is typed on the first line goes into the lines that had text in
    // the block.
    const lines = [];
    for (let l = b.first; l <= b.last; l++) if (pieces[l - b.first] !== "") lines.push(l);
    setCursor(pos(b.first, Math.min(b.c0, lineLength(b.first))));
    startInsert(null, null, true, "", { first: b.first, last: b.last, col: b.c0, pad: false, lines });
    return;
  }
  setCursor(clampNormal(pos(b.first, b.c0)));
}

/** I and A in the visual mode: on a block, typed on its first line and put
 * into the others at <Esc> (A pads short lines; after $ each line's end);
 * otherwise at the selection's start or after its end. */
export function visualInsert(where: "I" | "A"): void {
  const v = state.visual;
  if (v === null) return;
  if (v.kind !== "block") {
    const s = selection();
    exitVisual();
    if (where === "I") {
      setCursor(s.kind === "char" ? s.start : firstNonBlank(s.first));
      insert("i", null);
    } else {
      setCursor(s.kind === "char" ? t.positionOf(s.end, "grapheme", -1) : lineEnd(s.last));
      insert(s.kind === "char" ? "a" : "A", null);
    }
    return;
  }
  const b = block();
  exitVisual();
  const lines = [];
  // I: the lines that reach the block (A pads the others).
  for (let l = b.first; l <= b.last; l++) if (where === "A" || lineLength(l) > b.c0) lines.push(l);
  t.undoBegin();
  if (where === "I") {
    setCursor(pos(b.first, Math.min(b.c0, lineLength(b.first))));
    startInsert(null, null, true, "", { first: b.first, last: b.last, col: b.c0, pad: false, lines });
    return;
  }
  if (b.toEnd) {
    setCursor(lineEnd(b.first));
    startInsert(null, null, true, "", { first: b.first, last: b.last, col: "end", pad: false, lines });
    return;
  }
  const col = b.c1 + 1;
  const len = lineLength(b.first);
  if (len < col) t.insert(" ".repeat(col - len), lineEnd(b.first));
  setCursor(pos(b.first, col));
  startInsert(null, null, true, "", { first: b.first, last: b.last, col, pad: true, lines });
}

/** r{char} in the visual mode: every selected character becomes `char`. */
export function visualReplace(key: KeyInfo): void {
  const v = state.visual;
  const ch = key.text;
  if (v === null) return;
  if (ch === null || ch === "" || key.keys === "<Esc>" || key.keys === "<CR>") {
    exitVisual();
    return;
  }
  const edits = [];
  let start: Position;
  if (v.kind === "block") {
    const b = block();
    start = pos(b.first, b.c0);
    for (let l = b.first; l <= b.last; l++) {
      const line = t.line(l);
      if (b.c0 >= line.length) continue;
      const end = Math.min(line.length, b.c1 + 1);
      const piece = line.slice(b.c0, end);
      edits.push({ range: { start: pos(l, b.c0), end: pos(l, end) }, text: ch.repeat([...piece].length) });
    }
  } else {
    const s = selection();
    const r = s.kind === "char" ? { start: s.start, end: s.end } : { start: pos(s.first, 0), end: lineEnd(s.last) };
    start = r.start;
    const old = t.getText(r);
    edits.push({ range: r, text: [...old].map((c) => (c === "\n" ? c : ch)).join("") });
  }
  exitVisual();
  if (edits.length > 0) t.edit(edits);
  setCursor(clampNormal(start));
}

// The normal mode's own commands.

export function insert(where: "i" | "a" | "I" | "A" | "o" | "O", count: number | null): void {
  const place = (): number => {
    const c = cursor();
    switch (where) {
      case "a":
        if (lineLength(c.line) > 0) setCursor(nextChar(c));
        break;
      case "I":
        setCursor(firstNonBlank(c.line));
        break;
      case "A":
        setCursor(lineEnd(c.line));
        break;
      case "o":
        t.insert("\n", lineEnd(c.line));
        setCursor(pos(c.line + 1, 0));
        break;
      case "O":
        t.insert("\n", pos(c.line, 0));
        setCursor(pos(c.line, 0));
        break;
    }
    return 1;
  };
  t.undoBegin();
  place();
  startInsert((n) => {
    place();
    return n ?? count ?? 1;
  }, count, true, where === "o" || where === "O" ? "\n" : "");
}

/** x X D C s S Y: shorthands of operators. */
export function shorthand(key: "x" | "X" | "D" | "C" | "s" | "S" | "Y", count: number | null): void {
  const register = takeRegister();
  const line = (n: number | null): Span => ({ kind: "line", first: cursor().line, last: Math.min(lastLine(), cursor().line + (n ?? 1) - 1) });
  switch (key) {
    case "x":
      if (lineLength(cursor().line) === 0) return;
      operator("d", motions.l(cursor(), count), register, (c) => motions.l(cursor(), c ?? count), count);
      return;
    case "X":
      operator("d", motions.h(cursor(), count), register, (c) => motions.h(cursor(), c ?? count), count);
      return;
    case "D":
      operator("d", motions.$(cursor(), count), register, (c) => motions.$(cursor(), c ?? count), count);
      return;
    case "C":
      if (lineLength(cursor().line) === 0) {
        t.undoBegin();
        startInsert(() => 1, null, true);
        return;
      }
      operator("c", motions.$(cursor(), count), register, (c) => motions.$(cursor(), c ?? count), count);
      return;
    case "s":
      if (lineLength(cursor().line) === 0) {
        t.undoBegin();
        startInsert(() => 1, null, true);
        return;
      }
      operator("c", motions.l(cursor(), count), register, (c) => motions.l(cursor(), c ?? count), count);
      return;
    case "S":
      runOperator("c", line(count), register, (c) => line(c ?? count));
      return;
    case "Y":
      operate("y", line(count), register);
      return;
  }
}

export function putRegister(after: boolean, count: number | null): void {
  const name = takeRegister();
  if (registers.isClipboard(name)) {
    // The client's clipboard comes later: put it then, as one change.
    dispatch(async (ctx) => {
      const text = await ctx.cmd.readClipboard({ primary: name === "*" });
      ctx.atomic(() => {
        if (text === "") throw new Error(`E353: Nothing in register ${name}`);
        putWith(registers.fromClipboard(text), after, count);
      });
    });
    return;
  }
  const reg = registers.get(name);
  if (reg === undefined) throw new Error(`E353: Nothing in register ${name ?? '"'}`);
  putWith(reg, after, count);
}

function putWith(reg: registers.Register, after: boolean, count: number | null): void {
  if (state.visual !== null) {
    // Replace the selection.
    const s = state.visual.kind === "block" ? null : selection();
    exitVisual();
    if (s === null) return;
    t.undoBegin();
    operate("d", s, null);
    // Where the selection was: before what follows it, or after the line's
    // (or the text's) end when nothing does.
    const after = s.kind === "line" ? s.first > lastLine() : s.start.character >= lineLength(s.start.line);
    if (s.kind === "char") setCursor(after ? clampNormal(s.start) : s.start);
    put(reg, after, count ?? 1);
    t.undoEnd();
    return;
  }
  put(reg, after, count ?? 1);
  state.lastChange = (c) => put(reg, after, c ?? count ?? 1);
}

/** r{char}: replace `count` characters. */
export function replaceChars(key: KeyInfo, count: number | null): void {
  const ch = key.keys === "<CR>" ? "\n" : key.text;
  if (ch === null || ch === "" || key.keys === "<Esc>") return;
  const run = (n: number) => {
    const c = cursor();
    let end: Position = c;
    for (let i = 0; i < n; i++) {
      if (end.character >= lineLength(c.line)) return;
      end = nextChar(end);
    }
    t.replace({ start: c, end }, ch === "\n" ? "\n" : ch.repeat(n));
    setCursor(ch === "\n" ? pos(c.line + 1, 0) : clampNormal(t.positionOf(c, "grapheme", n - 1)));
  };
  run(count ?? 1);
  state.lastChange = (c) => run(c ?? count ?? 1);
}

/** J: join `count` lines (at least two) with a space between. */
export function join(count: number | null): void {
  const run = (n: number) => {
    const first = cursor().line;
    const times = Math.max(1, n - 1);
    let at: Position = cursor();
    for (let i = 0; i < times && first < lastLine(); i++) {
      const cur = t.line(first);
      const next = t.line(first + 1);
      const rest = next.replace(/^\s+/, "");
      const space = cur === "" || /\s$/.test(cur) || rest === "" || rest.startsWith(")") ? "" : " ";
      t.replace({ start: pos(first, cur.length), end: pos(first + 1, next.length - rest.length) }, space);
      at = pos(first, cur.length);
    }
    setCursor(clampNormal(at));
  };
  run(count ?? 2);
  state.lastChange = (c) => run(c ?? count ?? 2);
}

/** ~: switch case of `count` characters and move on. */
export function tilde(count: number | null): void {
  const run = (n: number) => {
    const c = cursor();
    const len = lineLength(c.line);
    if (len === 0) return;
    let end: Position = c;
    for (let i = 0; i < n && end.character < len; i++) end = nextChar(end);
    const s = t.getText({ start: c, end });
    t.replace({ start: c, end }, [...s].map((x) => (x === x.toUpperCase() ? x.toLowerCase() : x.toUpperCase())).join(""));
    setCursor(clampNormal(end));
  };
  run(count ?? 1);
  state.lastChange = (c) => run(c ?? count ?? 1);
}

export function undo(redo: boolean, count: number | null): void {
  for (let i = 0; i < (count ?? 1); i++) {
    const ranges = redo ? t.redo() : t.undo();
    if (ranges.length === 0) break;
    setCursor(clampNormal(ranges[0].start));
  }
  clampKeep();
}

export function repeatLast(count: number | null): void {
  state.lastChange?.(count);
  clampKeep();
}

/** <C-d> <C-u> <C-f> <C-b> <C-e> <C-y>: scroll, the cursor along. */
export function scroll(how: "half" | "page" | "line", down: boolean, count: number | null): void {
  const r = t.visibleRange();
  const rows = Math.max(1, r.end.line - r.start.line);
  const n = how === "half" ? (count ?? Math.max(1, Math.floor(rows / 2))) : how === "page" ? (count ?? 1) : (count ?? 1);
  if (how === "page") {
    t.move("page", down ? n : -n);
  } else if (how === "half") {
    t.scroll(down ? n : -n);
    t.move("displayLine", down ? n : -n);
  } else {
    t.scroll(down ? n : -n);
  }
  clampKeep();
}

export function selectRegister(key: KeyInfo): void {
  leaveTransient();
  if (key.text !== null && registers.isName(key.text)) state.register = key.text;
}

export { endInsert };
