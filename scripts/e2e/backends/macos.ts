// macOS (a virtual display at scale 1, or a VM's own screen; disp: its
// place among the screens, global points, which are its pixels at scale
// 1): the pointer and keys through `termctl serve` (started once the
// terminal is up), kitty's keys through its remote control (it ignores key
// events posted to its process), screenshots of the display, the clipboard
// with pbcopy/pbpaste, windows through termctl. Not run since the move to
// TypeScript (todo.md): a copy of scripts/e2e.sh's.

import { kittyKey } from "../keys.ts";
import type { Lines } from "../lines.ts";
import { type Procs, pause } from "../procs.ts";
import type { Backend, WindowRect } from "./backend.ts";

export const termctl = "target/macos-gui/termctl";

export interface MacDisplay {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface MacState {
  disp: MacDisplay;
  term: string;
  kittySocket: string;
  /** `termctl serve`, once the terminal is up. */
  tc?: Lines;
}

export function macos(procs: Procs, st: MacState): Backend {
  const signal = procs.signal;
  const tc = (line: string) => {
    if (!st.tc) throw new Error("termctl serve is not running yet");
    return st.tc.ok(line, "termctl", signal);
  };
  const kitten = (...args: string[]) =>
    procs.run("kitten", ["@", "--to", `unix:${st.kittySocket}`, ...args]).then(() => {});
  const kittyKeyOf = async (k: string) => {
    const kk = kittyKey(k);
    if ("text" in kk) await kitten("send-text", "--", kk.text);
    else await kitten("send-key", kk.key);
  };
  return {
    move: (x, y) => tc(`move ${st.disp.x + x} ${st.disp.y + y}`),
    button: (state, btn) => tc(`${state} ${btn}`),
    async clicks(btn, n, delayMs) {
      if (btn === 4) return tc(`wheel -${n} ${delayMs}`);
      if (btn === 5) return tc(`wheel ${n} ${delayMs}`);
      for (let i = 0; i < n; i++) {
        if (i > 0) await pause(delayMs / 1000, signal);
        await tc(`down ${btn}`);
        await tc(`up ${btn}`);
      }
    },
    key: (k) => (st.term === "kitty" ? kittyKeyOf(k) : tc(`key ${k}`)),
    keydown: (k) => tc(`keydown ${k}`),
    keyup: (k) => tc(`keyup ${k}`),
    async type(text, delayMs) {
      if (st.term !== "kitty") return tc(`type ${delayMs ?? 0} ${text}`);
      if (delayMs === undefined) return kitten("send-text", "--", text);
      for (const c of text) {
        await kitten("send-text", "--", c);
        await pause(delayMs / 1000, signal);
      }
    },
    shot: (file) =>
      procs
        .run("screencapture", ["-x", "-R", `${st.disp.x},${st.disp.y},${st.disp.w},${st.disp.h}`, file])
        .then(() => {}),
    async clipboard() {
      const r = await procs.exec("pbpaste", []);
      return r.code === 0 ? r.stdout : undefined;
    },
    async setClipboard(text) {
      await procs.feed("pbcopy", [], text);
    },
    // The window's inside (without a title bar), on the virtual display.
    async windowRect(pid): Promise<WindowRect | undefined> {
      const r = await procs.exec(termctl, ["content", String(pid)]);
      const [x, y, w, h] = r.stdout.trim().split(/\s+/).map(Number);
      if (r.code !== 0 || h === undefined || Number.isNaN(h)) return undefined;
      return { x: x! - st.disp.x, y: y! - st.disp.y, w: w!, h };
    },
    async focus(pid) {
      await procs.run(termctl, ["activate", String(pid)]);
    },
    async otherWindow() {
      const before = new Set((await procs.exec("pgrep", ["-x", "TextEdit"])).stdout.split("\n").filter(Boolean));
      await procs.run("open", ["-g", "-n", "-a", "TextEdit"]);
      for (let i = 0; i < 50; i++) {
        for (const p of (await procs.exec("pgrep", ["-x", "TextEdit"])).stdout.split("\n").filter(Boolean)) {
          if (!before.has(p)) return Number(p);
        }
        await pause(0.1, signal);
      }
      throw new Error("no TextEdit window");
    },
  };
}
