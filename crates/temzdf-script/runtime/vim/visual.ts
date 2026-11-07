// The visual mode: a selection from an anchor to the cursor, both on
// characters (inclusive), by characters, lines or a block; drawn as a
// decoration so the cursor stays where Vim shows it.
import { cmd } from "temzdf";
import type { Position } from "temzdf";
import type { Span } from "./operators";
import { state } from "./state";
import type { Motion, VisualKind } from "./state";
import { cursor, lastLine, lineCount, lineEnd, lineLength, max, min, nextChar, pos, t } from "./util";

export function enterVisual(kind: VisualKind): void {
  if (state.visual !== null) {
    if (state.visual.kind === kind) {
      exitVisual();
      return;
    }
    state.visual.kind = kind;
    draw();
    return;
  }
  state.visual = { anchor: cursor(), kind };
  cmd.switchMode("vim/visual");
  draw();
}

/** gv: the last selection again. */
export function reselect(): void {
  const v = state.lastVisual;
  if (v === null) return;
  state.visual = { anchor: v.anchor, kind: v.kind };
  t.setCursor(v.cursor);
  if (cmd.currentMode() !== "vim/visual") cmd.switchMode("vim/visual");
  draw();
}

export function exitVisual(): void {
  const v = state.visual;
  if (v === null) return;
  state.lastVisual = { anchor: v.anchor, cursor: cursor(), kind: v.kind };
  state.visual = null;
  t.clearNamespace("vim-visual");
  if (cmd.currentMode() === "vim/visual") cmd.exitMode();
}

/** The selection's columns for a block (start, end inclusive). */
function blockColumns(a: Position, b: Position): [number, number] {
  return [Math.min(a.character, b.character), Math.max(a.character, b.character)];
}

export function draw(): void {
  t.clearNamespace("vim-visual");
  const v = state.visual;
  if (v === null) return;
  const c = cursor();
  const deco = (start: Position, end: Position) =>
    t.addDecoration({ start, end }, { namespace: "vim-visual", style: "Selection", priority: 100 });
  if (v.kind === "block") {
    const [c0, c1] = blockColumns(v.anchor, c);
    for (let l = Math.min(v.anchor.line, c.line); l <= Math.max(v.anchor.line, c.line); l++) {
      const len = lineLength(l);
      if (c0 < len) deco(pos(l, c0), pos(l, Math.min(len, c1 + 1)));
    }
    return;
  }
  const s = selection();
  if (s.kind === "line") {
    deco(pos(s.first, 0), s.last + 1 < lineCount() ? pos(s.last + 1, 0) : lineEnd(s.last));
  } else {
    deco(s.start, s.end);
  }
}

/** The selection as a span (not for blocks). */
export function selection(): Span {
  const v = state.visual!;
  const c = cursor();
  if (v.kind === "line") {
    return { kind: "line", first: Math.min(v.anchor.line, c.line), last: Math.min(lastLine(), Math.max(v.anchor.line, c.line)) };
  }
  const start = min(v.anchor, c);
  const last = max(v.anchor, c);
  // Inclusive; on an empty line (or past the end) its line break.
  const end =
    last.character >= lineLength(last.line) ? (last.line + 1 < lineCount() ? pos(last.line + 1, 0) : last) : nextChar(last);
  return { kind: "char", start, end };
}

/** The block's lines and columns. */
export function block(): { first: number; last: number; c0: number; c1: number } {
  const v = state.visual!;
  const c = cursor();
  const [c0, c1] = blockColumns(v.anchor, c);
  return { first: Math.min(v.anchor.line, c.line), last: Math.max(v.anchor.line, c.line), c0, c1 };
}

/** A motion in the visual mode moves the cursor; the anchor stays. */
export function moveVisual(m: Motion): void {
  if ("vertical" in m) {
    const v = (m as Motion & { vertical: { unit: "line"; count: number } }).vertical;
    t.move(v.unit, v.count);
  } else {
    const to = m.to;
    const len = lineLength(to.line);
    t.setCursor(len > 0 && to.character > len ? lineEnd(to.line) : to);
  }
  draw();
}

/** o: the cursor to the other end. */
export function swapEnds(): void {
  const v = state.visual;
  if (v === null) return;
  const c = cursor();
  t.setCursor(v.anchor);
  v.anchor = c;
  draw();
}
