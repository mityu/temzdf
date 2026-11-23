// Starting a run on macOS (scripts/e2e.sh -d macos as it was): a virtual
// display at scale 1 (screenshot pixels are points and match the
// terminal's) with room above the window for the menu bar every display
// shows and for title bars, the terminal started through `open` with the
// run's environment, `termctl serve`, and macOS's window edges masked in
// comparisons. Not run since the move to TypeScript (todo.md).

import { chmodSync, existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { Lines } from "../lines.ts";
import { magick } from "../magick.ts";
import { until } from "../procs.ts";
import { type Run, StartError } from "../run.ts";
import { macos, termctl } from "./macos.ts";

export const menuBar = 40;

export async function startMacos(r: Run): Promise<void> {
  const { width, height, term } = r.opts;
  // A locked screen takes no screenshots: every run would fail while starting.
  const io = await r.procs.exec("ioreg", ["-n", "Root", "-d1"]);
  if (io.stdout.includes('"IOConsoleLocked" = Yes')) {
    throw new StartError("e2e: the screen is locked; unlock it (or use scripts/macos-vm.sh)");
  }
  await r.procs.run("scripts/macos-gui.sh", ["build"]);
  // A locked screen takes no input and gives no screenshots, and the
  // display going to sleep locks it: kept awake while this runs.
  r.procs.start("caffeinate", ["-d", "-w", String(process.pid)]);
  let w = width;
  let h = height + menuBar;
  if (term === "gui" || term === "wezterm") {
    w = width + 100;
    h = height + 100 + menuBar;
  }
  r.screen = { w, h };
  // The user's clipboard as it was (text only), at the end.
  const clip = await r.procs.exec("pbpaste", []);
  r.procs.defer(() => r.procs.feed("pbcopy", [], clip.stdout));
  // In a VM (scripts/macos-vm.sh) its own screen is nobody's desktop: no
  // virtual display there. E2E_MACOS_DISPLAY=main|virtual decides otherwise.
  const vm = (await r.procs.exec("sysctl", ["-n", "kern.hv_vmm_present"])).stdout.trim() === "1";
  const which = process.env.E2E_MACOS_DISPLAY || (vm ? "main" : "virtual");
  let disp: { x: number; y: number; w: number; h: number };
  if (which === "main") {
    const [x, y, dw, dh, scale] = (await r.procs.run(termctl, ["main"])).trim().split(/\s+/).map(Number);
    if (scale !== 1 || dw! < w || dh! < h) {
      throw new StartError(
        `e2e: the main display (${dw}x${dh} at scale ${scale}) needs scale 1 and ${w}x${h} at least`,
      );
    }
    disp = { x: x!, y: y!, w: dw!, h: dh! };
  } else {
    const out = r.path("vdisp.out");
    r.startLogged("target/macos-gui/vdisp", [String(w), String(h), "1"], r.env, out);
    await until(() => existsSync(out) && readFileSync(out, "utf8").trim() !== "", 5, 0.1, r.signal);
    const f = readFileSync(out, "utf8").trim().split(/\s+/);
    if (f.length < 5) throw new StartError(`e2e: no virtual display: ${f.join(" ")}`);
    disp = { x: Number(f[1]), y: Number(f[2]), w: Number(f[3]), h: Number(f[4]) };
  }
  r.mac = { disp, term, kittySocket: r.kittySocket };
  r.be = macos(r.procs, r.mac);
}

/** The terminal (or the GUI) on the display, below the menu bar; its
 * startup time. `open` starts apps in / with launchd's environment: a
 * script gives the command this run's environment and directory, and
 * starts it once the window is on the virtual display (the go file). */
export async function launchMacos(r: Run, argv: string[]): Promise<number> {
  const { width, height, term } = r.opts;
  const mac = r.mac!;
  const launch = path.join(r.runtimeDir, "launch.sh");
  const go = path.join(r.runtimeDir, "go");
  const q = (s: string) => `'${s.replaceAll("'", `'\\''`)}'`;
  const keep =
    /^(PATH|TERM|TERM_PROGRAM.*|COLORTERM|TERMINFO|KITTY_.*|WEZTERM_.*|GHOSTTY_.*|WINDOWID|_|SHLVL|PWD|OLDPWD)$/;
  const lines = ["#!/usr/bin/env bash", `export PATH=${q(r.cleanEnv.PATH ?? "")}`];
  for (const [k, v] of Object.entries(r.env).sort()) {
    if (v === undefined || keep.test(k) || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(k)) continue;
    lines.push(`export ${k}=${q(v)}`);
  }
  lines.push(`cd ${q(process.cwd())}`);
  // Ghostty runs it through login(1), which leaves "Last login: ..." on
  // the screen: empty, as elsewhere (screenRestored).
  lines.push(`printf '\\033[H\\033[2J'`);
  lines.push(`for ((i = 0; i < 600; i++)); do [[ -e ${q(go)} ]] && break; sleep 0.05; done`);
  const unset = [
    "TEMZDF_LOG_DIR",
    "TEMZDF_SESSION_NAME",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
    "TMUX",
    "TMUX_PANE",
  ];
  const cleanEnv = [
    "env",
    ...unset.flatMap((u) => ["-u", u]),
    `XDG_STATE_HOME=${r.outdir}/state`,
    `TEMZDF_RECORD_INPUT=${r.outdir}/input.txt`,
    `PATH=${r.cleanEnv.PATH}`,
  ];
  lines.push(`exec ${cleanEnv.map(q).join(" ")} "$@"`);
  writeFileSync(launch, `${lines.join("\n")}\n`);
  chmodSync(launch, 0o755);
  const front = (await r.procs.run(termctl, ["front"])).trim();
  let app = "";
  let proc = "";
  let startup: number;
  let targs: string[] = [];
  switch (term) {
    case "kitty":
      // Cells from the top left (kitty centres the grid in the window).
      app = "kitty";
      proc = "kitty";
      startup = 3;
      targs = [
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
        "placement_strategy=top-left",
        "-o",
        "hide_window_decorations=yes",
        "-o",
        "remember_window_size=no",
        "-o",
        "allow_remote_control=socket-only",
        "--listen-on",
        `unix:${r.kittySocket}`,
        launch,
      ];
      break;
    case "ghostty":
      app = "Ghostty";
      proc = "ghostty";
      startup = 6;
      targs = [
        "--config-default-files=false",
        "--window-save-state=never",
        "--quit-after-last-window-closed=true",
        "--alpha-blending=linear-corrected",
        "--keybind=alt+left=unbind",
        "--keybind=alt+right=unbind",
        "--window-decoration=false",
        "--window-padding-x=0",
        "--window-padding-y=0",
        "--app-notifications=no-clipboard-copy",
        "--background=000000",
        `--window-width=${Math.floor(width / 8)}`,
        `--window-height=${Math.floor(height / 17)}`,
        "-e",
        launch,
      ];
      break;
    case "wezterm":
      app = "WezTerm";
      proc = "wezterm-gui";
      startup = 6;
      targs = [
        "-n",
        "--config",
        'front_end="WebGpu"',
        "--config",
        'window_decorations="TITLE|RESIZE"',
        "--config",
        "enable_tab_bar=false",
        "--config",
        "window_padding={left=0,right=0,top=0,bottom=0}",
        "--config",
        `initial_cols=${Math.floor(width / 7)}`,
        "--config",
        `initial_rows=${Math.floor(height / 16)}`,
        "start",
        "--always-new-process",
        "--cwd",
        process.cwd(),
        "--",
        launch,
      ];
      break;
    case "gui":
      r.termPid = r.startLogged(argv[0]!, argv.slice(1), r.cleanEnv, r.path("terminal.log")).pid ?? 0;
      startup = 5;
      break;
  }
  const pids = async () => (await r.procs.exec("pgrep", ["-x", proc])).stdout.split("\n").filter(Boolean);
  const before = new Set(term === "gui" ? [] : await pids());
  if (term !== "gui") {
    // -g: not to the front (Ghostty opens its window only once activated).
    const log = r.path("terminal.log");
    await r.procs.run("open", ["-g", "-n", "-a", app, "--stdout", log, "--stderr", log, "--args", ...targs, ...argv]);
  }
  const windows = async () =>
    r.termPid > 0 && (await r.procs.exec(termctl, ["windows", String(r.termPid)])).stdout.trim() !== "";
  for (let i = 0; i < 150; i++) {
    if (r.termPid === 0) for (const p of await pids()) if (!before.has(p)) r.termPid = Number(p);
    if (r.termPid > 0) {
      if (await windows()) break;
      if (i % 20 === 10 && term === "ghostty") await r.procs.exec(termctl, ["activate", String(r.termPid)]);
    }
    await r.sleep(0.1);
  }
  if (!(await windows())) throw new StartError(`e2e: ${term} did not open a window`);
  // Not our child: ended at the end.
  r.procs.defer(async () => {
    try {
      process.kill(r.termPid);
    } catch {}
    for (let i = 0; i < 100 && r.alive(); i++) await new Promise((res) => setTimeout(res, 50));
  });
  r.rec.phase("window");
  // Below the menu bar; at scale 1 it draws again at that density, then the
  // command starts.
  await r.procs.run(termctl, ["move", String(r.termPid), String(mac.disp.x), String(mac.disp.y + menuBar)]);
  if ((await r.procs.run(termctl, ["front"])).trim() !== front) await r.procs.exec(termctl, ["activate", front]);
  await r.sleep(0.5);
  writeFileSync(go, "");
  return startup;
}

/** `termctl serve` (2 ms after each event: typing at about 4 ms a
 * character), and the window in front for the run, as on Linux, where it
 * has the focus (unfocused, Ghostty draws a hollow cursor over the images). */
export async function serveMacos(r: Run): Promise<void> {
  const child = r.procs.start(termctl, ["serve", String(r.termPid), r.opts.term === "wezterm" ? "global" : "pid"], {
    stdio: ["pipe", "pipe", "ignore"],
    env: { ...r.env, TERMCTL_SETTLE_MS: process.env.E2E_SETTLE_MS || "2" },
  });
  const tc = new Lines(child);
  r.mac!.tc = tc;
  // The front and the pointer back to where they were.
  r.procs.defer(async () => {
    await tc.ok("done", "termctl", AbortSignal.timeout(5000)).catch(() => {});
    tc.close();
  });
  await tc.ok("front", "termctl", r.signal);
}

/** OUTDIR/NAME.window-mask.png: black where the window's edges show (its
 * border, rounded corners, a title bar's separator line), clear elsewhere,
 * over W x H pixels from the drawing area's origin. */
export async function windowEdgesMask(r: Run, name: string, w: number, h: number): Promise<void> {
  const mac = r.mac!;
  const first = (await r.procs.run(termctl, ["windows", String(r.termPid)])).split("\n")[0] ?? "";
  const [wid, fx, fy, fw, fh] = first.trim().split(/\s+/);
  const shot = r.path(`${name}.window.png`);
  await r.procs.run("screencapture", ["-x", "-o", "-l", wid!, shot]);
  const ox = mac.disp.x + r.origin.x - Number(fx);
  const oy = mac.disp.y + r.origin.y - Number(fy);
  const line = oy > 0 ? ["-draw", `line 0,${oy} ${Number(fw) - 1},${oy}`] : [];
  const out = r.path(`${name}.window-mask.png`);
  await magick(
    [
      shot,
      "-alpha",
      "extract",
      "-threshold",
      "99%",
      "-negate",
      "-morphology",
      "Dilate",
      "Square:1",
      "-fill",
      "none",
      "-stroke",
      "white",
      "-draw",
      `rectangle 0,0 ${Number(fw) - 1},${Number(fh) - 1}`,
      ...line,
      "-crop",
      `${w}x${h}+${ox}+${oy}`,
      "+repage",
      "-background",
      "black",
      "-alpha",
      "shape",
      "-strip",
      out,
    ],
    r.signal,
  );
  rmSync(shot, { force: true });
  // Ghostty (1.3) shows a handle for dragging the split (three dots) at
  // the top middle of the terminal when the pointer comes near the top.
  if (r.opts.term === "ghostty") {
    await magick(
      [out, "-fill", "black", "-draw", `rectangle ${Math.floor(w / 2) - 12},0 ${Math.floor(w / 2) + 12},7`, out],
      r.signal,
    );
  }
}

/** macOS draws a border over every window's edge and rounds its corners:
 * those pixels go black on both sides of NAME's comparison. */
export async function maskWindowEdges(r: Run, name: string, w: number, h: number): Promise<void> {
  await windowEdgesMask(r, name, w, h);
  const mask = r.path(`${name}.window-mask.png`);
  for (const f of ["actual", "expected"]) {
    const file = r.path(`${name}.${f}.png`);
    await magick([file, mask, "-composite", file], r.signal);
  }
}

/** What wait_ready and wait_still watch on macOS: the window only, where
 * it was put (the display around it is never of one colour). */
export async function watchShot(r: Run, file: string): Promise<void> {
  const d = r.mac!.disp;
  await r.procs.run("screencapture", ["-x", "-R", `${d.x},${d.y + menuBar},${r.opts.width},${r.opts.height}`, file]);
}
