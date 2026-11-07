// Operators on the range a motion or text object covers: delete, change,
// yank, shift, case.
import type { Position, Range } from "temzdf";
import { setChanged } from "./marks";
import * as registers from "./registers";
import { options, state } from "./state";
import type { Motion, Operator } from "./state";
import {
  clampNormal,
  cmp,
  cursor,
  firstNonBlank,
  lastLine,
  lineCount,
  lineEnd,
  lineLength,
  max,
  min,
  nextChar,
  pos,
  setCursor,
  t,
  text,
} from "./util";

/** A range to operate on: characters (end excluded) or whole lines. */
export type Span = { kind: "char"; start: Position; end: Position } | { kind: "line"; first: number; last: number };

/** The span `m` covers from `at` (Vim's rules for exclusive motions that
 * end at a line's start included). */
export function span(m: Motion, at: Position): Span {
  const a0 = m.from ?? at;
  if (m.kind === "linewise") {
    return { kind: "line", first: Math.min(a0.line, m.to.line), last: Math.min(lastLine(), Math.max(a0.line, m.to.line)) };
  }
  let start = min(a0, m.to);
  let end = max(a0, m.to);
  if (m.kind === "inclusive") {
    // The character at the end; on an empty line (or past the end) its
    // line break.
    end = end.character >= lineLength(end.line) ? (end.line < lineCount() - 1 ? pos(end.line + 1, 0) : end) : nextChar(end);
  } else if (m.from === undefined && end.character === 0 && end.line > start.line) {
    // An exclusive motion ending at a line's start ends at the previous
    // line's end; from at or before the first non-blank it is linewise.
    if (start.character <= firstNonBlank(start.line).character) {
      return { kind: "line", first: start.line, last: end.line - 1 };
    }
    end = lineEnd(end.line - 1);
  }
  start = min(start, end);
  return { kind: "char", start, end };
}

/** The range of a span, for the framework: a line span with its breaks. */
function range(s: Span): Range {
  if (s.kind === "char") return { start: s.start, end: s.end };
  if (s.last + 1 < lineCount()) return { start: pos(s.first, 0), end: pos(s.last + 1, 0) };
  // The last line without a final line break: the break before goes.
  if (s.first > 0) return { start: lineEnd(s.first - 1), end: lineEnd(s.last) };
  return { start: pos(0, 0), end: lineEnd(s.last) };
}

/** What a span holds, as a register keeps it. */
export function take(s: Span): registers.Register {
  if (s.kind === "char") return { text: text({ start: s.start, end: s.end }), kind: "char" };
  const lines: string[] = [];
  for (let l = s.first; l <= s.last; l++) lines.push(t.line(l));
  return { text: `${lines.join("\n")}\n`, kind: "line" };
}

function indent(line: string, out: boolean): string {
  if (out) {
    if (line === "") return line;
    return (options.expandtab ? " ".repeat(options.shiftwidth) : "\t") + line;
  }
  if (line.startsWith("\t")) return line.slice(1);
  let n = 0;
  while (n < options.shiftwidth && line[n] === " ") n++;
  return line.slice(n);
}

function changeCase(s: string, how: "g~" | "gu" | "gU"): string {
  if (how === "gu") return s.toLowerCase();
  if (how === "gU") return s.toUpperCase();
  return [...s].map((c) => (c === c.toUpperCase() ? c.toLowerCase() : c.toUpperCase())).join("");
}

/** Do `op` on `s`; the register named for it (`"x`) or the default one.
 * Returns whether the insert mode should follow (c). */
export function operate(op: Operator, s: Span, register: string | null): boolean {
  const changes = op !== "y" && op !== "c";
  const first = s.kind === "char" ? s.start : pos(s.first, 0);
  const last = s.kind === "char" ? (cmp(s.end, s.start) > 0 ? t.positionOf(s.end, "grapheme", -1) : s.end) : pos(s.last, Math.max(0, lineLength(s.last) - 1));
  const c = operate1(op, s, register);
  // The [ ] marks: what was yanked, or where the change was (its text,
  // or where it was taken out); c sets them when its insert ends.
  if (op === "d") {
    const p = clampNormal(first);
    setChanged(p, p, true);
  } else if (op !== "c") {
    setChanged(first, last, changes);
  }
  return c;
}

function operate1(op: Operator, s: Span, register: string | null): boolean {
  const at = cursor();
  switch (op) {
    case "y": {
      registers.store(register, take(s), "yank");
      if (s.kind === "char") setCursor(clampNormal(s.start));
      else if (s.first < at.line) setCursor(clampNormal(pos(s.first, at.character)));
      return false;
    }
    case "d": {
      registers.store(register, take(s), "delete");
      t.delete(range(s));
      if (s.kind === "char") setCursor(clampNormal(s.start));
      else setCursor(firstNonBlank(Math.min(s.first, lastLine())));
      return false;
    }
    case "c": {
      registers.store(register, take(s), "delete");
      if (s.kind === "char") {
        t.delete({ start: s.start, end: s.end });
        setCursor(s.start);
      } else {
        // The lines become one empty line.
        t.replace({ start: pos(s.first, 0), end: lineEnd(s.last) }, "");
        setCursor(pos(s.first, 0));
      }
      return true;
    }
    case ">":
    case "<": {
      const first = s.kind === "line" ? s.first : s.start.line;
      const last = s.kind === "line" ? s.last : s.end.line;
      const edits = [];
      for (let l = first; l <= last; l++) {
        const old = t.line(l);
        const now = indent(old, op === ">");
        if (now !== old) edits.push({ range: { start: pos(l, 0), end: lineEnd(l) }, text: now });
      }
      if (edits.length > 0) t.edit(edits);
      setCursor(firstNonBlank(first));
      return false;
    }
    default: {
      const r = range(s);
      const old = text(r);
      const now = changeCase(old, op);
      if (now !== old) t.replace(r, now);
      setCursor(clampNormal(s.kind === "char" ? s.start : pos(s.first, 0)));
      return false;
    }
  }
}

/** Put a register's text: after the cursor (`after`), `count` times; the
 * cursor ends as Vim's does. */
export function put(reg: registers.Register, after: boolean, count: number): void {
  const at = cursor();
  const body = reg.text.repeat(count);
  if (reg.kind === "line") {
    const lines = body.endsWith("\n") ? body : `${body}\n`;
    const line = after ? at.line + 1 : at.line;
    if (after && at.line === lineCount() - 1) {
      // After a last line without a final line break.
      t.insert(`\n${lines.slice(0, -1)}`, lineEnd(at.line));
    } else {
      t.insert(lines, pos(line, 0));
    }
    setCursor(firstNonBlank(line));
    const n = (lines.match(/\n/g) ?? []).length;
    setChanged(pos(line, 0), pos(line + n - 1, 0), true);
    return;
  }
  const where = after && lineLength(at.line) > 0 ? nextChar(at) : at;
  t.insert(body, where);
  {
    const end = t.positionOf(where, "grapheme", [...body].length);
    setChanged(where, end.character > 0 ? t.positionOf(end, "grapheme", -1) : end, true);
  }
  // On the last character put (a multi-line put: on its start).
  if (body.includes("\n")) {
    setCursor(where);
  } else {
    const end = t.positionOf(where, "grapheme", [...body].length);
    setCursor(clampNormal(t.positionOf(end, "grapheme", -1)));
  }
}

export { cmp };
