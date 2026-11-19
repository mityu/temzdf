// Ex commands scripts define (doc/design/ex-commands.md): a registry,
// `command.execute` running a command line, and helpers parsing one. A
// module of its own (`temzdf/command`) so every script shares the one
// registry; the shipped scripts import it by that name too.
import { action } from "temzdf";
import type { KeyContext } from "temzdf";

/** What modifier commands (`:vertical`, say) pass on to the rest. */
export type Mods = { [key: string]: unknown };

/** One address of a Vim range, as typed (not yet a line). */
export type VimLineAddress = {
  base:
    | { kind: "number"; number: number } // as typed: 1 is the first line
    | { kind: "current" } // `.`, or nothing before an offset
    | { kind: "last" } // `$`
    | { kind: "mark"; name: string } // `'x`
    | { kind: "pattern"; pattern: string; backward: boolean } // `/pat/`, `?pat?`
    | { kind: "lastPattern"; backward: boolean } // `\/`, `\?`
    | { kind: "lastSubstitute" }; // `\&`
  /** The sum of the `+N` and `-N` after it. */
  offset: number;
};

/** A Vim range as typed: `%` is `1,$`. */
export type VimRangeSpec = {
  start: VimLineAddress;
  end?: VimLineAddress;
  separator?: "," | ";";
};

/** What a command's function gets. */
export type CmdCtx = {
  /** The full name, even when abbreviated. */
  name: string;
  /** Everything after the name, untrimmed (with `bar: "auto"`, up to the `|`). */
  args: string;
  /** The range typed (a command with `prefix: "vim-range"`; else undefined). */
  range: VimRangeSpec | undefined;
  /** What the caller of `command.execute` passed, else {}. */
  mods: Mods;
  /** This command's part of the line: its range, name and arguments. */
  line: string;
};

export type CmdOpts = {
  /** The shortest abbreviation accepted (a prefix of the name); without it, only the full name. */
  shortName?: string;
  /** The command exists only while this holds (the context a mapping's `when` gets). */
  when?: (ctx: KeyContext) => boolean;
  /** Accepts a prefix: "vim-range", a Vim range. Without it a range is an error (E481). */
  prefix?: "vim-range";
  /** `|` after it: "auto" (default) splits there and runs the rest after it; "handle" passes the rest, `|` included, to the function. */
  bar?: "auto" | "handle";
  desc?: string;
  /** Adding the same name with the same namespace again replaces it; `clear` removes a namespace's commands. */
  namespace?: string;
};

/** A registered command, as `command.list` shows it. */
export type CmdSpec = {
  name: string;
  shortName?: string;
  prefix?: "vim-range";
  bar: "auto" | "handle";
  desc?: string;
  namespace?: string;
};

type Entry = CmdSpec & { fn: (ctx: CmdCtx) => void; when?: (ctx: KeyContext) => boolean };

/** In the order defined: the last one wins. */
const registry: Entry[] = [];

function now<T>(fn: () => T): T {
  return fn();
}

function check(name: string, fn: unknown, opts: CmdOpts): void {
  if (!/^[A-Za-z]+$/.test(name)) throw new TypeError(`command.add(${name}): a name is letters only`);
  if (typeof fn !== "function") throw new TypeError(`command.add(${name}): expected a function`);
  const s = opts.shortName;
  if (s !== undefined && (s === "" || !name.startsWith(s))) {
    throw new TypeError(`command.add(${name}): shortName ${s} is not a prefix of the name`);
  }
  if (opts.prefix !== undefined && opts.prefix !== "vim-range") {
    throw new TypeError(`command.add(${name}): unknown prefix ${String(opts.prefix)}`);
  }
  if (opts.bar !== undefined && opts.bar !== "auto" && opts.bar !== "handle") {
    throw new TypeError(`command.add(${name}): bar is "auto" or "handle", not ${String(opts.bar)}`);
  }
}

function removeWhere(keep: (e: Entry) => boolean): void {
  const kept = registry.filter(keep);
  registry.length = 0;
  registry.push(...kept);
}

/** The context `when` gets, as a mapping's (no keys typed). */
function keyContext(): KeyContext {
  const pane = action.focusedPane();
  const info = pane === null ? undefined : action.panes().find((p) => p.id === pane);
  return {
    pane: pane ?? -1,
    paneKind: info?.kind ?? null,
    floating: info?.floating ?? false,
    mode: action.currentMode(),
    keys: "",
  };
}

/** The command `input` names: exact names first, then abbreviations; the last defined of either. */
function lookup(input: string): Entry | undefined {
  let ctx: KeyContext | undefined;
  const live = (e: Entry) => e.when === undefined || e.when((ctx ??= keyContext()));
  let abbreviated: Entry | undefined;
  for (let i = registry.length - 1; i >= 0; i--) {
    const e = registry[i];
    if (e.name === input) {
      if (live(e)) return e;
    } else if (
      abbreviated === undefined &&
      e.shortName !== undefined &&
      e.name.startsWith(input) &&
      input.length >= e.shortName.length &&
      live(e)
    ) {
      abbreviated = e;
    }
  }
  return abbreviated;
}

function run(line: string, mods: Mods): void {
  let rest = line;
  for (;;) {
    rest = rest.replace(/^[\s:]+/, "");
    if (rest === "") return;
    const [range, afterRange] = parsePrefix(rest);
    const name = /^[A-Za-z]*/.exec(afterRange)![0];
    // A range alone (`:5`) is undecided (doc/design/ex-commands.md): an error for now.
    const cmd = name === "" ? undefined : lookup(name);
    if (cmd === undefined) throw new Error(`E492: Not an editor command: ${rest.trimEnd()}`);
    if (range !== undefined && cmd.prefix === undefined) throw new Error("E481: No range allowed");
    const args = afterRange.slice(name.length);
    if (cmd.bar === "handle") {
      cmd.fn({ name: cmd.name, args, range, mods, line: rest });
      return;
    }
    const [head, tail] = splitBar(args);
    const own = rest.slice(0, rest.length - args.length + head.length);
    cmd.fn({ name: cmd.name, args: head.replace(/\\\|/g, "|"), range, mods, line: own });
    if (tail === undefined) return;
    rest = tail;
    mods = {};
  }
}

export const command = {
  /** Define an Ex command (`:name`). */
  add(name: string, fn: (ctx: CmdCtx) => void, opts: CmdOpts = {}): void {
    check(name, fn, opts);
    const { shortName, when, prefix, bar = "auto", desc, namespace } = opts;
    if (namespace !== undefined) removeWhere((e) => !(e.name === name && e.namespace === namespace));
    registry.push({ name, fn, shortName, when, prefix, bar, desc, namespace });
  },
  /** Remove the commands called `name`: those of a namespace, or all. */
  remove(name: string, filter: { namespace?: string } = {}): void {
    removeWhere((e) => !(e.name === name && (filter.namespace === undefined || e.namespace === filter.namespace)));
  },
  /** Remove the commands of a namespace, or every one. */
  clear(filter: { namespace?: string } = {}): void {
    removeWhere((e) => filter.namespace !== undefined && e.namespace !== filter.namespace);
  },
  /** The commands, in the order defined (a snapshot). */
  list(): CmdSpec[] {
    return registry.map(({ name, shortName, prefix, bar, desc, namespace }) => ({
      name,
      shortName,
      prefix,
      bar,
      desc,
      namespace,
    }));
  },
  /** Run a command line (`|` separating commands as they say). Errors reach the caller. */
  execute: now(() => {
    /** Nested `command.execute` calls, refused past `MAX_DEPTH` (Vim's E169).
     * Vim allows 200; here 200 levels of a command running itself exhaust the
     * guest's WebAssembly stack first (150 still fit, 2026-11-19), which ends
     * the script host, so half that leaves room for commands doing more. */
    let depth = 0;
    const MAX_DEPTH = 100;
    return (line: string, opts: { mods?: Mods } = {}): void => {
      if (depth >= MAX_DEPTH) throw new Error("E169: Command too recursive");
      depth++;
      try {
        run(line, opts.mods ?? {});
      } finally {
        depth--;
      }
    };
  }),
};

/** `!` at the start, and what follows it. */
export function parseBang(s: string): [boolean, string] {
  return s.startsWith("!") ? [true, s.slice(1)] : [false, s];
}

/** Split at spaces; a backslash takes the next character as it is (`\ `, `\\`). */
export function splitArgs(s: string): string[] {
  const out: string[] = [];
  let cur = "";
  let any = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === "\\" && (s[i + 1] === " " || s[i + 1] === "\t" || s[i + 1] === "\\")) {
      cur += s[++i];
      any = true;
    } else if (c === " " || c === "\t") {
      if (any) out.push(cur);
      cur = "";
      any = false;
    } else {
      cur += c;
      any = true;
    }
  }
  if (any) out.push(cur);
  return out;
}

/** The text before the first `|` not after a backslash, and the rest after it (undefined: none). */
export function splitBar(s: string): [string, string | undefined] {
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && s[i + 1] === "|") i++;
    else if (s[i] === "|") return [s.slice(0, i), s.slice(i + 1)];
  }
  return [s, undefined];
}

/** A pattern delimited by `delim` from `s[i]` (after the opening one): the pattern and where it ends. */
function pattern(s: string, i: number, delim: string): [string, number] {
  let p = "";
  while (i < s.length && s[i] !== delim) {
    if (s[i] === "\\" && s[i + 1] === delim) {
      p += delim;
      i += 2;
    } else if (s[i] === "\\" && i + 1 < s.length) {
      p += s[i] + s[i + 1];
      i += 2;
    } else {
      p += s[i++];
    }
  }
  return [p, i < s.length ? i + 1 : i];
}

/** An address at `s[i]`, and where it ends (undefined: none there). */
function address(s: string, i: number): [VimLineAddress | undefined, number] {
  let base: VimLineAddress["base"] | undefined;
  const c = s[i];
  if (c === ".") {
    base = { kind: "current" };
    i++;
  } else if (c === "$") {
    base = { kind: "last" };
    i++;
  } else if (c === "'" && i + 1 < s.length) {
    base = { kind: "mark", name: s[i + 1] };
    i += 2;
  } else if (c === "/" || c === "?") {
    const [p, j] = pattern(s, i + 1, c);
    base = { kind: "pattern", pattern: p, backward: c === "?" };
    i = j;
  } else if (c === "\\" && (s[i + 1] === "/" || s[i + 1] === "?")) {
    base = { kind: "lastPattern", backward: s[i + 1] === "?" };
    i += 2;
  } else if (c === "\\" && s[i + 1] === "&") {
    base = { kind: "lastSubstitute" };
    i += 2;
  } else {
    const m = /^\d+/.exec(s.slice(i));
    if (m !== null) {
      base = { kind: "number", number: Number(m[0]) };
      i += m[0].length;
    }
  }
  let offset = 0;
  let signed = false;
  for (;;) {
    const m = /^([+-])(\d*)/.exec(s.slice(i));
    if (m === null) break;
    offset += (m[1] === "+" ? 1 : -1) * (m[2] === "" ? 1 : Number(m[2]));
    signed = true;
    i += m[0].length;
  }
  if (base === undefined && !signed) return [undefined, i];
  return [{ base: base ?? { kind: "current" }, offset }, i];
}

const current: VimLineAddress = { base: { kind: "current" }, offset: 0 };

/** The range at the start of `s` (spaces and `:` before it skipped), as typed, and what follows it. */
export function parsePrefix(s: string): [VimRangeSpec | undefined, string] {
  let i = /^[\s:]*/.exec(s)![0].length;
  if (s[i] === "%") {
    return [
      { start: { base: { kind: "number", number: 1 }, offset: 0 }, end: { base: { kind: "last" }, offset: 0 }, separator: "," },
      s.slice(i + 1),
    ];
  }
  const [start, j] = address(s, i);
  i = j;
  if (s[i] !== "," && s[i] !== ";") return [start === undefined ? undefined : { start }, s.slice(i)];
  const separator = s[i] as "," | ";";
  // A missing address on either side of the separator is the current line.
  const [end, k] = address(s, i + 1);
  return [{ start: start ?? current, end: end ?? current, separator }, s.slice(k)];
}

/** Where `resolveRange` finds lines (zero based), for the pane the range is for. */
export type VimRangeEnv = {
  current(): number;
  last(): number;
  mark(name: string): number;
  /** The line of the next match of `pattern` after `from` (before it: backward); undefined: patterns are refused. */
  search?(pattern: string | undefined, backward: boolean, from: number): number;
};

function resolveAddress(a: VimLineAddress, env: VimRangeEnv, cur: number): number {
  const b = a.base;
  let line: number;
  switch (b.kind) {
    case "number":
      line = b.number - 1;
      break;
    case "current":
      line = cur;
      break;
    case "last":
      line = env.last();
      break;
    case "mark":
      line = env.mark(b.name);
      break;
    case "pattern":
    case "lastPattern":
    case "lastSubstitute": {
      if (env.search === undefined) throw new Error("E16: Invalid range");
      const p = b.kind === "pattern" ? b.pattern : undefined;
      line = env.search(p, b.kind !== "lastSubstitute" && b.backward, cur);
      break;
    }
  }
  return line + a.offset;
}

/** The lines (zero based, first ≤ last) of a range typed, read with `env`. */
export function resolveRange(spec: VimRangeSpec, env: VimRangeEnv): { first: number; last: number } {
  const start = resolveAddress(spec.start, env, env.current());
  // After `;` the second address counts from the first.
  const end = spec.end === undefined ? start : resolveAddress(spec.end, env, spec.separator === ";" ? start : env.current());
  return { first: Math.min(start, end), last: Math.max(start, end) };
}
