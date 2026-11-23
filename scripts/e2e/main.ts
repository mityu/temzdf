// The e2e harness: runs a scenario (tests/e2e/NAME.ts) in a real terminal
// (kitty, Ghostty or WezTerm) or the GUI on a display server of its own,
// drives it with the scenario's helpers (scripts/e2e/helpers.ts says what
// each does), judges it (expect, check, NAME.cells: OUTDIR/expect.txt) and
// saves screenshots. scripts/e2e.sh runs this in the dev shell.
//
// Usage: scripts/e2e.sh [-t kitty|ghostty|wezterm|gui] [-d x11|wayland|macos|windows]
//          [-m zellij|tmux] [-I] [-P shared|socket] [-s WxH] [-o OUTDIR] SCENARIO [-- COMMAND...]
//   SCENARIO  a scenario module (tests/e2e/NAME.ts, or NAME)
//   COMMAND   instead of the scenario's own (its first word made absolute
//             when it names a file)
//   -t TERM   terminal to run (default kitty); gui: COMMAND is temzdf-gui
//             itself, run under the xfwm4 window manager (X11; sway on
//             Wayland) on lavapipe ($TEMZDF_LAVAPIPE_ICD); on X11 the screen
//             is 100 px larger each way for its decorations
//   -I        (gui) Japanese input: fcitx5 with Mozc (its HOME in OUTDIR,
//             ended with the run), over XIM on X11 (XMODIFIERS), over
//             input-method-v2 and text-input-v3 on Wayland; `ime` switches
//             it (the scenario may ask for it: `ime: true`)
//   -d DISP   display server: x11 (default; Xvfb, xdotool, import), wayland
//             (headless sway; keys and pointer through crates/wlinput,
//             $E2E_WLINPUT or built here; screenshots with grim), macos (the
//             default there; not run since the move to TypeScript, todo.md):
//             the terminal or the GUI on a virtual display at scale 1 beside
//             the real screen (in a VM, scripts/macos-vm.sh: on its own
//             screen), keys and the pointer through `termctl serve`
//             (scripts/macos/termctl.swift), screenshots with screencapture;
//             needs the accessibility and screen recording permissions and
//             the screen unlocked (doc/terminals/macos.md); windows (-t gui
//             only): temzdf-gui in the Windows VM of scripts/windows-vm.sh
//             ($TEMZDF_WINDOWS_VM, running, with temzdf and temzdf-gui built
//             in it), driven from this host: the pointer and keys through
//             QEMU's QMP, screenshots with QMP's screendump, commands and
//             files over SSH. COMMAND is the guest's temzdf-gui.exe (e.g.
//             C:/t/target/release/temzdf-gui.exe --clean FILE); arguments
//             naming files of this host are copied into the guest first.
//             How it works and why: "Windows in a VM" in doc/dev/testing.md
//   -m MUX    run COMMAND inside a multiplexer in that terminal (zellij: no
//             pane frames, a single pane, its config and layout in
//             OUTDIR/zellij; tmux: a private server with passthrough and
//             mouse on, no status)
//   -P PATH   how temzdf's clients take frames' pixels from the session:
//             shared (memory, the default) or socket (inside the messages);
//             sets TEMZDF_PIXELS for COMMAND
//   -s WxH    window/screen size in pixels (default the scenario's, or 900x600)
//   -o OUTDIR where screenshots and logs go (default: target/e2e)
//
// temzdf's log (geometry, warnings) goes to OUTDIR/state/temzdf/temzdf.log,
// when each phase began to OUTDIR/timing.txt (milliseconds), the input sent
// to OUTDIR/events.txt, COMMAND to OUTDIR/command.txt. It waits for the
// first screen (temzdf's log says a complete frame was written, then the
// screen stands still for 150 ms; at most 3 s, 6 s for Ghostty and WezTerm,
// 5 s for the GUI; inside a multiplexer that long, 3 s more in zellij, 1 s
// in tmux) and sends `q` at the end, waiting up to half a second for the
// terminal to go. E2E_RENDERER (a temzdf) and E2E_GUI_RENDERER (a
// temzdf-gui) draw `expect`'s headless renders instead of COMMAND's builds
// (release builds are quicker; scripts/e2e-all.sh -R). E2E_INPUT_LOG names
// a file for when each command to an input helper went out.
//
// COMMAND runs without ZELLIJ/TMUX in its environment, so a multiplexer
// around the harness does not leak into terminal detection, without
// TEMZDF_SESSION_NAME (a session around the harness would refuse to show
// the run's of the same name inside itself), and with its own
// XDG_RUNTIME_DIR (also for the scenario's commands), so its sessions stay
// apart from the user's and end with the run; links are not opened, only
// noted in OUTDIR/opened.txt. A COMMAND that runs temzdf must pass --clean
// or --config FILE (checked). A run ends after the scenario's timeout (180
// s by default); everything it started ends with it, also on Ctrl+C.
//
// Exit status: 0; 1 when the run could not start or stopped (a helper
// failed outside `check`, a timeout); 3 when the terminal died ("TERMINAL
// CRASHED"); 4 when a judgement failed ("EXPECTATIONS FAILED", see
// OUTDIR/expect.txt); 2 for a bad command line.

import { existsSync, mkdirSync, readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { run } from "node:test";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { defaultDisplay, type Display, type Mux, type Options, optionsVar, type Term } from "./options.ts";
import type { RunResult, Scenario } from "./scenario.ts";

const usage = `usage: scripts/e2e.sh [-t kitty|ghostty|wezterm|gui] [-d x11|wayland|macos|windows]
  [-m zellij|tmux] [-I] [-P shared|socket] [-s WxH] [-o OUTDIR] SCENARIO [-- COMMAND...]
(scripts/e2e/main.ts says more)`;

function fail(message: string): never {
  console.error(`e2e: ${message}`);
  console.error(usage);
  process.exit(2);
}

/** A scenario module's path: a file, or a name in tests/e2e/. */
export function scenarioPath(arg: string): string {
  if (existsSync(arg)) return realpathSync(arg);
  const named = path.resolve("tests/e2e", `${arg}.ts`);
  if (existsSync(named)) return named;
  fail(`no scenario ${arg}`);
}

async function main(argv: string[]): Promise<number> {
  const dash = argv.indexOf("--");
  const { values, positionals } = parseArgs({
    args: dash < 0 ? argv : argv.slice(0, dash),
    allowPositionals: true,
    options: {
      t: { type: "string", default: "kitty" },
      d: { type: "string", default: defaultDisplay() },
      m: { type: "string" },
      I: { type: "boolean" },
      P: { type: "string" },
      s: { type: "string" },
      o: { type: "string", default: "target/e2e" },
      h: { type: "boolean" },
    },
  });
  if (values.h) {
    console.log(usage);
    process.exit(0);
  }
  if (positionals.length !== 1) fail("one SCENARIO");
  const file = scenarioPath(positionals[0]!);
  const { def } = ((await import(pathToFileURL(file).href)) as { default: Scenario }).default;
  const term = values.t as Term;
  if (!["kitty", "ghostty", "wezterm", "gui"].includes(term)) fail(`unknown terminal ${term}`);
  const display = values.d as Display;
  if (!["x11", "wayland", "macos", "windows"].includes(display)) {
    fail(`unknown display server ${display} (x11, wayland, macos or windows)`);
  }
  if (values.m && values.m !== "zellij" && values.m !== "tmux") fail(`unknown multiplexer: ${values.m}`);
  if (values.P && values.P !== "shared" && values.P !== "socket") fail("-P shared or socket");
  const ime = values.I ?? def.ime ?? false;
  if (display === "macos" && ime) fail("-I is X11 and Wayland only so far");
  if (display === "windows" && (term !== "gui" || ime || values.m))
    fail("-d windows runs -t gui only, without -I or -m");
  let width = def.size?.width ?? 900;
  let height = def.size?.height ?? 600;
  if (values.s) {
    const m = /^([0-9]+)x([0-9]+)$/.exec(values.s);
    if (!m) fail(`bad size ${values.s}`);
    width = Number(m[1]);
    height = Number(m[2]);
  }
  mkdirSync(values.o!, { recursive: true });
  const outdir = realpathSync(values.o!);
  const opts: Options = {
    name: path.basename(file, ".ts"),
    term,
    display,
    mux: values.m as Mux | undefined,
    ime,
    pixels: values.P as Options["pixels"],
    width,
    height,
    outdir,
    command: dash < 0 ? undefined : argv.slice(dash + 1),
  };
  if (opts.command?.length === 0) fail("an empty COMMAND");
  process.env[optionsVar] = JSON.stringify([opts]);
  // forceExit: the scenario's process ends with its tests, whatever a
  // command it ran left behind.
  // Ctrl+C or SIGTERM here ends the scenario's process with its cleanup
  // (scenario.ts), then this one.
  const abort = new AbortController();
  let stopped: number | undefined;
  for (const [sig, n] of [
    ["SIGINT", 2],
    ["SIGTERM", 15],
    ["SIGHUP", 1],
  ] as const) {
    process.on(sig, () => {
      stopped ??= 128 + n;
      abort.abort();
    });
  }
  const stream = run({ files: [file], concurrency: 1, isolation: "process", forceExit: true, signal: abort.signal });
  stream.on("test:stdout", (e) => process.stdout.write(e.message));
  stream.on("test:stderr", (e) => process.stderr.write(e.message));
  stream.on("test:fail", (e) => {
    const err = e.details.error as Error & { cause?: unknown; failureType?: string };
    // A judgement's FAIL is in expect.txt already; the scenario's own
    // failure (an exception, a timeout) is said here.
    if (e.nesting === 0 && err.failureType !== "subtestsFailed") {
      const cause = err.cause instanceof Error ? err.cause.message : String(err.cause ?? err.message);
      console.error(`e2e: ${e.name}: ${cause}`);
    }
  });
  // The stream has to be read for the run to go on.
  for await (const _ of stream) {
  }
  if (stopped !== undefined) return stopped;
  let result: RunResult | undefined;
  try {
    result = JSON.parse(readFileSync(path.join(outdir, "result.json"), "utf8")) as RunResult;
  } catch {}
  if (!result?.done) return 1;
  console.log(`screenshots in ${outdir}`);
  if (result.crashed) {
    console.log(`TERMINAL CRASHED (see ${outdir}/terminal.log)`);
    return 3;
  }
  const expect = path.join(outdir, "expect.txt");
  if (existsSync(expect) && /^FAIL/m.test(readFileSync(expect, "utf8"))) {
    console.log(`EXPECTATIONS FAILED (see ${expect})`);
    return 4;
  }
  return 0;
}

process.exitCode = await main(process.argv.slice(2));
