// Fuzzy finders (doc/design/completion.md, "The fuzzy finder"): a finder
// is defined by name (`define`) and started by it (`start`, `:FF NAME
// ARGS`). Its screen is two floats centred on the screen: a prompt one row
// high at the top, whose text is the query, and below it the list
// (createList) without the focus. Keys typed in the prompt move the
// selection, mark, accept or pick an action (the keyset `finderKeys`).
//
// Accepting closes the finder's panes first and gives the focus back to
// the pane it was started from, then runs the action there (an action
// returning `true` opens the finder again, its query and marks kept).
// Keys acting on the list typed before it shows the current query (typed
// ahead) wait for it.
import { action, dispatch, on, schedule } from "temzdf";
import type { KeyContext, TaskContext } from "temzdf";
import { splitArgs } from "temzdf/command";
import {
  bufferActions,
  bufferRenderer,
  bufferSource,
  byScore,
  commandActions,
  commandRenderer,
  commandSource,
  fuzzy,
  lineActions,
  lineNumberRenderer,
  lineSource,
} from "temzdf/ff/builtin";
import { keyset } from "temzdf/keyset";
import { composeMatchers, composeRenderers, composeSorters } from "./compose";
import { createList } from "./list";
import type { List } from "./list";
import { createSession } from "./session";
import type { Session } from "./session";
import type {
  Action,
  Curator,
  Detail,
  DetailUnit,
  InvokeParams,
  Matcher,
  Previewer,
  Renderer,
  Sorter,
  Source,
} from "./types";

/** What `define` takes: a source (collects once) or a curator (collects
 * again for each query), how its items are matched, sorted and shown, and
 * what accepting does. */
export interface FinderDefinition<T extends Detail = DetailUnit> {
  readonly source?: Source<T>;
  readonly curator?: Curator<T>;
  /** Applied in turn, each narrowing what the one before matched (default: `fuzzy()`). */
  readonly matchers?: readonly Matcher<T>[];
  /** Applied in turn (default: `byScore()`). */
  readonly sorters?: readonly Sorter<T>[];
  /** Applied in turn on the lines written (default: the value as it is). */
  readonly renderers?: readonly Renderer<T>[];
  /** Kept for later: nothing previews yet. */
  readonly previewers?: readonly Previewer<T>[];
  /** Named actions (the action picker lists them). */
  readonly actions: Readonly<Record<string, Action<T>>>;
  /** The action accepting runs (a name in `actions`). */
  readonly defaultAction: string;
  /** Shown beside the name in `:FF`'s completion. */
  readonly desc?: string;
}

/** What `start` takes besides the arguments. */
export interface StartOptions {
  /** The first query (""). */
  readonly query?: string;
}

/** The highlight group of the prompt's marker. */
const PROMPT_STYLE = "Prompt";
const PROMPT = "> ";
/** The highlight group of the rule under the prompt. */
const RULE_STYLE = "FFBorder";

const registry = new Map<string, FinderDefinition<Detail>>();

function checkDefinition(name: unknown, def: unknown): asserts def is FinderDefinition<Detail> {
  const what = `ff.define(${String(name)})`;
  if (typeof name !== "string" || !/^\S+$/.test(name)) throw new TypeError(`${what}: a name without blanks`);
  if (typeof def !== "object" || def === null) throw new TypeError(`${what}: a definition object`);
  const d = def as Partial<FinderDefinition<Detail>>;
  if ((d.source === undefined) === (d.curator === undefined)) {
    throw new TypeError(`${what}: a source or a curator (one of them)`);
  }
  if (typeof d.actions !== "object" || d.actions === null) throw new TypeError(`${what}: actions`);
  if (typeof d.defaultAction !== "string" || !(d.defaultAction in d.actions)) {
    throw new TypeError(`${what}: defaultAction names one of the actions`);
  }
}

/** Define the finder `name` (again: replaces it). */
export function define<T extends Detail = DetailUnit>(name: string, def: FinderDefinition<T>): void {
  checkDefinition(name, def);
  registry.set(name, def as unknown as FinderDefinition<Detail>);
}

/** Forget the finder `name`. */
export function undefine(name: string): void {
  registry.delete(name);
}

/** The finders defined, by name (sorted), with their descriptions. */
export function finders(): { name: string; desc?: string }[] {
  return [...registry.entries()]
    .map(([name, d]) => ({ name, desc: d.desc }))
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}

let current: Finder<Detail> | undefined;

/** The finder showing now (undefined: none). */
export function active(): Finder<Detail> | undefined {
  return current;
}

/** Whether `pane` (a mapping's context's, the focused one) is a finder's
 * prompt: for `when`. */
export function isFinder(pane?: number | KeyContext): boolean {
  const id = typeof pane === "object" ? pane.pane : (pane ?? action.focusedPane());
  return current !== undefined && current.prompt !== undefined && current.prompt.pane === id;
}

/** Start the finder `name` with `args` (its source's arguments), closing
 * the one showing. Call it where the plain `action` works; the source
 * starts collecting at once, so a source reads what it needs of the
 * session (the focused pane: the one the finder starts from) before its
 * first `await`. */
export function start(name: string, args: readonly string[] = [], o: StartOptions = {}): Finder<Detail> {
  const def = registry.get(name);
  if (def === undefined) throw new Error(`ff: no finder ${name}`);
  return open(name, def, args, o);
}

/** A finder over `def` without defining it (the action picker's). */
function open<T extends Detail>(
  name: string,
  def: FinderDefinition<T>,
  args: readonly string[],
  o: StartOptions & { onCancel?: () => void } = {},
): Finder<T> {
  current?.close();
  const f = new Finder<T>(name, def, args, o.query ?? "", o.onCancel);
  f.show();
  return f;
}

/** Where the finder goes: the frame's content (a row for the prompt, a
 * rule, the list's rows), in openFloat's cells. */
interface Layout {
  /** The frame's top left (its border's). */
  readonly col: number;
  readonly row: number;
  /** The content's width. */
  readonly width: number;
  /** The list's rows. */
  readonly list: number;
}

/** Centred, 80% of the screen wide and high (at least the smallest frame
 * that fits, and never more than the screen). */
function layout(): Layout {
  const { width: W, height: H } = action.screenSize();
  const outer = Math.min(W, Math.max(Math.min(W, 40), Math.round(W * 0.8)));
  const total = Math.min(H, Math.max(Math.min(H, 8), Math.round(H * 0.8)));
  return {
    col: Math.max(0, Math.floor((W - outer) / 2)),
    row: Math.max(0, Math.floor((H - total) / 2)),
    width: Math.max(10, outer - 2),
    list: Math.max(2, total - 4),
  };
}

/** Run `fn` where the plain `action` works, from anywhere (a timer, a
 * task, a hook). */
function later(fn: () => void): void {
  dispatch((ctx) => {
    ctx.atomic(fn);
  });
}

const paneAlive = (pane: number) => action.panes().some((p) => p.id === pane);
const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/** A finder: its session, and while it shows its prompt and list. */
export class Finder<T extends Detail = DetailUnit> {
  readonly name: string;
  readonly args: readonly string[];
  readonly definition: FinderDefinition<T>;
  readonly session: Session<T>;
  /** The pane focused when it started, where actions run. */
  readonly origin: number | undefined;
  readonly #onCancel: (() => void) | undefined;
  readonly #ctrl = new AbortController();
  #list: List<T> | undefined;
  #prompt: { buffer: number; pane: number } | undefined;
  /** The bordered float around the prompt and the list (a rule between). */
  #frame: { buffer: number; pane: number } | undefined;
  #hooks: (() => void)[] = [];
  #closed = false;
  /** List operations typed before the list showed the query. */
  #pending: ((list: List<T>) => void)[] = [];
  #flushing = false;

  constructor(name: string, def: FinderDefinition<T>, args: readonly string[], query: string, onCancel?: () => void) {
    this.name = name;
    this.args = args;
    this.definition = def;
    this.#onCancel = onCancel;
    this.origin = action.focusedPane();
    this.session = createSession<T>({
      sources: [(def.source ?? def.curator)!],
      args,
      query,
      matcher: composeMatchers<T>(...(def.matchers ?? [fuzzy() as Matcher<T>])),
      sorter: composeSorters<T>(...(def.sorters ?? [byScore() as Sorter<T>])),
    });
    // Before its panes open: the focused pane is still the origin.
    this.session.start();
  }

  /** The list while the finder shows. */
  get list(): List<T> | undefined {
    return this.#list;
  }

  /** The prompt's buffer and pane while the finder shows. */
  get prompt(): { readonly buffer: number; readonly pane: number } | undefined {
    return this.#prompt;
  }

  get closed(): boolean {
    return this.#closed;
  }

  /** Whether its panes show. */
  get shown(): boolean {
    return this.#prompt !== undefined;
  }

  /** The query: the prompt's text. */
  get query(): string {
    return this.session.query;
  }

  /** Open its panes (again), the query in the prompt: a bordered float
   * with the finder's name and counts as its title, and over its content a
   * prompt row, a rule and the list, as floats without a border. */
  show(): void {
    if (this.#closed || this.shown) return;
    if (current !== undefined && current !== (this as unknown as Finder<Detail>)) current.close();
    current = this as unknown as Finder<Detail>;
    const l = layout();
    const inner = { col: l.col + 1, row: l.row + 1 };
    const plain = { wrap: false, number: "off", cursorline: false, scrolloff: 0 } as const;
    // Blank lines below the rule: a text pane draws `~` past its last line.
    const frameText = `\n${"─".repeat(l.width)}${"\n".repeat(l.list)}`;
    const frameBuffer = action.newBuffer({ text: frameText, attrs: ["nofile", "unlisted"] });
    const frame = action.openFloat(
      { buffer: frameBuffer },
      { col: l.col, row: l.row, width: l.width, height: l.list + 2, focus: false, focusable: false },
    );
    if (frame === undefined) {
      action.deleteBuffer(frameBuffer);
      throw new Error("ff: no float for the finder");
    }
    this.#frame = { buffer: frameBuffer, pane: frame };
    action.statusLine(false, frame);
    action.text.setOptions({ ...plain, highlights: { CursorUnfocused: "None" } }, { pane: frame });
    action.text.addDecoration(
      { start: { line: 1, character: 0 }, end: { line: 1, character: l.width } },
      { style: RULE_STYLE, namespace: "ff-frame", pane: frame },
    );
    const def = this.definition;
    this.#list = createList<T>({
      session: this.session,
      open: (buf) =>
        action.openFloat(
          { buffer: buf },
          {
            col: inner.col,
            row: inner.row + 2,
            width: l.width,
            // As high as its lines (#fit), from none.
            height: 1,
            border: false,
            focus: false,
            focusable: false,
          },
        ),
      renderer: def.renderers === undefined ? undefined : composeRenderers<T>(...def.renderers),
      // As for the frame's full height: the float shrinks to the lines.
      initial: 3 * l.list,
      margin: l.list,
      onChange: (list) => {
        this.#fit(list, l.list);
        this.#title();
        this.#later();
      },
    });
    const buffer = action.newBuffer({ text: this.session.query, basemode: "input", attrs: ["nofile", "unlisted"] });
    const pane = action.openFloat(
      { buffer },
      { col: inner.col, row: inner.row, width: l.width, height: 1, border: false, focus: true },
    );
    if (pane === undefined) {
      action.deleteBuffer(buffer);
      this.#prompt = { buffer, pane: -1 };
      this.hide();
      throw new Error("ff: no float for the prompt");
    }
    this.#prompt = { buffer, pane };
    action.statusLine(false, pane);
    action.text.setOptions(plain, { pane });
    const zero = { line: 0, character: 0 };
    action.text.addDecoration(
      { start: zero, end: zero },
      { virtText: [{ text: PROMPT, style: PROMPT_STYLE }], namespace: "ff-prompt", pane },
    );
    action.text.setCursor({ line: 0, character: this.session.query.length }, { pane });
    const mine = (p: number) => p === this.#prompt?.pane || p === this.#list?.pane || p === this.#frame?.pane;
    this.#hooks = [
      on("TextChanged", (buf) => {
        if (buf === this.#prompt?.buffer) schedule(() => this.#sync());
      }),
      on("PaneClosed", (p) => {
        if (mine(p)) schedule(() => this.close());
      }),
      // The focus elsewhere (a click, a key moving it) closes it.
      on("PaneFocused", (p) => {
        if (this.#prompt !== undefined && p !== this.#prompt.pane) {
          schedule(() => {
            if (this.#prompt !== undefined && action.focusedPane() !== this.#prompt.pane) this.close();
          });
        }
      }),
      // In a timer: the counts in the title, keys waiting for a query
      // that matched nothing.
      this.session.onChange(() =>
        later(() => {
          this.#title();
          this.#flush();
        }),
      ),
    ];
    this.#title();
  }

  /** Close its panes, keeping the session (show() opens them again). */
  hide(): void {
    if (!this.shown) return;
    for (const off of this.#hooks) off();
    this.#hooks = [];
    this.#pending = [];
    const prompt = this.#prompt!;
    const frame = this.#frame;
    const list = this.#list;
    this.#prompt = undefined;
    this.#frame = undefined;
    this.#list = undefined;
    if (current === (this as unknown as Finder<Detail>)) current = undefined;
    if (this.origin !== undefined && paneAlive(this.origin)) action.focusPane(this.origin);
    list?.dispose();
    for (const w of [prompt, frame]) {
      if (w === undefined) continue;
      if (paneAlive(w.pane)) action.closePane(w.pane);
      if (action.buffers({ all: true }).some((b) => b.id === w.buffer)) action.deleteBuffer(w.buffer);
    }
  }

  /** Close it for good: its panes, buffers and hooks, the session;
   * running actions are aborted. */
  close(): void {
    if (this.#closed) return;
    this.hide();
    this.#closed = true;
    this.#ctrl.abort();
    this.session.dispose();
  }

  /** Close it, as <Esc> does (the action picker goes back to its finder). */
  cancel(): void {
    this.close();
    this.#onCancel?.();
  }

  /** Resolves once the list shows everything the session published and
   * no typed-ahead key waits. */
  async whenIdle(): Promise<void> {
    for (;;) {
      const list = this.#list;
      if (list === undefined) return;
      await list.whenIdle();
      if (this.#pending.length === 0 && !this.#flushing) return;
      await tick();
    }
  }

  next(count = 1): void {
    this.#op((l) => l.next(count));
  }

  prev(count = 1): void {
    this.#op((l) => l.prev(count));
  }

  page(count = 1): void {
    this.#op((l) => l.page(count));
  }

  first(): void {
    this.#op((l) => l.first());
  }

  last(): void {
    this.#op((l) => l.last());
  }

  /** Mark the selected candidate (unmark it), then select the next. */
  toggleMark(): void {
    this.#op((l) => {
      l.toggleMark();
      l.next();
    });
  }

  /** Run the action `name` (the default one) on the selected candidate
   * and the marked ones. With neither, nothing happens. */
  accept(name?: string): void {
    const key = name ?? this.definition.defaultAction;
    const act = this.definition.actions[key];
    if (act === undefined) throw new Error(`ff: ${this.name} has no action ${key}`);
    this.#op((l) => {
      const p = l.params();
      if (p.item === undefined && p.marked.length === 0) return;
      this.#run(act, p);
    });
  }

  /** Pick the action from a finder over the actions (its `<Esc>` comes
   * back here). */
  pickAction(): void {
    this.#op((l) => {
      const p = l.params();
      if (p.item === undefined && p.marked.length === 0) return;
      this.hide();
      const names = Object.keys(this.definition.actions);
      const self = this;
      open(
        "action",
        {
          source: {
            async *collect() {
              for (const [i, value] of names.entries()) yield { id: i, value, detail: {} };
            },
          },
          sorters: [byScore()],
          actions: {
            invoke: {
              async invoke(pp, o) {
                const chosen = pp.item?.value;
                const act = chosen === undefined ? undefined : self.definition.actions[chosen];
                if (act === undefined) return;
                await self.#invokeIn(act, p, o);
              },
            },
          },
          defaultAction: "invoke",
        },
        [],
        { onCancel: () => this.show() },
      );
    });
  }

  /** Run `op` on the list now if it shows the query, else once it does. */
  #op(op: (list: List<T>) => void): void {
    if (!this.shown) return;
    this.#sync();
    this.#pending.push(op);
    this.#flush();
  }

  /** The list shows the current query: its first lines are written, or
   * none matched and everything is collected. */
  #ready(list: List<T>): boolean {
    const v = list.view();
    if (v.query !== this.session.query) return false;
    if (v.written > 0) return true;
    return v.matched === 0 && v.state === "done";
  }

  #later(): void {
    if (this.#pending.length === 0 || this.#flushing) return;
    this.#flushing = true;
    later(() => {
      this.#flushing = false;
      this.#flush();
    });
  }

  /** Run the waiting operations while the list is ready. */
  #flush(): void {
    while (this.#pending.length > 0) {
      const list = this.#list;
      if (list === undefined || list.disposed) {
        this.#pending = [];
        return;
      }
      if (!this.#ready(list)) return;
      this.#pending.shift()!(list);
    }
  }

  /** The prompt's text as the query. */
  #sync(): void {
    const prompt = this.#prompt;
    if (prompt === undefined || !paneAlive(prompt.pane)) return;
    const text = action.text.getText(undefined, { pane: prompt.pane }).replace(/\r?\n/g, " ");
    this.session.setQuery(text);
    this.#later();
  }

  /** The list's float as high as its lines (at most `rows`, one at
   * least): no `~` past them. */
  #fit(list: List<T>, rows: number): void {
    const pane = list.pane;
    if (pane === undefined) return;
    const height = Math.max(1, Math.min(rows, list.written().length));
    const now = action.panes().find((p) => p.id === pane)?.rect.height;
    if (now !== undefined && now !== height) action.floatConfig(pane, { height });
  }

  /** The frame's title: the name, matched / collected, marked, still
   * collecting. */
  #title(): void {
    const pane = this.#frame?.pane;
    if (pane === undefined || !paneAlive(pane)) return;
    const snap = this.session.snapshot({ end: 0 });
    const marked = snap.marked.length > 0 ? ` (${snap.marked.length} marked)` : "";
    const busy = snap.state === "done" ? "" : " ...";
    action.floatConfig(pane, { title: `${this.name}  ${snap.matched}/${snap.collected}${marked}${busy}` });
  }

  /** Close the panes, then run `act` in a dispatched task; it shows the
   * finder again when the action returns `true`, else closes it. */
  #run(act: Action<T>, p: InvokeParams<T>): void {
    this.hide();
    dispatch(async (ctx) => {
      await this.#invokeIn(act, p, ctx);
    });
  }

  async #invokeIn(act: Action<T>, p: InvokeParams<T>, ctx: TaskContext): Promise<void> {
    let keep = false;
    try {
      keep = (await act.invoke(p, { signal: this.#ctrl.signal, action: ctx.action, atomic: ctx.atomic })) === true;
    } finally {
      if (keep && !this.#closed) ctx.atomic(() => this.show());
      else this.close();
    }
  }
}

// --- The shipped finders ---------------------------------------------------

define("buffers", {
  source: bufferSource(),
  renderers: [bufferRenderer()],
  actions: bufferActions(),
  defaultAction: "open",
  desc: "the listed buffers: open, split, vsplit, delete",
});
define("lines", {
  source: lineSource(),
  renderers: [lineNumberRenderer()],
  actions: lineActions(),
  defaultAction: "jump",
  desc: "the lines of the current buffer: jump",
});
define("commands", {
  source: commandSource(),
  renderers: [commandRenderer()],
  actions: commandActions(),
  defaultAction: "execute",
  desc: "Ex commands: execute, edit",
});

// --- Keys and :FF ------------------------------------------------------------

/** The keys of a finder's prompt (namespace builtin-ff-finder, mode
 * `input` when `isFinder`, priority 1 over the mode's own): `<C-n>` /
 * `<Down>` and `<C-p>` / `<Up>` move, `<PageDown>` / `<PageUp>` a screen,
 * `<Tab>` marks and moves on, `<CR>` runs the default action, `<C-a>`
 * picks the action, `<Esc>` closes (`<C-c>` cannot be mapped). */
export const finderKeys = keyset("builtin-ff-finder", (d) => {
  const o = { when: (ctx: KeyContext) => isFinder(ctx), priority: 1 };
  d.map("input", ["<C-n>", "<Down>"], () => active()?.next(), { ...o, desc: "finder: next" });
  d.map("input", ["<C-p>", "<Up>"], () => active()?.prev(), { ...o, desc: "finder: previous" });
  d.map("input", ["<PageDown>"], () => active()?.page(1), { ...o, desc: "finder: a screen down" });
  d.map("input", ["<PageUp>"], () => active()?.page(-1), { ...o, desc: "finder: a screen up" });
  d.map("input", ["<Tab>"], () => active()?.toggleMark(), { ...o, desc: "finder: mark, next" });
  d.map("input", ["<CR>"], () => active()?.accept(), { ...o, desc: "finder: default action" });
  d.map("input", ["<C-a>"], () => active()?.pickAction(), { ...o, desc: "finder: pick an action" });
  d.map("input", ["<Esc>"], () => active()?.cancel(), { ...o, desc: "finder: close" });
});

/** `:FF NAME ARGS` starts a finder (names complete in the command line;
 * namespace builtin-ff). */
export const finderCommand = keyset("builtin-ff", (d) => {
  d.command(
    "FF",
    ({ args }) => {
      const [name, ...rest] = splitArgs(args.trim());
      if (name === undefined || name === "") throw new Error("ff: :FF NAME [ARGS]");
      start(name, rest);
    },
    {
      desc: "start a fuzzy finder",
      complete: ({ args }) => {
        if (/\s/.test(args.trimStart())) return undefined;
        return { items: finders().map((f) => ({ value: f.name, description: f.desc })), match: "prefix" };
      },
    },
  );
});
