// What the e2e harness does differently per host OS (not per display
// server: those are backends/): finding this run's temzdf among the
// processes, the run's private runtime directory, the environment the
// command runs in, and the program links would open with.

import { chmodSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import type { Procs } from "./procs.ts";

/** The run's runtime directory: short, as socket paths are limited (on
 * macOS /tmp itself, $TMPDIR is long there: doc/terminals/macos.md). */
export function makeRuntimeDir(): string {
  const base = process.platform === "darwin" ? "/tmp" : (process.env.TMPDIR ?? tmpdir());
  return mkdtempSync(path.join(base, "temzdf-e2e."));
}

/** Whether temzdf of this run still runs (a viewer or a session server):
 * known by its environment (XDG_STATE_HOME=OUTDIR/state). */
export async function temzdfAlive(outdir: string, procs: Procs): Promise<boolean> {
  const want = `XDG_STATE_HOME=${outdir}/state`;
  if (process.platform === "linux") {
    const uid = process.getuid?.();
    for (const p of readdirSync("/proc")) {
      if (!/^[0-9]+$/.test(p)) continue;
      try {
        if (statSync(`/proc/${p}`).uid !== uid) continue;
        if (readFileSync(`/proc/${p}/comm`, "utf8").trim() !== "temzdf") continue;
        if (readFileSync(`/proc/${p}/environ`, "latin1").split("\0").includes(want)) return true;
      } catch {
        // Gone, or not ours to read.
      }
    }
    return false;
  }
  // macOS: ps -E adds the environment to the command line.
  const ps = await procs.exec("ps", ["-Eww", "-U", String(process.getuid?.()), "-o", "comm=,command="]);
  return ps.stdout.split("\n").some((l) => /(^|\/)temzdf$/.test(l.trim().split(/\s+/)[0] ?? "") && l.includes(want));
}

/** Variables the command must not inherit: a multiplexer or a temzdf
 * session around the harness would leak into terminal detection, or make
 * a session refuse to show itself inside. */
const unset = [
  "TEMZDF_LOG_DIR",
  "TEMZDF_SESSION_NAME",
  "ZELLIJ",
  "ZELLIJ_SESSION_NAME",
  "ZELLIJ_PANE_ID",
  "TMUX",
  "TMUX_PANE",
];

/** The environment COMMAND runs in: the harness's without `unset`, its
 * state and recorded input in OUTDIR, and PATH starting with BIN. */
export function cleanEnv(outdir: string, bin: string, base: NodeJS.ProcessEnv = process.env): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...base };
  for (const k of unset) delete env[k];
  env.XDG_STATE_HOME = `${outdir}/state`;
  env.TEMZDF_RECORD_INPUT = `${outdir}/input.txt`;
  env.PATH = `${bin}:${base.PATH ?? ""}`;
  return env;
}

/** Links are not opened: what temzdf opens them with (`open` on macOS,
 * `xdg-open` elsewhere) only notes them in OUTDIR/opened.txt (a browser
 * would open them on the desktop of whoever runs the harness). Returns the
 * directory to put first on PATH. */
export function openerShim(runtimeDir: string, outdir: string): string {
  const bin = path.join(runtimeDir, "bin");
  mkdirSync(bin, { recursive: true });
  const opener = process.platform === "darwin" ? "open" : "xdg-open";
  const file = path.join(bin, opener);
  writeFileSync(file, `#!/bin/sh\necho "$*" >> '${path.join(outdir, "opened.txt").replaceAll("'", `'\\''`)}'\n`);
  chmodSync(file, 0o755);
  return bin;
}
