// The script host's event loop keeps JavaScript's order: after each task
// (a mapped function, a hook, a timer) its promise jobs run before the next
// task, a timer due at once included (doc/design/script-host.md).
import { keymap, on } from "temzdf";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

const namespace = "event-loop.test";

/** Queue jobs and a 0 ms timer, writing what runs to `log`. */
function queue(log: string[]): void {
  setTimeout(() => log.push("timer"), 0);
  Promise.resolve().then(() => {
    log.push("job");
    void Promise.resolve().then(() => log.push("nested job"));
  });
}

/** Wait until timers set until now have fired. */
const timersFired = () => new Promise((resolve) => setTimeout(resolve, 20));

async function mapped(t: TestContext, log: string[]): Promise<void> {
  t.openText("x\n");
  t.run(() => keymap.add("vim/normal", "gZ", () => queue(log), { namespace }));
  t.after(() => t.run(() => keymap.clear({ namespace })));
  await t.keys("gZ");
  await timersFired();
}

test("a mapped function's promise jobs run before its timer", async (assert, t) => {
  const log: string[] = [];
  await mapped(t, log);
  assert.deepEqual(log, ["job", "nested job", "timer"]);
});

test("a hook's promise jobs run before its timer", async (assert, t) => {
  t.openText("x\n");
  const log: string[] = [];
  const off = t.run(() =>
    on("KeyTyped", (key) => {
      if (key.keys === "<F7>") queue(log);
    }),
  );
  t.after(() => t.run(off));
  await t.keys("<F7>", { allowErrors: true });
  await timersFired();
  assert.deepEqual(log, ["job", "nested job", "timer"]);
});

test("a timer's promise jobs run before the next timer", async (assert) => {
  const log: string[] = [];
  setTimeout(() => queue(log), 0);
  setTimeout(() => log.push("next timer"), 0);
  await timersFired();
  assert.deepEqual(log, ["job", "nested job", "next timer", "timer"]);
});
