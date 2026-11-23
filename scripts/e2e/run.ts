// One e2e run: the display server, the terminal (or the GUI) running
// COMMAND, the helpers a scenario drives it with, and the end of it all.
// The judging (`expect`, the cells, rows) is in judge.ts. Moved from
// scripts/e2e.sh, which did the same in the same order; its comments say
// why things are as they are.

import type { ChildProcess } from "node:child_process";
import {
  closeSync,
  existsSync,
  mkdirSync,
  openSync,
  readdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import type { MacState } from "./backends/macos.ts";
import * as mac from "./backends/macos-start.ts";
import { wayland } from "./backends/wayland.ts";
import type { WinState } from "./backends/windows.ts";
import * as win from "./backends/windows-start.ts";
import { x11 } from "./backends/x11.ts";
import type { Backend } from "./backends/backend.ts";
import * as command from "./command.ts";
import { cleanEnv, makeRuntimeDir, openerShim } from "./host.ts";
import { buttonName, keyNotation, modKeys, typedNotation } from "./keys.ts";
import { Lines } from "./lines.ts";
import { colours } from "./magick.ts";
import type { Options } from "./options.ts";
import { Procs, pause, until } from "./procs.ts";
import { Records, type Verdict } from "./record.ts";

/** A run that could not start (exit 1). */
export class StartError extends Error {}

/** Thrown by a check's function: SKIP (this terminal cannot tell). */
export class Skip extends Error {}

export interface Ref {
  name: string;
  cols: number;
  rows: number;
  cw: number;
  ch: number;
}

export interface Geometry {
  cols: number;
  rows: number;
  cw: number;
  ch: number;
}

export class Run {
  readonly opts: Options;
  readonly outdir: string;
  readonly procs: Procs;
  readonly signal: AbortSignal;
  readonly rec: Records;
  /** COMMAND as run (the first word absolute when it names a file). */
  argv: string[];
  /** The harness's environment, for the helpers' commands (the display,
   * the run's runtime directory). */
  env: NodeJS.ProcessEnv;
  /** COMMAND's environment (cleanEnv). */
  cleanEnv: NodeJS.ProcessEnv = {};
  runtimeDir = "";
  kittySocket = "";
  be!: Backend;
  /** The terminal's (or the GUI's) process; on macOS not a child (started
   * through `open`), on Windows an SSH command that ends with the GUI. */
  termPid = 0;
  /** Where the terminal area (or the GUI's drawing area) starts on the screen. */
  origin = { x: 0, y: 0 };
  /** The GUI's drawing area. */
  win = { w: 0, h: 0, scale: 1 };
  screen = { w: 0, h: 0 };
  /** The expects whose cells drawReferences judges. */
  refs: Ref[] = [];
  /** The button held by `down` (and its MODS), for `move` and `up`. */
  held: { btn: number; mods: string } | undefined;
  pointerAt = { x: 0, y: 0 };
  // What COMMAND is (command.ts).
  temzdfInTerminal = false;
  sessionMode = false;
  hasShell = false;
  standalone: command.Standalone | undefined;
  forwardTemzdf = "temzdf";
  /** The session's temzdf (sessions, the GUI). */
  temzdf = "";
  guiBin = "";

  constructor(opts: Options, argv: string[], signal: AbortSignal) {
    this.opts = opts;
    this.outdir = opts.outdir;
    this.signal = signal;
    this.procs = new Procs(signal);
    mkdirSync(path.join(this.outdir, "state"), { recursive: true });
    rmSync(path.join(this.outdir, "state/temzdf/temzdf.log"), { force: true });
    rmSync(path.join(this.outdir, "expect.txt"), { force: true });
    this.rec = new Records(this.outdir);
    this.env = { ...process.env };
    // Some terminals (WezTerm) look the program up only in PATH: absolute.
    this.argv = [...argv];
    if (this.argv[0]?.includes("/") && existsSync(this.argv[0])) this.argv[0] = realpathSync(this.argv[0]);
    command.checkConfig(this.argv);
    const parsed = command.parse(this.argv);
    this.hasShell = parsed.hasShell;
    if (parsed.words.length > 0) {
      if (parsed.words[0]!.endsWith("/temzdf")) this.forwardTemzdf = parsed.words[0]!;
      this.standalone = command.standalone(parsed.words);
    }
    if (command.isSession(opts.term, this.argv)) {
      this.sessionMode = true;
      const t = command.sessionTemzdf(this.argv);
      this.temzdf = realpathSync(t ?? which("temzdf", this.env));
    }
    this.temzdfInTerminal = opts.term !== "gui" && command.runsTemzdf(this.argv);
  }

  /** Reports a judgement to the test runner (scenario.ts sets it: a
   * subtest per judgement). */
  report: (verdict: Verdict, name: string, line: string) => Promise<void> = async () => {};

  /** A judgement: its line in OUTDIR/expect.txt ("PASS NAME", "FAIL NAME:
   * why", "SKIP NAME: why", as scripts/e2e.sh wrote them) and the runner. */
  async verdict(verdict: Verdict, name: string, detail?: string): Promise<void> {
    const line =
      verdict === "PASS"
        ? `PASS ${name}${detail ? ` ${detail}` : ""}`
        : `${verdict} ${name}${detail ? `: ${detail}` : ""}`;
    this.rec.verdict(line);
    await this.report(verdict, name, line);
  }

  // ---- Small things the helpers share.

  /** Seconds to sleep, ended by the run's signal. */
  sleep(s: number): Promise<void> {
    return pause(s, this.signal);
  }

  path(name: string): string {
    return path.join(this.outdir, name);
  }

  /** Whether the terminal (or the GUI) still runs. */
  alive(): boolean {
    return this.termPid > 0 && pidAlive(this.termPid);
  }

  /** The pointer to (x, y) of the terminal area. */
  moveAt(x: number, y: number): Promise<void> {
    return this.be.move(x + this.origin.x, y + this.origin.y);
  }

  /** MODS down before a button or wheel step, up after it, with a moment in
   * between: WezTerm on Wayland took a click that came right after Ctrl as
   * a plain one now and then. */
  async modsDown(mods: string): Promise<void> {
    for (const k of modKeys(mods)) await this.be.keydown(k);
    if (mods) await this.sleep(0.05);
  }

  async modsUp(mods: string): Promise<void> {
    if (mods) await this.sleep(0.05);
    for (const k of modKeys(mods)) await this.be.keyup(k);
  }

  /** The terminal's cell grid, as temzdf last logged it. */
  geometry(): Geometry | undefined {
    let log: string;
    try {
      log = readFileSync(this.path("state/temzdf/temzdf.log"), "utf8");
    } catch {
      return undefined;
    }
    const all = [...log.matchAll(/geometry ([0-9]+)x([0-9]+) cells of ([0-9]+)x([0-9]+) px/g)];
    const m = all.at(-1);
    if (!m) return undefined;
    return { cols: Number(m[1]), rows: Number(m[2]), cw: Number(m[3]), ch: Number(m[4]) };
  }

  /** A screenshot of what wait_ready and wait_still watch: the screen (on
   * Windows the drawing area only: the desktop has a clock). */
  async watchShot(file: string): Promise<void> {
    if (this.mac) await mac.watchShot(this, file);
    else if (this.win32) await win.watchShot(this, file);
    else await this.be.shot(file);
  }

  /** Windows: QMP and the guest (backends/windows-start.ts). */
  win32: WinState | undefined;

  /** macOS: the display and `termctl serve` (backends/macos-start.ts). */
  mac: MacState | undefined;

  launchMacos(argv: string[]): Promise<number> {
    return mac.launchMacos(this, argv);
  }

  serveMacos(): Promise<void> {
    return mac.serveMacos(this);
  }

  // ---- Starting.

  /** The display server, the terminal running COMMAND, the first screen. */
  async start(): Promise<void> {
    this.rec.phase("launch");
    writeFileSync(this.path("command.txt"), `${this.argv.join("\n")}\n`);
    this.runtimeDir = makeRuntimeDir();
    const runtimeDir = this.runtimeDir;
    // Removing it at the end ends any session left behind (servers end once
    // their socket is gone).
    this.procs.defer(() => rmSync(runtimeDir, { recursive: true, force: true }));
    this.env.XDG_RUNTIME_DIR = runtimeDir;
    for (const k of ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID", "TMUX", "TMUX_PANE"]) delete this.env[k];
    if (this.opts.pixels) this.env.TEMZDF_PIXELS = this.opts.pixels;
    this.procs.env = this.env;
    this.kittySocket = path.join(runtimeDir, "kitty.sock");
    const { display } = this.opts;
    if (display === "x11") await this.startX11();
    else if (display === "wayland") await this.startWayland();
    else if (display === "macos") await mac.startMacos(this);
    else if (display === "windows") await win.startWindows(this);
    else throw new StartError(`e2e: -d ${display} is not in the TypeScript harness yet`);
    rmSync(this.path("opened.txt"), { force: true });
    this.cleanEnv = cleanEnv(this.outdir, openerShim(runtimeDir, this.outdir), this.env);
    await this.launch();
  }

  /** Xvfb of the run's own (a free display, -displayfd), and a session bus
   * for an input method. */
  async startX11(): Promise<void> {
    const { width, height, term, ime } = this.opts;
    // The GUI's window has decorations around its drawing area.
    const w = term === "gui" ? width + 100 : width;
    const h = term === "gui" ? height + 100 : height;
    this.screen = { w, h };
    const xvfb = this.procs.start("Xvfb", ["-displayfd", "3", "-screen", "0", `${w}x${h}x24`, "-nolisten", "tcp"], {
      stdio: ["ignore", "ignore", "ignore", "pipe"],
    });
    const number = await readLine(xvfb, 3, 10, this.signal);
    if (number === undefined) throw new StartError("e2e: Xvfb did not start");
    this.env.DISPLAY = `:${number}`;
    if (ime) await this.startSessionBus();
    this.be = x11(this.procs);
  }

  /** Headless sway in the run's runtime directory: one output of the
   * window's size, so the only window fills it (no borders) and the
   * terminal area, or the GUI's drawing area, starts at 0,0; the window
   * otherWindow opens floats over it instead. Keys and the pointer through
   * wlinput (E2E_WLINPUT: one built already). */
  async startWayland(): Promise<void> {
    const { width, height, ime } = this.opts;
    this.screen = { w: width, h: height };
    if (ime) await this.startSessionBus();
    const conf = path.join(this.runtimeDir, "sway.conf");
    writeFileSync(
      conf,
      `output HEADLESS-1 resolution ${width}x${height} position 0 0\ndefault_border none\ndefault_floating_border none\nfor_window [app_id="e2e-other"] floating enable\n`,
    );
    this.startLogged(
      "sway",
      ["-c", conf],
      { ...this.env, WLR_BACKENDS: "headless", WLR_RENDERER: "pixman", WLR_LIBINPUT_NO_DEVICES: "1" },
      this.path("sway.log"),
    );
    const find = (re: RegExp) => readdirSync(this.runtimeDir).find((f) => re.test(f));
    await until(() => !!find(/^wayland-[0-9]$/) && !!find(/^sway-ipc\./), 5, 0.1, this.signal);
    const display = find(/^wayland-[0-9]$/);
    const sock = find(/^sway-ipc\./);
    if (!display || !sock) throw new StartError("e2e: sway did not start (see sway.log)");
    this.env.WAYLAND_DISPLAY = display;
    this.env.SWAYSOCK = path.join(this.runtimeDir, sock);
    delete this.env.DISPLAY;
    let wlinput = process.env.E2E_WLINPUT;
    if (!wlinput) {
      await this.procs.run("cargo", ["build", "-q", "-p", "wlinput"]);
      wlinput = "target/debug/wlinput";
    }
    const log = openSync(this.path("wlinput.log"), "a");
    let child: ChildProcess;
    try {
      child = this.procs.start(wlinput, ["--size", `${width}x${height}`], { stdio: ["pipe", "pipe", log] });
    } finally {
      closeSync(log);
    }
    const wl = new Lines(child);
    // wlinput ends with its input.
    this.procs.defer(() => wl.close());
    this.be = wayland(this.procs, wl);
  }

  /** A session bus (an input method needs one), as dbus-run-session gives. */
  async startSessionBus(): Promise<void> {
    // With the runtime directory the harness was given (scripts/e2e.sh
    // started its bus before making the run's): what the bus starts (gvfs,
    // the document portal) mounts there, not in the run's.
    const bus = this.procs.start("dbus-daemon", ["--session", "--nofork", "--print-address=3"], {
      stdio: ["ignore", "ignore", "ignore", "pipe"],
      env: { ...this.env, XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR },
    });
    const address = await readLine(bus, 3, 10, this.signal);
    if (!address) throw new StartError("e2e: no session bus");
    this.env.DBUS_SESSION_BUS_ADDRESS = address;
  }

  /** The terminal (or the GUI) running COMMAND, until its first screen. */
  async launch(): Promise<void> {
    const { term, mux, width, height } = this.opts;
    let argv = this.argv;
    let muxStartup = 0;
    if (mux === "zellij") {
      // A single pane running COMMAND, no frames or tips; quit with the app.
      const kdl = (v: string) => `"${v.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"`;
      const zdir = this.path("zellij");
      mkdirSync(zdir, { recursive: true });
      writeFileSync(
        path.join(zdir, "config.kdl"),
        "pane_frames false\nshow_startup_tips false\nshow_release_notes false\n",
      );
      let layout = `layout {\n    pane command=${kdl(argv[0]!)} cwd=${kdl(process.cwd())} close_on_exit=true {\n`;
      if (argv.length > 1)
        layout += `        args${argv
          .slice(1)
          .map((a) => ` ${kdl(a)}`)
          .join("")}\n`;
      layout += "    }\n}\n";
      writeFileSync(path.join(zdir, "layout.kdl"), layout);
      argv = ["zellij", "--config", path.join(zdir, "config.kdl"), "--layout", path.join(zdir, "layout.kdl")];
      muxStartup = 3;
    } else if (mux === "tmux") {
      // A private server, no status line, passthrough and mouse on.
      const tdir = this.path("tmux");
      mkdirSync(tdir, { recursive: true });
      writeFileSync(
        path.join(tdir, "tmux.conf"),
        "set -g allow-passthrough on\nset -g mouse on\nset -g status off\nset -g escape-time 0\n",
      );
      const server = `e2e-${process.pid}`;
      argv = [
        "tmux",
        "-f",
        path.join(tdir, "tmux.conf"),
        "-L",
        server,
        "new-session",
        "-c",
        process.cwd(),
        `${argv.map(command.shQuote).join(" ")} `,
      ];
      muxStartup = 1;
      // The server outlives the terminal; kill-server leaves its socket file.
      this.procs.defer(async () => {
        const sock = await this.procs.exec("tmux", ["-L", server, "display-message", "-p", "#{socket_path}"], {
          env: this.env,
        });
        await this.procs.exec("tmux", ["-L", server, "kill-server"], { env: this.env });
        if (sock.code === 0 && sock.stdout.trim()) rmSync(sock.stdout.trim(), { force: true });
      });
    }
    const log = this.path("terminal.log");
    let startup: number;
    const env = this.cleanEnv;
    if (this.opts.display === "macos") startup = await this.launchMacos(argv);
    else if (this.opts.display === "windows") startup = await win.launchWindows(this);
    else
      switch (term) {
        case "kitty":
          this.termPid =
            this.startLogged(
              "kitty",
              [
                "--config",
                "NONE",
                "-o",
                "font_size=12",
                "-o",
                `initial_window_width=${width}`,
                "-o",
                `initial_window_height=${height}`,
                "-o",
                "window_padding_width=0",
                "-o",
                "hide_window_decorations=yes",
                "-o",
                "remember_window_size=no",
                "-o",
                "allow_remote_control=socket-only",
                "-o",
                `linux_display_server=${this.opts.display}`,
                "--listen-on",
                `unix:${this.kittySocket}`,
                ...argv,
              ],
              env,
              log,
            ).pid ?? 0;
          startup = 3;
          break;
        case "ghostty": {
          // Ghostty sizes windows in cells; 10x21 px cells at its default
          // font size. No decorations or padding (expect needs the terminal
          // area at the window's top left), no clipboard toast, a black
          // background (as kitty's and the mock terminal's).
          const genv = this.opts.display === "wayland" ? { ...env, GDK_BACKEND: "wayland" } : env;
          this.termPid =
            this.startLogged(
              "ghostty",
              [
                "--config-default-files=false",
                "--gtk-single-instance=false",
                "--window-decoration=false",
                "--window-padding-x=0",
                "--window-padding-y=0",
                "--app-notifications=no-clipboard-copy",
                "--background=000000",
                `--window-width=${Math.floor(width / 10)}`,
                `--window-height=${Math.floor(height / 21)}`,
                "-e",
                ...argv,
              ],
              genv,
              log,
            ).pid ?? 0;
          startup = 6;
          break;
        }
        case "gui": {
          // A window manager, as on a desktop (decorations, focus).
          if (this.opts.display === "x11") {
            this.startLogged("xfwm4", [], this.env, this.path("wm.log"));
            await this.sleep(1);
          }
          const genv: NodeJS.ProcessEnv = { ...env, VK_DRIVER_FILES: process.env.TEMZDF_LAVAPIPE_ICD ?? "" };
          if (this.opts.ime) {
            await this.startIme();
            if (this.opts.display === "x11") genv.XMODIFIERS = "@im=fcitx";
          }
          this.termPid = this.startLogged(argv[0]!, argv.slice(1), genv, log).pid ?? 0;
          startup = 5;
          break;
        }
        case "wezterm":
          // -n: not the user's config. No ligatures nor contextual alternates
          // (doc/terminals/terminal-compat.md); cells of 10x22 px.
          this.termPid =
            this.startLogged(
              "wezterm",
              [
                "-n",
                "--config",
                'front_end="Software"',
                "--config",
                `enable_wayland=${this.opts.display === "wayland"}`,
                "--config",
                'window_decorations="NONE"',
                "--config",
                "enable_tab_bar=false",
                "--config",
                "window_padding={left=0,right=0,top=0,bottom=0}",
                "--config",
                'harfbuzz_features={"calt=0","clig=0","liga=0"}',
                "--config",
                `initial_cols=${Math.floor(width / 10)}`,
                "--config",
                `initial_rows=${Math.floor(height / 22)}`,
                "start",
                "--always-new-process",
                "--cwd",
                process.cwd(),
                "--",
                ...argv,
              ],
              env,
              log,
            ).pid ?? 0;
          startup = 6;
          break;
      }
    // Until the first screen is up, at most the time startup used to take;
    // inside a multiplexer the whole of it.
    if (mux) {
      await this.sleep(startup + muxStartup);
      this.rec.phase("ready");
    } else {
      await this.waitReady(startup + muxStartup);
    }
    // A command that ends at once (a missing file, a bad option) leaves no
    // window to wait for.
    if (!this.alive()) {
      throw new StartError(`e2e: ${term} ended while starting; the end of its output:\n${tail(log, 20)}`);
    }
    // The GUI's drawing area: where its window's inside is on the screen
    // (on macOS every window's: below the menu bar).
    if (term === "gui" && this.opts.display !== "windows") {
      this.guiBin = this.argv.find((w) => w.endsWith("/temzdf-gui")) ?? "";
      this.temzdf = path.join(path.dirname(this.guiBin), "temzdf");
    }
    if (term === "gui" || this.opts.display === "macos") {
      const r = await this.be.windowRect(this.termPid);
      if (!r) throw new StartError(`e2e: no window of ${term}`);
      this.origin = { x: r.x, y: r.y };
      this.win = { ...this.win, w: r.w, h: r.h };
    }
    if (this.opts.display === "macos") await this.serveMacos();
    // Without a window manager WezTerm only gets mouse events once focused.
    if (term === "wezterm" && this.opts.display === "x11") {
      await this.procs.exec("xdotool", ["search", "--class", "wezterm", "windowfocus"]);
      await this.sleep(0.3);
    }
  }

  /** fcitx5 with Mozc, its HOME in OUTDIR (ended with the run: it and the
   * mozc_server it starts outlive it otherwise, known by their HOME). */
  async startIme(): Promise<void> {
    const home = this.path("ime");
    mkdirSync(path.join(home, ".config/fcitx5"), { recursive: true });
    writeFileSync(
      path.join(home, ".config/fcitx5/profile"),
      "[Groups/0]\nName=Default\nDefault Layout=us\nDefaultIM=mozc\n\n[Groups/0/Items/0]\nName=keyboard-us\nLayout=\n\n[Groups/0/Items/1]\nName=mozc\nLayout=\n\n[GroupOrder]\n0=Default\n",
    );
    const env = { ...this.env, HOME: home, XDG_CONFIG_HOME: path.join(home, ".config") };
    await this.procs.logged("fcitx5", ["-d", "--replace"], this.path("ime.log"), env);
    this.procs.defer(async () => {
      await this.procs.exec("fcitx5-remote", ["-e"], { env: this.env }).catch(() => {});
      killByEnv(`HOME=${home}`);
    });
    await this.sleep(2);
  }

  /** Starts a process that stays, its output into LOG. */
  startLogged(cmd: string, args: string[], env: NodeJS.ProcessEnv, log: string): ChildProcess {
    const out = openSync(log, "w");
    try {
      return this.procs.start(cmd, args, { env, stdio: ["ignore", out, out] });
    } finally {
      closeSync(out);
    }
  }

  /** Until the first screen is up, at most LIMIT seconds: temzdf's log
   * saying it wrote a complete frame (when the command runs temzdf in the
   * terminal), then the screen unchanged over 150 ms and not of one colour. */
  async waitReady(limit: number): Promise<void> {
    const deadline = performance.now() + limit * 1000;
    const log = this.path("state/temzdf/temzdf.log");
    const a = this.path("ready.a.png");
    const b = this.path("ready.b.png");
    if (this.temzdfInTerminal) {
      await until(
        () => grepFile(log, /complete frame written/) || performance.now() >= deadline || !this.alive(),
        limit + 1,
        0.05,
        this.signal,
      );
      this.rec.phase("written");
    }
    await this.watchShot(a);
    while (performance.now() < deadline && this.alive()) {
      await this.sleep(0.15);
      await this.watchShot(b);
      if (sameBytes(a, b) && (await colours(b, this.signal)) > 1) {
        rmSync(a, { force: true });
        rmSync(b, { force: true });
        this.rec.phase("ready");
        return;
      }
      renameOver(b, a);
    }
    rmSync(a, { force: true });
    rmSync(b, { force: true });
    this.rec.phase("ready (timed out)");
  }

  // ---- Input (what the helpers send is recorded: record.ts).

  /** N wheel steps (N < 0 scrolls up) at (x, y) of the terminal area,
   * 30 ms apart, in one call (separate ones are too slow for a flick); the
   * steps are recorded at the times it spaces them. */
  async wheel(n: number, x = Math.floor(this.opts.width / 2), y = Math.floor(this.opts.height / 2), mods = "") {
    let btn = 5;
    let dir = "down";
    if (n < 0) {
      btn = 4;
      dir = "up";
      n = -n;
    }
    await this.moveAt(x, y);
    this.rec.record(["move", x, y, mods || undefined]);
    await this.modsDown(mods);
    const first = this.rec.now();
    await this.be.clicks(btn, n, 30);
    for (let i = 0; i < n; i++) this.rec.record(["wheel", dir, x, y, mods || undefined], first + 30 * i);
    await this.modsUp(mods);
  }

  async down(x: number, y: number, btn = 1, mods = ""): Promise<void> {
    await this.moveAt(x, y);
    this.rec.record(["move", x, y, mods || undefined]);
    await this.modsDown(mods);
    await this.be.button("down", btn);
    this.rec.record(["press", x, y, buttonName(btn), mods || undefined]);
    this.held = { btn, mods };
    this.pointerAt = { x, y };
  }

  async up(btn?: number): Promise<void> {
    const b = btn ?? this.held?.btn ?? 1;
    const mods = this.held?.mods ?? "";
    await this.be.button("up", b);
    this.rec.record(["release", this.pointerAt.x, this.pointerAt.y, buttonName(b), mods || undefined]);
    await this.modsUp(mods);
    this.held = undefined;
  }

  /** The pointer: a drag while `down` holds a button, else hovering. */
  async move(x: number, y: number): Promise<void> {
    await this.moveAt(x, y);
    this.pointerAt = { x, y };
    if (this.held) this.rec.record(["drag", x, y, buttonName(this.held.btn), this.held.mods || undefined]);
    else this.rec.record(["move", x, y]);
  }

  async dragto(x: number, y: number, x2: number, y2: number, steps = 10): Promise<void> {
    await this.down(x, y);
    for (let i = 1; i <= steps; i++) {
      await this.move(x + Math.trunc(((x2 - x) * i) / steps), y + Math.trunc(((y2 - y) * i) / steps));
      await this.sleep(0.03);
    }
    await this.up();
  }

  async click(x: number, y: number, btn = 1, mods = ""): Promise<void> {
    const b = buttonName(btn);
    await this.moveAt(x, y);
    this.rec.record(["move", x, y, mods || undefined]);
    await this.modsDown(mods);
    await this.be.clicks(btn, 1, 0);
    this.rec.record(["press", x, y, b, mods || undefined]);
    this.rec.record(["release", x, y, b, mods || undefined]);
    await this.modsUp(mods);
  }

  async keys(keys: string[]): Promise<void> {
    for (const k of keys) {
      await this.be.key(k);
      // A key without a Vim notation (F5, W, bracketleft) is recorded
      // without one, as scripts/e2e.sh did (its key_notation failed inside
      // $(...), which did not stop the run): sessions do not replay it.
      let notation = "";
      try {
        notation = keyNotation(k);
      } catch (e) {
        console.error(e instanceof Error ? e.message : e);
      }
      this.rec.record(["key", notation]);
    }
  }

  /** Literal text, newlines as the Return key (Ghostty ignores those
   * xdotool types), MS between keys. */
  async typeText(text: string, ms?: number): Promise<void> {
    const lines = text.split("\n");
    for (let i = 0; i < lines.length; i++) {
      if (lines[i] !== "") await this.be.type(lines[i]!, ms);
      if (i < lines.length - 1) await this.be.key("Return");
    }
    this.rec.record(["key", typedNotation(text)]);
  }

  // ---- What differs per display server in the judging (macOS, Windows:
  // their backends replace these).

  /** On macOS a mask of the row's window edges (screenRestored). */
  rowMask = "";

  /** macOS draws a border and rounded corners over every window: those
   * pixels go black on both sides of NAME's comparison. Nothing elsewhere. */
  async maskWindowEdges(name: string, w: number, h: number): Promise<void> {
    if (this.mac) await mac.maskWindowEdges(this, name, w, h);
  }

  /** OUTDIR/NAME.window-mask.png (macOS). */
  async windowEdgesMask(name: string, w: number, h: number): Promise<void> {
    if (!this.mac) throw new Error("window edges are macOS's");
    await mac.windowEdgesMask(this, name, w, h);
  }

  /** The GUI's expected screen rendered in the Windows guest (expect).
   * Whether it was. */
  renderGuiInGuest(name: string): Promise<boolean> {
    return win.renderGuiInGuest(this, name);
  }

  // ---- The end.

  /** After the scenario: `q`, at most half a second for the terminal to go. */
  async quit(): Promise<{ crashed: boolean }> {
    this.rec.phase("quit");
    const crashed = !this.alive();
    await this.be.key("q").catch(() => {});
    for (let i = 0; i < 10 && this.alive(); i++) await this.sleep(0.05);
    return { crashed };
  }
}

// ---- Helpers of this module.

/** The first line a child writes on file descriptor FD, at most S seconds. */
async function readLine(child: ChildProcess, fd: number, s: number, signal: AbortSignal): Promise<string | undefined> {
  const stream = child.stdio[fd] as NodeJS.ReadableStream | null;
  if (!stream) return undefined;
  let text = "";
  const timeout = AbortSignal.any([signal, AbortSignal.timeout(s * 1000)]);
  try {
    for await (const chunk of stream as AsyncIterable<Buffer>) {
      timeout.throwIfAborted();
      text += chunk.toString();
      if (text.includes("\n")) return text.slice(0, text.indexOf("\n")).trim();
    }
  } catch {
    signal.throwIfAborted();
  }
  return text.trim() || undefined;
}

export function grepFile(file: string, re: RegExp): boolean {
  try {
    return re.test(readFileSync(file, "utf8"));
  } catch {
    return false;
  }
}

export function sameBytes(a: string, b: string): boolean {
  try {
    return readFileSync(a).equals(readFileSync(b));
  } catch {
    return false;
  }
}

export function renameOver(from: string, to: string): void {
  renameSync(from, to);
}

export function tail(file: string, n: number): string {
  try {
    return readFileSync(file, "utf8")
      .split("\n")
      .slice(-n - 1)
      .join("\n");
  } catch {
    return "";
  }
}

/** Whether process PID runs (children are reaped as they end). */
export function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** A program on PATH. */
export function which(name: string, env: NodeJS.ProcessEnv): string {
  for (const dir of (env.PATH ?? "").split(path.delimiter)) {
    const p = path.join(dir, name);
    try {
      if (statSync(p).isFile()) return p;
    } catch {}
  }
  throw new Error(`${name} not found on PATH`);
}

/** Kills this user's processes whose environment has ENTRY (VAR=value). */
export function killByEnv(entry: string): void {
  if (process.platform !== "linux") return;
  const uid = process.getuid?.();
  for (const p of readdirSync("/proc")) {
    if (!/^[0-9]+$/.test(p)) continue;
    try {
      if (statSync(`/proc/${p}/environ`).uid !== uid) continue;
      if (readFileSync(`/proc/${p}/environ`, "latin1").split("\0").includes(entry)) process.kill(Number(p));
    } catch {}
  }
}
