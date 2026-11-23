// The e2e harness's command line: runs a scenario (tests/e2e/NAME.ts) in a
// real terminal (kitty, Ghostty, WezTerm) or the GUI on a display server
// of its own, and judges it.
//
// Usage: node scripts/e2e/main.ts [-t kitty|ghostty|wezterm|gui] [-d x11|wayland|macos|windows]
//          [-m zellij|tmux] [-I] [-P shared|socket] [-s WxH] [-o OUTDIR] SCENARIO [-- COMMAND...]
//   SCENARIO  a scenario module (tests/e2e/NAME.ts, or NAME)
//   COMMAND   instead of the scenario's command
// The options are scripts/e2e.sh's. Exit status: 0; 1 when the run could
// not start or stopped (a helper failed outside `check`); 3 when the
// terminal died; 4 when a judgement failed (OUTDIR/expect.txt).

import { existsSync, mkdirSync, readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { run } from "node:test";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { defaultDisplay, type Display, type Mux, type Options, optionsVar, type Term } from "./options.ts";
import type { RunResult, Scenario } from "./scenario.ts";

const usage = `usage: node scripts/e2e/main.ts [-t kitty|ghostty|wezterm|gui] [-d x11|wayland|macos|windows]
  [-m zellij|tmux] [-I] [-P shared|socket] [-s WxH] [-o OUTDIR] SCENARIO [-- COMMAND...]`;

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
  if (values.h || positionals.length !== 1) fail(values.h ? "" : "one SCENARIO");
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
