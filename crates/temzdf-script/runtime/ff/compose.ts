// Defining and combining parts, after vim-fall's std (`defineSource`,
// `composeSources`, `refineSource`, ...). Where temzdf differs:
// composeSources keeps ids stable and unique (vim-fall numbers the items
// again), and composeActions keeps the session open when any action asks
// (vim-fall drops what its actions return).
import type {
  Action,
  Curator,
  Detail,
  DetailUnit,
  InvokeParams,
  Item,
  Matcher,
  PartOptions,
  Refiner,
  Renderer,
  Sorter,
  Source,
} from "./types";

export function defineSource<T extends Detail = DetailUnit>(collect: Source<T>["collect"]): Source<T> {
  return { collect };
}

export function defineCurator<T extends Detail = DetailUnit>(curate: Curator<T>["curate"]): Curator<T> {
  return { curate };
}

export function defineRefiner<T extends Detail = DetailUnit, U extends Detail = DetailUnit>(
  refine: Refiner<T, U>["refine"],
): Refiner<T, U> {
  return { refine };
}

export function defineMatcher<T extends Detail = DetailUnit>(match: Matcher<T>["match"]): Matcher<T> {
  return { match };
}

export function defineSorter<T extends Detail = DetailUnit>(sort: Sorter<T>["sort"]): Sorter<T> {
  return { sort };
}

export function defineRenderer<T extends Detail = DetailUnit>(render: Renderer<T>["render"]): Renderer<T> {
  return { render };
}

export function defineAction<T extends Detail = DetailUnit>(invoke: Action<T>["invoke"]): Action<T> {
  return { invoke };
}

/** The detail a refiner adds. */
type Added<R> = R extends Refiner<never, infer U> ? U : never;
/** Refiners in a pipe, each taking what the source and those before give. */
type Pipe<Rs extends readonly unknown[], In extends Detail> = Rs extends readonly [infer H, ...infer Tail]
  ? [Refiner<In, Added<H>>, ...Pipe<Tail, In & Added<H>>]
  : Rs;
/** What refiners in a pipe add together. */
type PipeAdded<Rs extends readonly unknown[]> = Rs extends readonly [infer H, ...infer Tail]
  ? Added<H> & PipeAdded<Tail>
  : DetailUnit;

/** The refiners one after the other: each gets what the one before yields. */
export function composeRefiners<T extends Detail, Rs extends Refiner<never, Detail>[]>(
  ...refiners: Pipe<Rs, T>
): Refiner<T, PipeAdded<Rs>> {
  return {
    async *refine(p, o) {
      let items: AsyncIterable<Item<never>> = p.items as AsyncIterable<Item<never>>;
      for (const r of refiners as Refiner<never, Detail>[])
        items = r.refine({ items }, o) as AsyncIterable<Item<never>>;
      yield* items;
    },
  } as Refiner<T, PipeAdded<Rs>>;
}

/** `source`'s items through `refiners` (composeRefiners). */
export function refineSource<T extends Detail, Rs extends Refiner<never, Detail>[]>(
  source: Source<T>,
  ...refiners: Pipe<Rs, T>
): Source<T & PipeAdded<Rs>> {
  const refiner = composeRefiners<T, Rs>(...refiners);
  return {
    collect: (p, o) => refiner.refine({ items: source.collect(p, o) }, o) as AsyncIterable<Item<T & PipeAdded<Rs>>>,
  };
}

/** `curator`'s items through `refiners` (composeRefiners). */
export function refineCurator<T extends Detail, Rs extends Refiner<never, Detail>[]>(
  curator: Curator<T>,
  ...refiners: Pipe<Rs, T>
): Curator<T & PipeAdded<Rs>> {
  const refiner = composeRefiners<T, Rs>(...refiners);
  return {
    curate: (p, o) => refiner.refine({ items: curator.curate(p, o) }, o) as AsyncIterable<Item<T & PipeAdded<Rs>>>,
  };
}

/** The sources' items one source after the other, as one source. An id
 * becomes `JSON.stringify([index, id])` (`[0,"a"]`, `[1,2]`): unique
 * across the sources and the same each time they collect. */
export function composeSources<T extends Detail>(...sources: readonly Source<T>[]): Source<T> {
  return {
    async *collect(p, o) {
      for (const [i, source] of sources.entries()) {
        for await (const item of source.collect(p, o)) yield { ...item, id: JSON.stringify([i, item.id]) };
      }
    },
  };
}

async function toArray<T>(items: AsyncIterable<T>): Promise<T[]> {
  const out: T[] = [];
  for await (const x of items) out.push(x);
  return out;
}

/** The matchers as a pipe: each narrows what the one before matched (and
 * adds its decorations to theirs). */
export function composeMatchers<T extends Detail>(...matchers: readonly Matcher<T>[]): Matcher<T> {
  return {
    async *match<V extends T>(p: { readonly query: string; readonly items: readonly Item<V>[] }, o: PartOptions) {
      let items = p.items;
      for (const m of matchers) items = await toArray(m.match({ query: p.query, items }, o));
      yield* items;
    },
  };
}

/** The sorters one after the other (with stable sorters the last one
 * decides first, the ones before break its ties). */
export function composeSorters<T extends Detail>(...sorters: readonly Sorter<T>[]): Sorter<T> {
  return {
    async sort(p, o) {
      for (const s of sorters) await s.sort(p, o);
    },
  };
}

/** The renderers one after the other, each on what the one before left. */
export function composeRenderers<T extends Detail>(...renderers: readonly Renderer<T>[]): Renderer<T> {
  return {
    async render(p, o) {
      for (const r of renderers) await r.render(p, o);
    },
  };
}

/** The actions one after the other; the session stays open when any of
 * them returned `true`. */
export function composeActions<T extends Detail>(...actions: readonly Action<T>[]): Action<T> {
  return {
    async invoke(p, o) {
      let keep = false;
      for (const a of actions) if ((await a.invoke(p, o)) === true) keep = true;
      return keep ? true : undefined;
    },
  };
}

/** The items an action acts on: the marked ones, else the cursor's. */
export function chosenItems<T extends Detail>(p: InvokeParams<T>): readonly Item<T>[] {
  if (p.marked.length > 0) return p.marked;
  return p.item === undefined ? [] : [p.item];
}
