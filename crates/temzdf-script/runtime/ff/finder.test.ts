// Fuzzy finders (runtime/ff/finder.ts): defining and starting them, the
// prompt's keys, actions and the action picker, closing, :FF, and the
// shipped finders (`temzdf test`, doc/dev/testing.md). How the finder
// looks is crates/temzdf/tests/tui.rs' business.
import { command } from "temzdf/command";
import * as ff from "temzdf/ff";
import type { Action, Finder, FinderDefinition, Source } from "temzdf/ff";
import * as completion from "temzdf/ff/completion";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

const NS = "ff-finder-test";

/** A promise and what resolves it. */
function gate(): { wait: Promise<void>; open: () => void } {
  let open = () => {};
  const wait = new Promise<void>((r) => {
    open = r;
  });
  return { wait, open };
}

/** A source yielding `values` (after `wait`, if given). */
function fake(values: readonly string[], wait?: Promise<void>): Source {
  return {
    async *collect() {
      if (wait !== undefined) await wait;
      for (const [id, value] of values.entries()) yield { id, value, detail: {} };
    },
  };
}

/** What an action saw. */
interface Call {
  readonly action: string;
  readonly item: string | undefined;
  readonly marked: readonly string[];
  /** The pane focused while it ran. */
  readonly focused: number | undefined;
}

/** An action recording its calls into `calls`; `keep` returns true. */
function recorder(name: string, calls: Call[], keep = false): Action {
  return {
    invoke(p, o) {
      calls.push({
        action: name,
        item: p.item?.value,
        marked: p.marked.map((i) => i.value),
        focused: o.action.focusedPane(),
      });
      return keep ? true : undefined;
    },
  };
}

const FRUITS = ["apple", "banana", "cherry", "date", "elderberry"];

/** The finder `name` over `values` with actions `open` (default),
 * `other` and `keep` (returns true), gone after the test. */
function defineFruits(
  t: TestContext,
  name: string,
  calls: Call[],
  o: Partial<FinderDefinition> & { values?: readonly string[]; wait?: Promise<void> } = {},
): void {
  t.run(() =>
    ff.define(name, {
      source: fake(o.values ?? FRUITS, o.wait),
      // In the order given (fuzzy's scores tie on the empty query).
      actions: { open: recorder("open", calls), other: recorder("other", calls), keep: recorder("keep", calls, true) },
      defaultAction: "open",
      ...o,
    }),
  );
  t.after(() => t.run(() => ff.undefine(name)));
}

/** Wait until the finder showing (if any) wrote what it has, and a
 * completion its popup. */
async function idle(t: TestContext): Promise<void> {
  for (let i = 0; i < 5; i++) {
    const f = t.run(() => ff.active());
    if (f !== undefined) await f.whenIdle();
    await completion.whenIdle();
    await t.settle();
  }
}

/** Type `k` and wait until it is all shown. Keys acting on the list wait
 * for it to show the query typed before them, but text typed meanwhile
 * goes into the prompt at once: a test types an action picker's query
 * after the picker shows (keys(t, "q"); keys(t, "<C-a>name<CR>")). */
async function keys(t: TestContext, k: string): Promise<void> {
  await t.keys(k);
  await idle(t);
}

/** Start the finder `name` (closed after the test). */
async function start(t: TestContext, name: string, args: readonly string[] = []): Promise<Finder<object>> {
  const f = t.run(() => ff.start(name, args));
  t.after(() => t.run(() => f.close()));
  await idle(t);
  return f;
}

/** The lines of the finder's list ([] when it does not show). */
function lines(t: TestContext, f: Finder<object> | undefined): string[] {
  const pane = f?.list?.pane;
  if (pane === undefined) return [];
  const text = t.action.text.getText(undefined, { pane });
  return text === "" ? [] : text.split("\n");
}

const selected = (t: TestContext, f: Finder<object>) => t.run(() => f.list?.selected()?.value);

/** What closing must give back: panes, buffers and the hooks. */
function counts(t: TestContext) {
  const hooks = ["TextChanged", "PaneClosed", "PaneFocused", "PaneScrolled"] as const;
  return {
    panes: t.action.panes().length,
    buffers: t.action.buffers({ all: true }).length,
    hooks: hooks.map((h) => t.action.test.hooks(h)),
  };
}

test("a defined finder starts: a prompt with the focus, the list beside it, centred floats", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  const origin = t.openText("x\n");
  const f = await start(t, "fruits");
  assert.equal(
    t.run(() => ff.active()),
    f,
  );
  assert.deepEqual(lines(t, f), FRUITS);
  const prompt = f.prompt!;
  assert.equal(t.action.focusedPane(), prompt.pane, "the prompt has the focus");
  assert.true(t.run(() => ff.isFinder()));
  assert.equal(t.action.currentMode(), "input");
  assert.equal(f.origin, origin);
  const panes = t.action.panes();
  const p = panes.find((x) => x.id === prompt.pane)!;
  const l = panes.find((x) => x.id === f.list!.pane)!;
  assert.deepEqual([p.floating, l.floating, l.focusable], [true, true, false]);
  assert.equal(p.rect.height, 1);
  assert.equal(l.rect.row, p.rect.row + 2, "the list below the prompt and a rule");
  assert.deepEqual([l.rect.col, l.rect.width], [p.rect.col, p.rect.width]);
  const frame = panes.find((x) => x.floating && !x.focusable && x.id !== l.id && x.rect.row === p.rect.row);
  assert.notEqual(frame, undefined, "a frame around both");
  assert.deepEqual({ ...frame?.rect, height: 0 }, { ...p.rect, height: 0 }, "the frame's content starts at the prompt");
  assert.true((frame?.rect.height ?? 0) >= l.rect.height + 2, "the list inside it");
  assert.equal(l.rect.height, FRUITS.length, "the list as high as its lines");
  const screen = t.action.screenSize();
  const left = p.rect.col - 1;
  const right = screen.width - (p.rect.col + p.rect.width + 1);
  assert.true(Math.abs(left - right) <= 1, `centred: ${left} and ${right} columns beside it`);
  assert.equal(selected(t, f), "apple", "the first selected");
  assert.equal(
    t.run(() => ff.finders().find((x) => x.name === "fruits")?.name),
    "fruits",
  );
});

test("typing narrows the list to the query", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  t.openText("x\n");
  const f = await start(t, "fruits");
  await keys(t, "an");
  assert.equal(f.query, "an");
  assert.deepEqual(lines(t, f), ["banana"]);
  await keys(t, "<BS><BS>err");
  assert.deepEqual(lines(t, f).slice().sort(), ["cherry", "elderberry"]);
  await keys(t, "<C-u>");
  assert.deepEqual(lines(t, f).length, FRUITS.length, "the empty query matches everything");
});

test("moves and marks: <C-n>/<Down>, <C-p>/<Up>, <Tab> marks and moves on", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  t.openText("x\n");
  const f = await start(t, "fruits");
  await keys(t, "<C-n>");
  assert.equal(selected(t, f), "banana");
  await keys(t, "<Down><Down>");
  assert.equal(selected(t, f), "date");
  await keys(t, "<Up>");
  assert.equal(selected(t, f), "cherry");
  await keys(t, "<C-p><C-p><C-p>");
  assert.equal(selected(t, f), "apple", "stops at the top");
  await keys(t, "<Tab><Tab>");
  assert.equal(selected(t, f), "cherry");
  assert.deepEqual(
    t.run(() => f.session.markedItems().map((i) => i.value)),
    ["apple", "banana"],
  );
  await keys(t, "<C-p><Tab>");
  assert.deepEqual(
    t.run(() => f.session.markedItems().map((i) => i.value)),
    ["apple"],
    "<Tab> again unmarks",
  );
  assert.equal(t.action.text.line(0, { pane: f.prompt!.pane }), "", "nothing typed into the prompt");
});

test("<CR> closes the finder, then runs the default action in the pane it started from", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  const origin = t.openText("x\n");
  const before = counts(t);
  await start(t, "fruits");
  await keys(t, "<C-n><Tab><Tab><CR>");
  assert.deepEqual(calls, [{ action: "open", item: "date", marked: ["banana", "cherry"], focused: origin }]);
  assert.equal(
    t.run(() => ff.active()),
    undefined,
  );
  assert.equal(t.action.focusedPane(), origin);
  assert.deepEqual(counts(t), before, "no pane, buffer or hook left");
});

test("a named action; nothing to act on does nothing", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  const origin = t.openText("x\n");
  const f = await start(t, "fruits");
  await keys(t, "zzz");
  assert.deepEqual(lines(t, f), []);
  await keys(t, "<CR>");
  assert.deepEqual(calls, [], "no candidate: no action");
  assert.equal(
    t.run(() => ff.active()),
    f,
    "still open",
  );
  await keys(t, "<C-u>dat");
  t.run(() => f.accept("other"));
  await idle(t);
  assert.deepEqual(calls, [{ action: "other", item: "date", marked: [], focused: origin }]);
  assert.true(f.closed);
  assert.throws(() => t.run(() => ff.start("no-such-finder")), /no finder no-such-finder/);
});

test("an action returning true opens the finder again with its query and marks", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  t.openText("x\n");
  const f = await start(t, "fruits");
  await keys(t, "e<Tab>");
  const query = f.query;
  t.run(() => f.accept("keep"));
  await idle(t);
  assert.equal(calls.length, 1);
  assert.equal(
    t.run(() => ff.active()),
    f,
    "open again",
  );
  assert.true(f.shown);
  assert.equal(f.query, query);
  assert.equal(t.action.text.line(0, { pane: f.prompt!.pane }), query, "the query in the prompt");
  assert.equal(t.action.focusedPane(), f.prompt!.pane);
  assert.equal(
    t.run(() => f.session.markedItems().length),
    1,
  );
});

test("<C-a> picks the action from a finder over the actions; <Esc> there comes back", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  const origin = t.openText("x\n");
  const before = counts(t);
  const f = await start(t, "fruits");
  await keys(t, "ch<C-a>");
  const picker = t.run(() => ff.active());
  assert.notEqual(picker, f);
  assert.equal(picker?.name, "action");
  assert.false(f.shown, "the finder hides meanwhile");
  assert.deepEqual(lines(t, picker), ["open", "other", "keep"]);
  await keys(t, "<Esc>");
  assert.equal(
    t.run(() => ff.active()),
    f,
    "back",
  );
  assert.equal(f.query, "ch");
  assert.deepEqual(lines(t, f), ["cherry"]);
  await keys(t, "<C-a>oth<CR>");
  assert.deepEqual(calls, [{ action: "other", item: "cherry", marked: [], focused: origin }]);
  assert.equal(
    t.run(() => ff.active()),
    undefined,
  );
  assert.deepEqual(counts(t), before, "both finders gone");
});

test("closing gives back every pane, buffer and hook: <Esc>, the focus elsewhere, the prompt closed", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  const origin = t.openText("x\n");
  const before = counts(t);
  for (const close of ["<Esc>", "focus", "pane"]) {
    const f = await start(t, "fruits");
    assert.notDeepEqual(counts(t), before, `${close}: the finder's own`);
    if (close === "focus") t.action.focusPane(origin);
    else if (close === "pane") t.action.closePane(f.prompt!.pane);
    else await t.keys(close);
    await idle(t);
    assert.true(f.closed, `${close}: closed`);
    assert.true(f.session.disposed, `${close}: the session disposed`);
    assert.deepEqual(counts(t), before, `${close}: nothing left`);
    assert.equal(t.action.focusedPane(), origin, `${close}: the focus back`);
  }
  assert.deepEqual(calls, []);
});

test("starting a finder closes the one showing", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  defineFruits(t, "veg", calls, { values: ["kale", "leek"] });
  t.openText("x\n");
  const before = counts(t);
  const a = await start(t, "fruits");
  const b = await start(t, "veg");
  assert.true(a.closed);
  assert.deepEqual(lines(t, b), ["kale", "leek"]);
  t.run(() => b.close());
  await idle(t);
  assert.deepEqual(counts(t), before);
});

test("keys typed before the list shows the query wait for it", async (assert, t) => {
  const calls: Call[] = [];
  const g = gate();
  defineFruits(t, "slow", calls, { wait: g.wait });
  const origin = t.openText("x\n");
  // (start() would wait for the source.)
  const f = t.run(() => ff.start("slow"));
  t.after(() => t.run(() => f.close()));
  await t.settle();
  assert.deepEqual(lines(t, f), [], "nothing collected yet");
  // As typed ahead: a move and <CR> before any line.
  await t.keys("<C-n><CR>");
  assert.deepEqual(calls, [], "<CR> waits");
  assert.false(f.closed);
  g.open();
  await idle(t);
  assert.deepEqual(calls, [{ action: "open", item: "banana", marked: [], focused: origin }], "then both apply");
});

test(":FF NAME starts a finder; finder names complete in the command line", async (assert, t) => {
  const calls: Call[] = [];
  defineFruits(t, "fruits", calls);
  t.openText("x\n");
  await keys(t, ":FF fr<Tab>");
  const p = t.run(() => completion.info()?.popup);
  assert.notEqual(p, undefined, "a popup");
  const names = t.action.text
    .getText(undefined, { pane: p! })
    .split("\n")
    .map((l) => l.split(/\s+/)[0]);
  assert.deepEqual(names, ["fruits"]);
  await keys(t, "<Tab>");
  assert.equal(t.action.text.line(t.action.text.cursor().line), "FF fruits");
  await keys(t, "<CR>");
  const f = t.run(() => ff.active());
  t.after(() => t.run(() => f?.close()));
  assert.equal(f?.name, "fruits");
  assert.deepEqual(lines(t, f), FRUITS);
  await keys(t, "<Esc>");
  const r = await t.keys(":FF nothing<CR>", { allowErrors: true });
  assert.true(
    r.errors.some((e) => e.includes("no finder nothing")),
    `an error: ${r.errors.join(" | ")}`,
  );
});

test("buffers: the listed buffers; open in the pane, split, delete", async (assert, t) => {
  const origin = t.openText("x\n");
  const buffer = t.action.newBuffer({ path: "ff-alpha.txt" });
  t.after(() => {
    if (t.action.buffers({ all: true }).some((b) => b.id === buffer)) t.action.deleteBuffer(buffer);
  });
  const originBuffer = t.action.text.info({ pane: origin }).buffer;
  let f = await start(t, "buffers");
  const names = t.action.buffers().map((b) => b.name);
  assert.equal(lines(t, f).length, names.length, "each listed buffer");
  assert.true(lines(t, f).includes("ff-alpha.txt"));
  await keys(t, "ff-alpha<CR>");
  assert.equal(t.action.text.info({ pane: origin }).buffer, buffer, "shown in the pane it started from");
  t.action.showBuffer(originBuffer, origin);

  const panes = t.action.panes().length;
  f = await start(t, "buffers");
  await keys(t, "ff-alpha");
  await keys(t, "<C-a>vsplit<CR>");
  assert.equal(t.action.panes().length, panes + 1, "a new pane");
  const fresh = t.action.focusedPane()!;
  assert.equal(t.action.text.info({ pane: fresh }).buffer, buffer);
  t.action.closePane(fresh);
  t.action.focusPane(origin);

  f = await start(t, "buffers");
  await keys(t, "ff-alpha");
  await keys(t, "<C-a>delete<CR>");
  assert.false(
    t.action.buffers({ all: true }).some((b) => b.id === buffer),
    "deleted",
  );
  assert.true(f.closed);
});

test("lines: the current buffer's lines, numbered; jump puts the cursor on the line", async (assert, t) => {
  const origin = t.openText("apple\n  banana\ncherry\n");
  const f = await start(t, "lines");
  assert.deepEqual(lines(t, f), ["1  apple", "2    banana", "3  cherry"]);
  await keys(t, "nan<CR>");
  assert.equal(t.action.focusedPane(), origin);
  assert.deepEqual(t.action.text.cursor({ pane: origin }), { line: 1, character: 2 }, "its first non-blank");
});

test("commands: the commands there with their descriptions; execute runs one, edit opens a command line", async (assert, t) => {
  const ran: string[] = [];
  t.run(() =>
    command.add("Fftestcmd", (c) => ran.push(`${c.name}:${c.args}`), { namespace: NS, desc: "a test command" }),
  );
  t.after(() => t.run(() => command.clear({ namespace: NS })));
  t.openText("x\n");
  let f = await start(t, "commands");
  const all = lines(t, f);
  assert.true(
    all.some((l) => /^Fftestcmd +a test command$/.test(l)),
    "the name and its description",
  );
  assert.true(
    all.some((l) => l.startsWith("FF ")),
    ":FF itself",
  );
  await keys(t, "Fftestcmd<CR>");
  assert.deepEqual(ran, ["Fftestcmd:"]);

  f = await start(t, "commands");
  await keys(t, "Fftestcmd");
  await keys(t, "<C-a>edit<CR>");
  assert.true(f.closed);
  assert.equal(t.action.text.line(t.action.text.cursor().line), "Fftestcmd ", "a command line with the name");
  await keys(t, "arg<CR>");
  assert.deepEqual(ran, ["Fftestcmd:", "Fftestcmd: arg"]);
});
