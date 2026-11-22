// The list of a fuzzy finder or a completion (doc/design/completion.md,
// "The list"): a session's matches written into a text buffer without a
// file, one candidate per line, shown in a pane the caller opens (a
// float, a split). The selected candidate is that pane's cursor line,
// drawn with `cursorline` and the pane's own highlight groups.
//
// What is written is always the first lines of the published order: a
// few screens at first, more in chunks as results come and as the cursor
// or the scrolling nears the end. When the order changes, the lines from
// the first that differs are written again, and the selected candidate
// (by its key) stays on the cursor line. Renderers run on what is written
// only.
//
// Writing waits for the renderers, so it runs in a dispatched task and
// changes the buffer in one ctx.atomic step per chunk; the methods
// (moving, marking, invoking) run where the plain `action` works (a
// mapped function, a scheduled one, t.run).
import { action, dispatch, on } from "temzdf";
import type { Actions } from "temzdf";
import type { ItemKey, SessionError, SessionState, Snapshot } from "./session";
import type { Action, Detail, DetailUnit, DisplayItem, InvokeParams, Item, Renderer } from "./types";

/** The highlight groups lists use (defined at SessionReady; a user's own
 * `highlight()` after that, in a SessionReady hook of the configuration,
 * wins): matched parts, the selected line, marked candidates. */
export const defaultHighlights = {
  FFMatch: { fg: 4, bold: true },
  FFSelection: { bg: 8 },
  FFMarked: { fg: 5, bold: true },
} as const satisfies Record<string, Parameters<Actions["highlight"]>[1]>;

/** Define the default highlight groups (again: over the user's own). */
export function defineHighlights(act: Actions = action): void {
  for (const [name, style] of Object.entries(defaultHighlights)) act.highlight(name, style);
}

on("SessionReady", () => defineHighlights());

/** The groups a list pane draws as others (text.setOptions' highlights):
 * the cursor line in FFSelection, no cursor of an unfocused pane. */
export const LIST_HIGHLIGHTS: Readonly<Record<string, string>> = { CursorLine: "FFSelection", CursorUnfocused: "None" };

/** The buffer attributes of a list's buffer (writing lifts nomodifiable
 * for a moment). */
const ATTRS = ["nofile", "unlisted", "nomodifiable"] as const;

/** What a list reads its candidates from: a Session, or another engine
 * shaped like one (completion mixes several sessions into one). */
export interface ListSession<T extends Detail> {
  onChange(fn: () => void): () => void;
  whenIdle(): Promise<void>;
  snapshot(range?: { start?: number; end?: number }): Snapshot<T>;
  keyOf(item: Item<T>): ItemKey | undefined;
  isMarked(item: Item<T>): boolean;
  mark(item: Item<T>, on?: boolean): boolean;
  toggleMark(item: Item<T>): boolean;
  clearMarks(): void;
  markedItems(): Item<T>[];
}

export interface ListOptions<T extends Detail> {
  readonly session: ListSession<T>;
  /** Opens the pane showing the list's buffer (returns it); without it,
   * attach() a pane later. */
  readonly open?: (buffer: number) => number | undefined;
  /** Sets labels and decorations of what is written. */
  readonly renderer?: Renderer<T>;
  /** Named actions, invoked with invoke(name). */
  readonly actions?: Readonly<Record<string, Action<T>>>;
  /** The action invoke() without a name runs (a name in `actions`). */
  readonly defaultAction?: string;
  /** Moving past an end goes round to the other (false). */
  readonly wrap?: boolean;
  /** Lines written at first (three screens of the pane). */
  readonly initial?: number;
  /** Lines written more each time (as many as `initial`). */
  readonly chunk?: number;
  /** More is written when the cursor or the pane's last line comes this
   * close to the end of what is written (a screen). */
  readonly margin?: number;
  /** Highlight groups drawn as others in the pane, over LIST_HIGHLIGHTS. */
  readonly highlights?: Readonly<Record<string, string>>;
  /** The decorations' namespace (`ff-list-BUFFER`). */
  readonly namespace?: string;
  /** dispose() closes the pane (true). */
  readonly closePane?: boolean;
  /** Called after lines were written or the selection moved, where the
   * plain `action` works (a status, a title). */
  readonly onChange?: (list: List<T>) => void;
  /** Called once, when the list is disposed (an action that did not keep
   * it open, dispose()). */
  readonly onClose?: () => void;
}

/** What a caller shows about a list. */
export interface ListView {
  /** The query of the matches written. */
  readonly query: string;
  readonly collected: number;
  readonly matched: number;
  /** Lines written. */
  readonly written: number;
  /** The selected line (0-based), undefined when there is none. */
  readonly cursor: number | undefined;
  readonly marked: number;
  readonly state: SessionState;
  readonly errors: readonly (SessionError | { readonly part: "list"; readonly error: unknown })[];
}

export interface MoveOptions {
  /** Over the list's `wrap`. */
  readonly wrap?: boolean;
}

const keyString = (k: ItemKey) => `${k.source}:${typeof k.id}:${k.id}`;
const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/** A list over a session; createList makes one. */
export class List<T extends Detail = DetailUnit> {
  readonly session: ListSession<T>;
  /** The list's buffer (nofile, unlisted, nomodifiable). */
  readonly buffer: number;
  readonly namespace: string;
  readonly #o: ListOptions<T>;
  #pane: number | undefined;
  #height = 10;
  #hooks: (() => void)[] = [];
  #offChange: () => void;
  #disposed = false;
  readonly #ctrl = new AbortController();
  // What is written, line by line: the items (as the session matched
  // them), their keys, labels, and decoration ids (marks' separately).
  #items: Item<T>[] = [];
  #keys: string[] = [];
  #index = new Map<string, number>();
  #labels: string[] = [];
  #decos: number[][] = [];
  #markDecos: (number | undefined)[] = [];
  #query: string | undefined;
  /** Lines wanted: the pump writes up to this many. */
  #want = 0;
  /** Where the list last put the cursor: another line means the user
   * moved it. */
  #cursor = 0;
  #moved = false;
  /** A new query's first lines are being written: the cursor goes to the
   * top. */
  #fresh = false;
  /** Follow the last line as more is written (last()). */
  #toLast = false;
  #dirty = false;
  #pumping = false;
  #idle: (() => void)[] = [];
  readonly #errors: { part: "list"; error: unknown }[] = [];

  constructor(o: ListOptions<T>) {
    this.#o = o;
    this.session = o.session;
    this.buffer = action.newBuffer({ attrs: [...ATTRS] });
    this.namespace = o.namespace ?? `ff-list-${this.buffer}`;
    this.#offChange = this.session.onChange(() => this.#kick());
    if (o.open !== undefined) {
      const pane = o.open(this.buffer);
      if (pane !== undefined) this.attach(pane);
    }
  }

  /** The pane showing the list (undefined: none attached). */
  get pane(): number | undefined {
    return this.#pane;
  }

  get disposed(): boolean {
    return this.#disposed;
  }

  /** Show the list in `pane` (it must show the list's buffer): its options
   * (no wrapping, numbers or status line; cursorline; the highlights) and
   * hooks. Writing waits for a pane: text actions act through one. */
  attach(pane: number): void {
    if (this.#disposed) throw new Error("the list is disposed");
    this.detach();
    this.#pane = pane;
    action.statusLine(false, pane);
    action.text.setOptions(
      {
        wrap: false,
        number: "off",
        cursorline: true,
        scrolloff: 0,
        highlights: { ...LIST_HIGHLIGHTS, ...this.#o.highlights },
      },
      { pane },
    );
    this.#measure();
    if (this.#want === 0) this.#want = this.#initial();
    this.#hooks = [
      on("PaneScrolled", (p, lines) => {
        if (p === this.#pane && lines.last + this.#margin() >= this.#items.length) this.#more();
      }),
      on("PaneClosed", (p) => {
        if (p === this.#pane) this.#pane = undefined;
      }),
    ];
    this.#kick();
  }

  /** Forget the pane (it stays open); writing waits for the next one. */
  detach(): void {
    for (const off of this.#hooks) off();
    this.#hooks = [];
    this.#pane = undefined;
  }

  /** Stop: the hooks go, the pane closes (`closePane`), the buffer and
   * its decorations go, running actions are aborted. The session stays
   * (its owner disposes it). */
  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#ctrl.abort();
    this.#offChange();
    const pane = this.#pane;
    this.detach();
    if (pane !== undefined && (this.#o.closePane ?? true) && action.panes().some((p) => p.id === pane)) {
      action.closePane(pane);
    }
    if (action.buffers({ all: true }).some((b) => b.id === this.buffer)) action.deleteBuffer(this.buffer);
    this.#wake();
    this.#o.onClose?.();
  }

  /** Resolves once the session is idle and everything it published is
   * written (or the list is disposed). */
  async whenIdle(): Promise<void> {
    for (;;) {
      await this.session.whenIdle();
      if (this.#disposed || (!this.#pumping && !this.#dirty)) return;
      await new Promise<void>((resolve) => this.#idle.push(resolve));
    }
  }

  /** The items written, in order (as the session matched them). */
  written(): readonly Item<T>[] {
    return this.#items;
  }

  /** What a caller shows: counts, the state, the selected line. */
  view(): ListView {
    const snap = this.session.snapshot({ end: 0 });
    return {
      query: this.#query ?? snap.query,
      collected: snap.collected,
      matched: snap.matched,
      written: this.#items.length,
      cursor: this.#items.length === 0 ? undefined : this.#cursorLine(),
      marked: snap.marked.length,
      state: snap.state,
      errors: [...snap.errors, ...this.#errors],
    };
  }

  /** The selected item: the one on the pane's cursor line. */
  selected(): Item<T> | undefined {
    if (this.#items.length === 0) return undefined;
    return this.#items[this.#cursorLine()];
  }

  /** Move the selection `count` lines (negative: up). Past the end of
   * what is written while more matched, it stops at the last line written
   * and more is written; with `wrap`, past the ends of everything it goes
   * round. */
  move(count: number, o: MoveOptions = {}): void {
    const n = this.#items.length;
    if (n === 0 || count === 0) return;
    const wrap = o.wrap ?? this.#o.wrap ?? false;
    const at = this.#cursorLine();
    const to = at + count;
    const all = n >= this.session.snapshot({ end: 0 }).matched;
    if (to < 0) {
      if (wrap && at === 0) this.last();
      else this.#select(0);
    } else if (to >= n) {
      if (wrap && all && at === n - 1) this.#select(0);
      else this.#select(n - 1);
    } else {
      this.#select(to);
    }
  }

  next(count = 1, o: MoveOptions = {}): void {
    this.move(count, o);
  }

  prev(count = 1, o: MoveOptions = {}): void {
    this.move(-count, o);
  }

  /** `count` screens down (negative: up), never round. */
  page(count = 1): void {
    this.move(count * this.#height, { wrap: false });
  }

  first(): void {
    if (this.#items.length > 0) this.#select(0);
  }

  /** The last match: everything is written (in chunks), the selection
   * following the last line. */
  last(): void {
    if (this.#items.length === 0) return;
    this.#select(this.#items.length - 1);
    this.#toLast = true;
    this.#want = Number.POSITIVE_INFINITY;
    this.#kick();
  }

  /** Mark `item` (the selected one) if it was not, else unmark it;
   * whether it is marked now. */
  toggleMark(item: Item<T> | undefined = this.selected()): boolean {
    if (item === undefined) return false;
    const on = this.session.toggleMark(item);
    this.#showMark(item);
    return on;
  }

  /** Mark (or unmark) `item` (the selected one). */
  mark(item: Item<T> | undefined = this.selected(), on = true): void {
    if (item === undefined) return;
    this.session.mark(item, on);
    this.#showMark(item);
  }

  clearMarks(): void {
    const marked = this.session.markedItems();
    this.session.clearMarks();
    for (const item of marked) this.#showMark(item);
  }

  /** What an action gets now: the selected item, the marked ones, every
   * match. */
  params(): InvokeParams<T> {
    return {
      item: this.selected(),
      marked: this.session.markedItems(),
      matched: this.session.snapshot().items,
    };
  }

  /** Run the action `name` (the default action) with params(), in a
   * dispatched task (its `action` is the task's). Unless it returns
   * `true` the list is disposed after it. Resolves with whether the list
   * stays (also after the action failed: its error is reported as a
   * task's). */
  invoke(name?: string): Promise<boolean> {
    const key = name ?? this.#o.defaultAction;
    if (key === undefined) throw new Error("ff: no action named and no default action");
    const act = this.#o.actions?.[key];
    if (act === undefined) throw new Error(`ff: no action ${key}`);
    return this.invokeWith(act);
  }

  /** invoke() with an action of the caller's. */
  invokeWith(act: Action<T>): Promise<boolean> {
    const p = this.params();
    const signal = this.#ctrl.signal;
    return new Promise((resolve) =>
      dispatch(async (ctx) => {
        let keep = true;
        try {
          keep = (await act.invoke(p, { signal, action: ctx.action })) === true;
        } finally {
          if (!keep) ctx.atomic(() => this.dispose());
          resolve(!this.#disposed);
        }
      }),
    );
  }

  #initial(): number {
    return Math.max(1, this.#o.initial ?? 3 * this.#height);
  }

  #chunk(): number {
    return Math.max(1, this.#o.chunk ?? this.#initial());
  }

  #margin(): number {
    return Math.max(0, this.#o.margin ?? this.#height);
  }

  /** The pane's height in rows. */
  #measure(): void {
    const p = this.#pane === undefined ? undefined : action.panes().find((p) => p.id === this.#pane);
    if (p !== undefined) this.#height = Math.max(1, p.rect.height);
  }

  #cursorLine(): number {
    if (this.#pane === undefined) return Math.min(this.#cursor, Math.max(0, this.#items.length - 1));
    return action.text.cursor({ pane: this.#pane }).line;
  }

  /** Put the selection on `line` (the user's choice): more is written
   * when it comes near the end. */
  #select(line: number): void {
    this.#toLast = false;
    this.#moved = true;
    this.#setCursor(line);
    if (line + this.#margin() >= this.#items.length) this.#more();
    this.#o.onChange?.(this);
  }

  #setCursor(line: number): void {
    this.#cursor = line;
    if (this.#pane !== undefined) action.text.setCursor({ line, character: 0 }, { pane: this.#pane });
  }

  /** Want a chunk more than is written, if more matched. */
  #more(): void {
    if (this.#disposed) return;
    const want = this.#items.length + this.#chunk();
    if (want > this.#want) this.#want = want;
    this.#kick();
  }

  #kick(): void {
    if (this.#disposed) return;
    this.#dirty = true;
    if (this.#pumping) return;
    this.#pumping = true;
    dispatch(async (ctx) => {
      try {
        await this.#pump(ctx.atomic);
      } finally {
        this.#pumping = false;
        // Kicked again between the pump's end and here (a pane attached
        // by another task): pump again.
        if (this.#dirty && this.#pane !== undefined && !this.#disposed) this.#kick();
        else this.#wake();
      }
    });
  }

  #wake(): void {
    const waiting = this.#idle;
    this.#idle = [];
    for (const resolve of waiting) resolve();
  }

  /** Write what changed, a step at a time, until nothing is left. */
  async #pump(atomic: <R>(fn: () => R) => R): Promise<void> {
    while (this.#dirty && !this.#disposed) {
      this.#dirty = false;
      if (this.#pane === undefined) return;
      try {
        if (await this.#step(atomic)) {
          this.#dirty = true;
          await tick();
        }
      } catch (error) {
        if (this.#disposed) return;
        this.#errors.push({ part: "list", error });
        console.error("ff: writing the list:", error);
      }
    }
  }

  /** One step: from the first line that differs from the published
   * order, write again what was written, and at most a chunk more.
   * Returns whether more is left to write. */
  async #step(atomic: <R>(fn: () => R) => R): Promise<boolean> {
    const snap = this.session.snapshot({ end: this.#want });
    const fresh = snap.query !== this.#query;
    if (fresh) {
      this.#query = snap.query;
      this.#want = this.#initial();
      this.#toLast = false;
      this.#moved = false;
      this.#fresh = true;
    }
    const items = snap.items.slice(0, this.#want);
    const keys = items.map((i) => {
      const k = this.session.keyOf(i);
      return k === undefined ? "" : keyString(k);
    });
    const written = this.#items.length;
    let from = 0;
    if (!fresh) {
      const common = Math.min(written, items.length);
      while (from < common && this.#keys[from] === keys[from]) from++;
    }
    const end = Math.min(items.length, Math.max(written, from + this.#chunk()));
    if (from === end && end === written) return false;
    const display = await this.#render(items.slice(from, end));
    if (this.#disposed) return false;
    atomic(() => this.#write(from, display, items.slice(from, end), keys.slice(from, end)));
    return end < items.length;
  }

  /** `items` as renderers leave them (labels without line breaks). */
  async #render(items: readonly Item<T>[]): Promise<DisplayItem<T>[]> {
    const display = items.map(
      (i): DisplayItem<T> => ({ ...i, label: i.label ?? i.value, decorations: i.decorations ?? [] }),
    );
    if (this.#o.renderer !== undefined && display.length > 0) {
      await this.#o.renderer.render({ items: display }, { signal: this.#ctrl.signal });
    }
    for (const d of display) d.label = d.label.replace(/[\r\n]/g, " ");
    return display;
  }

  /** Lines `from` on become `display` (the rest goes); the selection
   * stays on its candidate. */
  #write(from: number, display: readonly DisplayItem<T>[], items: readonly Item<T>[], keys: readonly string[]): void {
    const pane = this.#pane;
    if (pane === undefined || this.#disposed) return;
    const old = this.#items.length;
    // The selection: the candidate on the cursor's line, once the user
    // moved it (the cursor elsewhere than where the list put it).
    const at = old === 0 ? 0 : action.text.cursor({ pane }).line;
    if (this.#fresh) this.#fresh = false;
    else if (at !== this.#cursor) this.#moved = true;
    const selected = this.#moved ? this.#keys[at] : undefined;
    // The decorations of the lines going.
    if (from === 0) action.text.clearNamespace(this.namespace, { pane });
    else {
      for (let l = from; l < old; l++) {
        for (const id of this.#decos[l] ?? []) action.text.deleteDecoration(id, { pane });
        const m = this.#markDecos[l];
        if (m !== undefined) action.text.deleteDecoration(m, { pane });
      }
    }
    const labels = display.map((d) => d.label);
    const start =
      from === 0 ? { line: 0, character: 0 } : { line: from - 1, character: this.#labels[from - 1]!.length };
    const last = Math.max(0, old - 1);
    const stop = { line: last, character: this.#labels[last]?.length ?? 0 };
    const text = from === 0 ? labels.join("\n") : labels.length === 0 ? "" : `\n${labels.join("\n")}`;
    action.setBufferAttrs(this.buffer, ["nofile", "unlisted"]);
    try {
      action.text.replace({ start, end: stop }, text, { pane });
    } finally {
      action.setBufferAttrs(this.buffer, [...ATTRS]);
    }
    for (const k of this.#keys.slice(from)) this.#index.delete(k);
    this.#items.splice(from, Number.POSITIVE_INFINITY, ...items);
    this.#keys.splice(from, Number.POSITIVE_INFINITY, ...keys);
    this.#labels.splice(from, Number.POSITIVE_INFINITY, ...labels);
    this.#decos.length = from;
    this.#markDecos.length = from;
    for (const [i, d] of display.entries()) {
      const line = from + i;
      this.#index.set(keys[i]!, line);
      const ids: number[] = [];
      for (const dec of d.decorations) {
        if (dec.style === undefined || dec.length <= 0) continue;
        const s = Math.min(dec.start, d.label.length);
        const e = Math.min(dec.start + dec.length, d.label.length);
        if (e <= s) continue;
        ids.push(
          action.text.addDecoration(
            { start: { line, character: s }, end: { line, character: e } },
            { style: dec.style, namespace: this.namespace, priority: 2, pane },
          ),
        );
      }
      this.#decos.push(ids);
      this.#markDecos.push(this.session.isMarked(items[i]!) ? this.#addMark(line) : undefined);
    }
    // Where the selection goes.
    const n = this.#items.length;
    let to = 0;
    if (this.#toLast) to = Math.max(0, n - 1);
    else if (selected !== undefined) to = this.#index.get(selected) ?? Math.min(at, Math.max(0, n - 1));
    else if (this.#moved) to = Math.min(at, Math.max(0, n - 1));
    this.#cursor = to;
    if (to !== at || old === 0) action.text.setCursor({ line: to, character: 0 }, { pane });
    this.#measure();
    this.#o.onChange?.(this);
  }

  #addMark(line: number): number | undefined {
    const pane = this.#pane;
    const label = this.#labels[line] ?? "";
    if (pane === undefined) return undefined;
    return action.text.addDecoration(
      { start: { line, character: 0 }, end: { line, character: label.length } },
      { style: "FFMarked", namespace: this.namespace, priority: 1, pane },
    );
  }

  /** Show whether `item`'s line is marked. */
  #showMark(item: Item<T>): void {
    const k = this.session.keyOf(item);
    const line = k === undefined ? undefined : this.#index.get(keyString(k));
    if (line === undefined || this.#pane === undefined) return;
    const old = this.#markDecos[line];
    if (old !== undefined) action.text.deleteDecoration(old, { pane: this.#pane });
    this.#markDecos[line] = this.session.isMarked(item) ? this.#addMark(line) : undefined;
  }
}

/** A list over `o.session`, in the pane `o.open` opens (or one attached
 * later). Call it where the plain `action` works. */
export function createList<T extends Detail = DetailUnit>(o: ListOptions<T>): List<T> {
  return new List(o);
}
