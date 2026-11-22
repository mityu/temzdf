// temzdf/ff (runtime/ff/compose.ts): defining and combining parts
// (`temzdf test`, doc/dev/testing.md).
import type { Actions } from "temzdf";
import {
  chosenItems,
  composeActions,
  composeMatchers,
  composeRefiners,
  composeSorters,
  composeSources,
  defineAction,
  defineCurator,
  defineMatcher,
  defineRefiner,
  defineSorter,
  defineSource,
  refineCurator,
  refineSource,
} from "temzdf/ff";
import type { Item } from "temzdf/ff";
import { prefix, substring } from "temzdf/ff/builtin";
import { test } from "temzdf/test";

const signal = new AbortController().signal;

async function all<T>(xs: AsyncIterable<T>): Promise<T[]> {
  const out: T[] = [];
  for await (const x of xs) out.push(x);
  return out;
}

const words = defineSource<{ n: number }>(async function* () {
  for (const [i, value] of ["alpha", "beta", "gamma"].entries()) yield { id: i, value, detail: { n: i } };
});

test("refineSource pipes the refiners, each adding to the detail", async (assert) => {
  const length = defineRefiner<{ n: number }, { len: number }>(async function* ({ items }) {
    for await (const item of items) yield { ...item, detail: { ...item.detail, len: item.value.length } };
  });
  const dropShort = defineRefiner<{ len: number }>(async function* ({ items }) {
    for await (const item of items) if (item.detail.len > 4) yield item;
  });
  const source = refineSource(words, length, dropShort);
  const got = await all(source.collect({ args: [] }, { signal }));
  // The detail keeps the source's own fields and the refiners' (typed).
  assert.deepEqual(
    got.map((i) => [i.value, i.detail.n, i.detail.len]),
    [
      ["alpha", 0, 5],
      ["gamma", 2, 5],
    ],
  );
  const curator = refineCurator(
    defineCurator<{ n: number }>(async function* ({ query }) {
      yield { id: 0, value: query, detail: { n: 0 } };
    }),
    composeRefiners(length),
  );
  assert.deepEqual(
    (await all(curator.curate({ args: [], query: "abc" }, { signal }))).map((i) => i.detail),
    [{ n: 0, len: 3 }],
  );
});

test("composeSources: one after the other, ids unique and stable", async (assert) => {
  const other = defineSource<{ n: number }>(async function* () {
    yield { id: 0, value: "delta", detail: { n: 9 } };
  });
  const both = composeSources(words, other);
  const got = await all(both.collect({ args: [] }, { signal }));
  assert.deepEqual(
    got.map((i) => [i.id, i.value]),
    [
      ["[0,0]", "alpha"],
      ["[0,1]", "beta"],
      ["[0,2]", "gamma"],
      ["[1,0]", "delta"],
    ],
  );
  const again = await all(both.collect({ args: [] }, { signal }));
  assert.deepEqual(
    again.map((i) => i.id),
    got.map((i) => i.id),
  );
});

test("composeMatchers: each narrows what the one before matched", async (assert) => {
  const seen: string[][] = [];
  const spy = defineMatcher(async function* ({ items }) {
    seen.push(items.map((i) => i.value));
    yield* items;
  });
  const m = composeMatchers(prefix(), spy, substring());
  const items: Item[] = ["set", "search", "reset"].map((value, id) => ({ id, value, detail: {} }));
  const got = await all(m.match({ query: "se", items }, { signal }));
  assert.deepEqual(seen, [["set", "search"]], "the second got only the first's matches");
  assert.deepEqual(
    got.map((i) => i.decorations?.length),
    [2, 2],
    "decorations from both",
  );
});

test("composeSorters: one after the other", async (assert) => {
  const items: Item<{ k: number }>[] = [3, 1, 2].map((k) => ({ id: k, value: String(k), detail: { k } }));
  const order: string[] = [];
  const asc = defineSorter<{ k: number }>(({ items }) => {
    order.push("asc");
    items.sort((a, b) => a.detail.k - b.detail.k);
  });
  const rev = defineSorter<{ k: number }>(async ({ items }) => {
    order.push("rev");
    items.reverse();
  });
  await composeSorters(asc, rev).sort({ items }, { signal });
  assert.deepEqual(
    items.map((i) => i.id),
    [3, 2, 1],
  );
  assert.deepEqual(order, ["asc", "rev"]);
});

test("composeActions: every action runs; any true keeps the session", async (assert) => {
  const ran: string[] = [];
  const a = defineAction(() => {
    ran.push("a");
  });
  const b = defineAction(async () => {
    ran.push("b");
    return true as const;
  });
  const p = { item: undefined, marked: [], matched: [] };
  const o = { signal, action: {} as Actions };
  assert.equal(await composeActions(b, a).invoke(p, o), true);
  assert.equal(await composeActions(a, a).invoke(p, o), undefined);
  assert.deepEqual(ran, ["b", "a", "a", "a"]);
});

test("chosenItems: the marked ones, else the cursor's", (assert) => {
  const x: Item = { id: 1, value: "x", detail: {} };
  const y: Item = { id: 2, value: "y", detail: {} };
  assert.deepEqual(chosenItems({ item: x, marked: [y], matched: [] }), [y]);
  assert.deepEqual(chosenItems({ item: x, marked: [], matched: [] }), [x]);
  assert.deepEqual(chosenItems({ item: undefined, marked: [], matched: [] }), []);
});
