// What the harness decides without a terminal: how a check's result
// becomes PASS, FAIL or SKIP, ImageMagick's "differ" told from its
// failures, waits ending on the run's signal, and a key without a Vim
// notation recorded without one (as scripts/e2e.sh did).

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { helpers } from "./helpers.ts";
import { compareAE, gradient } from "./magick.ts";
import { pause } from "./procs.ts";
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
