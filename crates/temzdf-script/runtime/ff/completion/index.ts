// temzdf/ff/completion: completion in text panes while text is typed (the
// Vim insert mode, Notepad's `edit`, prompts' `input`), on temzdf/ff
// (doc/design/completion.md, "Completion sources" and what follows).
//
// Sources are registered as key mappings are (`add`, with `when`,
// `priority`, `namespace`, `group`, `auto`, `minLength`). A completion runs
// every applicable source, each in a session of its own (its context, its
// query from its anchor to the cursor, its matcher), and mixes their
// matches in a popup: a list (createList) in a float without a border or
// the focus, below the anchor (above when it does not fit), hidden while
// nothing matches. Group N shows only when no earlier group has a match.
//
// The engine hears of text changes (TextChanged), cursor moves (InvokePost),
// the mode, the focus and the pane going (ModeChanged, PaneFocused,
// PaneClosed); hooks only schedule() a check, which asks each source's
// findContext again and narrows, collects again or ends.
//
// Selecting a candidate (next, prev) puts its text in the buffer at once
// (Vim's way), replacing its range; cancel() puts the original text back,
// accept() keeps it and runs the source's action (in a dispatched task).
// Typing on after a candidate went in accepts it too.
import { action, dispatch, on, schedule } from "temzdf";
import type { Actions, KeyContext, Position, Range } from "temzdf";
import { createList, createSession, defineSource } from "temzdf/ff";
import type {
  Detail,
  DetailUnit,
  DisplayItem,
  Item,
  List,
  Matcher,
  PartOptions,
  Renderer,
  Session,
  Sorter,
} from "temzdf/ff";
import { byScore, fuzzy } from "temzdf/ff/builtin";
import { isCmdline } from "temzdf/command";
import { keyset } from "temzdf/keyset";
import { byMatch, exCommandSource } from "./cmdline";
import { Merged } from "./merged";
import type { MergedSlot } from "./merged";

// ---------------------------------------------------------------- types

/** Where a source completes: its `anchor` (candidates without a range of
 * their own replace the text from it to the cursor; the query is that
 * text) and its own data, fixed when findContext ran. */
export interface CompletionContext<T = unknown> {
  readonly anchor: Position;
  readonly detail: T;
}

/** A candidate: an item with completion's own fields. */
export type CompletionItem<D extends Detail = DetailUnit> = Item<D> & {
  /** What goes in (default: `value`). */
  insertText?: string;
  /** What it replaces (default: from the anchor to the cursor); its start
   * also starts the text it is narrowed by (up to the cursor). An end
   * before the cursor counts as the cursor. */
  range?: Range;
  /** Shown in a column of its own (a function, a keyword, ...). */
  kind?: string;
  /** Shown in a column after the kind. */
  description?: string;
};

/** findContext's answer: a new context (collect again with it), `prev`
 * itself (keep what was collected; a collection running goes on) or
 * undefined (the source does not apply here). Called synchronously where
 * the plain `action` and the helpers below work. */
export type FindContext<T> = (prev: CompletionContext<T> | undefined) => CompletionContext<T> | undefined;

/** Collects once per context; the engine narrows to the query. */
export interface CompletionSource<T = unknown, D extends Detail = DetailUnit> {
  findContext: FindContext<T>;
  source(p: { readonly detail: T }, o: PartOptions): AsyncIterable<CompletionItem<D>>;
}

/** Collects once per context too, given the query of that moment (a
 * prefix lookup, a server). */
export interface CompletionCurator<T = unknown, D extends Detail = DetailUnit> {
  findContext: FindContext<T>;
  curator(p: { readonly query: string; readonly detail: T }, o: PartOptions): AsyncIterable<CompletionItem<D>>;
}

// biome-ignore lint/suspicious/noExplicitAny: a registration takes a source of any context and detail.
export type AnySource = CompletionSource<any, any> | CompletionCurator<any, any>;

/** What accepting a candidate does. Runs in a dispatched task (`o.action`
 * is the task's) after the completion ended. */
export interface CompletionAction<D extends Detail = DetailUnit> {
  invoke(
    p: {
      readonly item: CompletionItem<D>;
      /** The context of the item's source. */
      readonly context: CompletionContext<unknown>;
      /** Where the item's text is now (what it replaced, while selected). */
      readonly range: Range;
      /** The text pane completed in. */
      readonly pane: number;
    },
    o: PartOptions & { readonly action: Actions },
  ): void | Promise<void>;
}

export interface AddOptions {
  /** The source takes part only while this holds (as a mapping's `when`;
   * the focused pane and its mode, `keys` empty). */
  readonly when?: (ctx: KeyContext) => boolean;
  /** Sources are mixed by priority (higher first), then in the order they
   * were added. */
  readonly priority?: number;
  /** Adding the same source in the same namespace again replaces it;
   * remove() and clear() take a namespace. */
  readonly namespace?: string;
  /** Group N (0) shows only when no earlier group has a match. */
  readonly group?: number;
  /** Starts a completion by itself as text is typed (false: by hand,
   * start(), <C-n>). */
  readonly auto?: boolean;
  /** An automatic start wants at least this many characters of query
   * (1). */
  readonly minLength?: number;
  /** Narrows the candidates (fuzzy()). */
  // biome-ignore lint/suspicious/noExplicitAny: matchers of any detail.
  readonly matcher?: Matcher<any>;
  /** Orders the matches (byScore()). */
  // biome-ignore lint/suspicious/noExplicitAny: sorters of any detail.
  readonly sorter?: Sorter<any>;
  /** What accepting does after the text went in (replaceAction()). */
  // biome-ignore lint/suspicious/noExplicitAny: actions of any detail.
  readonly action?: CompletionAction<any>;
}

/** A registration as get() gives it. */
export interface Registration {
  readonly source: AnySource;
  readonly when: ((ctx: KeyContext) => boolean) | undefined;
  readonly priority: number;
  readonly namespace: string | undefined;
  readonly group: number;
  readonly auto: boolean;
  readonly minLength: number;
}

interface Reg extends Registration {
  readonly order: number;
  // biome-ignore lint/suspicious/noExplicitAny: as AddOptions.
  readonly matcher: Matcher<any>;
  // biome-ignore lint/suspicious/noExplicitAny: as AddOptions.
  readonly sorter: Sorter<any> | undefined;
  // biome-ignore lint/suspicious/noExplicitAny: as AddOptions.
  readonly action: CompletionAction<any>;
}

/** The popup's settings (configure()). */
export interface PopupOptions {
  /** Rows at most (10). */
  maxHeight: number;
  /** Columns at most (60). */
  maxWidth: number;
  /** Sets the lines' labels (columns(): label, kind, description). */
  // biome-ignore lint/suspicious/noExplicitAny: renderers of any detail.
  renderer: Renderer<any>;
  /** The modes where text is typed: completion lives only in them. */
  modes: readonly string[];
}

// ------------------------------------------------------------ positions

const cmp = (a: Position, b: Position) => a.line - b.line || a.character - b.character;
const same = (a: Position, b: Position) => cmp(a, b) === 0;

/** Where `text` inserted at `start` ends. */
function endOf(start: Position, text: string): Position {
  const lines = text.split("\n");
  if (lines.length === 1) return { line: start.line, character: start.character + text.length };
  return { line: start.line + lines.length - 1, character: lines[lines.length - 1]!.length };
}

// -------------------------------------------------------------- adapter

/** What completion does to the text pane it completes in. */
export interface CompletionAdapter {
  readonly pane: number;
  readonly buffer: number;
  cursor(): Position;
  setCursor(pos: Position): void;
  line(n: number): string;
  getText(range: Range): string;
  replace(range: Range, text: string): void;
  /** The buffer's version (it grows with every change). */
  version(): number;
}

/** The adapter of a text pane, acting with `act` (the plain `action`, or
 * a task's). */
export function textPaneAdapter(pane: number, act: Actions = action): CompletionAdapter {
  const o = { pane };
  return {
    pane,
    buffer: act.text.info(o).buffer,
    cursor: () => act.text.cursor(o),
    setCursor: (pos) => act.text.setCursor(pos, o),
    line: (n) => act.text.line(n, o),
    getText: (range) => act.text.getText(range, o),
    replace: (range, text) => act.text.replace(range, text, o),
    version: () => act.text.info(o).version,
  };
}

// -------------------------------------------------------------- helpers

/** The pane findContext runs for (helpers read it). */
let target: number | undefined;

function here(): number {
  if (target !== undefined) return target;
  const p = action.focusedPane();
  if (p === undefined) throw new Error("ff/completion: no pane");
  return p;
}

/** The cursor of the pane being completed in. */
export function cursor(): Position {
  return action.text.cursor({ pane: here() });
}

/** The text before the cursor on its line. */
export function lineBeforeCursor(): string {
  const pane = here();
  const c = action.text.cursor({ pane });
  return action.text.line(c.line, { pane }).slice(0, c.character);
}

/** Where the word before the cursor starts: a run of the buffer's word
 * characters (`chars`, a regex character class) ending at the cursor (the
 * cursor itself when there is none). */
export function wordStart(chars?: string): Position {
  const pane = here();
  const c = action.text.cursor({ pane });
  const before = action.text.line(c.line, { pane }).slice(0, c.character);
  let re: RegExp;
  try {
    re = new RegExp(`${chars ?? action.text.wordchars({ pane })}+$`, "u");
  } catch {
    re = /[\p{L}\p{N}_]+$/u;
  }
  const m = re.exec(before);
  return { line: c.line, character: m === null ? c.character : m.index };
}

/** The text from `anchor` to the cursor ("" when the cursor is before it). */
export function textFrom(anchor: Position): string {
  const pane = here();
  const c = action.text.cursor({ pane });
  if (cmp(c, anchor) <= 0) return "";
  return action.text.getText({ start: anchor, end: c }, { pane });
}

/** The pane's buffer: its id, path, name, whether it is a prompt, and its
 * file type (the path's extension, until temzdf knows file types). */
export function bufferInfo(): { buffer: number; path: string | undefined; name: string; fileType: string | undefined } {
  const info = action.text.info({ pane: here() });
  const ext = info.path === undefined ? undefined : /\.([^./\\]+)$/.exec(info.path)?.[1];
  return { buffer: info.buffer, path: info.path, name: info.name, fileType: ext };
}

// --------------------------------------------------------- registration

const regs: Reg[] = [];
let order = 0;
const settings: PopupOptions = {
  maxHeight: 10,
  maxWidth: 60,
  renderer: columns(),
  modes: ["vim/insert", "vim/cmdline", "input", "edit"],
};

/** Register `source`. */
export function add(source: AnySource, o: AddOptions = {}): void {
  if (typeof source?.findContext !== "function" || !("source" in source || "curator" in source)) {
    throw new TypeError("completion.add: a source has findContext and source (or curator)");
  }
  if (o.namespace !== undefined) remove(source, { namespace: o.namespace });
  regs.push({
    source,
    when: o.when,
    priority: o.priority ?? 0,
    namespace: o.namespace,
    group: o.group ?? 0,
    auto: o.auto ?? false,
    minLength: o.minLength ?? 1,
    order: order++,
    matcher: o.matcher ?? fuzzy(),
    sorter: o.sorter === undefined ? byScore() : o.sorter,
    action: o.action ?? replaceAction(),
  });
  regs.sort((a, b) => a.group - b.group || b.priority - a.priority || a.order - b.order);
  hooks();
}

/** Remove `source`'s registrations (those of a namespace). */
export function remove(source: AnySource, o: { namespace?: string } = {}): void {
  for (let i = regs.length - 1; i >= 0; i--) {
    const r = regs[i]!;
    if (r.source === source && (o.namespace === undefined || r.namespace === o.namespace)) regs.splice(i, 1);
  }
  hooks();
}

/** Remove the registrations of a namespace (all). */
export function clear(o: { namespace?: string } = {}): void {
  for (let i = regs.length - 1; i >= 0; i--) {
    if (o.namespace === undefined || regs[i]!.namespace === o.namespace) regs.splice(i, 1);
  }
  hooks();
}

/** The registrations (of a namespace), in the order they are mixed. */
export function get(o: { namespace?: string } = {}): Registration[] {
  return regs
    .filter((r) => o.namespace === undefined || r.namespace === o.namespace)
    .map(({ source, when, priority, namespace, group, auto, minLength }) => ({
      source,
      when,
      priority,
      namespace,
      group,
      auto,
      minLength,
    }));
}

/** Change the popup's settings. */
export function configure(o: Partial<PopupOptions>): void {
  Object.assign(settings, o);
}

// ------------------------------------------------------------ wrappers

interface FirstOfDetail {
  readonly index: number;
  readonly inner: CompletionContext<unknown>;
}

/** One source of `sources`: the first that applies (asked in turn each
 * time), its context kept as long as it answers with it. */
// biome-ignore lint/suspicious/noExplicitAny: sources of any context and detail.
export function firstOf(...sources: readonly AnySource[]): CompletionCurator<FirstOfDetail, any> {
  return {
    findContext(prev) {
      for (const [index, s] of sources.entries()) {
        const innerPrev = prev?.detail.index === index ? prev.detail.inner : undefined;
        const r = s.findContext(innerPrev);
        if (r === undefined) continue;
        if (prev !== undefined && r === innerPrev) return prev;
        return { anchor: r.anchor, detail: { index, inner: r } };
      }
      return undefined;
    },
    curator({ query, detail }, o) {
      const s = sources[detail.index]!;
      return "curator" in s
        ? s.curator({ query, detail: detail.inner.detail }, o)
        : s.source({ detail: detail.inner.detail }, o);
    },
  };
}

/** The default action: the item's text replaces `range` unless it is
 * there already (selecting put it there). */
export function replaceAction(): CompletionAction<Detail> {
  return {
    invoke({ item, range, pane }, { action: act }) {
      const a = textPaneAdapter(pane, act);
      const text = item.insertText ?? item.value;
      if (a.getText(range) === text) return;
      a.replace(range, text);
      a.setCursor(endOf(range.start, text));
    },
  };
}

/** The default renderer: the label, the kind and the description in
 * columns (as wide as the widest of the lines written at once; widths
 * count UTF-16 code units). */
export function columns(): Renderer<Detail> {
  return {
    render({ items }) {
      const all = items as DisplayItem<Detail>[] as (DisplayItem<Detail> & CompletionItem<Detail>)[];
      const lw = Math.max(0, ...all.map((i) => i.label.length));
      const kw = Math.max(0, ...all.map((i) => (i.kind ?? "").length));
      for (const i of all) {
        const parts = [i.label.padEnd(lw)];
        if (kw > 0) parts.push((i.kind ?? "").padEnd(kw));
        if (i.description !== undefined && i.description !== "") parts.push(i.description);
        i.label = parts.join("  ").trimEnd();
      }
    },
  };
}

// ---------------------------------------------------------------- state

/** The text a source's matcher reads: the cursor and its line (updated
 * before each new query). */
interface View {
  cursor: Position;
  line: string;
}

interface Slot extends MergedSlot<Detail> {
  readonly reg: Reg;
  readonly ctx: CompletionContext<unknown>;
  readonly view: View;
}

interface Selection {
  readonly item: CompletionItem<Detail>;
  readonly slot: Slot;
  /** Where its text went: from `start` to `end`, replacing `original`. */
  readonly start: Position;
  readonly end: Position;
  readonly original: string;
  /** The buffer's version after it went in. */
  readonly version: number;
}

interface Completion {
  readonly pane: number;
  readonly adapter: CompletionAdapter;
  readonly manual: boolean;
  readonly merged: Merged<Detail>;
  readonly list: List<Detail>;
  slots: Slot[];
  /** The buffer's version and the cursor after the last change: moving
   * the cursor past it ends the completion. */
  version: number;
  end: Position;
  /** The cursor when the first candidate went in (the original text's
   * end). */
  origin: Position | undefined;
  sel: Selection | undefined;
  undo: boolean;
  popup: number | undefined;
  /** Layouts dispatched and not yet run. */
  pending: number;
  queued: boolean;
  /** The cursor at the last check (nothing changed: nothing to do). */
  seen: Position;
  /** Moves asked for before anything was written (applied once it is;
   * dropped when the text changes). */
  moves: number;
  moving: boolean;
  idle: (() => void)[];
  offMerged: () => void;
}

let current: Completion | undefined;
let uid = 0;

/** Whether a completion runs (for mappings' `when`). */
export function active(): boolean {
  return current !== undefined;
}

/** Whether its popup shows (it hides while nothing matches). */
export function visible(): boolean {
  return current?.popup !== undefined;
}

/** What a completion shows, for tests and status lines. */
export function info():
  | {
      readonly pane: number;
      readonly manual: boolean;
      /** The popup's pane, while it shows. */
      readonly popup: number | undefined;
      /** The candidate in the buffer. */
      readonly selected: CompletionItem<Detail> | undefined;
      readonly matched: number;
      /** Each source's anchor, in the order they are mixed. */
      readonly anchors: readonly Position[];
    }
  | undefined {
  const c = current;
  if (c === undefined) return undefined;
  return {
    pane: c.pane,
    manual: c.manual,
    popup: c.popup,
    selected: c.sel?.item,
    matched: c.merged.snapshot({ end: 0 }).matched,
    anchors: c.slots.map((s) => s.ctx.anchor),
  };
}

/** Resolves once no source collects or matches and the popup shows what
 * they found (at once without a completion). */
export async function whenIdle(): Promise<void> {
  for (;;) {
    const c = current;
    if (c === undefined) return;
    await c.merged.whenIdle();
    await c.list.whenIdle();
    if (c.pending > 0) await new Promise<void>((resolve) => c.idle.push(resolve));
    if (current !== c) return;
    const snap = c.merged.snapshot({ end: 0 });
    if (snap.state === "done" && c.pending === 0 && snap.matched > 0 === (c.popup !== undefined)) return;
    await new Promise<void>((resolve) => setTimeout(resolve, 1));
  }
}

// ---------------------------------------------------------------- hooks

let textHook: (() => void) | undefined;
let liveHooks: (() => void)[] | undefined;
let scheduled = false;
let textChanged = false;

/** Hear the session only while needed (every hook costs every event):
 * text changes while a completion runs or an automatic source is
 * registered, the rest while a completion runs. */
function hooks(): void {
  const wantText = current !== undefined || regs.some((r) => r.auto);
  if (wantText && textHook === undefined) textHook = on("TextChanged", onText);
  else if (!wantText && textHook !== undefined) {
    textHook();
    textHook = undefined;
  }
  const wantLive = current !== undefined;
  if (wantLive && liveHooks === undefined) liveHooks = live();
  else if (!wantLive && liveHooks !== undefined) {
    for (const off of liveHooks) off();
    liveHooks = undefined;
  }
}

function onText(buffer: number, change: { cause: string }): void {
  if (current !== undefined) {
    if (buffer === current.adapter.buffer) later(true);
  } else if (change.cause === "typing" || change.cause === "script") {
    if (regs.some((r) => r.auto)) {
      const p = action.focusedPane();
      const info = p === undefined ? undefined : action.panes().find((x) => x.id === p);
      if (info?.kind === "edit" && info.buffer === buffer) later(true);
    }
  }
}

function live(): (() => void)[] {
  return [
    on("InvokePost", (inv) => {
      if (current !== undefined && inv.pane === current.pane) later(false);
    }),
    on("ModeChanged", () => {
      if (current !== undefined) later(false);
    }),
    on("PaneFocused", () => {
      if (current !== undefined) later(false);
    }),
    on("PaneClosed", (pane) => {
      const c = current;
      if (c === undefined) return;
      if (pane === c.popup) {
        // Closed by someone else: the list forgot it.
        c.popup = undefined;
      }
      if (pane === c.pane) later(false);
    }),
  ];
}

/** Check at the next safe point (hooks only read). */
function later(changed: boolean): void {
  if (changed) textChanged = true;
  if (scheduled) return;
  scheduled = true;
  schedule(() => {
    scheduled = false;
    const changed = textChanged;
    textChanged = false;
    try {
      check(changed);
    } catch (error) {
      console.error("ff/completion:", error);
    }
  });
}

function paneAlive(pane: number): boolean {
  return action.panes().some((p) => p.id === pane);
}

function keyContext(pane: number): KeyContext {
  const p = action.panes().find((x) => x.id === pane);
  return { pane, paneKind: p?.kind, floating: p?.floating ?? false, mode: action.currentMode(), keys: "" };
}

function typing(): boolean {
  return settings.modes.includes(action.currentMode());
}

/** What happened since the last check, for the running completion (or a
 * new one, for text typed). */
function check(changed: boolean): void {
  const c = current;
  if (c === undefined) {
    if (changed) begin(false);
    return;
  }
  if (!paneAlive(c.pane) || action.focusedPane() !== c.pane || !typing()) {
    finish(c, "leave");
    return;
  }
  const cur = c.adapter.cursor();
  const version = c.adapter.version();
  if (c.sel !== undefined) {
    if (version === c.sel.version && same(cur, c.sel.end)) return;
    // Typed on after the candidate: accepted; the cursor moved: left as
    // it is.
    const typed = version !== c.sel.version;
    finish(c, typed ? "accept" : "leave");
    if (typed) begin(false);
    return;
  }
  if (version === c.version && same(cur, c.seen)) return;
  c.seen = cur;
  if (version !== c.version) {
    c.version = version;
    c.end = cur;
    c.moves = 0;
  } else if (cmp(cur, c.end) > 0) {
    // Moved past what was typed.
    finish(c, "leave");
    return;
  }
  if (!update(c, cur) && changed) begin(false);
}

/** The view of the pane for a source's matcher. */
function view(adapter: CompletionAdapter, cur: Position): View {
  return { cursor: cur, line: adapter.line(cur.line) };
}

/** The query of an anchor in `v`. */
function queryOf(anchor: Position, v: View): string | undefined {
  if (anchor.line !== v.cursor.line || anchor.character > v.cursor.character) return undefined;
  return v.line.slice(anchor.character, v.cursor.character);
}

/** A key that changes whenever a query of the line can. */
const keyOf = (v: View) => `${v.cursor.line}\u0000${v.line.slice(0, v.cursor.character)}`;

/** Ask the sources again; false when the completion ended. */
function update(c: Completion, cur: Position): boolean {
  // Before an anchor (or off its line): ended.
  if (c.slots.some((s) => queryOf(s.ctx.anchor, { cursor: cur, line: "" }) === undefined)) {
    finish(c, "leave");
    return false;
  }
  const v = view(c.adapter, cur);
  const kctx = keyContext(c.pane);
  const next: Slot[] = [];
  let moved = false;
  for (const reg of regs) {
    const old = c.slots.find((s) => s.reg === reg);
    const r = ask(reg, c.pane, kctx, old?.ctx, v);
    if (r === undefined) continue;
    if (old !== undefined && r === old.ctx) {
      Object.assign(old.view, v);
      old.session.setQuery(keyOf(v));
      next.push(old);
      continue;
    }
    if (old !== undefined && !same(old.ctx.anchor, r.anchor)) moved = true;
    next.push(slot(reg, r, v));
  }
  if ((c.manual && moved) || next.length === 0) {
    for (const s of next) if (!c.slots.includes(s)) s.session.dispose();
    finish(c, "leave");
    return false;
  }
  for (const s of c.slots) if (!next.includes(s)) s.session.dispose();
  c.slots = next;
  c.merged.setSlots(next);
  layout(c);
  return true;
}

/** A registration's context here (undefined: it does not apply). */
function ask(
  reg: Reg,
  pane: number,
  kctx: KeyContext,
  prev: CompletionContext<unknown> | undefined,
  v: View,
): CompletionContext<unknown> | undefined {
  try {
    if (reg.when !== undefined && !reg.when(kctx)) return undefined;
    target = pane;
    const r = reg.source.findContext(prev);
    if (r === undefined || queryOf(r.anchor, v) === undefined) return undefined;
    return r;
  } catch (error) {
    console.error("ff/completion: findContext:", error);
    return undefined;
  } finally {
    target = undefined;
  }
}

/** A matcher narrowing each item by the text from its range's start (the
 * anchor's) to the cursor. */
// biome-ignore lint/suspicious/noExplicitAny: matchers of any detail.
function ranged(inner: Matcher<any>, anchor: Position, v: View): Matcher<Detail> {
  return {
    async *match<V extends Detail>(
      { items }: { readonly query: string; readonly items: readonly Item<V>[] },
      o: PartOptions,
    ) {
      const groups = new Map<string, Item<V>[]>();
      for (const item of items) {
        const r = (item as CompletionItem<V>).range;
        const q = queryOf(r === undefined ? anchor : r.start, v);
        if (q === undefined) continue;
        const g = groups.get(q);
        if (g === undefined) groups.set(q, [item]);
        else g.push(item);
      }
      for (const [query, group] of groups) yield* inner.match({ query, items: group }, o);
    },
  };
}

function slot(reg: Reg, ctx: CompletionContext<unknown>, v: View): Slot {
  const own: View = { ...v };
  const query = queryOf(ctx.anchor, own) ?? "";
  const src = reg.source;
  const source = defineSource<Detail>((_p, o) =>
    "curator" in src ? src.curator({ query, detail: ctx.detail }, o) : src.source({ detail: ctx.detail }, o),
  );
  const session: Session<Detail> = createSession<Detail>({
    sources: [source],
    query: keyOf(own),
    matcher: ranged(reg.matcher, ctx.anchor, own),
    ...(reg.sorter === undefined ? {} : { sorter: reg.sorter }),
  });
  session.start();
  return { uid: uid++, group: reg.group, session, reg, ctx, view: own };
}

/** Start a completion: automatic (text typed: an auto source applies with
 * enough query) or by hand. Returns whether one started. */
function begin(manual: boolean): boolean {
  if (current !== undefined || regs.length === 0) return false;
  if (!manual && !regs.some((r) => r.auto)) return false;
  const pane = action.focusedPane();
  if (pane === undefined || !typing()) return false;
  if (!action.panes().some((p) => p.id === pane && p.kind === "edit")) return false;
  const adapter = textPaneAdapter(pane);
  const cur = adapter.cursor();
  const v = view(adapter, cur);
  const kctx = keyContext(pane);
  const found = regs.map((reg) => ({ reg, ctx: ask(reg, pane, kctx, undefined, v) }));
  const starts = found.some(
    ({ reg, ctx }) =>
      ctx !== undefined && (manual || (reg.auto && (queryOf(ctx.anchor, v) ?? "").length >= reg.minLength)),
  );
  if (!starts) return false;
  const slots = found.flatMap(({ reg, ctx }) => (ctx === undefined ? [] : [slot(reg, ctx, v)]));
  const merged = new Merged<Detail>();
  const c: Completion = {
    pane,
    adapter,
    manual,
    merged,
    list: undefined as unknown as List<Detail>,
    slots,
    version: adapter.version(),
    end: cur,
    origin: undefined,
    sel: undefined,
    undo: false,
    popup: undefined,
    pending: 0,
    seen: cur,
    moves: 0,
    moving: false,
    queued: false,
    idle: [],
    offMerged: () => {},
  };
  (c as { list: List<Detail> }).list = createList<Detail>({
    session: merged,
    renderer: settings.renderer,
    closePane: true,
    // Three screens of the largest popup, not of the popup's height of
    // the moment: it is sized after the lines are written, and three rows
    // of it (after a query matching one) would write three of the next
    // query's matches only, in a popup grown to ten rows.
    initial: 3 * settings.maxHeight,
    onChange: () => {
      layout(c);
      pendingMoves(c);
    },
  });
  current = c;
  c.offMerged = merged.onChange(() => {
    if (c.queued || current !== c) return;
    c.queued = true;
    c.pending++;
    dispatch((ctx) => {
      try {
        ctx.atomic(() => {
          c.queued = false;
          // An open popup is sized when the list wrote the new lines (its
          // onChange, in the same step): sized here, it would show the
          // old lines in the new size for a frame. Opened or hidden here.
          if (c.popup === undefined || c.merged.snapshot({ end: 0 }).matched === 0) layout(c);
        });
      } finally {
        c.pending--;
        if (c.pending === 0) for (const resolve of c.idle.splice(0)) resolve();
      }
    });
  });
  merged.setSlots(slots);
  hooks();
  return true;
}

/** End a completion: `accept` keeps the candidate in and runs its action,
 * `cancel` puts the original text back, `leave` keeps the text. */
function finish(c: Completion, how: "accept" | "cancel" | "leave"): void {
  if (current !== c) return;
  current = undefined;
  hooks();
  const sel = c.sel;
  try {
    if (how === "cancel" && sel !== undefined) restore(c, sel);
    if (c.undo) action.text.undoEnd({ pane: c.pane });
  } catch {
    // The pane is gone.
  }
  c.offMerged();
  c.popup = undefined;
  c.list.dispose();
  c.merged.dispose();
  for (const resolve of c.idle.splice(0)) resolve();
  if (how === "accept" && sel !== undefined) {
    const p = { item: sel.item, context: sel.slot.ctx, range: { start: sel.start, end: sel.end }, pane: c.pane };
    const act = sel.slot.reg.action;
    dispatch(async (ctx) => {
      try {
        await act.invoke(p, { signal: new AbortController().signal, action: ctx.action });
      } catch (error) {
        console.error("ff/completion: accepting:", error);
      }
    });
  }
}

/** Put the original text back where a candidate went. */
function restore(c: Completion, sel: Selection): void {
  c.adapter.replace({ start: sel.start, end: sel.end }, sel.original);
  if (c.origin !== undefined) c.adapter.setCursor(c.origin);
}

/** Put `item`'s text in, instead of the candidate in before (if any). */
function put(c: Completion, item: CompletionItem<Detail>): void {
  const s = c.merged.slotOf(item) as Slot | undefined;
  if (s === undefined) return;
  if (c.sel !== undefined) restore(c, c.sel);
  else {
    c.origin = c.adapter.cursor();
    if (!c.undo) {
      action.text.undoBegin({ pane: c.pane });
      c.undo = true;
    }
  }
  const cur = c.origin!;
  const start = item.range?.start ?? s.ctx.anchor;
  const end = item.range !== undefined && cmp(item.range.end, cur) > 0 ? item.range.end : cur;
  const original = c.adapter.getText({ start, end });
  const text = item.insertText ?? item.value;
  c.adapter.replace({ start, end }, text);
  const after = endOf(start, text);
  c.adapter.setCursor(after);
  c.sel = { item, slot: s, start, end: after, original, version: c.adapter.version() };
}

/** Back to the original text, nothing selected. */
function unselect(c: Completion): void {
  if (c.sel !== undefined) restore(c, c.sel);
  c.sel = undefined;
  if (c.popup !== undefined) action.text.setOptions({ cursorline: false }, { pane: c.popup });
}

// ---------------------------------------------------------------- popup

/** Place the popup (open it, move it, size it, or hide it). */
function layout(c: Completion): void {
  if (current !== c) return;
  const snap = c.merged.snapshot({ end: 0 });
  const anchor = c.slots.reduce<Position | undefined>(
    (a, s) => (a === undefined || cmp(s.ctx.anchor, a) < 0 ? s.ctx.anchor : a),
    undefined,
  );
  const pos = anchor === undefined ? undefined : action.text.screenPosition(c.sel?.start ?? anchor, { pane: c.pane });
  if (snap.matched === 0 || pos === undefined) {
    hide(c);
    return;
  }
  const screen = action.screenSize();
  let width: number;
  if (c.popup !== undefined) {
    const text = action.text.getText(undefined, { pane: c.popup });
    width = Math.max(1, ...text.split("\n").map((l) => l.length));
  } else {
    // Until lines are written: the values, kinds and descriptions.
    width = Math.max(
      1,
      ...c.merged.snapshot({ end: 3 * settings.maxHeight }).items.map((i) => {
        const ci = i as CompletionItem<Detail>;
        return ci.value.length + (ci.kind ? ci.kind.length + 2 : 0) + (ci.description ? ci.description.length + 2 : 0);
      }),
    );
  }
  width = Math.max(1, Math.min(width, settings.maxWidth, screen.width));
  const want = Math.max(1, Math.min(settings.maxHeight, snap.matched));
  const below = screen.height - pos.row - 1;
  const above = pos.row;
  let row: number;
  let height: number;
  if (want <= below) [row, height] = [pos.row + 1, want];
  else if (want <= above) [row, height] = [pos.row - want, want];
  else if (below >= above) [row, height] = [pos.row + 1, below];
  else [row, height] = [0, above];
  if (height < 1) {
    hide(c);
    return;
  }
  const col = Math.max(0, Math.min(pos.col, screen.width - width));
  const geometry = { col, row, width, height };
  if (c.popup === undefined) {
    const pane = action.openFloat(
      { buffer: c.list.buffer },
      { ...geometry, border: false, focusable: false, focus: false },
    );
    if (pane === undefined) return;
    c.popup = pane;
    c.list.attach(pane);
    action.text.setOptions({ cursorline: c.sel !== undefined }, { pane });
    return;
  }
  const r = action.panes().find((p) => p.id === c.popup)?.rect;
  if (r === undefined || r.col !== col || r.row !== row || r.width !== width || r.height !== height) {
    action.floatConfig(c.popup, geometry);
  }
}

function hide(c: Completion): void {
  const p = c.popup;
  if (p === undefined) return;
  c.popup = undefined;
  c.list.detach();
  if (paneAlive(p)) action.closePane(p);
}

// ----------------------------------------------------------- operations

/** Start a completion by hand (all applicable sources); whether one runs. */
export function start(): boolean {
  return current !== undefined || begin(true);
}

/** Select the next candidate, putting its text in (after the last: the
 * original text again); starts a completion when none runs. */
export function next(): void {
  move(1);
}

/** Select the previous candidate (before the first: the original text). */
export function prev(): void {
  move(-1);
}

function move(dir: 1 | -1): void {
  const c = current;
  if (c === undefined) {
    start();
    return;
  }
  c.moving = true;
  try {
    step(c, dir);
  } finally {
    c.moving = false;
  }
}

function step(c: Completion, dir: 1 | -1): void {
  const view = c.list.view();
  if (view.written === 0 || c.popup === undefined) {
    c.moves += dir;
    return;
  }
  if (c.sel === undefined) {
    if (dir > 0) c.list.first();
    else c.list.last();
  } else {
    const at = view.cursor ?? 0;
    const to = at + dir;
    if (to < 0 || (to >= view.written && view.written >= view.matched)) {
      unselect(c);
      return;
    }
    c.list.move(dir);
  }
  const item = c.list.selected() as CompletionItem<Detail> | undefined;
  if (item === undefined) return;
  put(c, item);
  action.text.setOptions({ cursorline: true }, { pane: c.popup });
  layout(c);
}

/** Make the moves asked for before anything was written. */
function pendingMoves(c: Completion): void {
  if (c.moving) return;
  while (current === c && c.moves !== 0 && c.popup !== undefined && c.list.view().written > 0) {
    const dir = c.moves > 0 ? 1 : -1;
    c.moves -= dir;
    move(dir);
  }
}

/** Keep the candidate in and end, running its action (without one in:
 * just end). */
export function accept(): void {
  if (current !== undefined) finish(current, "accept");
}

/** Put the original text back and end. */
export function cancel(): void {
  if (current !== undefined) finish(current, "cancel");
}

// ------------------------------------------------------------- mappings

/** The keys: <C-n> and <C-p> select (starting a completion when none
 * runs), <C-y> accepts and <C-e> cancels while one runs; in the modes
 * where text is typed (settings.modes when enabled). <Tab> and <S-Tab> do
 * as <C-n> and <C-p> in the command line only (isCmdline). Namespace
 * builtin-ff-completion. */
export const keys = keyset("builtin-ff-completion", (d) => {
  const modes = [...settings.modes];
  d.map(modes, ["<C-n>"], () => next(), { desc: "completion: next" });
  d.map(modes, ["<C-p>"], () => prev(), { desc: "completion: previous" });
  // Vim's wildchar: in the command line, over the modes' own <Tab>.
  d.map(modes, ["<Tab>"], () => next(), { when: isCmdline, priority: 1, desc: "completion: next" });
  d.map(modes, ["<S-Tab>"], () => prev(), { when: isCmdline, priority: 1, desc: "completion: previous" });
  // Over the modes' own <C-y> and <C-e> while a completion runs.
  d.map(modes, ["<C-y>"], () => accept(), { when: active, priority: 1, desc: "completion: accept" });
  d.map(modes, ["<C-e>"], () => cancel(), { when: active, priority: 1, desc: "completion: cancel" });
});

// --------------------------------------------------------- command line

export type { ExContext, ExDetail } from "./cmdline";
export { byMatch, exCommandSource } from "./cmdline";

/** The source of Ex commands in the command line (exCommandSource()),
 * registered by `commandLine`. */
export const exCommands = exCommandSource();

/** Completing Ex commands in the command line (isCmdline): command names
 * (where a name is typed: at the start, after a range, after `|`) and
 * arguments (the command's `complete` option), started by hand (<Tab>,
 * <C-n>), names and options by prefix as Vim. Namespace
 * builtin-ff-cmdline. */
export const commandLine = keyset(
  "builtin-ff-cmdline",
  () => {
    add(exCommands, { when: isCmdline, namespace: "builtin-ff-cmdline", matcher: byMatch(), auto: false });
  },
  () => clear({ namespace: "builtin-ff-cmdline" }),
);
