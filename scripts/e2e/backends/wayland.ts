// Wayland (headless sway, started by the harness): keys and the pointer
// through crates/wlinput, one virtual keyboard and pointer for the whole
// run (wtype and wlrctl make a device per call: clicks and wheel steps got
// lost, and every key was a focus change); screenshots with grim, windows
// through swaymsg, the clipboard with wl-clipboard.

import type { Lines } from "../lines.ts";
import { type Procs, pause } from "../procs.ts";
import type { Backend, WindowRect } from "./backend.ts";

interface SwayNode {
  pid?: number;
  rect: { x: number; y: number; width: number; height: number };
  nodes?: SwayNode[];
  floating_nodes?: SwayNode[];
}

export function wayland(procs: Procs, wl: Lines): Backend {
  const signal = procs.signal;
  const cmd = (line: string) => wl.ok(line, "wlinput", signal);
  const rect = async (pid: number): Promise<WindowRect | undefined> => {
    const r = await procs.exec("swaymsg", ["-t", "get_tree"]);
    if (r.code !== 0) return undefined;
    const walk = (n: SwayNode): WindowRect | undefined => {
      if (n.pid === pid) return { x: n.rect.x, y: n.rect.y, w: n.rect.width, h: n.rect.height };
      for (const c of [...(n.nodes ?? []), ...(n.floating_nodes ?? [])]) {
        const found = walk(c);
        if (found) return found;
      }
      return undefined;
    };
    return walk(JSON.parse(r.stdout) as SwayNode);
  };
  return {
    move: (x, y) => cmd(`move ${x} ${y}`),
    button: (state, btn) => cmd(`${state} ${btn}`),
    async clicks(btn, n, delayMs) {
      if (btn === 4) return cmd(`wheel -${n} ${delayMs}`);
      if (btn === 5) return cmd(`wheel ${n} ${delayMs}`);
      for (let i = 0; i < n; i++) {
        if (i > 0) await pause(delayMs / 1000, signal);
        await cmd(`down ${btn}`);
        await cmd(`up ${btn}`);
      }
    },
    key: (k) => cmd(`key ${k}`),
    keydown: (k) => cmd(`keydown ${k}`),
    keyup: (k) => cmd(`keyup ${k}`),
    type: (text, delayMs) => cmd(`type ${delayMs ?? 0} ${text}`),
    shot: (file) => procs.run("grim", [file]).then(() => {}),
    async clipboard() {
      const r = await procs.exec("wl-paste", ["-n"]);
      return r.code === 0 ? r.stdout : undefined;
    },
    // wl-copy stays to serve the clipboard: nothing of ours may stay open in it.
    async setClipboard(text) {
      await procs.feed("wl-copy", ["--", text], "");
    },
    windowRect: rect,
    async focus(pid) {
      for (let i = 0; i < 50 && !(await rect(pid)); i++) await pause(0.1, signal);
      await procs.exec("swaymsg", ["-q", `[pid=${pid}] focus`]);
    },
    // Floating (sway.conf), so the window under test keeps its size.
    async otherWindow() {
      const child = procs.start("kitty", [
        "--config",
        "NONE",
        "-o",
        "linux_display_server=wayland",
        "--class",
        "e2e-other",
        "sh",
        "-c",
        "sleep 30",
      ]);
      return child.pid!;
    },
  };
}
