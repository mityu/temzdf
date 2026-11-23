// `scenario(def, body)`: an e2e scenario (tests/e2e/NAME.ts) on node:test.
// In the process main.ts starts for it (E2E_OPTS set), each run asked for
// (a terminal, a display) becomes a test: the display server and the
// terminal running the scenario's COMMAND start, BODY drives them with the
// helpers (all bound to the test's abort signal), the cells are judged, and
// everything started ends in the test's `after`. Each judgement is a
// subtest (a FAIL does not stop the scenario; a SKIP is the judgement's),
// and a line of OUTDIR/expect.txt as scripts/e2e.sh wrote it. Imported
// anywhere else, it only returns the definition.

import { writeFileSync } from "node:fs";
import { type TestContext, test } from "node:test";
import { type CommandSpec, commandLine } from "./command.ts";
import { type Helpers, helpers } from "./helpers.ts";
import { drawReferences } from "./judge.ts";
import { type Options, received, type Term } from "./options.ts";
import { Run, StartError } from "./run.ts";

export interface ScenarioDef extends CommandSpec {
  /** Files to set up before the run (relative to the repository's root). */
  prepare?: (fs: Helpers["fs"]) => void | Promise<void>;
  /** The window's size, when not 900x600 (`-s`). */
  size?: { width: number; height: number };
  /** Japanese input (`-I`). */
  ime?: boolean;
  /** Seconds before the run is ended as failed (default 600). */
  timeout?: number;
}

export type Body = (t: TestContext, h: Helpers) => Promise<void>;

export interface Scenario {
  def: ScenarioDef;
  body: Body;
}

/** What a run of the scenario left for main.ts (OUTDIR/result.json). */
export interface RunResult {
  /** The run got through its scenario (else it stopped: exit 1). */
  done: boolean;
  /** The terminal (or the GUI) had died by the end (exit 3). */
  crashed: boolean;
  /** Why it stopped, when it did. */
  error?: string;
}

export function scenario(def: ScenarioDef, body: Body): Scenario {
  for (const o of received() ?? []) {
    test(`${o.name ?? "scenario"} (${o.term})`, { timeout: (def.timeout ?? 600) * 1000 }, (t) =>
      runScenario(t, o, def, body),
    );
  }
  return { def, body };
}

async function runScenario(t: TestContext, o: Options, def: ScenarioDef, body: Body): Promise<void> {
  const r = new Run(o, o.command ?? commandLine(def, o.outdir), t.signal);
  const result: RunResult = { done: false, crashed: false };
  const write = () => writeFileSync(r.path("result.json"), `${JSON.stringify(result)}\n`);
  t.after(() => r.procs.cleanup());
  const onExit = () => r.procs.killAllSync();
  process.on("exit", onExit);
  t.after(() => process.off("exit", onExit));
  r.report = async (verdict, name, line) => {
    await t.test(name, (s) => {
      if (verdict === "SKIP") s.skip(line);
      else if (verdict === "FAIL") throw new Error(line);
    });
  };
  try {
    await def.prepare?.(helpers(r).fs);
    await r.start();
    // The recorded times start here (the first screen is up).
    r.rec.startEvents();
    r.rec.phase("actions");
    await body(t, helpers(r));
    await drawReferences(r);
    result.crashed = (await r.quit()).crashed;
    await r.procs.cleanup();
    r.rec.phase("done");
    result.done = true;
  } catch (e) {
    result.error = e instanceof Error ? e.message : String(e);
    if (e instanceof StartError) console.error(result.error);
    throw e;
  } finally {
    write();
  }
}

export type { Term };
