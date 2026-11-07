// Registers: the unnamed one ("), the yank register (0), the delete ring
// (1-9), the small delete register (-), named ones (a-z; A-Z append), the
// clipboard (+ and *, written with setClipboard; reading the system's is
// not possible yet, so they give back what was last put there) and the
// black hole (_).
import { cmd } from "temzdf";

export type Kind = "char" | "line" | "block";

export interface Register {
  text: string;
  kind: Kind;
}

const regs = new Map<string, Register>();

export function get(name: string | null): Register | undefined {
  const n = (name ?? '"').toLowerCase();
  if (n === "*") return regs.get("+");
  return regs.get(n);
}

/** Store what a yank (`yank`) or a delete or change took. */
export function store(name: string | null, reg: Register, how: "yank" | "delete"): void {
  if (name === "_") return;
  if (name !== null && /^[A-Z]$/.test(name)) {
    const lower = name.toLowerCase();
    const old = regs.get(lower);
    const joined =
      old === undefined
        ? reg
        : {
            text: old.kind === "line" || reg.kind === "line" ? ensureNl(old.text) + reg.text : old.text + reg.text,
            kind: old.kind === "line" || reg.kind === "line" ? ("line" as Kind) : old.kind,
          };
    regs.set(lower, joined);
    regs.set('"', joined);
    return;
  }
  if (name === "+" || name === "*") {
    regs.set("+", reg);
    cmd.setClipboard(reg.text);
  } else if (name !== null && name !== '"') {
    regs.set(name, reg);
  } else if (how === "yank") {
    regs.set("0", reg);
  } else if (reg.kind === "line" || reg.text.includes("\n")) {
    for (let i = 9; i > 1; i--) {
      const prev = regs.get(String(i - 1));
      if (prev !== undefined) regs.set(String(i), prev);
    }
    regs.set("1", reg);
  } else {
    regs.set("-", reg);
  }
  regs.set('"', reg);
}

function ensureNl(s: string): string {
  return s.endsWith("\n") ? s : `${s}\n`;
}

/** Whether `c` names a register. */
export function isName(c: string): boolean {
  return /^[a-zA-Z0-9"\-_+*]$/.test(c);
}
