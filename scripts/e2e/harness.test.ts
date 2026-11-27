// What the harness decides without a terminal: how a check's result
// becomes PASS, FAIL or SKIP, ImageMagick's "differ" told from its
// failures, waits ending on the run's signal or on a change of the screen,
// a key without a Vim notation recorded without one (as scripts/e2e.sh
// did), and this run's temzdf found in ps's output on macOS.

import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { helpers } from "./helpers.ts";
import { psHasTemzdf, temzdfAlive } from "./host.ts";
import { mark, waitChange } from "./judge.ts";
import { compareAE, gradient } from "./magick.ts";
import { Procs, pause } from "./procs.ts";
import { Records, type Verdict } from "./record.ts";
import { Run, Skip } from "./run.ts";

const hasMagick = (() => {
  try {
    execFileSync("magick", ["-version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

function tmp(): string {
  return mkdtempSync(path.join(tmpdir(), "e2e-harness-test-"));
}

test("check: true or nothing passes, false or an error fails, skip skips", async () => {
  const seen: string[] = [];
  const fake = {
    signal: new AbortController().signal,
    opts: { term: "ghostty" },
    async verdict(v: Verdict, name: string, detail?: string) {
      seen.push(`${v} ${name}${detail ? `: ${detail}` : ""}`);
    },
  } as unknown as Run;
  const { check } = helpers(fake);
  await check("yes", () => true);
  await check("nothing", () => {});
  await check("no", () => false);
  await check("threw", () => {
    throw new Error("boom");
  });
  await check("said-skip", () => "skip");
  await check("threw-skip", () => {
    throw new Skip();
  });
  await check("async-skip", async () => "skip");
  assert.deepEqual(seen, [
    "PASS yes",
    "PASS nothing",
    "FAIL no: false",
    "FAIL threw: boom",
    "SKIP said-skip: ghostty cannot tell",
    "SKIP threw-skip: ghostty cannot tell",
    "SKIP async-skip: ghostty cannot tell",
  ]);
});

test("pause ends at once on the run's signal", async () => {
  const abort = new AbortController();
  const t0 = performance.now();
  setTimeout(() => abort.abort(new Error("stopped")), 20);
  await assert.rejects(pause(5, abort.signal), { name: "AbortError" });
  assert.ok(performance.now() - t0 < 1000);
});

test("a key without a Vim notation is recorded without one", async () => {
  const dir = tmp();
  try {
    const rec = new Records(dir);
    rec.startEvents();
    const sent: string[] = [];
    const fake = {
      be: { key: async (k: string) => void sent.push(k) },
      rec,
      sleep: async () => {},
    };
    await Run.prototype.keys.call(fake as unknown as Run, ["j", "F5", "shift+g"]);
    assert.deepEqual(sent, ["j", "F5", "shift+g"]);
    const words = readFileSync(rec.events, "utf8")
      .split("\n")
      .filter(Boolean)
      .map((l) => l.slice(l.indexOf(" ") + 1));
    assert.deepEqual(words, ["key j", "key ", "key G"]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("compareAE: a count when the images differ, an error when it cannot compare", { skip: !hasMagick }, async () => {
  const dir = tmp();
  try {
    const a = path.join(dir, "a.png");
    const b = path.join(dir, "b.png");
    await gradient(20, 10, "red", "blue", a);
    await gradient(20, 10, "red", "blue", b);
    assert.equal(await compareAE(a, b, path.join(dir, "same.png")), 0);
    await gradient(20, 10, "green", "blue", b);
    assert.ok((await compareAE(a, b, path.join(dir, "diff.png"))) > 0);
    // A missing file is magick's exit 2: not "0 pixels differ".
    await assert.rejects(compareAE(a, path.join(dir, "missing.png"), path.join(dir, "x.png")));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("waitChange: ends once the screen differs from the mark, else FAIL wait_change", async () => {
  const dir = tmp();
  try {
    const seen: string[] = [];
    let screen = "notice";
    let shots = 0;
    const fake = {
      path: (n: string) => path.join(dir, n),
      async watchShot(file: string) {
        shots++;
        writeFileSync(file, screen);
      },
      sleep: async () => {
        // The notice goes after the third look.
        if (shots === 3) screen = "gone";
      },
      rec: { phase: (p: string) => seen.push(p) },
      async verdict(v: Verdict, name: string) {
        seen.push(`${v} ${name}`);
      },
    } as unknown as Run;
    await mark(fake, "copied");
    await waitChange(fake, "copied");
    assert.deepEqual(seen, ["mark copied", "changed copied"]);
    assert.equal(shots, 4);
    // It never changes.
    seen.length = 0;
    await mark(fake, "still");
    await waitChange(fake, "still", 0.05);
    assert.deepEqual(seen, ["mark still", "changed still (timed out)", "FAIL wait_change"]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("psHasTemzdf: a temzdf by its executable's name and this run's state", () => {
  const want = "XDG_STATE_HOME=/r/out/state";
  // ps -E -o ucomm=,command= on macOS 27: ucomm padded, then the command
  // line with the environment after it.
  const ours = "temzdf           target/debug/temzdf standalone pdf a.pdf XDG_STATE_HOME=/r/out/state HOME=/h";
  const other = "temzdf           target/debug/temzdf standalone pdf a.pdf XDG_STATE_HOME=/r/other/state";
  const shell = "zsh              zsh -c target/debug/temzdf XDG_STATE_HOME=/r/out/state";
  assert.equal(psHasTemzdf(`${shell}\n${ours}\n`, want), true);
  assert.equal(psHasTemzdf(`${shell}\n${other}\n`, want), false);
  assert.equal(psHasTemzdf("", want), false);
});

const hasCc = (() => {
  try {
    execFileSync("cc", ["--version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

// The harness runs on Linux and macOS only (a Windows guest is driven from
// Linux), and the program below is POSIX (`pause`).
const unixCc = hasCc && process.platform !== "win32";

test("temzdfAlive: finds a temzdf started as target/debug/temzdf", { skip: !unixCc }, async () => {
  // A program named temzdf, started as the scenarios start it: on macOS 27
  // ps's comm is then "target/debug/tem" (argv[0] cut to 16 characters).
  const dir = tmp();
  const exe = path.join(dir, "temzdf");
  writeFileSync(path.join(dir, "main.c"), "#include <unistd.h>\nint main(void) { pause(); return 0; }\n");
  execFileSync("cc", ["-o", exe, path.join(dir, "main.c")]);
  const outdir = path.join(dir, "out");
  const child = spawn(exe, [], {
    argv0: "target/debug/temzdf",
    env: { ...process.env, XDG_STATE_HOME: `${outdir}/state` },
    stdio: "ignore",
  });
  try {
    const procs = new Procs(new AbortController().signal);
    let found = false;
    for (let i = 0; i < 40 && !found; i++) {
      found = await temzdfAlive(outdir, procs);
      if (!found) await new Promise((r) => setTimeout(r, 50));
    }
    assert.equal(found, true);
    assert.equal(await temzdfAlive(path.join(dir, "other"), procs), false);
  } finally {
    child.kill();
    rmSync(dir, { recursive: true, force: true });
  }
});
