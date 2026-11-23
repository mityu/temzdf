// Windows (the VM of scripts/windows-vm.sh, driven from this host): the
// pointer and keys as QEMU's USB tablet and keyboard through QMP (JSON
// lines on the VM's second QMP socket, here over node:net; as vitro's
// src/qmp.rs sends them), screenshots with QMP's screendump (QEMU runs
// here, so it writes them here), commands through one PowerShell in the
// guest over SSH (a PowerShell started per command takes 1.5 s), files
// with scp. What needs the desktop (the GUI, the clipboard) runs as a
// scheduled task on it (onDesktop): SSH's session has none. "Windows in a
// VM" in doc/dev/testing.md says why each thing is so.

import { readFileSync, writeFileSync } from "node:fs";
import { createConnection, type Socket } from "node:net";
import { createInterface } from "node:readline";
import { charKey, isModifierQcode, qcodes, tablet } from "../keys.ts";
import { inputLog, type Lines } from "../lines.ts";
import { type Procs, pause } from "../procs.ts";
import type { Backend, WindowRect } from "./backend.ts";

/** The guest's directory for the run. */
export const gdir = "C:/e2e";

/** QMP on a Unix socket: one command at a time, its answer (events in
 * between skipped). */
export class Qmp {
  readonly #sock: Socket;
  readonly #lines: string[] = [];
  #waiting: (() => void) | undefined;

  private constructor(sock: Socket) {
    this.#sock = sock;
    createInterface({ input: sock }).on("line", (l) => {
      this.#lines.push(l);
      this.#waiting?.();
    });
  }

  /** Connects, reads the greeting and negotiates capabilities. */
  static async connect(path: string, signal: AbortSignal): Promise<Qmp> {
    const sock = createConnection(path);
    await new Promise<void>((resolve, reject) => {
      sock.once("connect", resolve);
      sock.once("error", reject);
    });
    const q = new Qmp(sock);
    if ((await q.#line(10, signal)) === undefined) throw new Error(`e2e: no QMP at ${path}`);
    await q.command('{"execute":"qmp_capabilities"}', signal);
    return q;
  }

  async #line(s: number, signal: AbortSignal): Promise<string | undefined> {
    const deadline = performance.now() + s * 1000;
    while (this.#lines.length === 0) {
      signal.throwIfAborted();
      const left = deadline - performance.now();
      if (left <= 0) return undefined;
      await new Promise<void>((resolve) => {
        const t = setTimeout(resolve, Math.min(left, 100));
        this.#waiting = () => {
          clearTimeout(t);
          resolve();
        };
      });
      this.#waiting = undefined;
    }
    return this.#lines.shift();
  }

  /** A JSON line to QMP; its answer, skipping the events in between. */
  async command(json: string, signal: AbortSignal): Promise<void> {
    // Nothing goes out after an abort.
    signal.throwIfAborted();
    inputLog("qmp", json);
    this.#sock.write(`${json}\n`);
    for (;;) {
      const line = await this.#line(10, signal);
      if (line === undefined) throw new Error(`e2e: no answer over QMP (${json})`);
      if (line.startsWith('{"return"')) return;
      if (line.startsWith('{"error"')) throw new Error(`e2e: QMP: ${line} (${json})`);
    }
  }

  close(): void {
    this.#sock.end();
  }
}

/** The guest's PowerShell over SSH, and copying files. */
export class Guest {
  readonly procs: Procs;
  readonly sshOpts: string[];
  readonly host: string;
  readonly ps: Lines;

  constructor(procs: Procs, sshOpts: string[], host: string, ps: Lines) {
    this.procs = procs;
    this.sshOpts = sshOpts;
    this.host = host;
    this.ps = ps;
  }

  /** One line of PowerShell in the guest: its output; throws with the exit
   * status of the last native program it ran, or when it threw. */
  async run(cmd: string): Promise<string> {
    const r = await this.exec(cmd);
    if (!r.ok) throw new Error(`e2e: in the guest: ${cmd}`);
    return r.out;
  }

  /** As run, whether it succeeded and its output. */
  async exec(cmd: string): Promise<{ ok: boolean; out: string }> {
    this.ps.send(
      `$LASTEXITCODE = 0; try { ${cmd} } catch { [Console]::Error.WriteLine($_); $LASTEXITCODE = 1 }; "@@done $LASTEXITCODE"`,
    );
    let out = "";
    for (;;) {
      const line = await this.ps.next(300, this.procs.signal);
      if (line === undefined) throw new Error(`e2e: no answer from the guest (${cmd})`);
      const l = line.replace(/\r$/, "");
      if (l.startsWith("@@done ")) return { ok: l.slice(7) === "0", out };
      out += `${l}\n`;
    }
  }

  copyIn(from: string, to: string): Promise<string> {
    return this.procs.run("scp", ["-q", ...this.sshOpts, from, `${this.host}:${to}`]);
  }

  copyOut(from: string, to: string): Promise<string> {
    return this.procs.run("scp", ["-q", ...this.sshOpts, `${this.host}:${from}`, to]);
  }

  /** Runs the guest's cmd.exe line CMD on the desktop of the user logged
   * on, without a console window (conhost --headless), as the scheduled
   * task e2e-NAME; returns once it started. */
  onDesktop(name: string, cmd: string): Promise<string> {
    return this.run(
      `schtasks /create /tn e2e-${name} /tr 'conhost.exe --headless ${cmd}' /sc once /st 00:00 /it /f >$null 2>&1; schtasks /run /tn e2e-${name} >$null`,
    );
  }

  /** Until guest file FILE exists, at most S seconds. */
  async wait(file: string, s: number): Promise<boolean> {
    return (
      await this.exec(
        `$n = 0; while (-not (Test-Path ${file}) -and $n -lt ${s * 10}) { Start-Sleep -Milliseconds 100; $n++ }; if (-not (Test-Path ${file})) { $LASTEXITCODE = 1 }`,
      )
    ).ok;
  }
}

export interface WinState {
  qmp: Qmp;
  guest: Guest;
  screen: { w: number; h: number };
  /** Where the pointer is on the screen (the screendumps show it). */
  ptr?: { x: number; y: number };
  /** A local file for the clipboard's text. */
  clipFile: string;
}

export function windows(procs: Procs, st: WinState): Backend {
  const signal = procs.signal;
  const events = (json: string) =>
    st.qmp.command(`{"execute":"input-send-event","arguments":{"events":[${json}]}}`, signal);
  // Each event its own call, the next one 30 ms later: events sent
  // together arrive as one HID report (a press and its release as none).
  const gap = () => pause(0.03, signal);
  const qmpButton = (btn: number) => {
    const names = ["", "left", "middle", "right", "wheel-up", "wheel-down"];
    if (!names[btn]) throw new Error(`bad button ${btn}`);
    return names[btn];
  };
  const press = (down: boolean, b: string) => events(`{"type":"btn","data":{"down":${down},"button":"${b}"}}`);
  // When the last event went out (performance.now()).
  let last = 0;
  // A press or a release 50 ms or more after what came before, and the
  // next event 50 ms after it, as scripts/e2e.sh's took them (measured
  // there: 54 ms from a move to a press, 48 ms from a press to the drag's
  // first move; its 30 ms waits plus the time its subshells took). With 30
  // ms the guest read a press still queued when the drag's first move came
  // at that move's position: the selection of gui's drag lost its first
  // characters every time ("Windows in a VM", doc/dev/testing.md).
  const settle = async (ms: number) => {
    const wait = last + ms - performance.now();
    if (wait > 0) await pause(wait / 1000, signal);
  };
  const key = (down: boolean, q: string) =>
    events(`{"type":"key","data":{"down":${down},"key":{"type":"qcode","data":"${q}"}}}`);
  // A modifier and the key or button after it reach the guest as separate
  // devices' reports: 150 ms apart, or they come in either order (vitro).
  const keydown = async (k: string) => {
    for (const q of qcodes(k)) {
      await key(true, q);
      await pause(isModifierQcode(q) ? 0.15 : 0.02, signal);
    }
  };
  const keyup = async (k: string) => {
    const qs = qcodes(k);
    for (let i = qs.length - 1; i >= 0; i--) {
      if (isModifierQcode(qs[i]!)) await pause(0.15, signal);
      await key(false, qs[i]!);
    }
    await gap();
  };
  const be: Backend = {
    async move(x, y) {
      await events(
        `{"type":"abs","data":{"axis":"x","value":${tablet(x, st.screen.w)}}},{"type":"abs","data":{"axis":"y","value":${tablet(y, st.screen.h)}}}`,
      );
      st.ptr = { x, y };
      last = performance.now();
      // A precaution: a press still queued when a drag's first move comes
      // can take that move's position (QEMU merges reports not read yet).
      await gap();
    },
    async button(state, btn) {
      await settle(50);
      await press(state === "down", qmpButton(btn));
      last = performance.now();
      await settle(50);
    },
    // A wheel notch is a press and a release of a button, too.
    async clicks(btn, n, delayMs) {
      const b = qmpButton(btn);
      for (let i = 0; i < n; i++) {
        if (i > 0) await pause(delayMs / 1000, signal);
        await press(true, b);
        await press(false, b);
      }
      await gap();
    },
    keydown,
    keyup,
    async key(k) {
      await keydown(k);
      await keyup(k);
    },
    async type(text, delayMs) {
      let i = 0;
      for (const c of text) {
        if (i++ > 0 && delayMs) await pause(delayMs / 1000, signal);
        await be.key(charKey(c));
      }
    },
    async shot(file) {
      await st.qmp.command(
        `{"execute":"screendump","arguments":{"filename":${JSON.stringify(file)},"format":"png"}}`,
        signal,
      );
    },
    // The clipboard through PowerShell's Get-Clipboard and Set-Clipboard
    // on the desktop (clip.ps1), as UTF-8 files.
    async clipboard() {
      const g = st.guest;
      try {
        await g.run(`Remove-Item ${gdir}/clip.txt -ErrorAction SilentlyContinue`);
        await g.onDesktop("clip", `powershell.exe -NoProfile -ExecutionPolicy Bypass -File ${gdir}/clip.ps1 get`);
        if (!(await g.wait(`${gdir}/clip.txt`, 10))) return undefined;
        await g.copyOut(`${gdir}/clip.txt`, st.clipFile);
        return readFileSync(st.clipFile, "utf8");
      } catch {
        signal.throwIfAborted();
        return undefined;
      }
    },
    async setClipboard(text) {
      const g = st.guest;
      writeFileSync(st.clipFile, text);
      await g.run(`Remove-Item ${gdir}/clip.done -ErrorAction SilentlyContinue`);
      await g.copyIn(st.clipFile, `${gdir}/clip-in.txt`);
      await g.onDesktop("clip", `powershell.exe -NoProfile -ExecutionPolicy Bypass -File ${gdir}/clip.ps1 set`);
      if (!(await g.wait(`${gdir}/clip.done`, 10))) throw new Error("e2e: the guest's clipboard was not set");
    },
    // Where the drawing area is: what temzdf-gui wrote (TEMZDF_GUI_GEOMETRY:
    // X Y W H SCALE, in pixels on the screen).
    async windowRect(): Promise<WindowRect | undefined> {
      const r = await st.guest.exec(`Get-Content ${gdir}/geometry.txt`);
      const [x, y, w, h, s] = r.out.trim().split(/\s+/).map(Number);
      if (!r.ok || s === undefined || Number.isNaN(s)) return undefined;
      return { x: x!, y: y!, w: w!, h: h! };
    },
    focus: async () => {
      throw new Error("e2e: focus_window is not there on Windows yet");
    },
    otherWindow: async () => {
      throw new Error("e2e: other_window is not there on Windows yet");
    },
    pointer: () => st.ptr,
  };
  return be;
}
