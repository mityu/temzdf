// temzdf/process (runtime/process.ts on src/native_process.rs): finding
// commands and running them. cargo is on PATH wherever these tests run
// (Linux, macOS, the Windows CI job); the native tests check the rest
// (limits, kills, the search itself).
import { errors, executable, exepath, run } from "temzdf/process";
import { test } from "temzdf/test";

test("commands are found as run finds them", async (assert) => {
  const cargo = await exepath("cargo");
  assert.true(typeof cargo === "string" && cargo.length > 0, `cargo at ${cargo}`);
  assert.true(await executable("cargo"));
  assert.equal(await exepath("temzdf-no-such-command"), undefined);
  assert.false(await executable("temzdf-no-such-command"));
  assert.equal(await exepath(""), undefined);
});

test("a command runs to its end with its output", async (assert) => {
  const r = await run(["cargo", "--version"]);
  assert.true(r.success);
  assert.equal(r.code, 0);
  assert.equal(r.signal, undefined);
  assert.true(new TextDecoder().decode(r.stdout).startsWith("cargo "), new TextDecoder().decode(r.stdout));
  const failed = await run(["cargo", "--temzdf-no-such-option"]);
  assert.false(failed.success, "a failure is a result, not an error");
  assert.notEqual(failed.code, 0);
  assert.true(failed.stderr.length > 0);
});

test("what cannot run is an error of its class", async (assert) => {
  await assert.rejects(run(["temzdf-no-such-command"]), errors.NotFound);
  await assert.rejects(run(["cargo", "--version"], { cwd: "relative" }), TypeError);
  await assert.rejects(run([]), TypeError);
  await assert.rejects(run(["cargo"], { stdin: 1 as unknown as string }), TypeError);
});

test("an aborted signal rejects with its reason", async (assert) => {
  const ac = new AbortController();
  ac.abort("stop");
  await assert.rejects(
    run(["cargo", "--version"], { signal: ac.signal }),
    (e: unknown) => e === "stop",
    "aborted before",
  );
  const later = new AbortController();
  const running = run(["cargo", "--version"], { signal: later.signal });
  later.abort("now");
  await assert.rejects(running, (e: unknown) => e === "now", "aborted while it runs");
});
