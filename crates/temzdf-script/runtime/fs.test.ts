// temzdf/fs (runtime/fs.ts): its errors. Reading files the host's tests
// make (crates/temzdf-script/tests/host/cases.rs) is checked there.
import { errors, exists, FsError, readTextFile, stat } from "temzdf/fs";
import { test } from "temzdf/test";

/** What `p` rejects with. */
async function rejection(p: Promise<unknown>): Promise<unknown> {
  try {
    await p;
  } catch (e) {
    return e;
  }
  throw new Error("it did not reject");
}

test("a path that is not there is NotFound, and does not exist", async (assert) => {
  const missing = "/temzdf-fs-test/certainly/not/here.txt";
  const e = await rejection(readTextFile(missing));
  assert.true(e instanceof errors.NotFound);
  assert.true(e instanceof FsError);
  assert.equal((e as Error).name, "NotFound");
  assert.equal((e as Error).message, `readTextFile: ${missing}: no such file or directory`);
  assert.false(await exists(missing));
});

test("relative paths and non-strings are TypeErrors", async (assert) => {
  const relative = await rejection(stat("config.ts"));
  assert.true(relative instanceof TypeError);
  assert.equal((relative as Error).message, "stat: config.ts: not an absolute path");
  assert.true((await rejection(stat(1 as unknown as string))) instanceof TypeError);
  assert.true((await rejection(exists("/", { isFile: true, isDirectory: true }))) instanceof TypeError);
});
