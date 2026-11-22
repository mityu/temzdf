// temzdf/ff/builtin (runtime/ff/builtin/index.ts): the shipped matchers,
// sorter and renderer (`temzdf test`, doc/dev/testing.md).
import type { Item, Matcher } from "temzdf/ff";
import { byScore, fuzzy, identity, MATCH_STYLE, prefix, substring } from "temzdf/ff/builtin";
import { test } from "temzdf/test";

const items = (...values: string[]): Item[] => values.map((value, id) => ({ id, value, detail: {} }));

async function run(m: Matcher, query: string, values: readonly string[]): Promise<Item[]> {
  const out: Item[] = [];
  const signal = new AbortController().signal;
  for await (const i of m.match({ query, items: items(...values) }, { signal })) out.push(i);
  return out;
}

/** Each match's value and its decorations as [start, length]. */
const shown = (got: readonly Item[]) =>
  got.map((i) => [i.value, (i.decorations ?? []).map((d) => [d.start, d.length])] as const);

test("substring: every word, case ignored, decorated", async (assert) => {
  const got = await run(substring(), "bar FO", ["foo bar", "bar", "Foobar baz", "fo"]);
  assert.deepEqual(shown(got), [
    [
      "foo bar",
      [
        [0, 2],
        [4, 3],
      ],
    ],
    [
      "Foobar baz",
      [
        [0, 2],
        [3, 3],
      ],
    ],
  ]);
  assert.equal(got[0]?.decorations?.[0]?.style, MATCH_STYLE);
  assert.equal((await run(substring({ ignoreCase: false }), "Foo", ["foo", "Foo"])).length, 1);
  assert.deepEqual(
    (await run(substring({ ignoreCase: "smart" }), "foo", ["foo", "Foo"])).map((i) => i.value),
    ["foo", "Foo"],
    "smart: lower case ignores case",
  );
  assert.deepEqual(
    (await run(substring({ ignoreCase: "smart" }), "Foo", ["foo", "Foo"])).map((i) => i.value),
    ["Foo"],
    "smart: upper case respects it",
  );
  assert.equal((await run(substring(), "", ["a", "b"])).length, 2, "an empty query matches all");
});

test("positions count UTF-16 code units", async (assert) => {
  assert.deepEqual(shown(await run(substring(), "bar", ["🍣 sushi Bar"])), [["🍣 sushi Bar", [[9, 3]]]]);
  assert.deepEqual(shown(await run(substring(), "テキ", ["日本語のテキスト"])), [["日本語のテキスト", [[4, 2]]]]);
  // İ lower-cases to two code units: it stays as it is, the rest keeps its place.
  assert.deepEqual(shown(await run(substring(), "stan", ["İstanbul"])), [["İstanbul", [[1, 4]]]]);
  assert.deepEqual(shown(await run(fuzzy(), "sb", ["🍣 sushi Bar"])), [
    [
      "🍣 sushi Bar",
      [
        [3, 1],
        [9, 1],
      ],
    ],
  ]);
});

test("prefix: the start of the value only", async (assert) => {
  const got = await run(prefix(), "se", ["set", "Search", "reset", "s"]);
  assert.deepEqual(shown(got), [
    ["set", [[0, 2]]],
    ["Search", [[0, 2]]],
  ]);
  assert.deepEqual(shown(await run(prefix(), "", ["a"])), [["a", []]]);
});

test("fuzzy: subsequences, scored, matched code units decorated", async (assert) => {
  const got = await run(fuzzy(), "fb", ["foo_bar", "xfxxbx", "bf", "fooBar"]);
  assert.deepEqual(
    got.map((i) => i.value),
    ["foo_bar", "xfxxbx", "fooBar"],
  );
  const score = (v: string) => (got.find((i) => i.value === v)?.detail as { score?: number } | undefined)?.score ?? 0;
  assert.ok(score("foo_bar") > score("fooBar"), "after a separator beats camel case");
  assert.ok(score("fooBar") > score("xfxxbx"), "camel case beats the middle of a word");
  assert.deepEqual(shown(got.slice(0, 1)), [
    [
      "foo_bar",
      [
        [0, 1],
        [4, 1],
      ],
    ],
  ]);
  const run1 = await run(fuzzy(), "ab", ["xab", "a_b"]);
  assert.ok(
    ((run1[0]?.detail as { score?: number } | undefined)?.score ?? 0) > 0 && run1.length === 2,
    "consecutive and boundary matches both count",
  );
  assert.deepEqual(shown(await run(fuzzy(), "abc", ["ab-abc"])), [["ab-abc", [[3, 3]]]], "the better start wins");
});

test("byScore: highest first, stable, unscored as 0", async (assert) => {
  const list: Item[] = [
    { id: 1, value: "a", detail: { score: 1 } },
    { id: 2, value: "b", detail: {} },
    { id: 3, value: "c", detail: { score: 5 } },
    { id: 4, value: "d", detail: { score: 1 } },
  ];
  await byScore().sort({ items: list }, { signal: new AbortController().signal });
  assert.deepEqual(
    list.map((i) => i.id),
    [3, 1, 4, 2],
  );
});

test("identity leaves the items alone", async (assert) => {
  const list = [{ id: 1, value: "a", detail: {}, label: "a", decorations: [] }];
  await identity().render({ items: list }, { signal: new AbortController().signal });
  assert.deepEqual(list, [{ id: 1, value: "a", detail: {}, label: "a", decorations: [] }]);
});
