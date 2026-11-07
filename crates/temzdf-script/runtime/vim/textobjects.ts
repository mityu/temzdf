// Text objects (iw, aw, iW, aW, quotes, brackets): ranges around the
// cursor, as motions with a start of their own.
import type { Position } from "temzdf";
import type { Motion } from "./state";
import { firstNonBlank, lineLength, nextChar, pos, t, wordRegExp } from "./util";

type Class = "blank" | "word" | "punct";

function classOf(c: string, word: RegExp, big: boolean): Class {
  if (c === "" || /\s/.test(c)) return "blank";
  if (big) return "word";
  return word.test(c) ? "word" : "punct";
}

/** iw, aw, iW, aW: the run of one class under the cursor (`count` runs);
 * `around` adds the blanks after it (or before, when none follow). */
function word(from: Position, count: number, around: boolean, big: boolean): Motion | null {
  const line = t.line(from.line);
  if (line.length === 0) return null;
  const w = wordRegExp();
  const chars = [...line];
  // Columns in UTF-16 units of each code point.
  const cols: number[] = [];
  let c = 0;
  for (const ch of chars) {
    cols.push(c);
    c += ch.length;
  }
  cols.push(c);
  let i = cols.findIndex((x) => x >= from.character);
  if (i < 0 || i >= chars.length) i = chars.length - 1;
  const cls = (k: number) => classOf(chars[k], w, big);
  let start = i;
  while (start > 0 && cls(start - 1) === cls(i)) start--;
  let end = i;
  for (let n = 0; n < count; n++) {
    const k = cls(end);
    while (end + 1 < chars.length && cls(end + 1) === k) end++;
    if (n + 1 < count && end + 1 < chars.length) end++;
  }
  if (around) {
    if (cls(i) !== "blank" && end + 1 < chars.length && cls(end + 1) === "blank") {
      while (end + 1 < chars.length && cls(end + 1) === "blank") end++;
    } else if (cls(i) !== "blank") {
      while (start > 0 && cls(start - 1) === "blank") start--;
    } else {
      // On blanks: they and the word after.
      if (end + 1 < chars.length) {
        end++;
        const k = cls(end);
        while (end + 1 < chars.length && cls(end + 1) === k) end++;
      }
    }
  }
  return { from: pos(from.line, cols[start]), to: pos(from.line, cols[end]), kind: "inclusive" };
}

/** i" a" (and ' `): the quoted string the cursor is in or before. */
function quote(from: Position, q: string, around: boolean): Motion | null {
  const line = t.line(from.line);
  const at: number[] = [];
  for (let i = 0; i < line.length; i++) {
    if (line[i] === q && (i === 0 || line[i - 1] !== "\\")) at.push(i);
  }
  let open = -1;
  let close = -1;
  for (let k = 0; k + 1 < at.length; k += 2) {
    if (from.character <= at[k + 1]) {
      open = at[k];
      close = at[k + 1];
      break;
    }
  }
  if (open < 0) return null;
  if (!around) {
    if (close === open + 1) return { from: pos(from.line, close), to: pos(from.line, close), kind: "exclusive" };
    return { from: pos(from.line, open + 1), to: pos(from.line, close - 1), kind: "inclusive" };
  }
  let end = close;
  while (end + 1 < line.length && (line[end + 1] === " " || line[end + 1] === "\t")) end++;
  let start = open;
  if (end === close) while (start > 0 && (line[start - 1] === " " || line[start - 1] === "\t")) start--;
  return { from: pos(from.line, start), to: pos(from.line, end), kind: "inclusive" };
}

/** i( a( and the like: the pair around the cursor (`count` levels out). */
function pair(from: Position, open: string, close: string, around: boolean, count: number): Motion | null {
  const here = t.line(from.line)[from.character];
  let o: Position | null = here === open ? from : t.findPair(from, { open, close, direction: "backward" });
  for (let i = 1; i < count && o !== null; i++) o = t.findPair(o, { open, close, direction: "backward" });
  if (o === null) return null;
  const c = t.findPair(o, { open, close });
  if (c === null) return null;
  if (around) return { from: o, to: c, kind: "inclusive" };
  const start = nextChar(o);
  // Multi-line, the pair on lines of their own: the lines between.
  if (start.character >= lineLength(o.line) && c.line > o.line + 1 && firstNonBlank(c.line).character === c.character) {
    return { from: pos(o.line + 1, 0), to: pos(c.line - 1, 0), kind: "linewise" };
  }
  if (start.line === c.line && start.character === c.character) return { from: start, to: start, kind: "exclusive" };
  return { from: start, to: c, kind: "exclusive" };
}

const pairs: Record<string, [string, string]> = {
  "(": ["(", ")"],
  ")": ["(", ")"],
  b: ["(", ")"],
  "{": ["{", "}"],
  "}": ["{", "}"],
  B: ["{", "}"],
  "[": ["[", "]"],
  "]": ["[", "]"],
  "<": ["<", ">"],
  ">": ["<", ">"],
};

/** Text objects by their keys (after i or a). */
export function textObject(key: string, from: Position, count: number | null): Motion | null {
  const around = key[0] === "a";
  const what = key.slice(1);
  const n = count ?? 1;
  if (what === "w") return word(from, n, around, false);
  if (what === "W") return word(from, n, around, true);
  if (what === '"' || what === "'" || what === "`") return quote(from, what, around);
  const p = pairs[what];
  return p === undefined ? null : pair(from, p[0], p[1], around, n);
}

export const textObjectKeys: string[] = ["w", "W", '"', "'", "`", ...Object.keys(pairs)].flatMap((k) => [`i${k}`, `a${k}`]);
