// X11 (Xvfb, started by the harness): xdotool for the pointer, keys and
// windows, ImageMagick's import for screenshots, xclip for the clipboard.

import type { Procs } from "../procs.ts";
import type { Backend, WindowRect } from "./backend.ts";

export function x11(procs: Procs): Backend {
  const xdotool = (...args: string[]) => procs.run("xdotool", args).then(() => {});
  // The first visible window of process PID, waited for up to 10 s.
  const window = async (pid: number): Promise<string | undefined> => {
    const r = await procs.exec("xdotool", ["search", "--sync", "--onlyvisible", "--pid", String(pid)], { timeout: 10 });
    const ids = r.stdout.trim().split("\n").filter(Boolean);
    return ids.at(-1);
  };
  return {
    move: (x, y) => xdotool("mousemove", String(x), String(y)),
    button: (state, btn) => xdotool(state === "down" ? "mousedown" : "mouseup", String(btn)),
    clicks: (btn, n, delayMs) => xdotool("click", "--repeat", String(n), "--delay", String(delayMs), String(btn)),
    key: (k) => xdotool("key", k),
    keydown: (k) => xdotool("keydown", k),
    keyup: (k) => xdotool("keyup", k),
    type: (text, delayMs) =>
      xdotool("type", ...(delayMs !== undefined ? ["--delay", String(delayMs)] : []), "--", text),
    shot: (file) => procs.run("import", ["-window", "root", file]).then(() => {}),
    async clipboard() {
      const r = await procs.exec("xclip", ["-selection", "clipboard", "-o"]);
      return r.code === 0 ? r.stdout : undefined;
    },
    async setClipboard(text) {
      if ((await procs.feed("xclip", ["-selection", "clipboard"], text)) !== 0) throw new Error("xclip failed");
    },
    async windowRect(pid): Promise<WindowRect | undefined> {
      const id = await window(pid);
      if (!id) return undefined;
      const info = await procs.run("xwininfo", ["-id", id]);
      const num = (re: RegExp) => Number(re.exec(info)?.[1]);
      return {
        x: num(/Absolute upper-left X:\s+(-?\d+)/),
        y: num(/Absolute upper-left Y:\s+(-?\d+)/),
        w: num(/Width:\s+(\d+)/),
        h: num(/Height:\s+(\d+)/),
      };
    },
    async focus(pid) {
      const id = await window(pid);
      if (!id) throw new Error(`no window of ${pid}`);
      await xdotool("windowfocus", "--sync", id);
    },
    async otherWindow() {
      const child = procs.start("xterm", ["-e", "sh", "-c", "sleep 30"]);
      return child.pid!;
    },
  };
}
