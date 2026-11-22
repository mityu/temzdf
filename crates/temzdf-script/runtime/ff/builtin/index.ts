// temzdf/ff/builtin: the parts shipped with temzdf/ff (matchers, sorters,
// renderers). Matched parts are decorated with the style MATCH_STYLE;
// positions count UTF-16 code units.
import type { Decoration, Detail, DetailUnit, Item, Matcher, Renderer, Sorter } from "temzdf/ff";

/** The style of matched parts. */
export const MATCH_STYLE = "FFMatch";

/** Case: `true` ignores it, `false` respects it, `"smart"` ignores it
 * unless the query has an upper-case letter. */
export type IgnoreCase = boolean | "smart";

export interface MatchOptions {
  /** Default `true`. */
  readonly ignoreCase?: IgnoreCase;
}

/** What the fuzzy matcher adds to an item's detail. */
export interface ScoreDetail {
  /** Higher is better. */
  readonly score: number;
}

/** `s` with each UTF-16 code unit lower-cased where that keeps it one code
 * unit (so positions in it are positions in `s`). */
function fold(s: string): string {
  const lower = s.toLowerCase();
  if (lower.length === s.length) return lower;
  let out = "";
  for (const c of s) {
    const l = c.toLowerCase();
    out += l.length === c.length ? l : c;
  }
  return out;
}

/** Whether to ignore case for `query`. */
function ignores(o: MatchOptions | undefined, query: string): boolean {
  const ic = o?.ignoreCase ?? true;
  return ic === "smart" ? query === query.toLowerCase() : ic;
}

const terms = (query: string) => query.split(/\s+/).filter((t) => t !== "");

/** Positions merged into runs, sorted. */
function runs(positions: readonly number[], style = MATCH_STYLE): Decoration[] {
  const sorted = [...new Set(positions)].sort((a, b) => a - b);
  const out: Decoration[] = [];
  for (const p of sorted) {
    const last = out[out.length - 1];
    if (last !== undefined && last.start + last.length === p) {
      out[out.length - 1] = { start: last.start, length: last.length + 1, style };
    } else {
      out.push({ start: p, length: 1, style });
    }
  }
  return out;
}

/** `item` with `decorations` added to its own. */
function decorate<V extends Detail>(item: Item<V>, decorations: readonly Decoration[]): Item<V> {
  if (decorations.length === 0) return item;
  return { ...item, decorations: [...(item.decorations ?? []), ...decorations] };
}

/** Items whose value contains every word of the query (split at white
 * space), each word's first occurrence decorated. An empty query matches
 * everything. */
export function substring(o?: MatchOptions): Matcher<DetailUnit> {
  return {
    async *match({ query, items }, { signal }) {
      const ic = ignores(o, query);
      const words = terms(ic ? fold(query) : query);
      for (const item of items) {
        if (signal.aborted) return;
        const value = ic ? fold(item.value) : item.value;
        const decorations: Decoration[] = [];
        let ok = true;
        for (const w of words) {
          const at = value.indexOf(w);
          if (at < 0) {
            ok = false;
            break;
          }
          decorations.push({ start: at, length: w.length, style: MATCH_STYLE });
        }
        if (ok)
          yield decorate(
            item,
            decorations.sort((a, b) => a.start - b.start),
          );
      }
    },
  };
}

/** Items whose value starts with the query (as it is, white space
 * included). */
export function prefix(o?: MatchOptions): Matcher<DetailUnit> {
  return {
    async *match({ query, items }, { signal }) {
      const ic = ignores(o, query);
      const q = ic ? fold(query) : query;
      for (const item of items) {
        if (signal.aborted) return;
        const value = ic ? fold(item.value) : item.value;
        if (!value.startsWith(q)) continue;
        yield decorate(item, q.length === 0 ? [] : [{ start: 0, length: q.length, style: MATCH_STYLE }]);
      }
    },
  };
}

const BOUNDARY = new Set([..." \t/\\_-.:,;|()[]{}<>'\"`"]);
/** Bonuses of the fuzzy score. */
const SCORE = { char: 1, start: 8, boundary: 6, camel: 4, consecutive: 5, gap: 1 } as const;

/** The score of `q`'s code units matched in `v` (folded as wanted) from
 * `from` on, taking each code unit at its first occurrence after the one
 * before; `undefined` when not all match. */
function greedy(v: string, original: string, q: string, from: number): { score: number; at: number[] } | undefined {
  const at: number[] = [];
  let score = 0;
  let i = from;
  for (let k = 0; k < q.length; k++) {
    const j = v.indexOf(q[k]!, i);
    if (j < 0) return undefined;
    score += SCORE.char;
    if (j === 0) score += SCORE.start;
    else if (BOUNDARY.has(v[j - 1]!)) score += SCORE.boundary;
    else if (isLower(original[j - 1]!) && isUpper(original[j]!)) score += SCORE.camel;
    const prev = at[at.length - 1];
    if (prev !== undefined && prev === j - 1) score += SCORE.consecutive;
    else if (prev !== undefined) score -= SCORE.gap;
    at.push(j);
    i = j + 1;
  }
  return { score, at };
}

const isLower = (c: string) => c !== c.toUpperCase() && c === c.toLowerCase();
const isUpper = (c: string) => c !== c.toLowerCase() && c === c.toUpperCase();

/** The best of the greedy matches of `q` in `v` starting at each
 * occurrence of its first code unit. */
function best(v: string, original: string, q: string): { score: number; at: number[] } | undefined {
  let top: { score: number; at: number[] } | undefined;
  for (let from = v.indexOf(q[0]!); from >= 0; from = v.indexOf(q[0]!, from + 1)) {
    const m = greedy(v, original, q, from);
    if (m === undefined) break; // nothing later matches either
    if (top === undefined || m.score > top.score) top = m;
  }
  return top;
}

/** Items whose value has each word of the query (split at white space) as
 * a subsequence, scored (`detail.score`, higher is better: matches at the
 * start, after a separator or a lower-case letter, and in a row score
 * more) and the matched code units decorated. A light matcher: each
 * occurrence of a word's first code unit is tried, the rest is taken
 * greedily. An empty query matches everything with score 0. */
export function fuzzy(o?: MatchOptions): Matcher<DetailUnit> {
  return {
    async *match<V extends DetailUnit>(
      { query, items }: { readonly query: string; readonly items: readonly Item<V>[] },
      { signal }: { readonly signal: AbortSignal },
    ) {
      const ic = ignores(o, query);
      const words = terms(ic ? fold(query) : query);
      for (const item of items) {
        if (signal.aborted) return;
        const value = ic ? fold(item.value) : item.value;
        let score = 0;
        const at: number[] = [];
        let ok = true;
        for (const w of words) {
          const m = best(value, item.value, w);
          if (m === undefined) {
            ok = false;
            break;
          }
          score += m.score;
          at.push(...m.at);
        }
        if (!ok) continue;
        const scored = { ...item, detail: { ...item.detail, score } } as Item<V & ScoreDetail>;
        yield decorate(scored, runs(at)) as Item<V>;
      }
    },
  };
}

/** By `detail.score`, highest first (items without one count as 0); stable,
 * so equal scores keep their order. Integer scores (the fuzzy matcher's)
 * are sorted by counting, without comparisons: the engine sorts every
 * match again as more come. */
export function byScore(): Sorter<DetailUnit> {
  return {
    sort({ items }) {
      const scores = items.map((i) => {
        const s = (i.detail as Partial<ScoreDetail>).score;
        return typeof s === "number" && !Number.isNaN(s) ? s : 0;
      });
      let lo = Number.POSITIVE_INFINITY;
      let hi = Number.NEGATIVE_INFINITY;
      let integers = true;
      for (const s of scores) {
        if (s < lo) lo = s;
        if (s > hi) hi = s;
        if (!Number.isInteger(s)) integers = false;
      }
      if (lo === hi) return;
      if (integers && hi - lo <= 4 * items.length + 1024) {
        // Stable counting sort, highest first.
        const count = new Array<number>(hi - lo + 2).fill(0);
        for (const s of scores) count[hi - s + 1]!++;
        for (let k = 1; k < count.length; k++) count[k]! += count[k - 1]!;
        const sorted = new Array<(typeof items)[number]>(items.length);
        for (let i = 0; i < items.length; i++) sorted[count[hi - scores[i]!]!++] = items[i]!;
        for (let i = 0; i < items.length; i++) items[i] = sorted[i]!;
        return;
      }
      const order = items.map((item, i) => ({ item, score: scores[i]! }));
      order.sort((a, b) => b.score - a.score);
      for (let i = 0; i < items.length; i++) items[i] = order[i]!.item;
    },
  };
}

/** Leaves the items as they are (label: value). */
export function identity(): Renderer<DetailUnit> {
  return { render() {} };
}
