// Searching: / ? n N * #, with Vim's patterns converted (pattern.ts), and
// hlsearch through highlightPattern.
import { cmd } from "temzdf";
import type { Position } from "temzdf";
import { convert } from "./pattern";
import { options, state } from "./state";
import { cursor, lastLine, lineLength, nextChar, pos, t, wordRegExp } from "./util";

/** Where the `count`th match of `pattern` is from the cursor (null: none,
 * said in an error). It becomes the last search. */
export function find(pattern: string, backward: boolean, count: number): Position | null {
  if (pattern === "") {
    if (state.lastSearch === null) throw new Error("E35: No previous regular expression");
    pattern = state.lastSearch.pattern;
  }
  state.lastSearch = { pattern, backward };
  const conv = convert(pattern, options.ignorecase, options.smartcase);
  let at = cursor();
  for (let i = 0; i < count; i++) {
    // Forward: from after the cursor (the next line when it is at a
    // line's end).
    let from = at;
    if (!backward) {
      from = at.character < lineLength(at.line) ? nextChar(at) : at.line < lastLine() + 1 ? pos(at.line + 1, 0) : at;
    }
    const r = t.search(conv.rust, { from, backward, wrap: options.wrapscan, group: conv.group });
    if (r === null) {
      highlight(conv.rust);
      throw new Error(`E486: Pattern not found: ${pattern}`);
    }
    at = r.range.start;
  }
  highlight(conv.rust);
  return at;
}

function highlight(rust: string): void {
  if (options.hlsearch) cmd.highlightPattern("vim-search", rust, "Search");
}

export function noHighlight(): void {
  cmd.highlightPattern("vim-search", null);
}

/** n and N: the last search again, the same way or (`reverse`) the other. */
export function again(reverse: boolean, count: number): Position | null {
  const last = state.lastSearch;
  if (last === null) throw new Error("E35: No previous regular expression");
  const backward = reverse ? !last.backward : last.backward;
  const p = find(last.pattern, backward, count);
  state.lastSearch = last;
  return p;
}

/** Vim's pattern for a literal string. */
function escape(s: string): string {
  return s.replace(/[\\/.*$^~[\]]/g, (c) => `\\${c}`);
}

/** * and #: the word under the cursor (or the next on its line), as a
 * whole word. */
export function wordUnder(backward: boolean, count: number): Position | null {
  const line = t.line(cursor().line);
  const w = wordRegExp();
  let i = cursor().character;
  while (i < line.length && !w.test(line[i])) i++;
  if (i >= line.length) throw new Error("E348: No string under cursor");
  let start = i;
  while (start > 0 && w.test(line[start - 1])) start--;
  let end = i;
  while (end < line.length && w.test(line[end])) end++;
  const word = line.slice(start, end);
  // From the word's start, so the match under the cursor is not the first.
  t.setCursor(pos(cursor().line, start));
  return find(`\\<${escape(word)}\\>`, backward, count);
}
