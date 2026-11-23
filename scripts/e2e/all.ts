// Runs the judged e2e scenarios (tests/e2e/NAME.ts) in one or more
// terminals, and sums up: each run is `node scripts/e2e/main.ts` of its
// own (scripts/e2e-all.sh runs this in the dev shell).
//
// Usage: scripts/e2e-all.sh [-t TERM]... [-d DISP] [-P PATH] [-o DIR] [-j N] [-R] [SCENARIO...]
//   -t TERM   kitty, ghostty, wezterm or gui (repeatable; default: every one
//             that can run here: in a macOS VM not kitty, which needs OpenGL)
//   -d DISP   display server, passed on (x11, wayland; macos; windows)
//   -P PATH   how clients take pixels, passed on (shared, socket)
//   -o DIR    where the runs go, as DIR/TERM/SCENARIO (default target/e2e-all,
//             target/e2e-all-DISP with -d)
//   -j N      run N scenarios at once (default 1). A scenario runs in its
//             terminals one after another, never two of it at once: some use
//             fixed paths (target/e2e-sync, target/e2e-reload, ...). Each run
//             has its own display server. On macOS -j is 1.
//   -R        draw `expect`'s comparisons with the release builds
//             (target/release/temzdf, temzdf-gui: E2E_RENDERER and
//             E2E_GUI_RENDERER), much quicker; builds them first
//   SCENARIO  scenario names (default: every judged one for the terminal;
//             gui is the GUI's only)
// Prints PASS/FAIL/SKIP counts per terminal and scenario and their sum;
// exits 1 if anything failed or crashed. Builds nothing but -R's renderers,
// termpix's scroll_demo example (the scroll_demo scenario's command) and
// wlinput (-d wayland): build temzdf and temzdf-gui first.

import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { parseArgs } from "node:util";

const usage = "usage: scripts/e2e-all.sh [-t TERM]... [-d DISP] [-P PATH] [-o DIR] [-j N] [-R] [SCENARIO...]";

// The terminal scenarios, in a stable order, and the GUI's. Not on macOS:
// gui-focus (AppKit sends no presses for keys held on focus-in) and gui-ime
// (-I, fcitx5, is Linux's).
const judged = [
  "session",
  "terminal",
  "terminal-extras",
  "edit",
  "markdown",
  "tabs",
  "floats",
  "resize",
  "float-wide",
  "expect",
  "links",
  "preview",
  "search",
  "select",
  "select-autoscroll",
  "viewer",
  "viewer-stress",
  "synctex",
  "synctex-reload",
  "reload",
  "no-graphics",
  "visual",
  "scroll_demo",
];
const guiJudged = process.platform === "darwin" ? ["gui"] : ["gui", "gui-focus", "gui-ime"];
// Scenarios that need the CPU to themselves (smooth scrolling and tiles
// arriving within their pauses): after the others, one at a time.
const alone: string[] = [];

interface Counts {
  pass: number;
  fail: number;
  skip: number;
  rc: number;
}

function cargo(args: string[]): void {
  execFileSync("cargo", args, { stdio: "inherit" });
}

async function main(argv: string[]): Promise<number> {
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: {
      t: { type: "string", multiple: true },
      d: { type: "string" },
      P: { type: "string" },
      o: { type: "string" },
      j: { type: "string", default: "1" },
      R: { type: "boolean" },
      h: { type: "boolean" },
    },
  });
  if (values.h) {
    console.error(usage);
    return 2;
  }
  const pass = [...(values.d ? ["-d", values.d] : []), ...(values.P ? ["-P", values.P] : [])];
  const outdir = values.o ?? `target/e2e-all${values.d ? `-${values.d}` : ""}`;
  let jobs = Number(values.j);
  if (values.R) {
    // Up to date for sure (cargo knows; a second when they are).
    cargo(["build", "-q", "--release", "-p", "temzdf", "-p", "temzdf-gui"]);
    process.env.E2E_RENDERER = path.resolve("target/release/temzdf");
    process.env.E2E_GUI_RENDERER = path.resolve("target/release/temzdf-gui");
  }
  // The scroll_demo scenario's command (a second when it is up to date).
  cargo(["build", "-q", "-p", "termpix", "--example", "scroll_demo"]);
  // Sway's runs drive it with wlinput: built once here, not by each run.
  if (values.d === "wayland") {
    cargo(["build", "-q", "-p", "wlinput"]);
    process.env.E2E_WLINPUT = path.resolve("target/debug/wlinput");
  }
  let inVm = false;
  if (process.platform === "darwin") {
    jobs = 1;
    try {
      inVm = execFileSync("sysctl", ["-n", "kern.hv_vmm_present"], { encoding: "utf8" }).trim() === "1";
    } catch {}
  }
  let terms = values.t ?? [];
  if (terms.length === 0) terms = inVm ? ["ghostty", "wezterm", "gui"] : ["kitty", "ghostty", "wezterm", "gui"];

  // The scenarios and, for each, the terminals it runs in.
  const start = performance.now();
  const inTerms = new Map<string, string[]>();
  for (const t of terms) {
    if (t === "kitty" && inVm) {
      console.log(`${t}: not in a VM (no OpenGL there)`);
      continue;
    }
    const list = positionals.length > 0 ? positionals : t === "gui" ? guiJudged : judged;
    for (const s of list) {
      // The GUI runs its own scenarios only (gui*), terminals not those.
      if ((t === "gui") !== s.startsWith("gui")) continue;
      inTerms.set(s, [...(inTerms.get(s) ?? []), t]);
      rmSync(path.join(outdir, t, `${s}.line`), { force: true });
      rmSync(path.join(outdir, t, `${s}.counts`), { force: true });
    }
  }
  const order = [...inTerms.keys()];

  /** Scenario S in terminal T; its line of the summary goes to OUT.line. */
  const runOne = async (t: string, s: string): Promise<void> => {
    const out = path.join(outdir, t, s);
    const t0 = performance.now();
    rmSync(out, { recursive: true, force: true });
    mkdirSync(out, { recursive: true });
    const file = path.join("tests/e2e", `${s}.ts`);
    if (!existsSync(file)) {
      writeFileSync(`${out}.line`, `${t} ${s}: no scenario\n`);
      if (jobs === 1) process.stdout.write(readFileSync(`${out}.line`));
      return;
    }
    const log = openSync(`${out}.log`, "w");
    const child = spawn(process.execPath, ["scripts/e2e/main.ts", "-t", t, ...pass, "-o", out, file], {
      stdio: ["ignore", log, log],
    });
    const rc = await new Promise<number>((resolve) => child.on("exit", (code) => resolve(code ?? 1)));
    let p = 0;
    let f = 0;
    let k = 0;
    try {
      const text = readFileSync(path.join(out, "expect.txt"), "utf8");
      p = (text.match(/^PASS/gm) ?? []).length;
      f = (text.match(/^FAIL/gm) ?? []).length;
      k = (text.match(/^SKIP/gm) ?? []).length;
    } catch {}
    const failed = (readFileSyncOr(path.join(out, "expect.txt")).match(/^FAIL.*$/gm) ?? [])
      .map((l) => l.split(":")[0]!.split(" ")[1])
      .join(" ");
    // 4: failed expectations (counted); anything else is the run's.
    const secs = Math.round((performance.now() - t0) / 1000);
    const note = rc !== 0 && rc !== 4 ? ` (exit ${rc}, see ${out}.log)` : "";
    const line = `${t.padEnd(8)} ${s.padEnd(16)} ${String(secs).padStart(3)}s PASS ${String(p).padStart(2)} FAIL ${f} SKIP ${k}${note} ${failed}\n`;
    writeFileSync(`${out}.line`, line);
    writeFileSync(`${out}.counts`, `${p} ${f} ${k} ${rc}\n`);
    // One at a time: as each ends.
    if (jobs === 1) process.stdout.write(line);
  };
  const runScenario = async (s: string) => {
    for (const t of inTerms.get(s) ?? []) await runOne(t, s);
  };

  const later: string[] = [];
  const running = new Set<Promise<void>>();
  for (const s of order) {
    if (jobs > 1 && alone.includes(s)) {
      later.push(s);
      continue;
    }
    if (running.size >= jobs) await Promise.race(running);
    const p = runScenario(s).finally(() => running.delete(p));
    running.add(p);
  }
  await Promise.all(running);
  for (const s of later) await runScenario(s);

  // The summary (with -j, the lines too), in the order of terminals and
  // scenarios.
  let failed = false;
  let total = { pass: 0, fail: 0, skip: 0 };
  for (const t of terms) {
    for (const s of order) {
      if (!inTerms.get(s)?.includes(t)) continue;
      const out = path.join(outdir, t, s);
      if (jobs > 1) process.stdout.write(readFileSyncOr(`${out}.line`) || `${t} ${s}: no result\n`);
      const c = readFileSyncOr(`${out}.counts`).trim().split(" ").map(Number);
      if (c.length === 4) {
        const counts: Counts = { pass: c[0]!, fail: c[1]!, skip: c[2]!, rc: c[3]! };
        total = { pass: total.pass + counts.pass, fail: total.fail + counts.fail, skip: total.skip + counts.skip };
        if (counts.rc !== 0) failed = true;
      } else {
        failed = true;
      }
    }
  }
  const secs = Math.round((performance.now() - start) / 1000);
  console.log(`total: PASS ${total.pass} FAIL ${total.fail} SKIP ${total.skip} in ${secs}s (${outdir})`);
  return failed ? 1 : 0;
}

function readFileSyncOr(file: string): string {
  try {
    return readFileSync(file, "utf8");
  } catch {
    return "";
  }
}

process.exitCode = await main(process.argv.slice(2));
