// What the harness reads from a COMMAND (command.ts), on the commands
// scripts/e2e-all.sh runs, and the modules' commands turned into the same.

import assert from "node:assert/strict";
import { test } from "node:test";
import {
  checkConfig,
  commandLine,
  isSession,
  parse,
  runsTemzdf,
  sessionTemzdf,
  shQuote,
  splitWords,
  standalone,
} from "./command.ts";

const pdf = [
  "sh",
  "-c",
  'SHELL=/bin/sh PS1="$ " target/debug/temzdf standalone pdf --clean target/fixtures/sample.pdf; exec sh',
];
const session = ["sh", "-c", 'SHELL=/bin/sh PS1="$ " target/debug/temzdf --clean -s e2e-term; exec sh'];
const synctex = [
  "sh",
  "-c",
  "target/debug/temzdf standalone pdf --config tests/e2e/configs/forward.ts \\\n        --editor 'echo %{input}:%{line} > target/e2e-sync/inverse.txt' target/e2e-sync/sample.pdf; exec sh",
];
const gui = ["target/debug/temzdf-gui", "--clean", "target/fixtures/sample.pdf"];
const env = ["env", "TERMPIX_MEDIUM=direct", ...pdf];
const demo = ["env", "SCROLL_DEMO_LOG=/x/scroll_demo.log", "target/debug/examples/scroll_demo"];
const windows = ["C:/t/target/release/temzdf-gui.exe", "--clean", "target/fixtures/sample.pdf"];

test("parse: the words of what runs, and a shell after it", () => {
  assert.deepEqual(parse(pdf), {
    words: ["target/debug/temzdf", "standalone", "pdf", "--clean", "target/fixtures/sample.pdf"],
    hasShell: true,
  });
  assert.deepEqual(parse(env).words.slice(0, 3), ["target/debug/temzdf", "standalone", "pdf"]);
  assert.equal(parse(session).hasShell, true);
  assert.deepEqual(parse(synctex).words.at(-1), "target/e2e-sync/sample.pdf");
  assert.deepEqual(parse(gui), { words: gui, hasShell: false });
  assert.deepEqual(parse(demo).words, ["target/debug/examples/scroll_demo"]);
  assert.deepEqual(parse(["sh", "-c", "temzdf standalone pdf --clean a.pdf"]).hasShell, false);
});

test("standalone: what expect renders with", () => {
  assert.deepEqual(standalone(parse(synctex).words), {
    renderer: "target/debug/temzdf",
    options: ["--config", "tests/e2e/configs/forward.ts"],
    file: "target/e2e-sync/sample.pdf",
  });
});

test("runsTemzdf, isSession and the session's temzdf", () => {
  assert.equal(runsTemzdf(pdf), true);
  assert.equal(runsTemzdf(gui), true);
  assert.equal(runsTemzdf(windows), true);
  assert.equal(runsTemzdf(demo), false);
  assert.equal(runsTemzdf(["less", "target/temzdf-notes.txt"]), false);
  assert.equal(isSession("kitty", session), true);
  assert.equal(isSession("kitty", pdf), false);
  assert.equal(isSession("gui", gui), false);
  assert.equal(sessionTemzdf(session), "target/debug/temzdf");
});

test("checkConfig: temzdf runs with --clean or --config", () => {
  checkConfig(pdf);
  checkConfig(synctex);
  checkConfig(demo);
  assert.throws(() => checkConfig(["target/debug/temzdf", "a.pdf"]));
});

test("shQuote and splitWords undo each other", () => {
  for (const w of ["plain", "a b", "it's", '"q"', "$ ", "%{input}:%{line}", ""]) {
    assert.deepEqual(splitWords(shQuote(w)), w === "" ? [""] : [w], w);
  }
});

test("commandLine: a module's command reads as e2e-all's did", () => {
  const line = commandLine({
    command: ["target/debug/temzdf", "standalone", "pdf", "--clean", "target/fixtures/sample.pdf"],
    env: { SHELL: "/bin/sh", PS1: "$ " },
    shellAfter: true,
  });
  assert.deepEqual(parse(line), parse(pdf));
  assert.deepEqual(standalone(parse(line).words), standalone(parse(pdf).words));
  assert.equal(isSession("kitty", line), false);
  const s = commandLine({ command: ["target/debug/temzdf", "--clean", "-s", "e2e-term"], env: {}, shellAfter: true });
  assert.equal(isSession("kitty", s), true);
  assert.deepEqual(commandLine({ command: ["a"], env: { X: "1" } }), ["env", "X=1", "a"]);
  // The shell runs the assignments as assignments.
  assert.equal(
    line[2],
    "SHELL=/bin/sh PS1='$ ' target/debug/temzdf standalone pdf --clean target/fixtures/sample.pdf; exec sh",
  );
});
