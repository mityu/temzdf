// Vim's patterns to Rust's regex syntax (doc/design/text-editor.md section
// 6): the magic levels (\v \m \M \V), quantifiers (\+ \= \? \{n,m} and the
// non-greedy \{-}), groups and alternation, \< \>, \zs \ze (a capture
// group that counts as the match), Vim's character classes (\s \d \w \a
// \l \u \x \h, \k as word characters), \n, \_x (with line breaks), \c \C,
// back references (\1, through fancy-regex). The rest is taken literally.

export interface Converted {
  /** The Rust pattern. */
  rust: string;
  /** The capture group that is the match (\zs \ze), 0 for all of it. */
  group: number;
}

type Magic = "very" | "magic" | "nomagic" | "verynomagic";

const classes: Record<string, string> = {
  s: "[ \\t]",
  S: "[^ \\t\\n]",
  d: "[0-9]",
  D: "[^0-9\\n]",
  w: "[0-9A-Za-z_]",
  W: "[^0-9A-Za-z_\\n]",
  a: "[A-Za-z]",
  A: "[^A-Za-z\\n]",
  l: "[a-z]",
  L: "[^a-z\\n]",
  u: "[A-Z]",
  U: "[^A-Z\\n]",
  x: "[0-9A-Fa-f]",
  X: "[^0-9A-Fa-f\\n]",
  o: "[0-7]",
  O: "[^0-7\\n]",
  h: "[A-Za-z_]",
  H: "[^A-Za-z_\\n]",
  k: "[\\p{L}\\p{N}_]",
  i: "[\\p{L}\\p{N}_]",
  f: "[^\\s]",
  p: "[^\\p{Cc}]",
};

/** Characters special to Rust's regex that are literal here. */
function literal(c: string): string {
  return /[\\.+*?()|[\]{}^$]/.test(c) ? `\\${c}` : c;
}

/** Convert `pattern`; `ignorecase` and `smartcase` as Vim's options. */
export function convert(pattern: string, ignorecase = false, smartcase = false): Converted {
  let magic: Magic = "magic";
  let out = "";
  let caseFlag: boolean | null = null;
  // \zs and \ze: where the match starts and ends inside the pattern.
  let zs = -1;
  let ze = -1;
  let groups = 0;
  let upper = false;
  const chars = [...pattern];
  for (let i = 0; i < chars.length; i++) {
    const c = chars[i];
    if (c !== "\\") {
      if (/[A-Z]/.test(c)) upper = true;
      const special = magicSpecial(c, magic);
      if (c === "[" && magic !== "verynomagic" && magic !== "nomagic") {
        // A class, as it is (Vim's and Rust's agree for the usual ones).
        const end = classEnd(chars, i);
        if (end > 0) {
          out += classText(chars.slice(i, end + 1).join(""));
          i = end;
          continue;
        }
        out += "\\[";
        continue;
      }
      if (special && c === "{") {
        const close = chars.indexOf("}", i);
        if (close > 0) {
          out += braces(chars.slice(i + 1, close).join(""));
          i = close;
          continue;
        }
      }
      if (special) {
        out += veryMagic(c);
        if (c === "(") groups++;
      } else if ((c === "^" && i === 0) || (c === "$" && i === chars.length - 1)) {
        out += c;
      } else {
        out += literal(c);
      }
      continue;
    }
    const n = chars[++i];
    if (n === undefined) {
      out += "\\\\";
      break;
    }
    switch (n) {
      case "v":
        magic = "very";
        continue;
      case "m":
        magic = "magic";
        continue;
      case "M":
        magic = "nomagic";
        continue;
      case "V":
        magic = "verynomagic";
        continue;
      case "c":
        caseFlag = true;
        continue;
      case "C":
        caseFlag = false;
        continue;
      case "<":
        out += "\\b{start}";
        continue;
      case ">":
        out += "\\b{end}";
        continue;
      case "n":
        out += "\\n";
        continue;
      case "t":
        out += "\\t";
        continue;
      case "z":
        if (chars[i + 1] === "s") {
          zs = out.length;
          i++;
          continue;
        }
        if (chars[i + 1] === "e") {
          ze = out.length;
          i++;
          continue;
        }
        out += "z";
        continue;
      case "_": {
        const k = chars[++i];
        if (k === ".") out += "(?s:.)";
        else if (k !== undefined && classes[k] !== undefined) out += `(?:${classes[k]}|\\n)`;
        else if (k === "^") out += "^";
        else if (k === "$") out += "$";
        else out += literal(k ?? "_");
        continue;
      }
    }
    if (/[1-9]/.test(n)) {
      out += `\\${n}`;
      continue;
    }
    if (classes[n] !== undefined) {
      out += classes[n];
      continue;
    }
    // In magic modes these are special with a backslash, plain without.
    if (magicSpecial(n, magic)) {
      out += literal(n);
      continue;
    }
    switch (n) {
      case "(":
        out += "(";
        groups++;
        continue;
      case "%":
        if (chars[i + 1] === "(") {
          out += "(?:";
          i++;
          continue;
        }
        out += "%";
        continue;
      case ")":
        out += ")";
        continue;
      case "|":
        out += "|";
        continue;
      case "+":
        out += "+";
        continue;
      case "=":
      case "?":
        out += "?";
        continue;
      case "{": {
        const close = chars.indexOf("}", i);
        if (close < 0) {
          out += "\\{";
          continue;
        }
        out += braces(chars.slice(i + 1, close).join("").replace(/\\$/, ""));
        i = close;
        continue;
      }
      case "*":
        out += magic === "verynomagic" || magic === "nomagic" ? "*" : "\\*";
        continue;
      case ".":
        out += magic === "verynomagic" || magic === "nomagic" ? "." : "\\.";
        continue;
      case "[":
        out += magic === "verynomagic" || magic === "nomagic" ? "[" : "\\[";
        continue;
      case "/":
        out += "/";
        continue;
      default:
        out += literal(n);
    }
  }
  let group = 0;
  if (zs >= 0 || ze >= 0) {
    const s = zs >= 0 ? zs : 0;
    const e = ze >= 0 ? ze : out.length;
    // The groups before \zs come first: the match group's number follows.
    group = countGroups(out.slice(0, s)) + 1;
    out = `${out.slice(0, s)}(${out.slice(s, e)})${out.slice(e)}`;
  }
  const insensitive = caseFlag ?? (ignorecase && !(smartcase && upper));
  void groups;
  return { rust: insensitive ? `(?i)${out}` : out, group };
}

function countGroups(s: string): number {
  let n = 0;
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\") i++;
    else if (s[i] === "(" && s[i + 1] !== "?") n++;
  }
  return n;
}

/** Whether `c` is special without a backslash in this mode. */
function magicSpecial(c: string, magic: Magic): boolean {
  switch (magic) {
    case "very":
      return /[()|+?={@<>.*]/.test(c);
    case "magic":
      return c === "." || c === "*";
    case "nomagic":
      return false;
    case "verynomagic":
      return false;
  }
}

/** A very-magic special character as Rust has it. */
function veryMagic(c: string): string {
  switch (c) {
    case "=":
      return "?";
    case "<":
      return "\\b{start}";
    case ">":
      return "\\b{end}";
    case "{":
      return "{";
    case "@":
      return "\\@";
    default:
      return c;
  }
}

/** \{n,m} \{-n,m} and the like. */
function braces(inner: string): string {
  const lazy = inner.startsWith("-");
  const body = lazy ? inner.slice(1) : inner;
  const q = body === "" ? "*" : body.includes(",") ? `{${body.startsWith(",") ? `0${body}` : body}}` : `{${body}}`;
  return lazy ? `${q}?` : q;
}

/** Where a [...] class ends (-1: not a class). */
function classEnd(chars: string[], start: number): number {
  let i = start + 1;
  if (chars[i] === "^") i++;
  if (chars[i] === "]") i++;
  for (; i < chars.length; i++) {
    if (chars[i] === "\\") i++;
    else if (chars[i] === "]") return i;
  }
  return -1;
}

/** A class as Rust reads it: Vim's [[:alpha:]] and such kept, a `[` inside
 * escaped. */
function classText(cls: string): string {
  return cls.replace(/\[(?!:)/g, (m, off: number) => (off === 0 ? m : "\\["));
}
