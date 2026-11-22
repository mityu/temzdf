// The parts of the shipped fuzzy finders (temzdf/ff defines them as
// `buffers`, `lines` and `commands`): sources reading the session when
// the finder starts (before their first `await`, where the plain `action`
// works), renderers and actions. Actions run once the finder's panes are
// closed, with the focus back on the pane the finder started from.
import { action, openPrompt } from "temzdf";
import type { Actions, PaneKind } from "temzdf";
import { CMDLINE_VAR, command } from "temzdf/command";
import type { Action, Decoration, Detail, InvokeParams, Item, Renderer, Source } from "temzdf/ff";

/** The items an action acts on: the marked ones, else the cursor's. */
function chosen<T extends Detail>(p: InvokeParams<T>): readonly Item<T>[] {
  if (p.marked.length > 0) return p.marked;
  return p.item === undefined ? [] : [p.item];
}

/** Decorations moved `by` code units (a renderer put something before). */
const shift = (ds: readonly Decoration[], by: number): Decoration[] => ds.map((d) => ({ ...d, start: d.start + by }));

/** An error as a message (the commands' and the sessions' refusals). */
function report(e: unknown): void {
  action.message(e instanceof Error ? e.message : String(e), { error: true });
}

// --- Buffers -------------------------------------------------------------

export interface BufferDetail {
  readonly buffer: number;
  readonly kind: PaneKind;
  readonly modified: boolean;
}

/** The listed buffers (`buffers()`), by name. */
export function bufferSource(): Source<BufferDetail> {
  return {
    collect() {
      const bufs = action.buffers();
      return (async function* () {
        for (const b of bufs) {
          yield { id: b.id, value: b.name, detail: { buffer: b.id, kind: b.kind, modified: b.modified } };
        }
      })();
    },
  };
}

/** The name, `[+]` after a buffer with unsaved changes, the kind of one
 * that is not text. */
export function bufferRenderer(): Renderer<BufferDetail> {
  return {
    render({ items }) {
      for (const i of items) {
        const extra = [i.detail.modified ? "[+]" : "", i.detail.kind === "edit" ? "" : `(${i.detail.kind})`];
        const tail = extra.filter((x) => x !== "").join(" ");
        if (tail !== "") i.label = `${i.label} ${tail}`;
      }
    },
  };
}

/** `open` (the first chosen, in the pane the finder started from),
 * `split` and `vsplit` (each chosen in a new pane below or beside),
 * `delete` (each chosen; one with unsaved changes is refused). */
export function bufferActions(): Record<"open" | "split" | "vsplit" | "delete", Action<BufferDetail>> {
  const each = (fn: (act: Actions, buffer: number) => void): Action<BufferDetail> => ({
    invoke(p, o) {
      for (const i of chosen(p)) fn(o.action, i.detail.buffer);
    },
  });
  return {
    open: {
      invoke(p, o) {
        const first = chosen(p)[0];
        if (first !== undefined) o.action.showBuffer(first.detail.buffer);
      },
    },
    split: each((act, buffer) => act.split({ side: "bottom", buffer })),
    vsplit: each((act, buffer) => act.split({ side: "right", buffer })),
    delete: {
      invoke(p, o) {
        for (const i of chosen(p)) {
          try {
            o.action.deleteBuffer(i.detail.buffer);
          } catch (e) {
            // A hook's refusal (unsaved changes).
            o.atomic(() => report(e));
          }
        }
      },
    },
  };
}

// --- Lines ---------------------------------------------------------------

export interface LineDetail {
  /** The pane whose buffer the line is in. */
  readonly pane: number;
  /** 0-based. */
  readonly line: number;
  /** How many lines the buffer has (for the numbers' width). */
  readonly count: number;
}

/** The lines of the text buffer in the focused pane (the one the finder
 * starts from); nothing for another kind of pane. */
export function lineSource(): Source<LineDetail> {
  return {
    collect() {
      const info = action.panes().find((p) => p.focused);
      const pane = info?.id;
      const lines =
        pane === undefined || info?.kind !== "edit" ? [] : action.text.getText(undefined, { pane }).split("\n");
      // The newline ending the last line ends no line of its own.
      if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
      const count = lines.length;
      return (async function* () {
        if (pane === undefined) return;
        for (const [line, value] of lines.entries()) yield { id: line, value, detail: { pane, line, count } };
      })();
    },
  };
}

/** The line's number before it. */
export function lineNumberRenderer(): Renderer<LineDetail> {
  return {
    render({ items }) {
      for (const i of items) {
        const width = String(i.detail.count).length;
        const head = `${String(i.detail.line + 1).padStart(width)}  `;
        i.label = head + i.label;
        i.decorations = shift(i.decorations, head.length);
      }
    },
  };
}

/** `jump`: the cursor to the line's first non-blank. */
export function lineActions(): Record<"jump", Action<LineDetail>> {
  return {
    jump: {
      invoke(p, o) {
        const i = p.item;
        if (i === undefined) return;
        const character = /^\s*/.exec(i.value)![0].length;
        o.action.text.setCursor({ line: i.detail.line, character }, { pane: i.detail.pane });
      },
    },
  };
}

// --- Commands ------------------------------------------------------------

export interface CommandDetail {
  readonly name: string;
  readonly desc: string | undefined;
}

/** The Ex commands that exist in the focused pane (command.list with
 * `live`), each name once, sorted. */
export function commandSource(): Source<CommandDetail> {
  return {
    collect() {
      const pane = action.focusedPane();
      const byName = new Map<string, string | undefined>();
      // In the order defined: the last one of a name wins (as running it).
      for (const c of command.list({ live: true, pane })) byName.set(c.name, c.desc);
      const names = [...byName.keys()].sort();
      return (async function* () {
        for (const name of names) yield { id: name, value: name, detail: { name, desc: byName.get(name) } };
      })();
    },
  };
}

/** The name, and the description after the names written at once. */
export function commandRenderer(): Renderer<CommandDetail> {
  return {
    render({ items }) {
      const width = Math.max(0, ...items.map((i) => i.label.length));
      for (const i of items) {
        if (i.detail.desc === undefined || i.detail.desc === "") continue;
        i.label = `${i.label.padEnd(width)}  ${i.detail.desc}`;
      }
    },
  };
}

/** `execute` runs `:NAME`; `edit` opens a command line with `NAME ` in it,
 * for the arguments. */
export function commandActions(): Record<"execute" | "edit", Action<CommandDetail>> {
  return {
    execute: {
      invoke(p, o) {
        const name = p.item?.detail.name;
        if (name === undefined) return;
        o.atomic(() => {
          try {
            command.execute(name);
          } catch (e) {
            report(e);
          }
        });
      },
    },
    edit: {
      invoke(p, o) {
        const name = p.item?.detail.name;
        if (name === undefined) return;
        o.atomic(() => {
          const line = openPrompt({
            prompt: ":",
            text: `${name} `,
            onLine: (l) => {
              try {
                command.execute(l);
              } catch (e) {
                report(e);
              }
            },
          });
          if (line !== undefined) action.setVar("buffer", CMDLINE_VAR, ":", line.buffer);
        });
      },
    },
  };
}
