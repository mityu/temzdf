// Completing Ex commands in the command line (a prompt marked with
// CMDLINE_VAR ":"; doc/design/completion.md, "The command line"): the
// command's name where it is typed (after a range, after `|`), its
// arguments by the command's `complete` option (command.add). Commands and
// their `when` are looked up for the pane the command line runs in (the
// prompt's origin), as command.execute will.
import { action, promptOrigin } from "temzdf";
import type { KeyContext, Position } from "temzdf";
import { command, parsePrefix, splitBar } from "temzdf/command";
import type { CmdCandidate, CmdComplete, CmdCompletion, CmdSpec } from "temzdf/command";
import type { Detail, Item, Matcher, PartOptions } from "temzdf/ff";
import { fuzzy, prefix, substring } from "temzdf/ff/builtin";
import type { CompletionContext, CompletionItem, CompletionSource } from "./index";

type Match = NonNullable<CmdCompletion["match"]>;

/** Each candidate says how it is matched (its context's way). */
export interface ExDetail {
  readonly match: Match;
}

/** What the source keeps of a context: the candidates found with it, and
 * a key telling contexts apart beyond their anchor. */
export interface ExContext {
  readonly key: string;
  readonly items: readonly CompletionItem<ExDetail>[];
}

/** What is completed where the cursor is, found in the text before it. */
interface Found {
  /** Where the completed text starts in the line. */
  readonly start: number;
  readonly key: string;
  readonly match: Match;
  readonly items: () => readonly CmdCandidate[] | readonly CompletionItem<ExDetail>[];
}

/** The pane the command line runs in, and its context for `when`. */
function originContext(pane: number): { pane: number | undefined; ctx: KeyContext } {
  const buffer = action.panes().find((p) => p.id === pane)?.buffer;
  const origin = promptOrigin(buffer);
  const info = origin === undefined ? undefined : action.panes().find((p) => p.id === origin);
  return {
    pane: origin,
    ctx: {
      pane: origin ?? -1,
      paneKind: info?.kind,
      floating: info?.floating ?? false,
      mode: action.currentMode(),
      keys: "",
    },
  };
}

/** The commands there are for `pane` (`ranged`: those taking a range),
 * each name once, sorted. */
function commandNames(pane: number | undefined, ranged: boolean): CompletionItem<ExDetail>[] {
  const seen = new Map<string, CmdSpec>();
  for (const c of command.list({ live: true, pane })) {
    if (c.name === "" || (ranged && c.prefix === undefined)) continue;
    seen.set(c.name, c);
  }
  return [...seen.values()]
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
    .map((c) => ({
      id: c.name,
      value: c.name,
      detail: { match: "prefix" },
      kind: "command",
      ...(c.desc === undefined ? {} : { description: c.desc }),
    }));
}

/** Where the last blank not after a backslash ends in `s`. */
function wordStart(s: string): number {
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\") i++;
    else if (s[i] === " " || s[i] === "\t") start = i + 1;
  }
  return start;
}

/** The shipped completers (`complete: "buffer"` and `"command"`). */
function builtin(name: "buffer" | "command", pane: number | undefined): Exclude<CmdComplete, string> {
  if (name === "buffer") {
    return () => ({
      start: 0,
      match: "substring",
      items: action
        .buffers()
        .filter((b) => b.name !== "")
        .sort((a, b) => a.id - b.id)
        .map((b) => ({ value: b.name, kind: "buffer" })),
    });
  }
  return () => ({ match: "prefix", items: commandNames(pane, false) });
}

/** What completes at the end of `before` (the line up to the cursor). */
function find(before: string, pane: number | undefined, ctx: KeyContext): Found | undefined {
  let pos = 0;
  for (;;) {
    pos += /^[\s:]*/.exec(before.slice(pos))![0].length;
    const rest = before.slice(pos);
    const [range, afterRange] = parsePrefix(rest);
    const nameStart = pos + rest.length - afterRange.length;
    const name = /^[A-Za-z]*/.exec(afterRange)![0];
    const nameEnd = nameStart + name.length;
    if (nameEnd === before.length) {
      const ranged = range !== undefined;
      return { start: nameStart, key: `name ${ranged}`, match: "prefix", items: () => commandNames(pane, ranged) };
    }
    const cmd = command.find(name, { pane });
    if (cmd === undefined) return undefined;
    const args = before.slice(nameEnd);
    if (cmd.bar === "auto") {
      const [head, tail] = splitBar(args);
      if (tail !== undefined) {
        pos = nameEnd + head.length + 1;
        continue;
      }
    }
    return argument(cmd, args, nameEnd, pane, ctx);
  }
}

/** What completes in `cmd`'s arguments `args` (starting at `at`). */
function argument(
  cmd: CmdSpec,
  args: string,
  at: number,
  pane: number | undefined,
  ctx: KeyContext,
): Found | undefined {
  if (cmd.complete === undefined) return undefined;
  const bang = args.startsWith("!");
  let i = bang ? 1 : 0;
  const blanks = /^\s*/.exec(args.slice(i))![0].length;
  // Arguments start after a blank.
  if (blanks === 0) return undefined;
  i += blanks;
  const text = args.slice(i);
  const completer = typeof cmd.complete === "string" ? builtin(cmd.complete, pane) : cmd.complete;
  const r = completer({ args: text, name: cmd.name, bang, context: ctx });
  if (r === undefined) return undefined;
  const start = Math.max(0, Math.min(text.length, r.start ?? wordStart(text)));
  const match = r.match ?? "prefix";
  return { start: at + i + start, key: `arg ${cmd.name} ${match}`, match, items: () => r.items };
}

function items(found: Found): CompletionItem<ExDetail>[] {
  return found.items().map((c, i): CompletionItem<ExDetail> => {
    if (typeof c === "string") return { id: i, value: c, detail: { match: found.match } };
    if ("detail" in c) return c as CompletionItem<ExDetail>;
    const { value, kind, description } = c as Exclude<CmdCandidate, string>;
    return {
      id: i,
      value,
      detail: { match: found.match },
      ...(kind === undefined ? {} : { kind }),
      ...(description === undefined ? {} : { description }),
    };
  });
}

const same = (a: Position, b: Position) => a.line === b.line && a.character === b.character;

/** The source completing Ex commands in the command line (the focused
 * pane's prompt): command names and their arguments. */
export function exCommandSource(): CompletionSource<ExContext, ExDetail> {
  return {
    findContext(prev: CompletionContext<ExContext> | undefined) {
      const pane = action.focusedPane();
      if (pane === undefined) return undefined;
      const cur = action.text.cursor({ pane });
      const before = action.text.line(cur.line, { pane }).slice(0, cur.character);
      const origin = originContext(pane);
      const found = find(before, origin.pane, origin.ctx);
      if (found === undefined) return undefined;
      const anchor = { line: cur.line, character: found.start };
      if (prev !== undefined && prev.detail.key === found.key && same(prev.anchor, anchor)) return prev;
      return { anchor, detail: { key: found.key, items: items(found) } };
    },
    async *source({ detail }) {
      yield* detail.items;
    },
  };
}

/** A matcher matching each candidate its own way (`detail.match`): prefix
 * respecting case, substring and fuzzy ignoring it unless the text typed
 * has an upper-case letter. */
export function byMatch(): Matcher<Detail> {
  const matchers: Record<Match, Matcher<Detail>> = {
    prefix: prefix({ ignoreCase: false }),
    substring: substring({ ignoreCase: "smart" }),
    fuzzy: fuzzy({ ignoreCase: "smart" }),
  };
  return {
    async *match<V extends Detail>(
      { query, items }: { readonly query: string; readonly items: readonly Item<V>[] },
      o: PartOptions,
    ) {
      const groups = new Map<Match, Item<V>[]>();
      for (const item of items) {
        const m = (item.detail as Partial<ExDetail>).match ?? "prefix";
        const g = groups.get(m);
        if (g === undefined) groups.set(m, [item]);
        else g.push(item);
      }
      for (const [m, group] of groups) yield* matchers[m].match({ query, items: group }, o);
    },
  };
}
