// Positions and the text, through the framework's primitives
// (doc/design/text-editor.md). Vim's cursor is *on* a character: here, the
// character after the framework's cursor (which sits between characters).
import { cmd } from "temzdf";
import type { Position, Range } from "temzdf";

export const t = cmd.text;

export const pos = (line: number, character: number): Position => ({ line, character });

export function cmp(a: Position, b: Position): number {
  return a.line - b.line || a.character - b.character;
}

export const min = (a: Position, b: Position): Position => (cmp(a, b) <= 0 ? a : b);
export const max = (a: Position, b: Position): Position => (cmp(a, b) <= 0 ? b : a);

export function lineCount(): number {
  return t.lineCount();
}

/** The text has a final line break: its last "line" is the empty one after it. */
export function lastLine(): number {
  const n = t.lineCount();
  return n > 1 && t.line(n - 1) === "" ? n - 2 : n - 1;
}

export function lineLength(line: number): number {
  return t.line(line).length;
}

/** The start of the last character of `line` (0 when it is empty). */
export function lastChar(line: number): Position {
  const len = lineLength(line);
  if (len === 0) return pos(line, 0);
  return t.positionOf(pos(line, len), "grapheme", -1);
}

/** The position after the character at `p` (within its line). */
export function nextChar(p: Position): Position {
  if (p.character >= lineLength(p.line)) return p;
  return t.positionOf(p, "grapheme", 1);
}

/** The position of the character before `p` (within its line). */
export function prevChar(p: Position): Position {
  if (p.character === 0) return p;
  return t.positionOf(p, "grapheme", -1);
}

export function firstNonBlank(line: number): Position {
  return t.positionOf(pos(line, 0), "firstNonBlank");
}

/** In the normal mode the cursor stays on a character (an empty line has
 * none). */
export function clampNormal(p: Position): Position {
  const line = Math.min(p.line, lastLine());
  const len = lineLength(line);
  if (len === 0) return pos(line, 0);
  if (p.character >= len) return lastChar(line);
  return pos(line, p.character);
}

export function cursor(): Position {
  return t.cursor();
}

export function setCursor(p: Position): void {
  t.setCursor(p);
}

export function text(r: Range): string {
  return t.getText(r);
}

/** The character at `p`, if any (within its line). */
export function charAt(p: Position): string {
  const line = t.line(p.line);
  return p.character < line.length ? line.slice(p.character, nextChar(p).character) : "";
}

/** Whether `c` is a word character of the buffer. */
export function isWordChar(c: string, wordchars: RegExp): boolean {
  return c !== "" && wordchars.test(c);
}

/** The buffer's word characters as a JavaScript regex (one character). */
export function wordRegExp(): RegExp {
  // Rust classes of the default kind (\p{L}, \p{N}) read the same in
  // JavaScript with the u flag.
  try {
    return new RegExp(`^${t.wordchars()}$`, "u");
  } catch {
    return /^[\p{L}\p{N}_]$/u;
  }
}

/** The end of the line `p` is on, after its last character. */
export function lineEnd(line: number): Position {
  return pos(line, lineLength(line));
}

export function repeat(s: string, n: number): string {
  return n <= 1 ? s : s.repeat(n);
}
