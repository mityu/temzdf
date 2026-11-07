// Macros: q{reg} records the keys typed (a KeyTyped hook, only while
// recording: it holds every key until it ran), q stops; @{reg} feeds a
// register's keys back (feedkeys, through the mappings), @@ the last
// again, @: the last Ex command. An error stops a macro (the server drops
// what feedkeys queued).
import { cmd, dispatch, feedkeys, on } from "temzdf";
import * as registers from "./registers";

let recording: null | { name: string; keys: string[]; stop: () => void } = null;
let lastRun: string | null = null;
/** The last Ex command line, for @:. */
let lastEx: string | null = null;

export function isRecording(): boolean {
  return recording !== null;
}

/** q{name}: start recording into register `name` (A-Z: append). */
export function startRecording(name: string): void {
  if (recording !== null || !/^[a-zA-Z0-9]$/.test(name)) return;
  const keys: string[] = [];
  const stop = on("KeyTyped", (key) => {
    if (key.source === "typed") keys.push(key.keys);
  });
  recording = { name, keys, stop };
  cmd.message(`recording @${name}`);
}

/** q while recording: the keys typed until it (not that q) go into the
 * register. */
export function stopRecording(): void {
  const r = recording;
  if (r === null) return;
  recording = null;
  r.stop();
  const keys = r.keys.slice(0, -1).join("");
  const name = r.name;
  if (/^[A-Z]$/.test(name)) {
    const old = cmd.register(name.toLowerCase())?.text ?? "";
    cmd.setRegister(name.toLowerCase(), { text: old + keys, kind: "char" });
  } else if (/^[0-9]$/.test(name)) {
    cmd.copyText(keys, { kind: "char", clipboard: false });
  } else {
    cmd.setRegister(name, { text: keys, kind: "char" });
  }
}

/** The Ex command line just run, for @:. */
export function rememberEx(line: string): void {
  lastEx = line;
}

/** @{name}: run register `name`'s keys `count` times (@@: the last run). */
export function runMacro(name: string, count: number | null, ex: (line: string) => void): void {
  const n = count ?? 1;
  let reg = name === "@" ? lastRun : name;
  if (reg === null) throw new Error("E748: No previously used register");
  if (reg === ":") {
    if (lastEx === null) throw new Error("E30: No previous command line");
    lastRun = ":";
    for (let i = 0; i < n; i++) ex(lastEx);
    return;
  }
  if (!/^[a-zA-Z0-9"\-+*]$/.test(reg)) throw new Error(`E354: Invalid register name: '${reg}'`);
  lastRun = reg;
  if (registers.isClipboard(reg)) {
    const primary = reg === "*";
    dispatch(async (ctx) => {
      const text = await ctx.cmd.readClipboard({ primary });
      ctx.atomic(() => feedkeys(text.repeat(n), { remap: true }));
    });
    return;
  }
  if (/^[A-Z]$/.test(reg)) reg = reg.toLowerCase();
  const r = registers.get(reg);
  if (r === undefined) throw new Error(`E354: Invalid register name: '${reg}'`);
  feedkeys(r.text.repeat(n), { remap: true });
}
