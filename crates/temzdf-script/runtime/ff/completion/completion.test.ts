// temzdf/ff/completion (runtime/ff/completion/index.ts): fake sources
// completing in a text pane in the Vim insert mode and in a prompt
// (`temzdf test`, doc/dev/testing.md).
import { keymap, openPrompt } from "temzdf";
import type { Position } from "temzdf";
import * as completion from "temzdf/ff/completion";
import type { AddOptions, CompletionContext, CompletionItem, CompletionSource } from "temzdf/ff/completion";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

const NS = "completion-test";

/** A keyword source over `words`: its anchor is where the word before the
 * cursor starts; the same anchor keeps the context (prev), so it collects
 * once per anchor. `calls` counts its collections. */
function words(
  list: readonly (string | Partial<CompletionItem>)[],
  o: { renew?: boolean } = {},
): CompletionSource<{ n: number }> & { calls: number; queries: string[] } {
  const src = {
    calls: 0,
    queries: [] as string[],
    findContext(prev: CompletionContext<{ n: number }> | undefined) {
      const anchor = completion.wordStart();
      if (!o.renew && prev !== undefined && same(prev.anchor, anchor)) return prev;
      return { anchor, detail: { n: 0 } };
    },
    async *source() {
      src.calls++;
      for (const [i, w] of list.entries()) {
        yield typeof w === "string" ? { id: i, value: w, detail: {} } : { id: i, value: "", detail: {}, ...w };
      }
    },
  };
  return src;
}

const same = (a: Position, b: Position) => a.line === b.line && a.character === b.character;

function register(t: TestContext, src: completion.AnySource, o: Omit<AddOptions, "namespace"> = {}): void {
  t.run(() => completion.add(src, { ...o, namespace: NS }));
  t.after(() =>
    t.run(() => {
      completion.cancel();
      completion.clear({ namespace: NS });
    }),
  );
}

/** A text pane in the Vim insert mode at the end of `text`. */
async function insert(t: TestContext, text: string): Promise<number> {
  const pane = t.openText(text);
  await t.keys("GA");
  return pane;
}

/** The completion's popup lines ([] without a popup). */
function popup(t: TestContext): string[] {
  const p = t.run(() => completion.info()?.popup);
  if (p === undefined) return [];
  return t.action.text.getText(undefined, { pane: p }).split("\n");
}

const line = (t: TestContext) => t.action.text.line(t.action.text.cursor().line);

async function keys(t: TestContext, k: string): Promise<void> {
  await t.keys(k);
  await completion.whenIdle();
  await t.settle();
}

test("<C-n> starts, then selects: each candidate goes in; <C-e> puts the original back", async (assert, t) => {
  register(t, words(["apple", "apricot", "banana"]));
  await insert(t, "x ap");
  await keys(t, "<C-n>");
  assert.true(
    t.run(() => completion.active()),
    "started",
  );
  assert.true(
    t.run(() => completion.visible()),
    "the popup shows",
  );
  assert.deepEqual(popup(t), ["apple", "apricot"]);
  assert.equal(line(t), "x ap", "nothing went in yet");
  await keys(t, "<C-n>");
  assert.equal(line(t), "x apple");
  assert.deepEqual(t.action.text.cursor(), { line: 0, character: 7 });
  await keys(t, "<C-n>");
  assert.equal(line(t), "x apricot");
  await keys(t, "<C-n>");
  assert.equal(line(t), "x ap", "past the last: the original");
  await keys(t, "<C-p>");
  assert.equal(line(t), "x apricot", "back from the original: the last");
  await keys(t, "<C-e>");
  assert.equal(line(t), "x ap");
  assert.false(t.run(() => completion.active()));
  assert.equal(t.action.currentMode(), "vim/insert");
});

test("<C-y> keeps the candidate and runs the source's action with its context and range", async (assert, t) => {
  const seen: { value: string; anchor: Position; range: unknown; text: string }[] = [];
  const src = words(["apple", "apricot"]);
  register(t, src, {
    action: {
      invoke({ item, context, range, pane }, { action }) {
        seen.push({ value: item.value, anchor: context.anchor, range, text: action.text.getText(range, { pane }) });
      },
    },
  });
  await insert(t, "x ap");
  await keys(t, "<C-n><C-n><C-n>");
  assert.equal(line(t), "x apricot");
  await keys(t, "<C-y>");
  assert.false(t.run(() => completion.active()));
  assert.equal(line(t), "x apricot");
  assert.deepEqual(seen, [
    {
      value: "apricot",
      anchor: { line: 0, character: 2 },
      range: { start: { line: 0, character: 2 }, end: { line: 0, character: 9 } },
      text: "apricot",
    },
  ]);
  assert.equal(t.action.currentMode(), "vim/insert");
});

test("typing narrows: findContext answering prev keeps what was collected, a new context collects again", async (assert, t) => {
  const kept = words(["apple", "apricot", "avocado"]);
  register(t, kept);
  await insert(t, "a");
  await keys(t, "<C-n>");
  assert.deepEqual(popup(t), ["apple", "apricot", "avocado"]);
  await keys(t, "p");
  assert.deepEqual(popup(t), ["apple", "apricot"]);
  await keys(t, "r");
  assert.deepEqual(popup(t), ["apricot"]);
  assert.equal(kept.calls, 1, "collected once");
  await keys(t, "<BS>");
  assert.deepEqual(popup(t), ["apple", "apricot"], "a shorter query: matched again");
  assert.equal(kept.calls, 1);
  await keys(t, "<C-e>");

  const renewed = words(["apple", "apricot", "avocado"], { renew: true });
  register(t, renewed);
  t.run(() => completion.remove(kept));
  await keys(t, " a<C-n>");
  assert.equal(renewed.calls, 1);
  await keys(t, "p");
  assert.equal(renewed.calls, 2, "a new context: collected again");
  assert.deepEqual(popup(t), ["apple", "apricot"]);
});

test("a curator gets the query of the moment its context was found", async (assert, t) => {
  const queries: string[] = [];
  register(t, {
    findContext: (prev) => prev ?? { anchor: completion.wordStart(), detail: undefined },
    async *curator({ query }) {
      queries.push(query);
      yield { id: 1, value: `${query}-one`, detail: {} };
    },
  });
  await insert(t, "x ab");
  await keys(t, "<C-n>");
  assert.deepEqual(popup(t), ["ab-one"]);
  assert.deepEqual(queries, ["ab"]);
});

test("typing on after a candidate went in accepts it", async (assert, t) => {
  let accepted = "";
  register(t, words(["apple", "apricot"]), {
    action: {
      invoke({ item }) {
        accepted = item.value;
      },
    },
  });
  await insert(t, "x ap");
  await keys(t, "<C-n><C-n>");
  await keys(t, "s");
  assert.equal(line(t), "x apples");
  assert.false(t.run(() => completion.active()));
  assert.equal(accepted, "apple");
});

test("groups: a later group shows only when no earlier one has a match", async (assert, t) => {
  register(t, words(["apple"]), { group: 0 });
  register(t, words(["apricot", "avocado"]), { group: 1 });
  await insert(t, "a");
  await keys(t, "<C-n>");
  assert.deepEqual(popup(t), ["apple"], "group 0 has a match");
  await keys(t, "pr");
  assert.deepEqual(popup(t), ["apricot"], "group 0 has none left: group 1");
  await keys(t, "<BS>");
  assert.deepEqual(popup(t), ["apple"]);
});

test("sources of a group mix by priority, then in the order added", async (assert, t) => {
  register(t, words(["ab-first"]));
  register(t, words(["ab-high"]), { priority: 5 });
  register(t, words(["ab-second"]));
  await insert(t, "ab");
  await keys(t, "<C-n>");
  assert.deepEqual(popup(t), ["ab-high", "ab-first", "ab-second"]);
});

test("when: a source takes part only while it holds", async (assert, t) => {
  let on = false;
  register(t, words(["apple"]), { when: () => on });
  register(t, words(["apricot"]), { when: (ctx) => ctx.mode === "vim/insert" && ctx.paneKind === "edit" });
  await insert(t, "ap");
  await keys(t, "<C-n>");
  assert.deepEqual(popup(t), ["apricot"]);
  on = true;
  await keys(t, "<C-e><C-n>");
  assert.deepEqual(popup(t), ["apple", "apricot"]);
});

test("firstOf: the first source that applies", async (assert, t) => {
  const hash = {
    findContext: () => {
      const before = completion.lineBeforeCursor();
      return before.startsWith("#")
        ? { anchor: { line: completion.cursor().line, character: 1 }, detail: "hash" }
        : undefined;
    },
    async *source() {
      yield { id: 1, value: "include", detail: {} };
    },
  };
  register(t, completion.firstOf(hash, words(["word"])));
  await insert(t, "#");
  await keys(t, "<C-n>");
  assert.deepEqual(popup(t), ["include"]);
  await keys(t, "<C-n>");
  assert.equal(line(t), "#include");
  await keys(t, "<C-y><Esc>o");
  await keys(t, "<C-n>");
  assert.deepEqual(popup(t), ["word"]);
});

test("a candidate's range: it replaces that range and is narrowed by the text from its start", async (assert, t) => {
  register(
    t,
    words([
      {
        value: "x.apple",
        insertText: "X.APPLE",
        range: { start: { line: 0, character: 0 }, end: { line: 0, character: 4 } },
      },
      { value: "x.zebra", range: { start: { line: 0, character: 0 }, end: { line: 0, character: 4 } } },
      "apricot",
    ]),
  );
  await insert(t, "x.ap");
  await keys(t, "<C-n>");
  assert.deepEqual(popup(t), ["x.apple", "apricot"], "x.zebra does not match x.ap");
  await keys(t, "<C-n>");
  assert.equal(line(t), "X.APPLE");
  await keys(t, "<C-n>");
  assert.equal(line(t), "x.apricot");
  await keys(t, "<C-e>");
  assert.equal(line(t), "x.ap");
});

test("the default renderer: label, kind and description in columns", async (assert, t) => {
  register(
    t,
    words([
      { value: "apple", kind: "fruit", description: "red" },
      { value: "apricot", kind: "f" },
    ]),
    { sorter: undefined },
  );
  await insert(t, "ap");
  await keys(t, "<C-n>");
  assert.deepEqual(popup(t), ["apple    fruit  red", "apricot  f"]);
});

test("ends: <Esc> leaves the insert mode (the candidate stays)", async (assert, t) => {
  register(t, words(["apple"]));
  await insert(t, "ap");
  await keys(t, "<C-n>");
  await keys(t, "<C-n>");
  await keys(t, "<Esc>");
  assert.false(t.run(() => completion.active()));
  assert.equal(t.action.text.line(0), "apple");
  assert.equal(t.action.currentMode(), "vim/normal");
  assert.false(
    t.action.panes().some((p) => p.floating),
    "the popup went",
  );
});

test("ends: the cursor before the anchor, or past what was typed", async (assert, t) => {
  register(t, words(["apple"]));
  await insert(t, "x ap");
  await keys(t, "<C-n><Left>");
  assert.true(
    t.run(() => completion.active()),
    "within the range",
  );
  assert.deepEqual(popup(t), ["apple"], "narrowed by a, still");
  await keys(t, "<Left><Left>");
  assert.false(
    t.run(() => completion.active()),
    "before the anchor",
  );
  await keys(t, "<End><C-n><Left>");
  assert.true(t.run(() => completion.active()));
  await keys(t, "<Right>");
  assert.true(
    t.run(() => completion.active()),
    "back to where typing ended",
  );
  await keys(t, "<Home><End>");
  assert.false(
    t.run(() => completion.active()),
    "moved elsewhere",
  );
});

test("ends: a new anchor ends a completion started by hand; an automatic one goes on", async (assert, t) => {
  const manual = words(["apple", "banana"]);
  register(t, manual);
  await insert(t, "ap");
  await keys(t, "<C-n>");
  assert.true(t.run(() => completion.active()));
  await keys(t, " b");
  assert.false(
    t.run(() => completion.active()),
    "a new word: ended",
  );
  t.run(() => completion.clear({ namespace: NS }));

  register(t, words(["apple", "banana"]), { auto: true });
  await keys(t, " a");
  assert.deepEqual(
    t.run(() => completion.info()?.manual),
    false,
    "started by typing",
  );
  assert.deepEqual(popup(t), ["apple", "banana"]);
  await keys(t, " b");
  assert.true(
    t.run(() => completion.active()),
    "a new word: goes on",
  );
  assert.deepEqual(
    t.run(() => completion.info()?.anchors),
    [{ line: 0, character: 7 }],
  );
  assert.deepEqual(popup(t), ["banana"]);
});

test("automatic sources start from minLength characters", async (assert, t) => {
  register(t, words(["apple"]), { auto: true, minLength: 2 });
  await insert(t, "");
  await keys(t, "a");
  assert.false(t.run(() => completion.active()));
  await keys(t, "p");
  assert.true(t.run(() => completion.active()));
  assert.deepEqual(popup(t), ["apple"]);
});

test("ends: no source applies any more", async (assert, t) => {
  register(t, {
    findContext: (prev) =>
      completion.lineBeforeCursor().includes("!") ? undefined : (prev ?? { anchor: completion.wordStart(), detail: 0 }),
    async *source() {
      yield { id: 1, value: "apple", detail: {} };
    },
  });
  await insert(t, "ap");
  await keys(t, "<C-n>");
  assert.true(t.run(() => completion.active()));
  await keys(t, "!");
  assert.false(t.run(() => completion.active()));
});

test("the popup: below the anchor, sized to its lines; hidden while nothing matches", async (assert, t) => {
  register(t, words(["apple", "apricot", "apex"]), { sorter: undefined });
  const pane = await insert(t, "x ap");
  await keys(t, "<C-n>");
  const pos = t.action.text.screenPosition({ line: 0, character: 2 }, { pane })!;
  const float = () => t.action.panes().find((p) => p.id === t.run(() => completion.info()?.popup));
  assert.deepEqual(float()?.rect, { col: pos.col, row: pos.row + 1, width: 7, height: 3 });
  assert.false(float()?.focusable, "never takes the focus");
  assert.equal(t.action.focusedPane(), pane);
  await keys(t, "z");
  assert.true(
    t.run(() => completion.active()),
    "still completing",
  );
  assert.false(t.run(() => completion.visible()));
  assert.false(
    t.action.panes().some((p) => p.floating),
    "no popup",
  );
  await keys(t, "<BS>");
  assert.true(t.run(() => completion.visible()));
  assert.deepEqual(popup(t), ["apple", "apricot", "apex"]);
  await keys(t, "x");
  assert.deepEqual(float()?.rect.height, 1, "as high as its matches");
});

test("the popup goes above the anchor when it does not fit below", async (assert, t) => {
  register(t, words(["apple", "apricot", "apex"]));
  const text = `${Array.from({ length: 80 }, (_, i) => `line ${i}`).join("\n")}\nap`;
  const pane = await insert(t, text);
  await keys(t, "<C-n>");
  const pos = t.action.text.screenPosition({ line: 80, character: 0 }, { pane })!;
  const screen = t.action.screenSize();
  assert.true(pos.row + 3 >= screen.height, "the anchor is near the bottom");
  const float = t.action.panes().find((p) => p.id === t.run(() => completion.info()?.popup));
  assert.deepEqual(float?.rect, { col: pos.col, row: pos.row - 3, width: 7, height: 3 });
});

test("visible() and active() as mappings' conditions", async (assert, t) => {
  let hits = 0;
  t.run(() => keymap.add("vim/insert", "<F5>", () => hits++, { when: () => completion.visible(), namespace: NS }));
  t.after(() => t.run(() => keymap.clear({ namespace: NS })));
  register(t, words(["apple"]));
  await insert(t, "ap");
  await keys(t, "<F5>");
  assert.equal(hits, 0);
  await keys(t, "<C-n>");
  await keys(t, "<F5>");
  assert.equal(hits, 1);
  await keys(t, "z");
  await keys(t, "<F5>");
  assert.equal(hits, 1, "active but hidden");
  assert.true(t.run(() => completion.active()));
  // <C-y> and <C-e> are the mode's own without a completion.
  await keys(t, "<C-e>");
  assert.false(t.run(() => completion.active()));
});

test("in a prompt: select, cancel, accept; closing it ends", async (assert, t) => {
  register(t, words(["apple", "apricot"]));
  const p = t.run(() => openPrompt({ prompt: ">" }));
  assert.ok(p);
  await keys(t, "ap<C-n>");
  assert.equal(t.action.currentMode(), "input");
  assert.deepEqual(popup(t), ["apple", "apricot"]);
  await keys(t, "<C-n>");
  assert.equal(line(t), "apple");
  await keys(t, "<C-e>");
  assert.equal(line(t), "ap", "<C-e> cancels while completing");
  await keys(t, "<C-e>");
  assert.false(t.run(() => completion.active()));
  await keys(t, "<C-n><C-n><C-n>");
  await keys(t, "<C-y>");
  assert.equal(line(t), "apricot");
  await keys(t, "<C-n>");
  assert.true(t.run(() => completion.visible()));
  await keys(t, "<Esc>");
  assert.false(
    t.run(() => completion.active()),
    "the prompt closed",
  );
  assert.false(t.action.panes().some((p) => p.floating));
});

test("the candidates put in and taken out are one undo step", async (assert, t) => {
  register(t, words(["apple", "apricot"]));
  t.run(() => openPrompt({ prompt: ">" }));
  await keys(t, "ap<C-n>");
  await keys(t, "<C-n><C-n><C-p>");
  assert.equal(line(t), "apple");
  await keys(t, "<C-y>");
  t.action.text.undo();
  assert.equal(line(t), "ap", "the candidates");
  t.action.text.undo();
  assert.equal(line(t), "", "then what was typed");
});
