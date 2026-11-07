// Registers, on the session's copy history (doc/design/registers.md), so
// what any pane copies is in them:
//   "       the newest entry
//   0       the newest copy without a register
//   1-9     the n-th newest delete of a line or more, without a register
//   -       the newest delete within a line, without a register
//   a-z     named registers (A-Z append)
//   + *     the system clipboard (read asynchronously: see normal.ts)
//   _       nothing is stored
import { cmd, type Copied } from "temzdf";

export type Kind = "char" | "line" | "block";

export interface Register {
  text: string;
  kind: Kind;
}

const plain = (e: Copied) => e.register === null;
const multiLine = (e: Copied) => e.kind === "line" || e.text.includes("\n");
const reg = (e: Copied | null | undefined): Register | undefined =>
  e === null || e === undefined ? undefined : { text: e.text, kind: e.kind };

/** Whether `name` is the system clipboard (read with readClipboard). */
export function isClipboard(name: string | null): boolean {
  return name === "+" || name === "*";
}

/** A register's content (not the clipboard's: see `isClipboard`). */
export function get(name: string | null): Register | undefined {
  const n = name ?? '"';
  if (n === '"') return reg(cmd.history({ limit: 1 })[0]);
  if (n === "_") return undefined;
  if (n === "0") return reg(cmd.history().find((e) => e.how === "copy" && plain(e)));
  if (/^[1-9]$/.test(n)) {
    return reg(cmd.history().filter((e) => e.how === "delete" && plain(e) && multiLine(e))[Number(n) - 1]);
  }
  if (n === "-") return reg(cmd.history().find((e) => e.how === "delete" && plain(e) && !multiLine(e)));
  return reg(cmd.register(n.toLowerCase()));
}

/** What the clipboard's `text` is: the kind it was copied as here, else
 * lines when it ends with a line break (as Vim). */
export function fromClipboard(text: string): Register {
  const known = cmd.history().find((e) => e.text === text);
  if (known !== undefined) return { text, kind: known.kind };
  return { text, kind: text.endsWith("\n") ? "line" : "char" };
}

/** Store what a yank (`yank`) or a delete or change took. */
export function store(name: string | null, r: Register, how: "yank" | "delete"): void {
  if (name === "_") return;
  const opts = { kind: r.kind, how: how === "yank" ? ("copy" as const) : ("delete" as const) };
  if (name !== null && /^[A-Z]$/.test(name)) {
    cmd.copyText(r.text, { ...opts, register: name.toLowerCase(), append: true });
  } else if (name !== null && /^[a-z]$/.test(name)) {
    cmd.copyText(r.text, { ...opts, register: name });
  } else if (isClipboard(name)) {
    cmd.copyText(r.text, { ...opts, clipboard: true });
  } else {
    // ", or a numbered one named: as the unnamed register.
    cmd.copyText(r.text, opts);
  }
}

/** Whether `c` names a register. */
export function isName(c: string): boolean {
  return /^[a-zA-Z0-9"\-_+*]$/.test(c);
}
