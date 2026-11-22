// temzdf/command (runtime/command.ts): the registry of Ex commands,
// running a command line, and the helpers parsing one.
import { command, parseBang, parsePrefix, resolveRange, splitArgs, splitBar } from "temzdf/command";
import type { CmdCtx, VimRangeEnv } from "temzdf/command";
import vim from "temzdf/edit/vim";
import { keyset } from "temzdf/keyset";
import { module, test } from "temzdf/test";
import type { Assert } from "temzdf/test";

function eq(assert: Assert, what: string, got: unknown, want: unknown): void {
  assert.deepEqual(got, want, what);
}

/** The message `fn` throws ("" when it does not). */
function thrown(fn: () => void): string {
  try {
    fn();
    return "";
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}

/** What the commands of a case ran. */
let log: unknown[] = [];
const record = (c: CmdCtx) => log.push([c.name, c.args]);

/** A case: synchronous, inside t.run (`when` reads the session), from an
 * empty registry. */
function cmdTest(name: string, body: (assert: Assert) => void): void {
  test(name, (assert, t) => t.run(() => body(assert)));
}

// The registry is the Vim mode's too (its Ex commands, the only shipped
// ones): it is off meanwhile, and enabled again over an empty registry.
module(
  "temzdf/command",
  {
    beforeEach(_assert, t) {
      t.run(() => {
        const wasOn = vim.enabled;
        vim.disable();
        command.clear();
        log = [];
        t.after(() =>
          t.run(() => {
            command.clear();
            if (wasOn) vim.enable();
          }),
        );
      });
    },
  },
  () => {
    cmdTest("registering checks its arguments", (assert) => {
      eq(assert, "name with a digit", thrown(() => command.add("w2", record)) !== "", true);
      eq(assert, "not a function", thrown(() => command.add("w", 42 as never)) !== "", true);
      eq(assert, "shortName not a prefix", thrown(() => command.add("write", record, { shortName: "x" })) !== "", true);
      eq(assert, "empty shortName", thrown(() => command.add("write", record, { shortName: "" })) !== "", true);
      eq(
        assert,
        "an empty shortName for the empty name",
        thrown(() => command.add("", record, { shortName: "" })) !== "",
        true,
      );
      eq(
        assert,
        "any shortName for the empty name",
        thrown(() => command.add("", record, { shortName: "a" })) !== "",
        true,
      );
      eq(assert, "unknown prefix", thrown(() => command.add("w", record, { prefix: "lines" as never })) !== "", true);
      eq(
        assert,
        "unknown bar",
        thrown(() => command.add("w", record, { bar: "split" as never })),
        'command.add(w): opts.bar: Invalid option: expected one of "auto"|"handle"',
      );
      eq(
        assert,
        "two problems",
        thrown(() => command.add("w", 42 as never, { when: true as never })),
        "command.add(w): fn: expected a function",
      );
      eq(
        assert,
        "shortName not a string",
        thrown(() => command.add("w", record, { shortName: 1 as never })),
        "command.add(w): opts.shortName: Invalid input: expected string, received number",
      );
      eq(assert, "nothing registered", command.list().length, 0);
    });

    cmdTest("names match exactly, or abbreviated down to shortName", (assert) => {
      command.add("write", record, { shortName: "w" });
      command.add("edit", record);
      for (const typed of ["w", "wri", "write"]) command.execute(typed);
      eq(assert, "abbreviations", log, [
        ["write", ""],
        ["write", ""],
        ["write", ""],
      ]);
      eq(
        assert,
        "too long",
        thrown(() => command.execute("writes")),
        "E492: Not an editor command: writes",
      );
      eq(
        assert,
        "no shortName: full name only",
        thrown(() => command.execute("ed")),
        "E492: Not an editor command: ed",
      );
    });

    cmdTest("an exact name beats an abbreviation, and the last defined wins", (assert) => {
      command.add("print", () => log.push("print"), { shortName: "p" });
      command.add("p", () => log.push("p"));
      command.add("put", () => log.push("put"), { shortName: "pu" });
      command.add("preserve", () => log.push("preserve"), { shortName: "pre" });
      command.add("previous", () => log.push("previous"), { shortName: "pre" });
      command.execute("p");
      command.execute("pr");
      command.execute("pre");
      command.add("p", () => log.push("p again"));
      command.execute("p");
      eq(assert, "picked", log, ["p", "print", "previous", "p again"]);
    });

    cmdTest("when leaves commands out", (assert) => {
      command.add("go", () => log.push("text"), { when: (c) => c.paneKind === "edit" });
      command.add("go", () => log.push("pdf"), { when: (c) => c.paneKind === "pdf" });
      command.execute("go");
      command.add("only", () => log.push("never"), { when: () => false });
      eq(
        assert,
        "none live",
        thrown(() => command.execute("only")),
        "E492: Not an editor command: only",
      );
      eq(assert, "the live one", log, ["text"]);
    });

    cmdTest("namespaces: replaced, removed, cleared", (assert) => {
      command.add("a", () => log.push("a1"), { namespace: "x" });
      command.add("a", () => log.push("a2"), { namespace: "x" });
      command.add("a", () => log.push("a3"), { namespace: "y" });
      eq(
        assert,
        "replaced in x, added in y",
        command.list().map((c) => c.namespace),
        ["x", "y"],
      );
      command.remove("a", { namespace: "y" });
      command.execute("a");
      command.add("b", record, { namespace: "x" });
      command.add("c", record);
      command.clear({ namespace: "x" });
      eq(
        assert,
        "left",
        command.list().map((c) => c.name),
        ["c"],
      );
      command.remove("c");
      eq(assert, "all gone", command.list().length, 0);
      eq(assert, "ran", log, ["a2"]);
    });

    cmdTest("arguments are passed untrimmed", (assert) => {
      command.add("w", record);
      command.execute("w!");
      command.execute("w !sort");
      command.execute("  :w  x  ");
      command.execute("w1");
      eq(assert, "args", log, [
        ["w", "!"],
        ["w", " !sort"],
        ["w", "  x  "],
        ["w", "1"],
      ]);
    });

    cmdTest("ranges are parsed as typed", (assert) => {
      const n = (number: number, offset = 0) => ({ base: { kind: "number", number }, offset });
      const cur = (offset = 0) => ({ base: { kind: "current" }, offset });
      const last = { base: { kind: "last" }, offset: 0 };
      eq(assert, "%", parsePrefix("%d"), [{ start: n(1), end: last, separator: "," }, "d"]);
      eq(assert, ".,$", parsePrefix(".,$d"), [{ start: cur(), end: last, separator: "," }, "d"]);
      eq(assert, "'<,'>", parsePrefix("'<,'>s/a/b/"), [
        {
          start: { base: { kind: "mark", name: "<" }, offset: 0 },
          end: { base: { kind: "mark", name: ">" }, offset: 0 },
          separator: ",",
        },
        "s/a/b/",
      ]);
      eq(assert, "3", parsePrefix("3"), [{ start: n(3) }, ""]);
      eq(assert, "+2", parsePrefix("+2y"), [{ start: cur(2) }, "y"]);
      eq(assert, "offsets add up", parsePrefix("$-3+1--"), [{ start: { base: { kind: "last" }, offset: -4 } }, ""]);
      eq(assert, "/a\\/b/", parsePrefix("/a\\/b/d"), [
        { start: { base: { kind: "pattern", pattern: "a/b", backward: false }, offset: 0 } },
        "d",
      ]);
      eq(assert, "1;?x?", parsePrefix("1;?x?+1p"), [
        { start: n(1), end: { base: { kind: "pattern", pattern: "x", backward: true }, offset: 1 }, separator: ";" },
        "p",
      ]);
      eq(assert, "a missing side is the current line", parsePrefix(",5"), [
        { start: cur(), end: n(5), separator: "," },
        "",
      ]);
      eq(assert, "\\/ \\& ", parsePrefix("\\/,\\&"), [
        {
          start: { base: { kind: "lastPattern", backward: false }, offset: 0 },
          end: { base: { kind: "lastSubstitute" }, offset: 0 },
          separator: ",",
        },
        "",
      ]);
      eq(assert, "no range", parsePrefix("  :write"), [undefined, "write"]);
    });

    cmdTest("a range goes only to commands with a prefix", (assert) => {
      command.add("d", (c) => log.push(c.range), { prefix: "vim-range" });
      command.add("pwd", record);
      command.execute("2,3d");
      command.execute("d");
      eq(
        assert,
        "no prefix",
        thrown(() => command.execute("2pwd")),
        "E481: No range allowed",
      );
      eq(
        assert,
        'a range alone, with no command named ""',
        thrown(() => command.execute("5")),
        "E492: Not an editor command: 5",
      );
      eq(assert, "passed as typed", log, [
        {
          start: { base: { kind: "number", number: 2 }, offset: 0 },
          end: { base: { kind: "number", number: 3 }, offset: 0 },
          separator: ",",
        },
        undefined,
      ]);
    });

    cmdTest('a line without a name runs the command named ""', (assert) => {
      command.add("", (c) => log.push([c.range?.start.base, c.args]), { prefix: "vim-range" });
      command.add("d", (c) => log.push(["d", c.range?.start.base]), { prefix: "vim-range" });
      command.execute("5");
      command.execute("5 | 3d");
      command.execute("!ls");
      eq(
        assert,
        "a name is not taken for it",
        thrown(() => command.execute("x")),
        "E492: Not an editor command: x",
      );
      command.execute("");
      eq(assert, "range, arguments", log, [
        [{ kind: "number", number: 5 }, ""],
        [{ kind: "number", number: 5 }, " "],
        ["d", { kind: "number", number: 3 }],
        [undefined, "!ls"],
      ]);
      command.add("", record);
      eq(
        assert,
        "without a prefix",
        thrown(() => command.execute("5")),
        "E481: No range allowed",
      );
    });

    cmdTest("an empty line does nothing", (assert) => {
      // Nothing to check but that it does not throw.
      assert.expect(0);
      command.execute("");
      command.execute("  : ");
    });

    cmdTest("bar auto: each command up to |, in order; \\| is a |", (assert) => {
      command.add("a", (c) => log.push(["a", c.args, c.line]));
      command.add("b", (c) => log.push(["b", c.args, c.line]), { prefix: "vim-range" });
      command.execute("a x\\|y | 2b|a");
      eq(assert, "ran", log, [
        ["a", " x|y ", "a x\\|y "],
        ["b", "", "2b"],
        ["a", "", "a"],
      ]);
    });

    cmdTest("bar auto: an error stops what follows", (assert) => {
      command.add("fail", () => {
        throw new Error("boom");
      });
      command.add("a", () => log.push("a"));
      eq(
        assert,
        "thrown",
        thrown(() => command.execute("a | fail | a")),
        "boom",
      );
      eq(
        assert,
        "unknown",
        thrown(() => command.execute("a | nope | a")),
        "E492: Not an editor command: nope | a",
      );
      eq(assert, "ran", log, ["a", "a"]);
    });

    cmdTest("bar handle: the rest as it is, continued with command.execute; mods only when passed", (assert) => {
      command.add("vertical", (c) => command.execute(c.args, { mods: { ...c.mods, vertical: true } }), {
        shortName: "vert",
        bar: "handle",
      });
      command.add("normal", (c) => log.push(["normal", c.args]), { bar: "handle" });
      command.add("split", (c) => log.push(["split", c.mods]), { shortName: "sp" });
      command.execute("vert sp | sp");
      command.execute("normal ihello|world");
      command.execute("sp", { mods: { tab: 2 } });
      eq(assert, "ran", log, [
        ["split", { vertical: true }],
        ["split", {}],
        ["normal", " ihello|world"],
        ["split", { tab: 2 }],
      ]);
    });

    cmdTest("bar handle: a callback may run the rest many times (as :g)", (assert) => {
      command.add(
        "each",
        (c) => {
          for (let i = 0; i < 3; i++) command.execute(c.args);
        },
        { bar: "handle" },
      );
      command.add("x", () => log.push("x"));
      command.execute("each x");
      eq(assert, "ran", log, ["x", "x", "x"]);
    });

    cmdTest("depth: a command running itself stops; long chains do not", (assert) => {
      command.add("again", () => command.execute("again"));
      eq(
        assert,
        "E169",
        thrown(() => command.execute("again")),
        "E169: Command too recursive",
      );
      command.add("n", () => log.push(1));
      command.execute(Array(1000).fill("n").join("|"));
      eq(assert, "a chain of 1000", log.length, 1000);
      eq(
        assert,
        "the depth came back",
        thrown(() => command.execute("n")),
        "",
      );
    });

    cmdTest("helpers", (assert) => {
      eq(assert, "bang", parseBang("! x"), [true, " x"]);
      eq(assert, "no bang", parseBang(" !x"), [false, " !x"]);
      eq(assert, "split args", splitArgs("  a\\ b  c\\\\ d\\x "), ["a b", "c\\", "d\\x"]);
      eq(assert, "no args", splitArgs("   "), []);
      eq(assert, "split bar", splitBar("a\\|b|c|d"), ["a\\|b", "c|d"]);
      eq(assert, "no bar", splitBar("a\\|b"), ["a\\|b", undefined]);
    });

    cmdTest("resolving a range", (assert) => {
      const env: VimRangeEnv = {
        current: () => 10,
        last: () => 99,
        mark: (name) => (name === "a" ? 4 : 7),
        search: (p, backward, from) => (backward ? from - 2 : from + (p === undefined ? 1 : 3)),
      };
      const r = (s: string) => resolveRange(parsePrefix(s)[0]!, env);
      eq(assert, "%", r("%"), { first: 0, last: 99 });
      eq(assert, "numbers, swapped", r("5,2"), { first: 1, last: 4 });
      eq(assert, ".+1,$-1", r(".+1,$-1"), { first: 11, last: 98 });
      eq(assert, "marks", r("'a,'b"), { first: 4, last: 7 });
      eq(assert, ", counts from the cursor", r("2,/x/"), { first: 1, last: 13 });
      eq(assert, "; counts from the first", r("2;/x/"), { first: 1, last: 4 });
      eq(assert, "backward", r("?x?"), { first: 8, last: 8 });
      eq(assert, "last pattern", r("\\/"), { first: 11, last: 11 });
      const noSearch: VimRangeEnv = { current: () => 0, last: () => 0, mark: () => 0 };
      eq(
        assert,
        "patterns refused",
        thrown(() => resolveRange(parsePrefix("/x/")[0]!, noSearch)),
        "E16: Invalid range",
      );
    });

    cmdTest("a keyset's commands go when it is disabled", (assert) => {
      const set = keyset("test-commands", (d) => d.command("hello", () => log.push("hello")));
      set.enable();
      command.execute("hello");
      eq(
        assert,
        "its namespace",
        command.list().map((c) => c.namespace),
        ["test-commands"],
      );
      set.disable();
      eq(assert, "gone", command.list().length, 0);
      set.enable();
      command.execute("hello");
      set.disable();
      eq(assert, "ran", log, ["hello", "hello"]);
    });
  },
);
