// Ex commands (after :): ranges (% . $ numbers '< '> and +N -N), :w :q :wq
// :x :e :s :noh :d :y, a line number alone, and a few of :set.
import { cmd } from "temzdf";
import { operate } from "./operators";
import { convert } from "./pattern";
import { noHighlight } from "./search";
import { options, state } from "./state";
import { cursor, firstNonBlank, lastLine, lineEnd, pos, t, text } from "./util";

type Range = { first: number; last: number };

/** Parse an address at `s[i]`: its line (zero based) and where it ends. */
function address(s: string, i: number): [number | null, number] {
  let line: number | null = null;
  const cur = cursor().line;
  if (s[i] === ".") {
    line = cur;
    i++;
  } else if (s[i] === "$") {
    line = lastLine();
    i++;
  } else if (s[i] === "'" && (s[i + 1] === "<" || s[i + 1] === ">")) {
    const v = state.lastVisual;
    if (v === null) throw new Error("E20: Mark not set");
    const lines = [v.anchor.line, v.cursor.line];
    line = s[i + 1] === "<" ? Math.min(...lines) : Math.max(...lines);
    i += 2;
  } else {
    const m = /^\d+/.exec(s.slice(i));
    if (m !== null) {
      line = Number(m[0]) - 1;
      i += m[0].length;
    }
  }
  // +N and -N (a sign alone: one).
  for (;;) {
    const m = /^([+-])(\d*)/.exec(s.slice(i));
    if (m === null || m[0] === "") break;
    const n = m[2] === "" ? 1 : Number(m[2]);
    line = (line ?? cur) + (m[1] === "+" ? n : -n);
    i += m[0].length;
  }
  return [line, i];
}

function parseRange(s: string): [Range | null, string] {
  let i = 0;
  while (s[i] === " " || s[i] === ":") i++;
  if (s[i] === "%") return [{ first: 0, last: lastLine() }, s.slice(i + 1)];
  const [a, j] = address(s, i);
  if (a === null) return [null, s.slice(j)];
  if (s[j] === "," || s[j] === ";") {
    const [b, k] = address(s, j + 1);
    return [{ first: a, last: b ?? a }, s.slice(k)];
  }
  return [{ first: a, last: a }, s.slice(j)];
}

function clampRange(r: Range): Range {
  const last = lastLine();
  const first = Math.max(0, Math.min(last, Math.min(r.first, r.last)));
  return { first, last: Math.max(0, Math.min(last, Math.max(r.first, r.last))) };
}

/** Quit the pane; the last one ends the session (refused with unsaved
 * changes unless `force`). */
function quit(force: boolean): void {
  const panes = cmd.panes().filter((p) => p.kind !== "input");
  if (panes.length <= 1) cmd.killSession({ force });
  else cmd.closePane();
}

/** Split `s` (after the delimiter) at the next unescaped `delim`. */
function cut(s: string, delim: string): [string, string | null] {
  let out = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && s[i + 1] === delim) {
      out += delim;
      i++;
    } else if (s[i] === "\\") {
      out += s[i] + (s[i + 1] ?? "");
      i++;
    } else if (s[i] === delim) {
      return [out, s.slice(i + 1)];
    } else {
      out += s[i];
    }
  }
  return [out, null];
}

/** A replacement as Vim builds it: & and \0 the match, \1-\9 groups, \r and
 * \n line breaks, \t, \u \l \U \L \E case. */
function replacement(rep: string, groups: string[]): string {
  let out = "";
  let once: "u" | "l" | null = null;
  let all: "U" | "L" | null = null;
  const add = (s: string) => {
    for (const c of s) {
      let ch = all === "U" ? c.toUpperCase() : all === "L" ? c.toLowerCase() : c;
      if (once !== null) {
        ch = once === "u" ? ch.toUpperCase() : ch.toLowerCase();
        once = null;
      }
      out += ch;
    }
  };
  for (let i = 0; i < rep.length; i++) {
    const c = rep[i];
    if (c === "&") {
      add(groups[0] ?? "");
      continue;
    }
    if (c !== "\\") {
      add(c);
      continue;
    }
    const n = rep[++i];
    if (n === undefined) break;
    if (/[0-9]/.test(n)) add(groups[Number(n)] ?? "");
    else if (n === "r" || n === "n") out += "\n";
    else if (n === "t") out += "\t";
    else if (n === "u" || n === "l") once = n;
    else if (n === "U" || n === "L") all = n;
    else if (n === "E" || n === "e") all = null;
    else add(n);
  }
  return out;
}

function substitute(range: Range, args: string): void {
  const delim = args[0];
  if (delim === undefined || /[\w\s"|]/.test(delim)) throw new Error("E488: Trailing characters");
  const [pat, rest] = cut(args.slice(1), delim);
  const [rep, flagsRest] = rest === null ? ["", null] : cut(rest, delim);
  const flags = flagsRest ?? "";
  const pattern = pat === "" ? (state.lastSearch?.pattern ?? "") : pat;
  if (pattern === "") throw new Error("E35: No previous regular expression");
  const ic = flags.includes("i") ? true : flags.includes("I") ? false : options.ignorecase;
  const conv = convert(pattern, ic, options.smartcase && !flags.includes("i") && !flags.includes("I"));
  const found = t.searchAll(conv.rust, { range: { start: pos(range.first, 0), end: lineEnd(range.last) }, group: conv.group });
  const global = flags.includes("g");
  const seen = new Set<number>();
  const edits = [];
  // The last substitution's line, once the line breaks before it (put in or
  // taken out) count.
  let lastLineChanged = -1;
  let shift = 0;
  for (const m of found) {
    const line = m.range.start.line;
    if (!global && seen.has(line)) continue;
    seen.add(line);
    // Groups: the match first (\zs \ze's group when there is one), then
    // the pattern's own (shifted past the one \zs \ze added).
    const groupTexts = m.groups.map((g) => (g === null ? "" : text(g)));
    const own = conv.group > 0 ? groupTexts.filter((_, k) => k !== conv.group && k !== 0) : groupTexts.slice(1);
    const groups = [groupTexts[conv.group] ?? "", ...own];
    const text_ = replacement(rep, groups);
    edits.push({ range: m.range, text: text_ });
    const breaks = (text_.match(/\n/g) ?? []).length;
    lastLineChanged = line + shift + breaks;
    shift += breaks - (m.range.end.line - m.range.start.line);
  }
  if (edits.length === 0) {
    if (flags.includes("e")) return;
    throw new Error(`E486: Pattern not found: ${pattern}`);
  }
  t.edit(edits);
  t.setCursor(firstNonBlank(Math.min(lastLine(), lastLineChanged)));
}

/** Run an Ex command line (what was typed after `:`). */
export function ex(input: string): void {
  const [range, rest] = parseRange(input);
  const m = /^\s*([a-zA-Z]*|&)(!?)\s*(.*)$/.exec(rest)!;
  const name = m[1];
  const bang = m[2] === "!";
  const arg = m[3].trim();
  const is = (short: string, full: string) => name.length >= short.length && full.startsWith(name);
  if (name === "" && arg === "") {
    if (range !== null) t.setCursor(firstNonBlank(clampRange(range).last));
    return;
  }
  if (name === "" || is("s", "substitute")) {
    const r = clampRange(range ?? { first: cursor().line, last: cursor().line });
    substitute(r, name === "" ? arg : m[3]);
    return;
  }
  if (is("w", "write")) {
    cmd.write(arg === "" ? { force: bang } : { path: arg, force: bang });
    return;
  }
  if (is("wq", "wq") || is("x", "xit") || is("exi", "exit")) {
    if (!is("x", "xit") || t.info().modified) cmd.write(arg === "" ? { force: bang } : { path: arg, force: bang });
    quit(bang);
    return;
  }
  if (is("q", "quit")) {
    quit(bang);
    return;
  }
  if (is("e", "edit")) {
    if (arg !== "") {
      cmd.openFile(arg, { pane: cmd.focusedPane() ?? undefined });
    } else if (bang || !t.info().modified) {
      cmd.reload();
    } else {
      throw new Error("E37: No write since last change (add ! to override)");
    }
    return;
  }
  if (is("noh", "nohlsearch")) {
    noHighlight();
    return;
  }
  if (is("d", "delete") || is("y", "yank")) {
    const r = clampRange(range ?? { first: cursor().line, last: cursor().line });
    const reg = /^[a-zA-Z0-9"\-_+*]$/.test(arg) ? arg : null;
    operate(is("d", "delete") ? "d" : "y", { kind: "line", first: r.first, last: r.last }, reg);
    return;
  }
  if (is("se", "set")) {
    for (const opt of arg.split(/\s+/).filter((s) => s !== "")) setOption(opt);
    return;
  }
  throw new Error(`E492: Not an editor command: ${input}`);
}

const names: Record<string, keyof typeof options> = {
  ic: "ignorecase",
  ignorecase: "ignorecase",
  scs: "smartcase",
  smartcase: "smartcase",
  hls: "hlsearch",
  hlsearch: "hlsearch",
  ws: "wrapscan",
  wrapscan: "wrapscan",
  et: "expandtab",
  expandtab: "expandtab",
  sw: "shiftwidth",
  shiftwidth: "shiftwidth",
};

function setOption(opt: string): void {
  const m = /^(no)?(\w+)(?:=(\d+))?$/.exec(opt);
  const key = m === null ? undefined : names[m[2]];
  if (m === null || key === undefined) throw new Error(`E518: Unknown option: ${opt}`);
  if (key === "shiftwidth") {
    if (m[3] === undefined) throw new Error(`E521: Number required after =: ${opt}`);
    options.shiftwidth = Number(m[3]);
    return;
  }
  (options as Record<string, unknown>)[key] = m[1] === undefined;
  if (key === "hlsearch" && m[1] !== undefined) noHighlight();
}
