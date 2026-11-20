// The cases of temzdf/command (runtime/command.ts), as a configuration:
// <F5> runs them, throws the failures, and sets the pane variable
// commandTests to "ok" when every case passed. Run by `command_cases` in
// crates/temzdf-server/src/session/vim_tests.rs.
import { action, keymap } from "temzdf";
import vim from "temzdf/edit/vim";
import { command, parseBang, parsePrefix, resolveRange, splitArgs, splitBar } from "temzdf/command";
import type { CmdCtx, VimRangeEnv } from "temzdf/command";
import { keyset } from "../lib/keyset";

const failures: string[] = [];

function eq(what: string, got: unknown, want: unknown): void {
  const g = JSON.stringify(got);
  const w = JSON.stringify(want);
  if (g !== w) failures.push(`${what}: got ${g}, want ${w}`);
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

/** Each case starts from an empty registry; `log` collects what ran. */
let log: unknown[] = [];
function fresh(): void {
  command.clear();
  log = [];
}
const record = (c: CmdCtx) => log.push([c.name, c.args]);

const cases: [string, () => void][] = [
  [
    "registering checks its arguments",
    () => {
      eq("name with a digit", thrown(() => command.add("w2", record)) !== "", true);
      eq("not a function", thrown(() => command.add("w", 42 as never)) !== "", true);
      eq("shortName not a prefix", thrown(() => command.add("write", record, { shortName: "x" })) !== "", true);
      eq("empty shortName", thrown(() => command.add("write", record, { shortName: "" })) !== "", true);
      eq(
        "an empty shortName for the empty name",
        thrown(() => command.add("", record, { shortName: "" })) !== "",
        true,
      );
      eq("any shortName for the empty name", thrown(() => command.add("", record, { shortName: "a" })) !== "", true);
      eq("unknown prefix", thrown(() => command.add("w", record, { prefix: "lines" as never })) !== "", true);
      eq("unknown bar", thrown(() => command.add("w", record, { bar: "split" as never })) !== "", true);
      eq("nothing registered", command.list().length, 0);
    },
  ],
  [
    "names match exactly, or abbreviated down to shortName",
    () => {
      command.add("write", record, { shortName: "w" });
      command.add("edit", record);
      for (const typed of ["w", "wri", "write"]) command.execute(typed);
      eq("abbreviations", log, [
        ["write", ""],
        ["write", ""],
        ["write", ""],
      ]);
      eq(
        "too long",
        thrown(() => command.execute("writes")),
        "E492: Not an editor command: writes",
      );
      eq(
        "no shortName: full name only",
        thrown(() => command.execute("ed")),
        "E492: Not an editor command: ed",
      );
    },
  ],
  [
    "an exact name beats an abbreviation, and the last defined wins",
    () => {
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
      eq("picked", log, ["p", "print", "previous", "p again"]);
    },
  ],
  [
    "when leaves commands out",
    () => {
      command.add("go", () => log.push("text"), { when: (c) => c.paneKind === "edit" });
      command.add("go", () => log.push("pdf"), { when: (c) => c.paneKind === "pdf" });
      command.execute("go");
      command.add("only", () => log.push("never"), { when: () => false });
      eq(
        "none live",
        thrown(() => command.execute("only")),
        "E492: Not an editor command: only",
      );
      eq("the live one", log, ["text"]);
    },
  ],
  [
    "namespaces: replaced, removed, cleared",
    () => {
      command.add("a", () => log.push("a1"), { namespace: "x" });
      command.add("a", () => log.push("a2"), { namespace: "x" });
      command.add("a", () => log.push("a3"), { namespace: "y" });
      eq(
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
        "left",
        command.list().map((c) => c.name),
        ["c"],
      );
      command.remove("c");
      eq("all gone", command.list().length, 0);
      eq("ran", log, ["a2"]);
    },
  ],
  [
    "arguments are passed untrimmed",
    () => {
      command.add("w", record);
      command.execute("w!");
      command.execute("w !sort");
      command.execute("  :w  x  ");
      command.execute("w1");
      eq("args", log, [
        ["w", "!"],
        ["w", " !sort"],
        ["w", "  x  "],
        ["w", "1"],
      ]);
    },
  ],
  [
    "ranges are parsed as typed",
    () => {
      const n = (number: number, offset = 0) => ({ base: { kind: "number", number }, offset });
      const cur = (offset = 0) => ({ base: { kind: "current" }, offset });
      const last = { base: { kind: "last" }, offset: 0 };
      eq("%", parsePrefix("%d"), [{ start: n(1), end: last, separator: "," }, "d"]);
      eq(".,$", parsePrefix(".,$d"), [{ start: cur(), end: last, separator: "," }, "d"]);
      eq("'<,'>", parsePrefix("'<,'>s/a/b/"), [
        {
          start: { base: { kind: "mark", name: "<" }, offset: 0 },
          end: { base: { kind: "mark", name: ">" }, offset: 0 },
          separator: ",",
        },
        "s/a/b/",
      ]);
      eq("3", parsePrefix("3"), [{ start: n(3) }, ""]);
      eq("+2", parsePrefix("+2y"), [{ start: cur(2) }, "y"]);
      eq("offsets add up", parsePrefix("$-3+1--"), [{ start: { base: { kind: "last" }, offset: -4 } }, ""]);
      eq("/a\\/b/", parsePrefix("/a\\/b/d"), [
        { start: { base: { kind: "pattern", pattern: "a/b", backward: false }, offset: 0 } },
        "d",
      ]);
      eq("1;?x?", parsePrefix("1;?x?+1p"), [
        { start: n(1), end: { base: { kind: "pattern", pattern: "x", backward: true }, offset: 1 }, separator: ";" },
        "p",
      ]);
      eq("a missing side is the current line", parsePrefix(",5"), [{ start: cur(), end: n(5), separator: "," }, ""]);
      eq("\\/ \\& ", parsePrefix("\\/,\\&"), [
        {
          start: { base: { kind: "lastPattern", backward: false }, offset: 0 },
          end: { base: { kind: "lastSubstitute" }, offset: 0 },
          separator: ",",
        },
        "",
      ]);
      eq("no range", parsePrefix("  :write"), [undefined, "write"]);
    },
  ],
  [
    "a range goes only to commands with a prefix",
    () => {
      command.add("d", (c) => log.push(c.range), { prefix: "vim-range" });
      command.add("pwd", record);
      command.execute("2,3d");
      command.execute("d");
      eq(
        "no prefix",
        thrown(() => command.execute("2pwd")),
        "E481: No range allowed",
      );
      eq(
        'a range alone, with no command named ""',
        thrown(() => command.execute("5")),
        "E492: Not an editor command: 5",
      );
      eq("passed as typed", log, [
        {
          start: { base: { kind: "number", number: 2 }, offset: 0 },
          end: { base: { kind: "number", number: 3 }, offset: 0 },
          separator: ",",
        },
        undefined,
      ]);
    },
  ],
  [
    'a line without a name runs the command named ""',
    () => {
      command.add("", (c) => log.push([c.range?.start.base, c.args]), { prefix: "vim-range" });
      command.add("d", (c) => log.push(["d", c.range?.start.base]), { prefix: "vim-range" });
      command.execute("5");
      command.execute("5 | 3d");
      command.execute("!ls");
      eq(
        "a name is not taken for it",
        thrown(() => command.execute("x")),
        "E492: Not an editor command: x",
      );
      command.execute("");
      eq("range, arguments", log, [
        [{ kind: "number", number: 5 }, ""],
        [{ kind: "number", number: 5 }, " "],
        ["d", { kind: "number", number: 3 }],
        [undefined, "!ls"],
      ]);
      command.add("", record);
      eq(
        "without a prefix",
        thrown(() => command.execute("5")),
        "E481: No range allowed",
      );
    },
  ],
  [
    "an empty line does nothing",
    () => {
      command.execute("");
      command.execute("  : ");
    },
  ],
  [
    "bar auto: each command up to |, in order; \\| is a |",
    () => {
      command.add("a", (c) => log.push(["a", c.args, c.line]));
      command.add("b", (c) => log.push(["b", c.args, c.line]), { prefix: "vim-range" });
      command.execute("a x\\|y | 2b|a");
      eq("ran", log, [
        ["a", " x|y ", "a x\\|y "],
        ["b", "", "2b"],
        ["a", "", "a"],
      ]);
    },
  ],
  [
    "bar auto: an error stops what follows",
    () => {
      command.add("fail", () => {
        throw new Error("boom");
      });
      command.add("a", () => log.push("a"));
      eq(
        "thrown",
        thrown(() => command.execute("a | fail | a")),
        "boom",
      );
      eq(
        "unknown",
        thrown(() => command.execute("a | nope | a")),
        "E492: Not an editor command: nope | a",
      );
      eq("ran", log, ["a", "a"]);
    },
  ],
  [
    "bar handle: the rest as it is, continued with command.execute; mods only when passed",
    () => {
      command.add("vertical", (c) => command.execute(c.args, { mods: { ...c.mods, vertical: true } }), {
        shortName: "vert",
        bar: "handle",
      });
      command.add("normal", (c) => log.push(["normal", c.args]), { bar: "handle" });
      command.add("split", (c) => log.push(["split", c.mods]), { shortName: "sp" });
      command.execute("vert sp | sp");
      command.execute("normal ihello|world");
      command.execute("sp", { mods: { tab: 2 } });
      eq("ran", log, [
        ["split", { vertical: true }],
        ["split", {}],
        ["normal", " ihello|world"],
        ["split", { tab: 2 }],
      ]);
    },
  ],
  [
    "bar handle: a callback may run the rest many times (as :g)",
    () => {
      command.add(
        "each",
        (c) => {
          for (let i = 0; i < 3; i++) command.execute(c.args);
        },
        { bar: "handle" },
      );
      command.add("x", () => log.push("x"));
      command.execute("each x");
      eq("ran", log, ["x", "x", "x"]);
    },
  ],
  [
    "depth: a command running itself stops; long chains do not",
    () => {
      command.add("again", () => command.execute("again"));
      eq(
        "E169",
        thrown(() => command.execute("again")),
        "E169: Command too recursive",
      );
      command.add("n", () => log.push(1));
      command.execute(Array(1000).fill("n").join("|"));
      eq("a chain of 1000", log.length, 1000);
      eq(
        "the depth came back",
        thrown(() => command.execute("n")),
        "",
      );
    },
  ],
  [
    "helpers",
    () => {
      eq("bang", parseBang("! x"), [true, " x"]);
      eq("no bang", parseBang(" !x"), [false, " !x"]);
      eq("split args", splitArgs("  a\\ b  c\\\\ d\\x "), ["a b", "c\\", "d\\x"]);
      eq("no args", splitArgs("   "), []);
      eq("split bar", splitBar("a\\|b|c|d"), ["a\\|b", "c|d"]);
      eq("no bar", splitBar("a\\|b"), ["a\\|b", undefined]);
    },
  ],
  [
    "resolving a range",
    () => {
      const env: VimRangeEnv = {
        current: () => 10,
        last: () => 99,
        mark: (name) => (name === "a" ? 4 : 7),
        search: (p, backward, from) => (backward ? from - 2 : from + (p === undefined ? 1 : 3)),
      };
      const r = (s: string) => resolveRange(parsePrefix(s)[0]!, env);
      eq("%", r("%"), { first: 0, last: 99 });
      eq("numbers, swapped", r("5,2"), { first: 1, last: 4 });
      eq(".+1,$-1", r(".+1,$-1"), { first: 11, last: 98 });
      eq("marks", r("'a,'b"), { first: 4, last: 7 });
      eq(", counts from the cursor", r("2,/x/"), { first: 1, last: 13 });
      eq("; counts from the first", r("2;/x/"), { first: 1, last: 4 });
      eq("backward", r("?x?"), { first: 8, last: 8 });
      eq("last pattern", r("\\/"), { first: 11, last: 11 });
      const noSearch: VimRangeEnv = { current: () => 0, last: () => 0, mark: () => 0 };
      eq(
        "patterns refused",
        thrown(() => resolveRange(parsePrefix("/x/")[0]!, noSearch)),
        "E16: Invalid range",
      );
    },
  ],
  [
    "a keyset's commands go when it is disabled",
    () => {
      const set = keyset("test-commands", (d) => d.command("hello", () => log.push("hello")));
      set.enable();
      command.execute("hello");
      eq(
        "its namespace",
        command.list().map((c) => c.namespace),
        ["test-commands"],
      );
      set.disable();
      eq("gone", command.list().length, 0);
      set.enable();
      command.execute("hello");
      set.disable();
      eq("ran", log, ["hello", "hello"]);
    },
  ],
];

vim.enable();
keymap.add("vim/normal", "<F5>", () => {
  for (const [name, run] of cases) {
    fresh();
    const before = failures.length;
    const e = thrown(run);
    if (e !== "") failures.push(`threw ${e}`);
    for (let i = before; i < failures.length; i++) failures[i] = `${name}: ${failures[i]}`;
  }
  command.clear();
  if (failures.length > 0) throw new Error(`${failures.length} failed:\n${failures.join("\n")}`);
  action.setVar("pane", "commandTests", `ok ${cases.length}`);
});
