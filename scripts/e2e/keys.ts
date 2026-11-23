// Key names as the e2e helpers take them (XKB keysym names, as xdotool
// takes them: `q`, `ctrl+f`, `shift+g`, `Return`), and their forms for
// what records and sends them: Vim's notation for `--render-frame
// --keys`/`--events`, QEMU's qcodes (the Windows backend), kitty's remote
// control (macOS). Moved from scripts/e2e.sh as they were there.

/** Modifier letters of the pointer helpers' MODS (`C` Ctrl, `S` Shift, `A` Alt) as XKB key names. */
export function modKeys(mods: string): string[] {
  return [...mods].map((m) => {
    switch (m) {
      case "C":
        return "ctrl";
      case "S":
        return "shift";
      case "A":
        return "alt";
      default:
        throw new Error(`bad modifier ${m}`);
    }
  });
}

/** Button numbers as `--events` names them. */
export function buttonName(btn: number): "left" | "middle" | "right" {
  switch (btn) {
    case 1:
      return "left";
    case 2:
      return "middle";
    case 3:
      return "right";
    default:
      throw new Error(`bad button ${btn}`);
  }
}

/** A key name in Vim's notation (the names actions use; others throw rather
 * than record something else). */
export function keyNotation(key: string): string {
  let k = key;
  let mods = "";
  while (k.includes("+")) {
    const m = k.slice(0, k.indexOf("+"));
    if (m === "ctrl") mods += "C-";
    else if (m === "alt") mods += "M-";
    else if (m === "shift") mods += "S-";
    else throw new Error(`key_notation: ${key}`);
    k = k.slice(k.indexOf("+") + 1);
  }
  const named: Record<string, string> = {
    Return: "<CR>",
    Escape: "<Esc>",
    Tab: "<Tab>",
    BackSpace: "<BS>",
    space: "<Space>",
    Up: "<Up>",
    Down: "<Down>",
    Left: "<Left>",
    Right: "<Right>",
    Home: "<Home>",
    End: "<End>",
    Page_Up: "<PageUp>",
    Prior: "<PageUp>",
    Page_Down: "<PageDown>",
    Next: "<PageDown>",
    plus: "+",
    minus: "-",
    equal: "=",
    slash: "/",
    question: "?",
    less: "<lt>",
    greater: ">",
    colon: ":",
    percent: "%",
  };
  let name: string;
  if (Object.hasOwn(named, k)) name = named[k]!;
  else if (/^[a-z0-9]$/.test(k)) name = k;
  else throw new Error(`key_notation: ${key}`);
  // Shift with a letter is the capital (as the terminal reports it).
  if (mods === "S-" && /^[a-z]$/.test(name)) {
    mods = "";
    name = name.toUpperCase();
  }
  if (mods !== "") {
    if (name.startsWith("<") && name.endsWith(">")) name = name.slice(1, -1);
    name = `<${mods}${name}>`;
  }
  return name;
}

/** Text typed, in Vim's notation as `type` records it. */
export function typedNotation(text: string): string {
  let out = "";
  for (const c of text) {
    if (c === "<") out += "<lt>";
    else if (c === " ") out += "<Space>";
    else if (c === "\n") out += "<CR>";
    else out += c;
  }
  return out;
}

/** The XKB name of a character typed (a letter as itself). */
export function charKey(c: string): string {
  const names: Record<string, string> = {
    " ": "space",
    "-": "minus",
    "=": "equal",
    "/": "slash",
    ",": "comma",
    ".": "period",
    ";": "semicolon",
    "'": "apostrophe",
    "`": "grave",
    "[": "bracketleft",
    "]": "bracketright",
    "\\": "backslash",
    "+": "plus",
    "?": "question",
    "<": "less",
    ">": "greater",
    ":": "colon",
    "%": "percent",
    _: "underscore",
    "!": "exclam",
    "@": "at",
    "#": "numbersign",
    $: "dollar",
    "^": "asciicircum",
    "&": "ampersand",
    "*": "asterisk",
    "(": "parenleft",
    ")": "parenright",
    "{": "braceleft",
    "}": "braceright",
    "|": "bar",
    '"': "quotedbl",
    "~": "asciitilde",
    "\t": "Tab",
  };
  return Object.hasOwn(names, c) ? names[c]! : c;
}

/** QEMU's key names (qcodes) for an XKB key name with its modifiers
 * (ctrl+shift+a; question: shift slash), on the US layout: the keys held,
 * then the key. */
export function qcodes(key: string): string[] {
  let k = key;
  const held: string[] = [];
  while (/\+./.test(k)) {
    const m = k.slice(0, k.indexOf("+"));
    if (m === "ctrl" || m === "Control_L") held.push("ctrl");
    else if (m === "shift" || m === "Shift_L") held.push("shift");
    else if (m === "alt" || m === "Alt_L") held.push("alt");
    else if (m === "super") held.push("meta_l");
    else throw new Error(`e2e: no qcode for ${m}`);
    k = k.slice(k.indexOf("+") + 1);
  }
  const plain: Record<string, string> = {
    Return: "ret",
    Escape: "esc",
    Tab: "tab",
    BackSpace: "backspace",
    Delete: "delete",
    space: "spc",
    Prior: "pgup",
    Page_Up: "pgup",
    Next: "pgdn",
    Page_Down: "pgdn",
    Insert: "insert",
    ctrl: "ctrl",
    Control_L: "ctrl",
    shift: "shift",
    Shift_L: "shift",
    alt: "alt",
    Alt_L: "alt",
    minus: "minus",
    equal: "equal",
    slash: "slash",
    comma: "comma",
    period: "dot",
    semicolon: "semicolon",
    apostrophe: "apostrophe",
    grave: "grave_accent",
    bracketleft: "bracket_left",
    bracketright: "bracket_right",
    backslash: "backslash",
  };
  const shifted: Record<string, string> = {
    plus: "equal",
    question: "slash",
    less: "comma",
    greater: "dot",
    colon: "semicolon",
    percent: "5",
    underscore: "minus",
    exclam: "1",
    at: "2",
    numbersign: "3",
    dollar: "4",
    asciicircum: "6",
    ampersand: "7",
    asterisk: "8",
    parenleft: "9",
    parenright: "0",
    braceleft: "bracket_left",
    braceright: "bracket_right",
    bar: "backslash",
    quotedbl: "apostrophe",
    asciitilde: "grave_accent",
  };
  let c: string;
  if (Object.hasOwn(plain, k)) c = plain[k]!;
  else if (/^(Up|Down|Left|Right|Home|End)$/.test(k)) c = k.toLowerCase();
  else if (/^F([0-9]|1[0-2])$/.test(k)) c = k.toLowerCase();
  else if (/^[a-z0-9]$/.test(k)) c = k;
  else if (/^[A-Z]$/.test(k)) {
    held.push("shift");
    c = k.toLowerCase();
  } else if (Object.hasOwn(shifted, k)) {
    held.push("shift");
    c = shifted[k]!;
  } else throw new Error(`e2e: no qcode for ${k}`);
  return [...held, c];
}

/** Whether a qcode is a modifier (pressed 150 ms apart from what it modifies). */
export function isModifierQcode(q: string): boolean {
  return q === "ctrl" || q === "shift" || q === "alt" || q === "meta_l";
}

/** A pixel of the screen on QEMU's tablet axis (0 to 0x7fff over the
 * screen): its middle, whichever way the guest rounds. */
export function tablet(px: number, size: number): number {
  return Math.floor(((2 * px + 1) * 32768) / (2 * size));
}

/** An XKB name as kitty's remote control takes it (macOS): what types a
 * character goes as that text (send-key shift+g would lack the shifted key
 * real typing reports), the rest as a key. */
export function kittyKey(key: string): { text: string } | { key: string } {
  let k = key;
  let mods = "";
  while (/\+./.test(k)) {
    mods += `${k.slice(0, k.indexOf("+"))}+`;
    k = k.slice(k.indexOf("+") + 1);
  }
  const names: Record<string, string> = {
    Return: "enter",
    Escape: "escape",
    Tab: "tab",
    BackSpace: "backspace",
    Delete: "delete",
    Prior: "page_up",
    Page_Up: "page_up",
    Next: "page_down",
    Page_Down: "page_down",
  };
  const chars: Record<string, string> = {
    plus: "+",
    minus: "-",
    equal: "=",
    slash: "/",
    question: "?",
    less: "<",
    greater: ">",
    colon: ":",
    semicolon: ";",
    percent: "%",
    comma: ",",
    period: ".",
    bracketleft: "[",
    bracketright: "]",
    braceleft: "{",
    braceright: "}",
    parenleft: "(",
    parenright: ")",
    backslash: "\\",
    bar: "|",
    apostrophe: "'",
    quotedbl: '"',
    grave: "`",
    asciitilde: "~",
    exclam: "!",
    at: "@",
    numbersign: "#",
    dollar: "$",
    asciicircum: "^",
    ampersand: "&",
    asterisk: "*",
    underscore: "_",
  };
  let name = "";
  let c = "";
  if (Object.hasOwn(names, k)) name = names[k]!;
  else if (k === "space") {
    name = "space";
    c = " ";
  } else if (Object.hasOwn(chars, k)) c = chars[k]!;
  else if ([...k].length === 1) c = k;
  else name = k.toLowerCase();
  if (c !== "" && (mods === "" || mods === "shift+")) {
    return { text: mods === "shift+" ? c.toUpperCase() : c };
  }
  return { key: mods + (name || c) };
}
