// The parts of a fuzzy finder or a completion (doc/design/completion.md),
// after vim-fall's @vim-fall/core: items, sources and curators that collect
// them, refiners that transform them, matchers, sorters and renderers, the
// previewer and actions. Positions count UTF-16 code units, as JavaScript
// strings and temzdf's text positions do.
import type { Actions } from "temzdf";

/** What an item carries for the parts that follow (a source's own data, a
 * refiner's additions, a matcher's score). */
export type Detail = object;
/** No detail. */
export type DetailUnit = Record<never, never>;

/** A part of a label (or of `value`, before a renderer set a label):
 * `length` UTF-16 code units from `start`, shown with the highlight group
 * `style`. */
export interface Decoration {
  readonly start: number;
  readonly length: number;
  readonly style?: string;
}

/** A candidate. */
export interface Item<T extends Detail = DetailUnit> {
  /** Unique within its source (the engine keys by it). */
  readonly id: string | number;
  /** What matchers match. */
  readonly value: string;
  /** The source's own data and what refiners and matchers added. */
  readonly detail: T;
  /** What is shown (default: `value`); set by renderers only. */
  label?: string;
  /** Matchers add the matched parts (on `value`), renderers styling. */
  decorations?: readonly Decoration[];
}

/** An item as renderers get it: a label and decorations filled in. */
export type DisplayItem<T extends Detail = DetailUnit> = Item<T> & {
  label: string;
  decorations: readonly Decoration[];
};

/** What every part gets besides its parameters. */
export interface PartOptions {
  readonly signal: AbortSignal;
}

/** Collects candidates once. */
export interface Source<T extends Detail = DetailUnit> {
  collect(p: { readonly args: readonly string[] }, o: PartOptions): AsyncIterable<Item<T>>;
}

/** Collects candidates again for each query. */
export interface Curator<T extends Detail = DetailUnit> {
  curate(p: { readonly args: readonly string[]; readonly query: string }, o: PartOptions): AsyncIterable<Item<T>>;
}

/** Turns items carrying `T` into items carrying `T & U` (or drops them). */
export interface Refiner<T extends Detail = DetailUnit, U extends Detail = DetailUnit> {
  refine<V extends T>(p: { readonly items: AsyncIterable<Item<V>> }, o: PartOptions): AsyncIterable<Item<V & U>>;
}

/** Narrows `items` to the query, yielding those that match with the
 * matched parts added to their decorations (a score in their detail). It
 * keeps the items' other fields (`{ ...item, decorations }`). */
export interface Matcher<T extends Detail = DetailUnit> {
  match<V extends T>(
    p: { readonly query: string; readonly items: readonly Item<V>[] },
    o: PartOptions,
  ): AsyncIterable<Item<V>>;
}

/** Orders `items` in place. */
export interface Sorter<T extends Detail = DetailUnit> {
  sort<V extends T>(p: { readonly items: Item<V>[] }, o: PartOptions): void | Promise<void>;
}

/** Sets the label and decorations of `items` in place (moving the
 * decorations along when it changes the label). */
export interface Renderer<T extends Detail = DetailUnit> {
  render<V extends T>(p: { readonly items: DisplayItem<V>[] }, o: PartOptions): void | Promise<void>;
}

/** What a previewer shows. */
export interface PreviewItem {
  readonly content: readonly string[];
  readonly line?: number;
  readonly column?: number;
  readonly filetype?: string;
  readonly filename?: string;
}

/** Shows an item (types only for now: the UI is to be designed). */
export interface Previewer<T extends Detail = DetailUnit> {
  preview(
    p: { readonly item: Item<T> },
    o: PartOptions,
    // biome-ignore lint/suspicious/noConfusingVoidType: a previewer may return nothing.
  ): void | PreviewItem | Promise<void | PreviewItem>;
}

/** What an action is given. */
export interface InvokeParams<T extends Detail = DetailUnit> {
  /** The cursor's item. */
  readonly item: Item<T> | undefined;
  /** The marked items. */
  readonly marked: readonly Item<T>[];
  /** Every matched item. */
  readonly matched: readonly Item<T>[];
}

/** What accepting a candidate does; `true` keeps the session open. */
export interface Action<T extends Detail = DetailUnit> {
  invoke(
    p: InvokeParams<T>,
    o: PartOptions & { readonly action: Actions },
    // biome-ignore lint/suspicious/noConfusingVoidType: an action may return nothing.
  ): void | true | Promise<void | true>;
}
