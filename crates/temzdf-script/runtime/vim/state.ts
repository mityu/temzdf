// What the Vim mode remembers between keys.
import type { Position } from "temzdf";

/** How a motion's range is read (Vim's `:help exclusive`). */
export type MotionKind = "exclusive" | "inclusive" | "linewise";

export interface Motion {
  to: Position;
  kind: MotionKind;
  /** A text object or such: the range starts here, not at the cursor. */
  from?: Position;
}

/** Computes a motion from a position, with a count (null: none given). */
export type MotionFn = (from: Position, count: number | null) => Motion | null;

export type Operator = "d" | "c" | "y" | ">" | "<" | "g~" | "gu" | "gU";

export type VisualKind = "char" | "line" | "block";

/** A block's I, A or c: what is typed on its first line goes into the
 * others at <Esc>, at column `col` ("end": each line's end); a line
 * shorter than `col` is padded (`pad`, A) or left alone. */
export interface BlockInsert {
  first: number;
  last: number;
  col: number | "end";
  pad: boolean;
  /** The lines that get it (c: those that had text in the block). */
  lines: number[];
}

export const state = {
  /** An operator waiting for its motion. */
  pending: null as null | { op: Operator; count: number | null; register: string | null },
  /** The register the next command uses (`"x`). */
  register: null as string | null,
  /** The last f, F, t or T, for ; and ,. */
  lastFind: null as null | { kind: "f" | "F" | "t" | "T"; char: string },
  /** The visual selection: where it started, and how. */
  visual: null as null | { anchor: Position; kind: VisualKind; toEnd?: boolean },
  /** The last visual selection, for gv and '< '>. */
  lastVisual: null as null | { anchor: Position; cursor: Position; kind: VisualKind },
  /** The last search (Vim's pattern and its direction), for n and N. */
  lastSearch: null as null | { pattern: string; backward: boolean },
  /** The last change, for `.`: run it again (a count replaces its own). */
  lastChange: null as null | ((count: number | null) => void),
  /** An insert session: where it began (a mark), and how `.` begins it again. */
  insert: null as null | { mark: number; replay: ((count: number | null) => void) | null; count: number | null; block?: BlockInsert },
};

/** Settings of the Vim mode (vim.options in a configuration). */
export const options = {
  shiftwidth: 8,
  expandtab: false,
  ignorecase: false,
  smartcase: false,
  hlsearch: true,
  incsearch: true,
  wrapscan: true,
};
