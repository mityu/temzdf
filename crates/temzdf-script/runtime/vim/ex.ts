// The Vim mode's Ex commands, defined with temzdf/command (exCommands):
// :w :q :e :s :d :y :marks, panes, buffers and tab pages, a range alone
// (the command with the empty name), and a few of :set. Ranges are Vim's
// (resolveRange, with this buffer's marks and searches). The commands on
// text are for text panes only; those on panes, buffers and tab pages work
// in any pane (<C-b>: opens a command line there).
import { action, o } from "temzdf";
import type { KeyContext, KeyInfo, Position, Range as TextRange } from "temzdf";
import { command, parseBang, resolveRange, splitBar } from "temzdf/command";
import type { CmdCtx, VimRangeEnv } from "temzdf/command";
import type { Define } from "../lib/keyset";
import { operate } from "./operators";
import { convert } from "./pattern";
import { deleteMarks, listMarks, markLine, recordJump, setMark } from "./marks";
import { find, noHighlight } from "./search";
import { options, state } from "./state";
import { cursor, firstNonBlank, lastLine, lineEnd, pos, t, text } from "./util";

type Range = { first: number; last: number };

/** Where this buffer's ranges find their lines (marks, searches). */
const env: VimRangeEnv = {
  current: () => cursor().line,
  last: () => lastLine(),
  mark: (name) => markLine(name),
  // /pat/ from the line after `from`, ?pat? from the lines before it (an
  // empty pattern, \/ \? \&: the last search's); it becomes the last search.
  search: (pattern, backward, from) => {
    const start = backward ? pos(from, 0) : lineEnd(from);
    return find(pattern ?? "", backward, 1, start)!.line;
  },
};

/** The lines a command's range gives, within the buffer; none: the cursor's. */
function lines(ctx: CmdCtx): Range {
  if (ctx.range === undefined) return { first: cursor().line, last: cursor().line };
  return clampRange(resolveRange(ctx.range, env));
}

function clampRange(r: Range): Range {
  const last = lastLine();
  const first = Math.max(0, Math.min(last, Math.min(r.first, r.last)));
  return { first, last: Math.max(0, Math.min(last, Math.max(r.first, r.last))) };
}

// Panes, buffers and tab pages (Vim's windows, buffers and tabs).

/** Panes on a script's own buffers (unlisted: prompts, the command line). */
function ownPanes(): (p: { buffer: number }) => boolean {
  const own = new Set(
    action
      .buffers({ all: true })
      .filter((b) => b.attrs.includes("unlisted"))
      .map((b) => b.id),
  );
  return (p) => own.has(p.buffer);
}

/** The panes of the current tab page a user works in (not command lines). */
function workPanes(): ReturnType<typeof action.panes> {
  const all = action.panes();
  const tab = all.find((p) => p.focused)?.tab ?? null;
  const own = ownPanes();
  return all.filter((p) => !own(p) && p.tab === tab);
}

/** :split, :vsplit, :new, :vnew: a pane beside, showing `buffer`, or the
 * file `file` (o.splitbelow and o.splitright say where). */
function splitPane(vertical: boolean, buffer: number, file: string): void {
  const side = vertical ? (o.splitright ? "right" : "left") : o.splitbelow ? "bottom" : "top";
  const pane = action.split({ side, buffer });
  if (pane !== null && file !== "") action.openFile(file, { pane });
}

/** The focused pane's buffer (in any kind of pane). */
function focusedBuffer(): number {
  const p = action.panes().find((x) => x.focused);
  if (p === undefined) throw new Error("no pane");
  return p.buffer;
}

/** The buffers :bnext, :ls and such go through. */
function bufferList(): ReturnType<typeof action.buffers> {
  return action.buffers().sort((a, b) => a.id - b.id);
}

/** The buffer `arg` names: its number, or a unique part of its name. */
function bufferNamed(arg: string): number {
  const list = bufferList();
  if (/^\d+$/.test(arg)) {
    const n = Number(arg);
    if (!list.some((b) => b.id === n)) throw new Error(`E86: Buffer ${n} does not exist`);
    return n;
  }
  const hits = list.filter((b) => b.name.includes(arg));
  if (hits.length === 0) throw new Error(`E94: No matching buffer for ${arg}`);
  if (hits.length > 1) throw new Error(`E93: More than one match for ${arg}`);
  return hits[0].id;
}

/** :bnext (`delta` 1) and :bprevious (-1), `count` times. */
function cycleBuffer(delta: number, count: number): void {
  const list = bufferList();
  if (list.length === 0) return;
  const cur = focusedBuffer();
  let i = list.findIndex((b) => b.id === cur);
  if (i < 0) i = 0;
  const n = list.length;
  i = (((i + delta * count) % n) + n) % n;
  action.showBuffer(list[i].id, action.focusedPane() ?? undefined);
}

/** :ls: Vim's list (% the pane's, a shown, h hidden, + modified). */
function listBuffers(): string {
  const cur = focusedBuffer();
  return bufferList()
    .map((b) => {
      const flags = `${b.id === cur ? "%" : " "}${b.panes.length > 0 ? "a" : "h"} ${b.modified ? "+" : " "}`;
      return `${String(b.id).padStart(3)} ${flags} "${b.name}"`;
    })
    .join("\n");
}

/** :bdelete: the buffer goes; its panes show another one (a new empty one
 * when it was the last), as Vim's windows do. */
function deleteBuffer(id: number, force: boolean): void {
  const list = bufferList();
  const b = list.find((x) => x.id === id);
  if (b === undefined) throw new Error(`E516: No buffers were deleted: ${id}`);
  if (b.modified && !force) throw new Error(`E89: No write since last change for buffer ${id} (add ! to override)`);
  if (force) action.setVar("buffer", "forceDelete", true, id);
  const others = list.filter((x) => x.id !== id);
  const alt = others.find((x) => x.panes.length === 0) ?? others[0];
  const instead = alt?.id ?? action.newBuffer();
  for (const pane of b.panes) action.showBuffer(instead, pane);
  action.deleteBuffer(id);
}

/** :wall: every text buffer with unsaved changes. */
function writeAll(): void {
  for (const b of action.buffers()) if (b.kind === "edit" && b.modified) action.write({ buffer: b.id });
}

/** :tabonly: the other tab pages close. */
function onlyTab(): void {
  const tabs = action.tabs();
  for (const tab of [...tabs].reverse()) if (!tab.current) action.closeTab(tab.index);
}

/** Quit the pane; the last one ends the session (refused with unsaved
 * changes unless `force`). */
function quit(force: boolean): void {
  const own = ownPanes();
  const panes = action.panes().filter((p) => !own(p));
  if (panes.length <= 1) action.killSession({ force });
  else action.closePane();
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

type Match = { range: TextRange; groups: (TextRange | null)[] };

/** A match's groups as a replacement reads them: the match first (\zs
 * \ze's group when there is one), then the pattern's own (shifted past
 * the one \zs \ze added). */
function groupsOf(m: Match, group: number): string[] {
  const groupTexts = m.groups.map((g) => (g === null ? "" : text(g)));
  const own = group > 0 ? groupTexts.filter((_, k) => k !== group && k !== 0) : groupTexts.slice(1);
  return [groupTexts[group] ?? "", ...own];
}

/** Where `s` put at `start` ends. */
function endOf(start: Position, s: string): Position {
  const lines = s.split("\n");
  if (lines.length === 1) return pos(start.line, start.character + s.length);
  return pos(start.line + lines.length - 1, lines[lines.length - 1].length);
}

// :s with c: each match is shown (IncSearch) and asked about, one key at a
// time in the transient mode vim/confirm; one undo step in all.
const CONFIRM = "vim-confirm";
let confirming: null | {
  rust: string;
  group: number;
  rep: string;
  global: boolean;
  /** A mark at the range's end (it follows the replacements). */
  end: number;
  at: Position;
  match: Match | null;
  /** The last line replaced on (-1: none). */
  lastChanged: number;
  /** What followed `|` after the flags: run once answered. */
  then: string | undefined;
} = null;

function confirmSubstitute(
  range: Range,
  conv: { rust: string; group: number },
  rep: string,
  global: boolean,
  pattern: string,
  then: string | undefined,
): void {
  const endAt = lineEnd(range.last);
  const first = t.search(conv.rust, {
    from: pos(range.first, 0),
    wrap: false,
    lines: [range.first, range.last],
    group: conv.group,
  });
  if (first === null) throw new Error(`E486: Pattern not found: ${pattern}`);
  t.undoBegin();
  const end = t.addDecoration({ start: endAt, end: endAt }, { namespace: CONFIRM, endGravity: "after" });
  confirming = {
    rust: conv.rust,
    group: conv.group,
    rep,
    global,
    end,
    at: pos(range.first, 0),
    match: null,
    lastChanged: -1,
    then,
  };
  action.switchMode("vim/confirm");
  if (!nextMatch()) finishConfirm();
}

/** Show the next match (false: none left in the range). */
function nextMatch(): boolean {
  const c = confirming!;
  t.clearNamespace(`${CONFIRM}-match`);
  const endPos = t.decoration(c.end)?.range.start ?? c.at;
  if (c.at.line > endPos.line) return false;
  const m = t.search(c.rust, { from: c.at, wrap: false, lines: [c.at.line, endPos.line], group: c.group });
  if (m === null || m.range.start.line > endPos.line) return false;
  c.match = m;
  t.addDecoration(m.range, { style: "IncSearch", namespace: `${CONFIRM}-match` });
  t.setCursor(m.range.start);
  action.message(`replace with ${c.rep} (y/n/a/q/l/^E/^Y)?`);
  return true;
}

/** Past the current match: the next search starts there (or on the next
 * line without g); `replaced`: what it became. */
function advance(replaced: string | null): void {
  const c = confirming!;
  const m = c.match!;
  const end = replaced === null ? m.range.end : endOf(m.range.start, replaced);
  if (!c.global) {
    c.at = pos(end.line + 1, 0);
  } else if (m.range.start.line === m.range.end.line && m.range.start.character === m.range.end.character) {
    // An empty match: one character on.
    c.at = end.character < t.line(end.line).length ? t.positionOf(end, "grapheme", 1) : pos(end.line + 1, 0);
  } else {
    c.at = end;
  }
}

function replaceMatch(): void {
  const c = confirming!;
  const m = c.match!;
  const now = replacement(c.rep, groupsOf(m, c.group));
  t.replace(m.range, now);
  c.lastChanged = endOf(m.range.start, now).line;
  advance(now);
}

function finishConfirm(): void {
  const c = confirming;
  confirming = null;
  if (c === null) return;
  t.clearNamespace(CONFIRM);
  t.clearNamespace(`${CONFIRM}-match`);
  t.undoEnd();
  if (action.currentMode() === "vim/confirm") action.exitMode();
  if (c.lastChanged >= 0) t.setCursor(firstNonBlank(Math.min(lastLine(), c.lastChanged)));
  if (c.then !== undefined) command.execute(c.then);
}

/** A key answering :s///c. */
export function confirmKey(key: KeyInfo): void {
  if (confirming === null) {
    if (action.currentMode() === "vim/confirm") action.exitMode();
    return;
  }
  switch (key.keys) {
    case "y":
      replaceMatch();
      break;
    case "l":
      replaceMatch();
      finishConfirm();
      return;
    case "n":
      advance(null);
      break;
    case "a":
      replaceMatch();
      while (nextMatch()) replaceMatch();
      finishConfirm();
      return;
    case "<C-e>":
    case "<C-y>":
      t.scroll(key.keys === "<C-e>" ? 1 : -1);
      return;
    case "q":
    case "<Esc>":
    case "<C-c>":
      finishConfirm();
      return;
    default:
      return;
  }
  if (!nextMatch()) finishConfirm();
}

/** :s/pat/rep/flags: `|` in the pattern and the replacement is theirs;
 * after the flags it runs what follows (when a :s///c is answered). */
function substitute(range: Range, args: string): void {
  const delim = args[0];
  if (delim === undefined || /[\w\s"|]/.test(delim)) throw new Error("E488: Trailing characters");
  const [pat, rest] = cut(args.slice(1), delim);
  const [rep, flagsRest] = rest === null ? ["", null] : cut(rest, delim);
  const [flagsText, then] = splitBar(flagsRest ?? "");
  const flags = flagsText.trim();
  substituteLines(range, pat, rep, flags, then);
  if (then !== undefined && !flags.includes("c")) command.execute(then);
}

function substituteLines(range: Range, pat: string, rep: string, flags: string, then: string | undefined): void {
  const pattern = pat === "" ? (state.lastSearch?.pattern ?? "") : pat;
  if (pattern === "") throw new Error("E35: No previous regular expression");
  const ic = flags.includes("i") ? true : flags.includes("I") ? false : options.ignorecase;
  const conv = convert(pattern, ic, options.smartcase && !flags.includes("i") && !flags.includes("I"));
  const global = flags.includes("g");
  if (flags.includes("c")) {
    confirmSubstitute(range, conv, rep, global, pattern, then);
    return;
  }
  const found = t.searchAll(conv.rust, {
    range: { start: pos(range.first, 0), end: lineEnd(range.last) },
    group: conv.group,
  });
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
    const text_ = replacement(rep, groupsOf(m, conv.group));
    edits.push({ range: m.range, newText: text_ });
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

/** Whether the focused pane is a text pane (the commands on text). */
const inText = (c: KeyContext) => c.paneKind === "edit";

/** `!` and the argument (trimmed), as :w! FILE has them. */
function bangArg(args: string): [boolean, string] {
  const [bang, rest] = parseBang(args);
  return [bang, rest.trim()];
}

function isModified(buffer: number): boolean {
  return action.buffers({ all: true }).find((b) => b.id === buffer)?.modified ?? false;
}

/** Define the Ex commands with `add` (the Vim mode's keyset's). */
export function exCommands(add: Define["command"]): void {
  const text = { when: inText };
  const range = { when: inText, prefix: "vim-range" } as const;
  const write = (bang: boolean, arg: string) => action.write(arg === "" ? { force: bang } : { path: arg, force: bang });

  // A range alone: to its last line.
  add(
    "",
    (c) => {
      if (c.args.trim() !== "") throw new Error(`E492: Not an editor command: ${c.line.trim()}`);
      if (c.range === undefined) return;
      const to = firstNonBlank(lines(c).last);
      const at = cursor();
      if (to.line !== at.line) recordJump(at);
      t.setCursor(to);
    },
    range,
  );
  add("substitute", (c) => substitute(lines(c), c.args.replace(/^\s+/, "")), {
    ...range,
    shortName: "s",
    bar: "handle",
  });
  add("write", (c) => write(...bangArg(c.args)), { ...text, shortName: "w" });
  add(
    "wq",
    (c) => {
      const [bang, arg] = bangArg(c.args);
      write(bang, arg);
      quit(bang);
    },
    text,
  );
  for (const [name, shortName] of [
    ["xit", "x"],
    ["exit", "exi"],
  ]) {
    add(
      name,
      (c) => {
        const [bang, arg] = bangArg(c.args);
        if (name !== "xit" || t.info().modified) write(bang, arg);
        quit(bang);
      },
      { ...text, shortName },
    );
  }
  add("quit", (c) => quit(bangArg(c.args)[0]), { shortName: "q" });
  add(
    "edit",
    (c) => {
      const [bang, arg] = bangArg(c.args);
      const pane = action.panes().find((p) => p.focused);
      if (arg !== "") {
        action.openFile(arg, { pane: pane?.id });
      } else if (pane?.kind !== "edit") {
        // Reloading is for text panes (the others follow their files).
        throw new Error("E32: No file name");
      } else if (bang || !isModified(pane.buffer)) {
        action.reload();
      } else {
        throw new Error("E37: No write since last change (add ! to override)");
      }
    },
    { shortName: "e" },
  );
  add("split", (c) => splitPane(false, focusedBuffer(), bangArg(c.args)[1]), { shortName: "sp" });
  add("vsplit", (c) => splitPane(true, focusedBuffer(), bangArg(c.args)[1]), { shortName: "vs" });
  add("new", (c) => splitPane(false, action.newBuffer(), bangArg(c.args)[1]));
  add("vnew", (c) => splitPane(true, action.newBuffer(), bangArg(c.args)[1]), { shortName: "vne" });
  add("enew", () => action.showBuffer(action.newBuffer(), action.focusedPane() ?? undefined), { shortName: "ene" });
  add(
    "close",
    () => {
      if (workPanes().length <= 1) throw new Error("E444: Cannot close last window");
      action.closePane();
    },
    { shortName: "clo" },
  );
  add(
    "only",
    () => {
      const me = action.focusedPane();
      for (const p of workPanes()) if (p.id !== me) action.closePane(p.id);
    },
    { shortName: "on" },
  );
  for (const [name, shortName, delta] of [
    ["bnext", "bn", 1],
    ["bprevious", "bp", -1],
    ["bNext", "bN", -1],
  ] as const) {
    add(
      name,
      (c) => {
        const arg = bangArg(c.args)[1];
        cycleBuffer(delta, arg === "" ? 1 : Number(arg) || 1);
      },
      { shortName },
    );
  }
  add(
    "buffer",
    (c) => {
      const arg = bangArg(c.args)[1];
      // :3b: the range is a buffer's number, not a line.
      if (arg === "" && c.range === undefined) return;
      const n = arg !== "" ? arg : String(resolveRange(c.range!, env).last + 1);
      action.showBuffer(bufferNamed(n), action.focusedPane() ?? undefined);
    },
    { shortName: "b", prefix: "vim-range" },
  );
  for (const name of ["ls", "buffers", "files"]) add(name, () => action.message(listBuffers()));
  add(
    "bdelete",
    (c) => {
      const [bang, arg] = bangArg(c.args);
      deleteBuffer(arg === "" ? focusedBuffer() : bufferNamed(arg), bang);
    },
    { shortName: "bd" },
  );
  const tabnew = (c: CmdCtx) => {
    const arg = bangArg(c.args)[1];
    action.newTab(arg === "" ? { buffer: action.newBuffer() } : { file: arg });
  };
  add("tabnew", tabnew);
  add("tabedit", tabnew, { shortName: "tabe" });
  add(
    "tabnext",
    (c) => {
      const arg = bangArg(c.args)[1];
      if (arg === "") action.nextTab();
      else action.gotoTab(Number(arg));
    },
    { shortName: "tabn" },
  );
  add("tabprevious", () => action.prevTab(), { shortName: "tabp" });
  add("tabNext", () => action.prevTab(), { shortName: "tabN" });
  add(
    "tabclose",
    () => {
      if (action.tabs().length <= 1) throw new Error("E784: Cannot close last tab page");
      action.closeTab();
    },
    { shortName: "tabc" },
  );
  add("tabonly", () => onlyTab(), { shortName: "tabo" });
  add("wall", () => writeAll(), { shortName: "wa" });
  const quitAll = (c: CmdCtx) => action.killSession({ force: bangArg(c.args)[0] });
  add("qall", quitAll, { shortName: "qa" });
  add("quitall", quitAll, { shortName: "quita" });
  const writeQuitAll = (c: CmdCtx) => {
    writeAll();
    quitAll(c);
  };
  add("wqall", writeQuitAll, { shortName: "wqa" });
  add("xall", writeQuitAll, { shortName: "xa" });
  add("marks", () => action.message(listMarks()), text);
  add(
    "delmarks",
    (c) => {
      const [bang, arg] = bangArg(c.args);
      if (arg === "" && !bang) throw new Error("E471: Argument required");
      deleteMarks(arg, bang);
    },
    { ...text, shortName: "delm" },
  );
  const mark = (c: CmdCtx) => {
    const arg = bangArg(c.args)[1];
    if (arg.length !== 1) throw new Error("E471: Argument required");
    setMark(arg, pos(lines(c).last, 0));
  };
  add("mark", mark, { ...range, shortName: "ma" });
  add("k", mark, range);
  add("nohlsearch", () => noHighlight(), { ...text, shortName: "noh" });
  for (const [name, op] of [
    ["delete", "d"],
    ["yank", "y"],
  ] as const) {
    add(
      name,
      (c) => {
        const r = lines(c);
        const arg = bangArg(c.args)[1];
        const reg = /^[a-zA-Z0-9"\-_+*]$/.test(arg) ? arg : null;
        operate(op, { kind: "line", first: r.first, last: r.last }, reg);
      },
      { ...range, shortName: op },
    );
  }
  add(
    "set",
    (c) => {
      for (const opt of bangArg(c.args)[1]
        .split(/\s+/)
        .filter((x) => x !== ""))
        setOption(opt);
    },
    { ...text, shortName: "se" },
  );
}

const names: Record<string, keyof typeof options> = {
  ic: "ignorecase",
  ignorecase: "ignorecase",
  scs: "smartcase",
  smartcase: "smartcase",
  hls: "hlsearch",
  hlsearch: "hlsearch",
  is: "incsearch",
  incsearch: "incsearch",
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
