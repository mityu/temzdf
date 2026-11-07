// temzdf's Vim mode (doc/design/vim-mode.md), temzdf/edit/vim: nothing
// happens on import; `vim.enable()` defines its modes, mappings (namespace
// builtin-edit-vim) and hook and makes text panes start in it (o.basemode),
// `vim.disable()` takes them away (defaults.ts enables it). Vim's editing
// as a script on the text editor framework's primitives, as Evil is in
// Emacs: modes vim/normal, vim/insert, vim/visual, and the transient
// vim/pending (an operator waiting for its motion), vim/char (the
// character of f, t, r), vim/register ("x) and vim/cmdline (/ ? :).
import { cmd, defineMode, isPrompt, keymap, o, schedule } from "temzdf";
import type { Define } from "../lib/keyset";
import { keyset } from "../lib/keyset";
import type { KeyInfo, MapContext } from "temzdf";
import { backspace, cancelCmdline, openCmdline, runCmdline, searchAgain, searchWord } from "./cmdline";
import { abandonInsert, endInsert, insertRegister } from "./insert";
import { aliases, findChar, motions } from "./motions";
import {
  cancelPending,
  insert,
  join,
  leaveTransient,
  lineOperator,
  putRegister,
  repeatLast,
  replaceChars,
  runMotion,
  runTextObject,
  scroll,
  selectRegister,
  shorthand,
  startOperator,
  tilde,
  undo,
  visualInsert,
  visualReplace,
} from "./normal";
import { options, state } from "./state";
import type { MotionFn, Operator } from "./state";
import { textObjectKeys } from "./textobjects";
import { confirmKey, ex } from "./ex";
import { isRecording, runMacro, startRecording, stopRecording } from "./macros";
import { isMarkName, jump, markPos, setMark } from "./marks";
import { cursor, firstNonBlank, t } from "./util";
import { draw, enterVisual, exitVisual, reselect, swapEnds } from "./visual";

const NAMESPACE = "builtin-edit-vim";
const NS = { namespace: NAMESPACE };

/** A key as the key notation writes it (`<` is <lt>, `|` is <Bar>). */
function lhs(k: string): string {
  return k.startsWith("<") && k.length > 1 && k.endsWith(">") ? k : k.replace(/</g, "<lt>").replace(/\|/g, "<Bar>");
}

type Fn = (ctx: MapContext) => void;

function map(modes: string[], keys: string[], fn: Fn): void {
  for (const m of modes) for (const k of keys) keymap.add(m, k, fn, NS);
}

const N = "vim/normal";
const P = "vim/pending";
const V = "vim/visual";
const I = "vim/insert";

/** Define the modes, mappings and hook (vim.enable()). */
function define(hook: Define["hook"]): void {
  defineMode(N, { fallback: "drop", count: true });
  defineMode(I, { fallback: "passthrough", count: false });
  defineMode(V, { fallback: "drop", count: true });
  defineMode(P, { transient: true, count: true, fallback: () => cancelPending() });
  // Digits are what they read (f1, r1, "1), not counts.
  defineMode("vim/char", { transient: true, count: false, fallback: (_ctx, key) => readChar(key) });
  defineMode("vim/register", { transient: true, count: false, fallback: (_ctx, key) => selectRegister(key) });
  // <C-r> in the insert mode reads a register's name.
  defineMode("vim/insreg", { transient: true, count: false, fallback: (_ctx, key) => insertRegister(key) });
  // :s///c reads its answers one key at a time.
  defineMode("vim/confirm", { transient: false, count: false, fallback: (_ctx, key) => confirmKey(key) });
  // The input mode's editing keys below it work.
  defineMode("vim/cmdline", { transient: true, keepalive: "always" });
  o.basemode = { ...o.basemode, edit: N };

  // Motions, in the normal, operator-pending and visual modes.
  const motionKeys = Object.keys(motions);
  for (const k of [...motionKeys, ...Object.keys(aliases)]) {
    const name = aliases[k] ?? k;
    const fn: MotionFn = motions[name];
    map([N, P, V], [lhs(k)], (ctx) => runMotion(name, fn, ctx.count ?? null));
  }

  // f F t T and r read one character.
  let charFor: null | { kind: "f" | "F" | "t" | "T" | "r" | "m" | "'" | "`" | "q" | "@"; count: number | null } = null;

  function readChar(key: KeyInfo): void {
    const what = charFor;
    charFor = null;
    // vim/char goes; an operator's vim/pending below it stays for the motion.
    cmd.exitMode();
    if (what === null || key.keys === "<Esc>") {
      cancelPending();
      return;
    }
    if (what.kind === "r") {
      if (state.visual !== null) visualReplace(key);
      else replaceChars(key, what.count);
      return;
    }
    if (what.kind === "q") {
      if (key.text !== null) startRecording(key.text);
      return;
    }
    if (what.kind === "@") {
      if (key.text !== null) runMacro(key.text, what.count, ex);
      return;
    }
    if (what.kind === "m" || what.kind === "'" || what.kind === "`") {
      markKey(what.kind, key.text ?? "", what.count);
      return;
    }
    const ch = key.text;
    if (ch === null || ch === "") {
      cancelPending();
      return;
    }
    const kind = what.kind;
    state.lastFind = { kind, char: ch };
    runMotion(kind, (from, n) => findChar(from, kind, ch, n ?? 1), what.count);
  }

  for (const kind of ["f", "F", "t", "T"] as const) {
    map([N, P, V], [kind], (ctx) => {
      charFor = { kind, count: ctx.count ?? null };
      cmd.switchMode("vim/char");
    });
  }
  map([N], ["r"], (ctx) => {
    charFor = { kind: "r", count: ctx.count ?? null };
    cmd.switchMode("vim/char");
  });

  // Marks: m{x} sets one; '{x} goes to its line, `{x} to it (motions).
  map([N], ["m"], () => {
    charFor = { kind: "m", count: null };
    cmd.switchMode("vim/char");
  });
  for (const kind of ["'", "`"] as const) {
    map([N, P, V], [kind], (ctx) => {
      charFor = { kind, count: ctx.count ?? null };
      cmd.switchMode("vim/char");
    });
  }

  // Macros: q{reg} records, q stops; @{reg} runs one, @@ the last, @: the
  // last Ex command.
  map([N], ["q"], () => {
    if (isRecording()) {
      stopRecording();
      return;
    }
    charFor = { kind: "q", count: null };
    cmd.switchMode("vim/char");
  });
  map([N], ["@"], (ctx) => {
    charFor = { kind: "@", count: ctx.count ?? null };
    cmd.switchMode("vim/char");
  });

  function markKey(kind: "m" | "'" | "`", name: string, count: number | null): void {
    if (kind === "m") {
      cancelPending();
      setMark(name);
      return;
    }
    if (!isMarkName(name)) {
      cancelPending();
      return;
    }
    runMotion(kind, (from) => {
      // Another buffer's mark: shown first, without an operator only.
      const p = markPos(name, state.pending === null && state.visual === null);
      if (p === null) throw new Error("E20: Mark not set");
      void from;
      return kind === "'" ? { to: firstNonBlank(p.line), kind: "linewise" } : { to: p, kind: "exclusive" };
    }, count);
  }

  // Operators.
  const operators: [string, Operator][] = [
    ["d", "d"],
    ["c", "c"],
    ["y", "y"],
    [">", ">"],
    ["<lt>", "<"],
    ["g~", "g~"],
    ["gu", "gu"],
    ["gU", "gU"],
  ];
  for (const [key, op] of operators) {
    map([N, V], [key], (ctx) => startOperator(op, ctx.count ?? null));
  }
  // The operator again: its lines (dd, yy, g~~ or g~g~, guu, gUU).
  for (const [key, op] of [...operators, ["~", "g~"], ["u", "gu"], ["U", "gU"]] as [string, Operator][]) {
    map([P], [key], (ctx) => lineOperator(op, ctx.count ?? null));
  }
  map([P], ["<Esc>"], () => cancelPending());
  for (const k of textObjectKeys) map([P, V], [lhs(k)], (ctx) => runTextObject(k, ctx.count ?? null));

  // The normal mode's commands.
  for (const k of ["i", "a", "I", "A", "o", "O"] as const) map([N], [k], (ctx) => insert(k, ctx.count ?? null));
  for (const k of ["x", "X", "D", "C", "s", "S", "Y"] as const) map([N], [k], (ctx) => shorthand(k, ctx.count ?? null));
  map([N], ["<Del>"], (ctx) => shorthand("x", ctx.count ?? null));
  map([N, V], ["p"], (ctx) => putRegister(true, ctx.count ?? null));
  map([N, V], ["P"], (ctx) => putRegister(false, ctx.count ?? null));
  map([N], ["J"], (ctx) => join(ctx.count ?? null));
  map([N], ["~"], (ctx) => tilde(ctx.count ?? null));
  map([N], ["u"], (ctx) => undo(false, ctx.count ?? null));
  map([N], ["<C-r>"], (ctx) => undo(true, ctx.count ?? null));
  map([N], ["."], (ctx) => repeatLast(ctx.count ?? null));
  map([N, V], ['"'], () => cmd.switchMode("vim/register"));
  map([N], ["v"], () => enterVisual("char"));
  map([N], ["V"], () => enterVisual("line"));
  map([N], ["<C-v>"], () => enterVisual("block"));
  map([N], ["gv"], () => reselect());
  map([N], ["<Esc>"], () => {
    state.register = null;
  });
  map([N, V], ["/"], (ctx) => openCmdline("/", ctx.count ?? null));
  map([N, V], ["?"], (ctx) => openCmdline("?", ctx.count ?? null));
  map([P], ["/"], (ctx) => {
    leaveTransient();
    openCmdline("/", ctx.count ?? null);
  });
  map([P], ["?"], (ctx) => {
    leaveTransient();
    openCmdline("?", ctx.count ?? null);
  });
  map([N, V], [":"], (ctx) => openCmdline(":", ctx.count ?? null));
  map([N, V, P], ["n"], (ctx) => searchAgain(false, ctx.count ?? null));
  map([N, V, P], ["N"], (ctx) => searchAgain(true, ctx.count ?? null));
  map([N, V, P], ["*"], (ctx) => searchWord(false, ctx.count ?? null));
  map([N, V, P], ["#"], (ctx) => searchWord(true, ctx.count ?? null));
  map([N, V], ["<C-d>"], (ctx) => scroll("half", true, ctx.count ?? null));
  map([N, V], ["<C-u>"], (ctx) => scroll("half", false, ctx.count ?? null));
  map([N, V], ["<C-f>", "<PageDown>"], (ctx) => scroll("page", true, ctx.count ?? null));
  map([N, V], ["<C-b>", "<PageUp>"], (ctx) => scroll("page", false, ctx.count ?? null));
  map([N, V], ["<C-e>"], (ctx) => scroll("line", true, ctx.count ?? null));
  map([N, V], ["<C-y>"], (ctx) => scroll("line", false, ctx.count ?? null));
  map([N], ["<C-o>"], (ctx) => jump(-(ctx.count ?? 1)));
  map([N], ["<C-i>", "<Tab>"], (ctx) => jump(ctx.count ?? 1));
  map([N], ["zz"], () => t.reveal(cursor(), "center"));
  map([N], ["zt"], () => t.reveal(cursor(), "top"));

  // The visual mode's own keys.
  map([V], ["<Esc>"], () => exitVisual());
  map([V], ["v"], () => enterVisual("char"));
  map([V], ["V"], () => enterVisual("line"));
  map([V], ["<C-v>"], () => enterVisual("block"));
  map([V], ["o"], () => swapEnds());
  map([V], ["x", "<Del>"], () => startOperator("d", null));
  map([V], ["s"], () => startOperator("c", null));
  map([V], ["~"], () => startOperator("g~", null));
  map([V], ["u"], () => startOperator("gu", null));
  map([V], ["U"], () => startOperator("gU", null));
  map([V], ["I"], () => visualInsert("I"));
  map([V], ["A"], () => visualInsert("A"));
  map([V], ["r"], () => {
    charFor = { kind: "r", count: null };
    cmd.switchMode("vim/char");
  });
  map([V], ["J"], () => {
    const v = state.visual!;
    const c = cursor();
    const first = Math.min(v.anchor.line, c.line);
    const n = Math.max(v.anchor.line, c.line) - first + 1;
    exitVisual();
    t.setCursor({ line: first, character: 0 });
    join(Math.max(2, n));
  });

  // The insert mode.
  map([I], ["<Esc>", "<C-[>"], () => endInsert());
  map([I], ["<BS>", "<C-h>"], () => t.erase("grapheme", -1));
  map([I], ["<Del>"], () => t.erase("grapheme", 1));
  map([I], ["<CR>", "<C-j>", "<C-m>"], () => t.type("\n"));
  map([I], ["<Tab>", "<C-i>"], () => t.type(options.expandtab ? " ".repeat(options.shiftwidth) : "\t"));
  map([I], ["<C-w>"], () => t.erase("wordStart", -1));
  map([I], ["<C-u>"], () => {
    const c = cursor();
    t.delete({ start: { line: c.line, character: 0 }, end: c });
  });
  map([I], ["<C-r>"], () => cmd.switchMode("vim/insreg"));
  map([I], ["<Left>"], () => t.move("grapheme", -1));
  map([I], ["<Right>"], () => t.move("grapheme", 1));
  map([I], ["<Up>"], () => t.move("displayLine", -1));
  map([I], ["<Down>"], () => t.move("displayLine", 1));
  map([I], ["<Home>"], () => t.move("lineStart"));
  map([I], ["<End>"], () => t.move("lineEnd"));

  // The command line.
  map(["vim/cmdline"], ["<CR>"], () => runCmdline());
  map(["vim/cmdline"], ["<Esc>"], () => cancelCmdline());
  map(["vim/cmdline"], ["<BS>", "<C-h>"], () => backspace());

  // The prefix reaches temzdf's normal mode, as from other panes.
  map([N, V, I], ["<C-b>"], () => cmd.switchMode("normal"));

  // The cursor's shape follows the mode; what a focus change interrupts is
  // closed (hooks only read: these wait for the safe point).
  hook("ModeChanged", (old: string, now: string) => {
    schedule(() => {
      const focused = cmd.panes().find((p) => p.focused);
      // Not a text pane, or a prompt (the command line): not the Vim mode's.
      if (focused === undefined || focused.kind !== "edit" || isPrompt(focused.buffer)) return;
      // vim/insreg (<C-r>) is a key read inside the insert mode.
      const inserting = (m: string) => m === I || m === "vim/insreg";
      const style = inserting(now) ? "bar" : now === P ? "underline" : "block";
      if (now.startsWith("vim/") || now === "edit") t.setCursorStyle(style);
      if (inserting(old) && !inserting(now) && state.insert !== null) abandonInsert();
      if (old === V && now !== V && !now.startsWith("vim/") && state.visual !== null) {
        state.visual = null;
        t.clearNamespace("vim-visual");
      }
    });
  });
}

/** What enable() left beyond mappings and hooks: text panes back to the
 * mode `edit`, what is open closed. */
function teardown(): void {
  if (isRecording()) stopRecording();
  if (state.insert !== null) abandonInsert();
  state.visual = null;
  state.pending = null;
  try {
    const focused = cmd.panes().find((p) => p.focused);
    if (focused !== undefined && focused.kind === "edit" && !isPrompt(focused.buffer)) t.clearNamespace("vim-visual");
  } catch {
    // Nothing to clear.
  }
  const b = { ...o.basemode };
  if (b.edit === N) {
    delete b.edit;
    o.basemode = b;
  }
}

const keys = keyset(NAMESPACE, (d) => define(d.hook), teardown);

void draw;

/** `import vim from "temzdf/edit/vim"; vim.enable(); vim.options.shiftwidth
 * = 2;`. */
const vim = {
  enable: () => keys.enable(),
  disable: () => keys.disable(),
  get enabled() {
    return keys.enabled;
  },
  options,
  state,
};
export default vim;
export { options };
