// Completion's sessions seen as one (doc/design/completion.md, "Registering,
// and how sources mix"): each source of a completion has a session of its
// own (its own context, query and matcher); the popup's list reads them
// mixed, in the sources' order, showing only the first group that has a
// match left. Items are keyed by their source's registration and their id,
// so a source's items keep their keys while the others change.
import type { Detail, Item, ItemKey, ListSession, Session, SessionError, SessionState, Snapshot } from "temzdf/ff";

/** A source's part of the mix. */
export interface MergedSlot<T extends Detail> {
  /** Unique among the slots ever mixed (an item's key's `source`). */
  readonly uid: number;
  readonly group: number;
  readonly session: Session<T>;
}

interface Built<T extends Detail> {
  readonly query: string;
  readonly items: readonly Item<T>[];
  readonly collected: number;
  readonly state: SessionState;
  readonly sources: Snapshot<T>["sources"];
  readonly errors: readonly SessionError[];
}

/** Several sessions read as one by a list; slots are given in order (group,
 * then priority). Marks are not kept (completion has none). */
export class Merged<T extends Detail> implements ListSession<T> {
  #slots: readonly MergedSlot<T>[] = [];
  readonly #offs = new Map<MergedSlot<T>, () => void>();
  readonly #listeners = new Set<() => void>();
  #built: Built<T> | undefined;
  #disposed = false;
  readonly #origin = new WeakMap<object, MergedSlot<T>>();

  get slots(): readonly MergedSlot<T>[] {
    return this.#slots;
  }

  /** Mix `slots` from now on (sessions no longer among them are not
   * disposed here: their owner does). */
  setSlots(slots: readonly MergedSlot<T>[]): void {
    if (this.#disposed) return;
    for (const [s, off] of this.#offs) {
      if (slots.includes(s)) continue;
      off();
      this.#offs.delete(s);
    }
    for (const s of slots) {
      if (!this.#offs.has(s))
        this.#offs.set(
          s,
          s.session.onChange(() => this.#changed()),
        );
    }
    this.#slots = slots;
    this.#changed();
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    for (const off of this.#offs.values()) off();
    this.#offs.clear();
    for (const s of this.#slots) s.session.dispose();
    this.#slots = [];
    this.#listeners.clear();
    this.#built = undefined;
  }

  onChange(fn: () => void): () => void {
    this.#listeners.add(fn);
    return () => this.#listeners.delete(fn);
  }

  async whenIdle(): Promise<void> {
    for (;;) {
      const slots = this.#slots;
      await Promise.all(slots.map((s) => s.session.whenIdle()));
      if (this.#disposed || slots === this.#slots) return;
    }
  }

  snapshot(range: { start?: number; end?: number } = {}): Snapshot<T> {
    const b = this.#build();
    return {
      query: b.query,
      items: b.items.slice(range.start, range.end),
      matched: b.items.length,
      collected: b.collected,
      state: b.state,
      sources: b.sources,
      marked: [],
      errors: b.errors,
    };
  }

  /** The slot whose session matched `item` (as a snapshot gave it). */
  slotOf(item: Item<T>): MergedSlot<T> | undefined {
    return this.#origin.get(item);
  }

  keyOf(item: Item<T>): ItemKey | undefined {
    const s = this.#origin.get(item);
    return s === undefined ? undefined : { source: s.uid, id: item.id };
  }

  isMarked(): boolean {
    return false;
  }

  mark(): boolean {
    return false;
  }

  toggleMark(): boolean {
    return false;
  }

  clearMarks(): void {}

  markedItems(): Item<T>[] {
    return [];
  }

  #changed(): void {
    this.#built = undefined;
    for (const fn of [...this.#listeners]) {
      try {
        fn();
      } catch (error) {
        console.error("ff/completion: a listener:", error);
      }
    }
  }

  #build(): Built<T> {
    if (this.#built !== undefined) return this.#built;
    const snaps = this.#slots.map((s) => ({ slot: s, snap: s.session.snapshot() }));
    // The first group with a match left (groups in ascending order).
    const groups = [...new Set(this.#slots.map((s) => s.group))].sort((a, b) => a - b);
    let items: Item<T>[] = [];
    for (const g of groups) {
      const shown: Item<T>[] = [];
      for (const { slot, snap } of snaps) {
        if (slot.group !== g) continue;
        for (const item of snap.items) {
          this.#origin.set(item, slot);
          shown.push(item);
        }
      }
      if (shown.length > 0) {
        items = shown;
        break;
      }
    }
    const states = snaps.map(({ snap }) => snap.state);
    const state: SessionState = states.includes("collecting")
      ? "collecting"
      : states.includes("matching")
        ? "matching"
        : "done";
    this.#built = {
      // Changes whenever a source's published query or the set of sources
      // does (the list then writes from the top).
      query: snaps.map(({ slot, snap }) => `${slot.uid}:${snap.query}`).join("\u0000"),
      items,
      collected: snaps.reduce((n, { snap }) => n + snap.collected, 0),
      state,
      sources: snaps.flatMap(({ snap }) => snap.sources),
      errors: snaps.flatMap(({ snap }) => snap.errors),
    };
    return this.#built;
  }
}
