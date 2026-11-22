// Completing Ex commands in the command line (runtime/ff/completion/
// cmdline.ts, registered by the defaults' `commandLine`): the marker,
// <Tab>, command names and arguments (`temzdf test`, doc/dev/testing.md).
import { isPrompt, promptOrigin } from "temzdf";
import { cmdlineKind, command, isCmdline } from "temzdf/command";
import type { CmdCtx } from "temzdf/command";
import * as completion from "temzdf/ff/completion";
import { test } from "temzdf/test";
import type { TestContext } from "temzdf/test";

const NS = "cmdline-test";

/** The completion's popup lines, trimmed ([] without a popup). */
function popup(t: TestContext): string[] {
  const p = t.run(() => completion.info()?.popup);
  if (p === undefined) return [];
  return t.action.text
    .getText(undefined, { pane: p })
    .split("\n")
    .map((l) => l.trimEnd());
}

/** The names in the popup (its first column). */
const names = (t: TestContext) => popup(t).map((l) => l.split(/\s+/)[0]!);

/** The focused pane's cursor line (the command line's). */
const line = (t: TestContext) => t.action.text.line(t.action.text.cursor().line);

async function keys(t: TestContext, k: string): Promise<void> {
  await t.keys(k);
  await completion.whenIdle();
  await t.settle();
}

/** A text pane in the Vim normal mode, and the command line opened from
 * it with `:`; returns the text pane. */
async function cmdline(t: TestContext, text = ""): Promise<number> {
  const pane = t.openText("one\ntwo\n");
  await keys(t, `:${text}`);
  return pane;
}

/** Test commands (namespace NS), gone after the test. */
function commands(t: TestContext, define: () => void): void {
  t.run(define);
  t.after(() => t.run(() => command.clear({ namespace: NS })));
}

test("the command line is marked: : and <C-b>: as Ex, / as a search; promptOrigin is the pane", async (assert, t) => {
  const pane = await cmdline(t);
  assert.equal(t.action.currentMode(), "vim/cmdline");
  assert.true(t.run(() => isCmdline()));
  assert.equal(
    t.run(() => promptOrigin()),
    pane,
  );
  await keys(t, "<Esc>");
  assert.false(t.run(() => isCmdline()));
  assert.equal(
    t.run(() => promptOrigin()),
    undefined,
    "not a prompt",
  );

  await keys(t, "/");
  assert.true(t.run(() => isPrompt()));
  assert.equal(
    t.run(() => cmdlineKind()),
    "/",
  );
  assert.false(t.run(() => isCmdline()));
  await keys(t, "<Esc>");

  await keys(t, "<C-b>:");
  assert.equal(t.action.currentMode(), "input");
  assert.true(t.run(() => isCmdline()));
  assert.equal(
    t.run(() => promptOrigin()),
    pane,
  );
});

test("<Tab> starts and selects in the command line, <S-Tab> goes back", async (assert, t) => {
  await cmdline(t, "tabn");
  await keys(t, "<Tab>");
  assert.true(t.run(() => completion.visible()));
  assert.deepEqual(names(t), ["tabnew", "tabnext"], "by prefix, case respected (not tabNext)");
  assert.deepEqual(popup(t), ["tabnew   command", "tabnext  command"]);
  assert.equal(line(t), "tabn", "nothing went in yet");
  await keys(t, "<Tab>");
  assert.equal(line(t), "tabnew");
  await keys(t, "<Tab>");
  assert.equal(line(t), "tabnext");
  await keys(t, "<S-Tab>");
  assert.equal(line(t), "tabnew");
  await keys(t, "<C-e>");
  assert.equal(line(t), "tabn");
});

test("<Tab> in the <C-b>: command line too, and over a search's prompt it does nothing of completion", async (assert, t) => {
  t.openText("one\n");
  await keys(t, "<C-b>:tabe<Tab>");
  assert.deepEqual(names(t), ["tabedit"], "an abbreviation's full name");
  await keys(t, "<Tab>");
  assert.equal(line(t), "tabedit");
  await keys(t, "<Esc>");
  await keys(t, "<Esc>");
  assert.false(t.run(() => isPrompt()));
  await keys(t, "/on<Tab>");
  assert.false(t.run(() => completion.active()));
});

test("<Tab> in a text buffer's insert mode stays a tab", async (assert, t) => {
  const pane = t.openText("");
  await keys(t, "ia<Tab>b");
  assert.false(t.run(() => completion.active()));
  assert.equal(t.action.text.line(0, { pane }), "a\tb");
});

test("command names: at the start, after a range (those taking one), after |", async (assert, t) => {
  const pane = await cmdline(t);
  await keys(t, "<Tab>");
  const every = new Set(t.run(() => command.list({ live: true, pane }).map((c) => c.name)));
  every.delete("");
  assert.equal(
    t.run(() => completion.info()?.matched),
    every.size,
    "every command there, each once, not the range's own",
  );
  assert.equal(names(t)[0], "bNext", "sorted");
  await keys(t, "<C-e>%");
  await keys(t, "<Tab>");
  const ranged = names(t);
  assert.true(ranged.includes("substitute") && ranged.includes("delete"), "commands taking a range");
  assert.false(ranged.includes("write"), "not those refusing one");
  await keys(t, "<C-e>s<Tab><Tab>");
  assert.equal(line(t), "%substitute");
  await keys(t, "<C-y><C-u>nohl | tabn<Tab>");
  assert.deepEqual(names(t), ["tabnew", "tabnext"], "after | of a command splitting there");
  await keys(t, "<Tab>");
  assert.equal(line(t), "nohl | tabnew");
  await keys(t, "<C-y><C-u>s/a|tabn<Tab>");
  assert.false(
    t.run(() => completion.active()),
    ":s takes | as its argument: nothing after it",
  );
});

test("a command's when is asked about the pane the command line runs in", async (assert, t) => {
  const pane = t.openText("x\n");
  commands(t, () => {
    command.add("Zhere", () => {}, { when: (c) => c.pane === pane, namespace: NS });
    command.add("Zelsewhere", () => {}, { when: (c) => c.pane !== pane, namespace: NS });
  });
  await keys(t, ":Z<Tab>");
  assert.deepEqual(names(t), ["Zhere"]);
});

test("arguments: :b by substring, :bd too; the abbreviation finds the command", async (assert, t) => {
  for (const name of ["alpha.md", "notes-alpha.txt", "beta.txt"]) {
    t.run(() => t.action.newBuffer({ path: name }));
  }
  await cmdline(t, "b alp");
  await keys(t, "<Tab>");
  assert.deepEqual(
    names(t).filter((n) => n.includes("alpha") || n.includes("beta")),
    ["alpha.md", "notes-alpha.txt"],
  );
  await keys(t, "<C-e><C-u>bd bet<Tab><Tab>");
  assert.equal(line(t), "bd beta.txt");
  await keys(t, "<C-y><C-u>bx <Tab>");
  assert.false(
    t.run(() => completion.active()),
    "no such command",
  );
  await keys(t, "<C-u>b<Tab>");
  assert.true(names(t).includes("buffer"), "no blank after the name: names");
});

test("arguments: :set by prefix, the boolean ones after no; nothing after =", async (assert, t) => {
  await cmdline(t, "se i");
  await keys(t, "<Tab>");
  assert.deepEqual(names(t), ["ignorecase", "incsearch"]);
  await keys(t, "<C-e><C-u>set hls no<Tab>");
  const after = names(t);
  assert.true(after.includes("expandtab") && !after.includes("shiftwidth"), "the boolean ones");
  await keys(t, "<Tab>");
  assert.equal(line(t), `set hls no${after[0]}`);
  await keys(t, "<C-y><C-u>set sw=<Tab>");
  assert.false(t.run(() => completion.active()));
});

test("<CR> after selecting runs the completed command (a complete function, and :b)", async (assert, t) => {
  const ran: string[] = [];
  const seen: unknown[] = [];
  commands(t, () => {
    command.add("Zrun", (c: CmdCtx) => ran.push(c.args), {
      namespace: NS,
      complete: (p) => {
        seen.push({ args: p.args, name: p.name, bang: p.bang });
        return { items: ["apple", { value: "apricot", description: "a fruit" }] };
      },
    });
  });
  await cmdline(t, "Zrun! x ap");
  await keys(t, "<Tab>");
  assert.deepEqual(popup(t), ["apple", "apricot  a fruit"]);
  assert.deepEqual(seen.at(-1), { args: "x ap", name: "Zrun", bang: true });
  await keys(t, "<Tab><Tab><CR>");
  assert.deepEqual(ran, ["! x apricot"]);
  assert.false(t.run(() => isPrompt()));
  assert.false(t.run(() => completion.active()));

  const gamma = t.run(() => t.action.newBuffer({ path: "gamma.txt" }));
  await keys(t, ":b gam<Tab>");
  await keys(t, "<Tab><CR>");
  assert.equal(t.action.panes().find((p) => p.focused)?.buffer, gamma, ":b gamma.txt ran");
});
