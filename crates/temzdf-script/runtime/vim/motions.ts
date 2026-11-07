// Motions: where the cursor goes, and how an operator reads the range
// (exclusive, inclusive, linewise). Built on the framework's motions
// (text.positionOf), which know graphemes, words and lines.
import type { Position, TextUnit } from "temzdf";
import { state } from "./state";
import type { Motion, MotionFn } from "./state";
import {
  charAt,
  cmp,
  firstNonBlank,
  lastChar,
  lastLine,
  lineEnd,
  lineLength,
  nextChar,
  pos,
  prevChar,
  t,
} from "./util";

const n1 = (n: number | null): number => n ?? 1;

/** A vertical motion: the framework moves the cursor (it keeps the column). */
export interface Vertical extends Motion {
  vertical: { unit: TextUnit; count: number };
}

function lines(from: Position, delta: number): Vertical | null {
  const line = Math.max(0, Math.min(lastLine(), from.line + delta));
  if (line === from.line) return null;
  return { to: t.positionOf(from, "line", line - from.line), kind: "linewise", vertical: { unit: "line", count: line - from.line } };
}

/** A word motion forward to just past `n` word ends: Vim's e (the last
 * character). */
function toEnd(from: Position, n: number, unit: TextUnit): Motion | null {
  const start = from.character < lineLength(from.line) ? nextChar(from) : from;
  const end = t.positionOf(start, unit, n);
  const to = end.character > 0 ? prevChar(end) : end;
  return cmp(to, from) === 0 ? null : { to, kind: "inclusive" };
}

/** Back to the last character of the `n`th word end before: Vim's ge. */
function backToEnd(from: Position, n: number, unit: TextUnit): Motion | null {
  const end = t.positionOf(from, unit, -n);
  const to = end.character > 0 ? prevChar(end) : end;
  return cmp(to, from) >= 0 ? null : { to, kind: "inclusive" };
}

function words(from: Position, n: number, unit: TextUnit): Motion | null {
  const to = t.positionOf(from, unit, n);
  return cmp(to, from) === 0 ? null : { to, kind: "exclusive" };
}

/** The `n`th `char` in the line from `from` (f, F, t, T). */
export function findChar(from: Position, kind: "f" | "F" | "t" | "T", char: string, n: number, again = false): Motion | null {
  const line = t.line(from.line);
  const forward = kind === "f" || kind === "t";
  // ; after t starts past the character it stopped before.
  let at = from.character;
  if (again && (kind === "t" || kind === "T")) at += forward ? 1 : -1;
  for (let i = 0; i < n; i++) {
    const next = forward ? line.indexOf(char, at + 1) : line.lastIndexOf(char, at - 1);
    if (next < 0 || (!forward && at - 1 < 0)) return null;
    at = next;
  }
  const found = pos(from.line, at);
  switch (kind) {
    case "f":
      return { to: found, kind: "inclusive" };
    case "t":
      return at - 1 < from.character ? null : { to: prevChar(found), kind: "inclusive" };
    case "F":
      return { to: found, kind: "exclusive" };
    case "T":
      return { to: nextChar(found), kind: "exclusive" };
  }
}

const brackets: Record<string, [string, string]> = {
  "(": ["(", ")"],
  ")": ["(", ")"],
  "[": ["[", "]"],
  "]": ["[", "]"],
  "{": ["{", "}"],
  "}": ["{", "}"],
};

function percent(from: Position, n: number | null): Motion | null {
  if (n !== null) {
    // N%: that percentage of the lines.
    const line = Math.max(0, Math.min(lastLine(), Math.ceil(((lastLine() + 1) * n) / 100) - 1));
    return { to: firstNonBlank(line), kind: "linewise" };
  }
  const text = t.line(from.line);
  let i = from.character;
  while (i < text.length && brackets[text[i]] === undefined) i++;
  if (i >= text.length) return null;
  const [open, close] = brackets[text[i]];
  const to = t.findPair(pos(from.line, i), { open, close });
  return to === null ? null : { to, kind: "inclusive" };
}

function paragraph(from: Position, n: number, forward: boolean): Motion {
  const last = lastLine();
  let line = from.line;
  for (let i = 0; i < n; i++) {
    // Off the blank lines, then to the next one.
    while (forward ? line < last && t.line(line) === "" : line > 0 && t.line(line) === "") line += forward ? 1 : -1;
    while (forward ? line < last && t.line(line) !== "" : line > 0 && t.line(line) !== "") line += forward ? 1 : -1;
  }
  if (forward && line === last && t.line(line) !== "") return { to: lineEnd(line), kind: "exclusive" };
  return { to: pos(line, 0), kind: "exclusive" };
}

function screen(which: "H" | "M" | "L", n: number | null): Motion {
  const r = t.visibleRange();
  const top = r.start.line;
  const bottom = Math.min(lastLine(), r.end.character === 0 && r.end.line > top ? r.end.line - 1 : r.end.line);
  const line =
    which === "H"
      ? Math.min(bottom, top + n1(n) - 1)
      : which === "L"
        ? Math.max(top, bottom - n1(n) + 1)
        : Math.floor((top + bottom) / 2);
  return { to: firstNonBlank(line), kind: "linewise" };
}

/** Motions by key. */
export const motions: Record<string, MotionFn> = {
  h: (from, n) => (from.character === 0 ? null : { to: t.positionOf(from, "grapheme", -Math.min(n1(n), from.character)), kind: "exclusive" }),
  l: (from, n) => {
    const len = lineLength(from.line);
    if (from.character >= len) return null;
    let to = from;
    for (let i = 0; i < n1(n) && to.character < len; i++) to = nextChar(to);
    return { to, kind: "exclusive" };
  },
  j: (from, n) => lines(from, n1(n)),
  k: (from, n) => lines(from, -n1(n)),
  "0": (from) => ({ to: pos(from.line, 0), kind: "exclusive" }),
  "^": (from) => ({ to: firstNonBlank(from.line), kind: "exclusive" }),
  $: (from, n) => {
    const line = Math.min(lastLine(), from.line + n1(n) - 1);
    return { to: lastChar(line), kind: "inclusive" };
  },
  w: (from, n) => words(from, n1(n), "wordStart"),
  W: (from, n) => words(from, n1(n), "bigWordStart"),
  b: (from, n) => words(from, -n1(n), "wordStart"),
  B: (from, n) => words(from, -n1(n), "bigWordStart"),
  e: (from, n) => toEnd(from, n1(n), "wordEnd"),
  E: (from, n) => toEnd(from, n1(n), "bigWordEnd"),
  ge: (from, n) => backToEnd(from, n1(n), "wordEnd"),
  gE: (from, n) => backToEnd(from, n1(n), "bigWordEnd"),
  gg: (_from, n) => ({ to: firstNonBlank(Math.min(lastLine(), n1(n) - 1)), kind: "linewise" }),
  G: (_from, n) => ({ to: firstNonBlank(n === null ? lastLine() : Math.min(lastLine(), n - 1)), kind: "linewise" }),
  _: (from, n) => ({ to: firstNonBlank(Math.min(lastLine(), from.line + n1(n) - 1)), kind: "linewise" }),
  "+": (from, n) => (from.line >= lastLine() ? null : { to: firstNonBlank(Math.min(lastLine(), from.line + n1(n))), kind: "linewise" }),
  "-": (from, n) => (from.line === 0 ? null : { to: firstNonBlank(Math.max(0, from.line - n1(n))), kind: "linewise" }),
  "|": (from, n) => ({ to: pos(from.line, Math.min(lineLength(from.line), n1(n) - 1)), kind: "exclusive" }),
  "%": percent,
  "}": (from, n) => paragraph(from, n1(n), true),
  "{": (from, n) => paragraph(from, n1(n), false),
  H: (_from, n) => screen("H", n),
  M: (_from, n) => screen("M", n),
  L: (_from, n) => screen("L", n),
  ";": (from, n) => (state.lastFind === null ? null : findChar(from, state.lastFind.kind, state.lastFind.char, n1(n), true)),
  ",": (from, n) => {
    const f = state.lastFind;
    if (f === null) return null;
    const flip = { f: "F", F: "f", t: "T", T: "t" } as const;
    return findChar(from, flip[f.kind], f.char, n1(n), true);
  },
};

/** Keys that are the same motions. */
export const aliases: Record<string, string> = {
  "<Left>": "h",
  "<BS>": "h",
  "<Right>": "l",
  "<Space>": "l",
  "<Down>": "j",
  "<Up>": "k",
  "<C-n>": "j",
  "<C-p>": "k",
  "<Home>": "0",
  "<End>": "$",
  "<CR>": "+",
};

export { charAt };
