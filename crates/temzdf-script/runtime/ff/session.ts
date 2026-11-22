// The engine of a fuzzy finder or a completion, without a UI
// (doc/design/completion.md, "Collecting, matching, sorting"): sources
// collect, the matcher narrows what they collected to the query, the
// sorter orders the matches, and a UI hears of changes (throttled) and
// reads a snapshot. Items are told apart by their source (its index) and
// their id.
//
// The engine breaks the work up and waits for a timer between the pieces:
// the script host runs every pending promise job before it takes the
// session's next message, so a long chain of awaits alone would keep
// mapped functions (keys) waiting until it ended.
import type { Curator, Detail, DetailUnit, Item, Matcher, Sorter, Source } from "./types";

/** Where a source is: still collecting, done, failed (`error`), or
 * stopped before it was done (stop(), dispose()). */
export type SourceState = "collecting" | "done" | "failed" | "stopped";

/** What the session is doing: some source is still collecting, else
 * matching (or sorting what was matched), else done. */
export type SessionState = "collecting" | "matching" | "done";

/** An item's key in a session: its source's index and its id. */
export interface ItemKey {
  readonly source: number;
  readonly id: string | number;
}

/** An error a part threw. */
export interface SessionError {
  readonly part: "source" | "matcher" | "sorter" | "listener";
  /** The source's index (source, and the matcher on that source's items). */
  readonly source?: number;
  readonly error: unknown;
}

export interface SessionOptions<T extends Detail> {
  /** Sources collect once, curators again for each query. */
  readonly sources: readonly (Source<T> | Curator<T>)[];
  /** The sources' arguments. */
  readonly args?: readonly string[];
  /** The first query (""). */
  readonly query?: string;
  /** Narrows the items (none: every item matches, undecorated). */
  readonly matcher?: Matcher<T>;
  /** Orders the matched items (none: as collected). */
  readonly sorter?: Sorter<T>;
  /** At least this many milliseconds between two changes heard (40);
   * the first after a quiet spell is heard at once. */
  readonly throttle?: number;
  /** The engine waits for the host after this many items (1000) ... */
  readonly chunkItems?: number;
  /** ... or this many milliseconds (100), collecting or matching. */
  readonly chunkTime?: number;
}

export interface SourceSnapshot {
  readonly state: SourceState;
  readonly error?: unknown;
  readonly collected: number;
}

/** What a UI reads. The items, their count and the query they match are
 * those last published: matched and sorted, updated at most every
 * `throttle` milliseconds. */
export interface Snapshot<T extends Detail> {
  readonly query: string;
  /** The matched items asked for (all, or a range of them). */
  readonly items: readonly Item<T>[];
  readonly matched: number;
  readonly collected: number;
  readonly state: SessionState;
  readonly sources: readonly SourceSnapshot[];
  /** The marked items' keys, in the order they were marked. */
  readonly marked: readonly ItemKey[];
  readonly errors: readonly SessionError[];
}

interface Slot<T extends Detail> {
  readonly part: Source<T> | Curator<T>;
  readonly curator: boolean;
  /** Everything collected (by the running collection, for a curator). */
  items: Item<T>[];
  byId: Map<string | number, Item<T>>;
  /** Collected but not yet queued for matching. */
  fresh: Item<T>[];
  state: SourceState;
  error: unknown;
  ctrl: AbortController | undefined;
}

/** A batch of a source's items waiting for the matcher. */
interface Batch<T extends Detail> {
  readonly source: number;
  readonly items: readonly Item<T>[];
}

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
/** Whether a timer ran since: the host had a turn. */
function nextTurn(): { came: boolean } {
  const t = { came: false };
  setTimeout(() => {
    t.came = true;
  }, 0);
  return t;
}
const markKey = (k: ItemKey) => `${k.source}:${typeof k.id}:${k.id}`;

/** A session: create it with createSession, start() it, set the query as
 * it changes, read snapshot() when onChange's listeners are called, and
 * dispose() it. */
export class Session<T extends Detail = DetailUnit> {
  readonly #args: readonly string[];
  readonly #matcher: Matcher<T> | undefined;
  readonly #sorter: Sorter<T> | undefined;
  readonly #throttle: number;
  readonly #chunkItems: number;
  readonly #chunkTime: number;
  readonly #slots: Slot<T>[];
  #query: string;
  #started = false;
  #disposed = false;
  /** Bumped by every new query: a match of an older one stops. */
  #gen = 0;
  #matchCtrl = new AbortController();
  #queue: Batch<T>[] = [];
  /** The generation whose match is running. */
  #worker: number | undefined;
  #freshTimer: number | undefined;
  /** What the current generation matched so far, as matched. */
  #current: Item<T>[] = [];
  /** Bumped when #current grows. */
  #version = 0;
  #published: Item<T>[] = [];
  #publishedQuery = "";
  #publishedGen = -1;
  #publishedVersion = -1;
  #flushTimer: number | undefined;
  #flushing = false;
  #flushAgain = false;
  #lastFlush = Number.NEGATIVE_INFINITY;
  /** How long the last sort took, in milliseconds. */
  #sortTime = 0;
  /** The source of each item collected or matched. */
  readonly #origin = new WeakMap<object, number>();
  readonly #marks = new Map<string, ItemKey>();
  readonly #errors: SessionError[] = [];
  readonly #listeners = new Set<() => void>();
  #idle: (() => void)[] = [];

  constructor(o: SessionOptions<T>) {
    this.#args = o.args ?? [];
    this.#query = o.query ?? "";
    this.#matcher = o.matcher;
    this.#sorter = o.sorter;
    this.#throttle = o.throttle ?? 40;
    this.#chunkItems = Math.max(1, o.chunkItems ?? 1000);
    this.#chunkTime = o.chunkTime ?? 100;
    this.#slots = o.sources.map((part) => ({
      part,
      curator: "curate" in part,
      items: [],
      byId: new Map(),
      fresh: [],
      state: "collecting",
      error: undefined,
      ctrl: undefined,
    }));
  }

  /** Start collecting (once). */
  start(): void {
    if (this.#started || this.#disposed) return;
    this.#started = true;
    for (const i of this.#slots.keys()) this.#collect(i);
    this.#changed();
  }

  /** Stop collecting; what was collected stays, and is matched again when
   * the query changes. */
  stop(): void {
    for (const s of this.#slots) {
      if (s.state !== "collecting") continue;
      s.ctrl?.abort();
      s.state = "stopped";
    }
    this.#changed();
  }

  /** Stop everything: collecting, matching, listeners. */
  dispose(): void {
    if (this.#disposed) return;
    this.stop();
    this.#disposed = true;
    this.#gen++;
    this.#matchCtrl.abort();
    this.#queue = [];
    for (const t of [this.#flushTimer, this.#freshTimer]) if (t !== undefined) clearTimeout(t);
    this.#flushTimer = this.#freshTimer = undefined;
    this.#listeners.clear();
    this.#wake();
  }

  get disposed(): boolean {
    return this.#disposed;
  }

  get query(): string {
    return this.#query;
  }

  /** Match everything collected against `query` (curators collect again);
   * a match of the query before stops. */
  setQuery(query: string): void {
    if (this.#disposed || query === this.#query) return;
    this.#query = query;
    this.#gen++;
    this.#matchCtrl.abort();
    this.#matchCtrl = new AbortController();
    this.#current = [];
    this.#version++;
    this.#queue = [];
    for (const [i, s] of this.#slots.entries()) {
      s.fresh = [];
      if (s.curator && this.#started) {
        s.items = [];
        s.byId = new Map();
        this.#collect(i);
      } else if (s.items.length > 0) {
        this.#queue.push({ source: i, items: s.items.slice() });
      }
    }
    this.#kick();
    this.#changed();
  }

  /** Call `fn` when the snapshot changed (at most every `throttle`
   * milliseconds); returns what stops it. */
  onChange(fn: () => void): () => void {
    this.#listeners.add(fn);
    return () => this.#listeners.delete(fn);
  }

  /** Resolves once nothing is collecting, matching or waiting to be
   * published (or the session is disposed). */
  whenIdle(): Promise<void> {
    if (this.#disposed || this.#isIdle()) return Promise.resolve();
    return new Promise((resolve) => this.#idle.push(resolve));
  }

  /** The state, with the matched items from `start` to `end` (all). */
  snapshot(range: { start?: number; end?: number } = {}): Snapshot<T> {
    return {
      query: this.#publishedQuery,
      items: this.#published.slice(range.start, range.end),
      matched: this.#published.length,
      collected: this.#slots.reduce((n, s) => n + s.items.length, 0),
      state: this.#state(),
      sources: this.#slots.map((s) => ({
        state: s.state,
        ...(s.error === undefined ? {} : { error: s.error }),
        collected: s.items.length,
      })),
      marked: [...this.#marks.values()],
      errors: this.#errors.slice(),
    };
  }

  /** The key of an item the session collected or matched. */
  keyOf(item: Item<T>): ItemKey | undefined {
    const source = this.#origin.get(item);
    return source === undefined ? undefined : { source, id: item.id };
  }

  /** The item a source collected with `id` (its collection's, for a
   * curator). */
  item(key: ItemKey): Item<T> | undefined {
    return this.#slots[key.source]?.byId.get(key.id);
  }

  isMarked(item: Item<T>): boolean {
    const k = this.keyOf(item);
    return k !== undefined && this.#marks.has(markKey(k));
  }

  /** Mark `item` (or unmark it with `on` false); false for an item the
   * session does not know. */
  mark(item: Item<T>, on = true): boolean {
    const k = this.keyOf(item);
    if (k === undefined) return false;
    const was = this.#marks.has(markKey(k));
    if (on && !was) this.#marks.set(markKey(k), k);
    if (!on && was) this.#marks.delete(markKey(k));
    if (on !== was) this.#changed();
    return true;
  }

  unmark(item: Item<T>): boolean {
    return this.mark(item, false);
  }

  /** Mark `item` if it was not, else unmark it; whether it is marked now. */
  toggleMark(item: Item<T>): boolean {
    const on = !this.isMarked(item);
    return this.mark(item, on) && on;
  }

  clearMarks(): void {
    if (this.#marks.size === 0) return;
    this.#marks.clear();
    this.#changed();
  }

  /** The marked items still collected, in the order they were marked. */
  markedItems(): Item<T>[] {
    const out: Item<T>[] = [];
    for (const k of this.#marks.values()) {
      const item = this.item(k);
      if (item !== undefined) out.push(item);
    }
    return out;
  }

  #state(): SessionState {
    if (this.#slots.some((s) => s.state === "collecting" && this.#started)) return "collecting";
    return this.#matchIdle() ? "done" : "matching";
  }

  #matchIdle(): boolean {
    return (
      this.#queue.length === 0 &&
      this.#worker === undefined &&
      this.#freshTimer === undefined &&
      this.#publishedGen === this.#gen &&
      this.#publishedVersion === this.#version
    );
  }

  #isIdle(): boolean {
    return (
      this.#state() === "done" &&
      this.#started &&
      !this.#flushing &&
      this.#flushTimer === undefined &&
      !this.#flushAgain
    );
  }

  #wake(): void {
    if (!this.#disposed && !this.#isIdle()) return;
    const waiting = this.#idle;
    this.#idle = [];
    for (const resolve of waiting) resolve();
  }

  /** Run source `i`'s collection (again: the running one is aborted). */
  #collect(i: number): void {
    const s = this.#slots[i]!;
    s.ctrl?.abort();
    const ctrl = new AbortController();
    s.ctrl = ctrl;
    s.state = "collecting";
    s.error = undefined;
    void this.#run(i, s, ctrl.signal);
  }

  async #run(i: number, s: Slot<T>, signal: AbortSignal): Promise<void> {
    try {
      const items = s.curator
        ? (s.part as Curator<T>).curate({ args: this.#args, query: this.#query }, { signal })
        : (s.part as Source<T>).collect({ args: this.#args }, { signal });
      // A chunk is what comes without a timer running in between: time
      // the source spent waiting for something (a process, a file, the
      // host's next turn) is not the engine's work, and the items that
      // come after it together stay together (a first item would
      // otherwise go to the matcher, and be published, on its own).
      let n = 0;
      let since = Date.now();
      let turn = nextTurn();
      for await (const item of items) {
        if (signal.aborted) break;
        if (turn.came) {
          n = 0;
          since = Date.now();
          turn = nextTurn();
        }
        this.#take(i, s, item);
        if (++n >= this.#chunkItems || Date.now() - since >= this.#chunkTime) {
          await tick();
          if (signal.aborted) break;
          n = 0;
          since = Date.now();
          turn = nextTurn();
        }
      }
      if (signal.aborted) return;
      s.state = "done";
    } catch (error) {
      if (signal.aborted) return;
      s.state = "failed";
      s.error = error;
      this.#errors.push({ part: "source", source: i, error });
    }
    this.#changed();
  }

  /** Keep an item collected and queue it for matching (with the others
   * collected until the host's next turn). */
  #take(i: number, s: Slot<T>, item: Item<T>): void {
    s.items.push(item);
    s.byId.set(item.id, item);
    s.fresh.push(item);
    this.#origin.set(item, i);
    this.#freshTimer ??= setTimeout(() => {
      this.#freshTimer = undefined;
      for (const [source, slot] of this.#slots.entries()) {
        if (slot.fresh.length === 0) continue;
        this.#queue.push({ source, items: slot.fresh });
        slot.fresh = [];
      }
      this.#kick();
    }, 0);
  }

  #kick(): void {
    if (this.#disposed || this.#worker === this.#gen || this.#queue.length === 0) return;
    void this.#match(this.#gen);
  }

  /** Match the queued batches for generation `gen`, a chunk at a time,
   * until the queue is empty or a new query came. */
  async #match(gen: number): Promise<void> {
    this.#worker = gen;
    const signal = this.#matchCtrl.signal;
    const query = this.#query;
    const live = () => gen === this.#gen;
    let source: number | undefined;
    try {
      while (live() && this.#queue.length > 0) {
        const batch = this.#queue.shift()!;
        source = batch.source;
        for (let at = 0; at < batch.items.length; at += this.#chunkItems) {
          const items = batch.items.slice(at, at + this.#chunkItems);
          let since = Date.now();
          if (this.#matcher === undefined) {
            this.#matched(batch.source, items);
          } else {
            for await (const m of this.#matcher.match({ query, items }, { signal })) {
              if (!live()) return;
              this.#matched(batch.source, [m]);
              if (Date.now() - since >= this.#chunkTime) {
                this.#changed();
                await tick();
                if (!live()) return;
                since = Date.now();
              }
            }
          }
          if (!live()) return;
          this.#changed();
          await tick();
          if (!live()) return;
        }
      }
    } catch (error) {
      if (!live()) return;
      this.#errors.push({ part: "matcher", ...(source === undefined ? {} : { source }), error });
      this.#queue = [];
    } finally {
      if (this.#worker === gen) this.#worker = undefined;
    }
    if (!live()) return;
    this.#changed();
    this.#kick();
  }

  #matched(source: number, items: readonly Item<T>[]): void {
    for (const m of items) {
      this.#origin.set(m, source);
      this.#current.push(m);
    }
    this.#version++;
  }

  /** Publish and tell the listeners: at once after a quiet spell, else
   * `throttle` milliseconds after the last time, or twice as long as the
   * last sort took if that is longer (the sorter orders every match each
   * time: sorting 100k items again and again would take most of the time
   * otherwise). */
  #changed(): void {
    if (this.#disposed) return;
    if (this.#flushing) {
      this.#flushAgain = true;
      return;
    }
    if (this.#flushTimer !== undefined) return;
    const wait = Math.max(0, this.#lastFlush + Math.max(this.#throttle, 2 * this.#sortTime) - Date.now());
    this.#flushTimer = setTimeout(() => {
      this.#flushTimer = undefined;
      void this.#flush();
    }, wait);
  }

  /** A new query matched nothing yet while matching or collecting goes on:
   * what was published before stays (a list is not emptied for a moment,
   * a completion's popup not closed and opened again) until the new
   * query's first matches, or its end (no match at all). */
  #holding(): boolean {
    return (
      this.#publishedGen >= 0 &&
      this.#gen !== this.#publishedGen &&
      this.#current.length === 0 &&
      (this.#queue.length > 0 ||
        this.#worker !== undefined ||
        this.#freshTimer !== undefined ||
        this.#slots.some((s) => s.state === "collecting"))
    );
  }

  async #flush(): Promise<void> {
    // Neither published nor heard, nor counted for the throttle: the
    // match's next step (or its end) asks again.
    if (this.#holding()) return;
    this.#flushing = true;
    const gen = this.#gen;
    const version = this.#version;
    try {
      if (gen !== this.#publishedGen || version !== this.#publishedVersion) {
        const items = this.#current.slice();
        try {
          if (this.#sorter !== undefined && items.length > 1) {
            const started = Date.now();
            await this.#sorter.sort({ items }, { signal: this.#matchCtrl.signal });
            this.#sortTime = Date.now() - started;
          }
        } catch (error) {
          if (gen === this.#gen) this.#errors.push({ part: "sorter", error });
        }
        if (gen === this.#gen && !this.#disposed) {
          this.#published = items;
          this.#publishedQuery = this.#query;
          this.#publishedGen = gen;
          this.#publishedVersion = version;
        }
      }
    } finally {
      this.#flushing = false;
      this.#lastFlush = Date.now();
    }
    for (const fn of [...this.#listeners]) {
      try {
        fn();
      } catch (error) {
        this.#errors.push({ part: "listener", error });
      }
    }
    if (this.#flushAgain || gen !== this.#gen || version !== this.#version) {
      this.#flushAgain = false;
      this.#changed();
    }
    this.#wake();
  }
}

/** A session over `o.sources` (not started). */
export function createSession<T extends Detail = DetailUnit>(o: SessionOptions<T>): Session<T> {
  return new Session(o);
}
