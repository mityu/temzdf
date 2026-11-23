// The key names' forms (keys.ts), against what scripts/e2e.sh made of them.

import assert from "node:assert/strict";
import { test } from "node:test";
import { charKey, keyNotation, kittyKey, qcodes, tablet, typedNotation } from "./keys.ts";

test("keyNotation: Vim's notation for --keys and --events", () => {
  const cases: [string, string][] = [
    ["q", "q"],
    ["j", "j"],
    ["0", "0"],
    ["plus", "+"],
    ["minus", "-"],
    ["slash", "/"],
    ["less", "<lt>"],
    ["Return", "<CR>"],
    ["Escape", "<Esc>"],
    ["Tab", "<Tab>"],
    ["space", "<Space>"],
    ["Prior", "<PageUp>"],
    ["Page_Down", "<PageDown>"],
    ["shift+g", "G"],
    ["ctrl+f", "<C-f>"],
    ["ctrl+b", "<C-b>"],
    ["ctrl+Return", "<C-CR>"],
    ["alt+Left", "<M-Left>"],
    ["shift+Tab", "<S-Tab>"],
    ["ctrl+shift+v", "<C-S-v>"],
  ];
  for (const [k, want] of cases) assert.equal(keyNotation(k), want, k);
  assert.throws(() => keyNotation("F1"));
  assert.throws(() => keyNotation("super+a"));
});

test("typedNotation: typed text as `type` records it", () => {
  assert.equal(typedNotation("ls -a <x>\n"), "ls<Space>-a<Space><lt>x><CR>");
});

test("qcodes: QEMU's keys on the US layout, modifiers first", () => {
  assert.deepEqual(qcodes("a"), ["a"]);
  assert.deepEqual(qcodes("G"), ["shift", "g"]);
  assert.deepEqual(qcodes("shift+g"), ["shift", "g"]);
  assert.deepEqual(qcodes("ctrl+shift+v"), ["ctrl", "shift", "v"]);
  assert.deepEqual(qcodes("question"), ["shift", "slash"]);
  assert.deepEqual(qcodes("plus"), ["shift", "equal"]);
  assert.deepEqual(qcodes("period"), ["dot"]);
  assert.deepEqual(qcodes("Return"), ["ret"]);
  assert.deepEqual(qcodes("Page_Up"), ["pgup"]);
  assert.deepEqual(qcodes("F12"), ["f12"]);
  assert.deepEqual(qcodes("Left"), ["left"]);
  assert.throws(() => qcodes("Hyper_L"));
});

test("charKey: typed characters as key names", () => {
  assert.equal(charKey(" "), "space");
  assert.equal(charKey("."), "period");
  assert.equal(charKey("?"), "question");
  assert.equal(charKey("a"), "a");
  assert.deepEqual(qcodes(charKey("_")), ["shift", "minus"]);
});

test("tablet: the middle of a pixel on QEMU's 0..0x7fff axis", () => {
  assert.equal(tablet(0, 1280), 12);
  assert.equal(tablet(639, 1280), 16371);
  assert.equal(tablet(1279, 1280), 32755);
});

test("kittyKey: characters as text, the rest as keys", () => {
  assert.deepEqual(kittyKey("q"), { text: "q" });
  assert.deepEqual(kittyKey("shift+g"), { text: "G" });
  assert.deepEqual(kittyKey("plus"), { text: "+" });
  assert.deepEqual(kittyKey("space"), { text: " " });
  assert.deepEqual(kittyKey("Return"), { key: "enter" });
  assert.deepEqual(kittyKey("ctrl+b"), { key: "ctrl+b" });
  assert.deepEqual(kittyKey("ctrl+plus"), { key: "ctrl++" });
  assert.deepEqual(kittyKey("Up"), { key: "up" });
});
