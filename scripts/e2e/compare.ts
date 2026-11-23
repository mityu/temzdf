// Compares e2e runs of two harnesses (or two builds): the judgements, the
// input sent, the command run, and the expected screens of `expect NAME
// KEYS` (which depend on nothing but the keys and the geometry).
//
// Usage: node scripts/e2e/compare.ts BASE... -- NEW...
//   BASE, NEW  directories of runs: e2e-all's (DIR/TERM/SCENARIO/) or a
//              single run's (with expect.txt); several of each are runs
//              repeated, whose results are counted
// Prints each difference; exits 1 if there was one. A judgement whose
// results vary within either side is listed as varying, with the counts,
// whatever they are: it is for a person to judge.

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { parse, splitWords } from "./command.ts";

/** Runs under DIR: "TERM/SCENARIO" (or "." for a single run) to its directory. */
export function runsIn(dir: string): Map<string, string> {
  const runs = new Map<string, string>();
  if (existsSync(path.join(dir, "expect.txt")) || existsSync(path.join(dir, "command.txt"))) {
    runs.set(".", dir);
    return runs;
  }
  for (const term of readdirSync(dir)) {
    const t = path.join(dir, term);
    if (!statSync(t).isDirectory()) continue;
    for (const s of readdirSync(t)) {
      const d = path.join(t, s);
      if (statSync(d).isDirectory()) runs.set(`${term}/${s}`, d);
    }
  }
  return runs;
}

/** A run's judgements in order: "NAME STATUS" (repeated names numbered). */
export function verdicts(text: string): string[] {
  const seen = new Map<string, number>();
  return text
    .split("\n")
    .filter((l) => /^(PASS|FAIL|SKIP) /.test(l))
    .map((l) => {
      const [status, rest] = [l.slice(0, 4), l.slice(5)];
      // The name: up to ":" (FAIL, SKIP) or a space (PASS NAME (N cells)).
      const name = rest.split(/[: ]/)[0]!;
      const n = (seen.get(name) ?? 0) + 1;
      seen.set(name, n);
      return `${n > 1 ? `${name}#${n}` : name} ${status}`;
    });
}

/** The input a run sent, without the times, its directory DIR (typed
 * into the shell, as typed: absolute, or from the current directory) as
 * OUTDIR. */
export function inputs(text: string, dir = ""): string[] {
  const abs = path.resolve(dir);
  const rel = path.relative(process.cwd(), abs);
  return text
    .split("\n")
    .filter(Boolean)
    .map((l) => {
      let w = l.slice(l.indexOf(" ") + 1);
      if (dir) w = w.replaceAll(abs, "OUTDIR").replaceAll(rel, "OUTDIR");
      return w;
    });
}

/** A run's command as the harness reads it: its words, whether a shell
 * follows, its variables (outside and inside `sh -c`), quoting and the
 * first word's absolute path aside. */
export function commandKey(text: string, dir = ""): string {
  let argv = text.replace(/\n$/, "").split("\n");
  if (dir) argv = argv.map((w) => w.replaceAll(path.resolve(dir), "OUTDIR"));
  const isVar = (w: string) => /^[A-Za-z_][A-Za-z0-9_]*=/.test(w);
  const env: string[] = [];
  let i = 0;
  while (i < argv.length && (argv[i] === "env" || isVar(argv[i]!))) {
    if (argv[i] !== "env") env.push(argv[i]!);
    i++;
  }
  const rest = argv.slice(i);
  if (rest.length >= 3 && (rest[0] === "sh" || rest[0]!.endsWith("/sh")) && rest[1] === "-c") {
    const text = rest[2]!;
    const semi = text.indexOf(";");
    for (const w of splitWords(semi < 0 ? text : text.slice(0, semi))) {
      if (!isVar(w)) break;
      env.push(w);
    }
  }
  const { words, hasShell } = parse(argv);
  return JSON.stringify({
    words: words.map((w) => (w.startsWith("/") && w.includes("/target/") ? w.slice(w.indexOf("target/")) : w)),
    hasShell,
    env: env.sort(),
  });
}

function read(file: string): string | undefined {
  try {
    return readFileSync(file, "utf8");
  } catch {
    return undefined;
  }
}

/** The expected screens of `expect NAME KEYS` in a run of the standalone
 * viewer: those without a replay of recorded input (NAME.events.txt). */
function keyedExpects(dir: string): string[] {
  const cmd = read(path.join(dir, "command.txt")) ?? "";
  if (!cmd.includes("standalone")) return [];
  return readdirSync(dir)
    .filter((f) => f.endsWith(".expected.png"))
    .map((f) => f.slice(0, -".expected.png".length))
    .filter((n) => !existsSync(path.join(dir, `${n}.events.txt`)));
}

const artefacts = (n: string) => [`${n}.expected.png`, `${n}.mask.png`, `${n}.cells.png`, `${n}.ansi`];

function count(xs: string[]): Map<string, number> {
  const m = new Map<string, number>();
  for (const x of xs) m.set(x, (m.get(x) ?? 0) + 1);
  return m;
}

const show = (m: Map<string, number>) =>
  [...m]
    .sort()
    .map(([k, v]) => `${k} x${v}`)
    .join(", ");

export function compare(bases: string[], news: string[]): string[] {
  const problems: string[] = [];
  const baseRuns = bases.map(runsIn);
  const newRuns = news.map(runsIn);
  const keys = new Set([...baseRuns, ...newRuns].flatMap((m) => [...m.keys()]));
  for (const key of [...keys].sort()) {
    const b = baseRuns.map((m) => m.get(key)).filter((d): d is string => d !== undefined);
    const n = newRuns.map((m) => m.get(key)).filter((d): d is string => d !== undefined);
    if (b.length === 0 || n.length === 0) {
      problems.push(`${key}: only in ${b.length === 0 ? "the new runs" : "the base"}`);
      continue;
    }
    // Judgements: name -> results over the runs of each side.
    const results = (dirs: string[]) => {
      const m = new Map<string, string[]>();
      for (const d of dirs) {
        for (const v of verdicts(read(path.join(d, "expect.txt")) ?? "")) {
          const [name, status] = v.split(" ");
          m.set(name!, [...(m.get(name!) ?? []), status!]);
        }
      }
      return m;
    };
    const rb = results(b);
    const rn = results(n);
    for (const name of new Set([...rb.keys(), ...rn.keys()])) {
      const cb = count(rb.get(name) ?? []);
      const cn = count(rn.get(name) ?? []);
      const varies = cb.size > 1 || cn.size > 1;
      const sameSet = cb.size === cn.size && [...cb.keys()].every((k) => cn.has(k));
      if (varies) problems.push(`${key} ${name}: varies: base ${show(cb)}; new ${show(cn)}`);
      else if (!sameSet || (rb.get(name)?.length ?? 0) !== b.length || (rn.get(name)?.length ?? 0) !== n.length) {
        problems.push(`${key} ${name}: base ${show(cb) || "absent"}; new ${show(cn) || "absent"}`);
      }
    }
    // The input sent and the command: the same in every run.
    const one = (dirs: string[], file: string, f: (t: string, d: string) => string) =>
      new Set(dirs.map((d) => f(read(path.join(d, file)) ?? "", d)));
    const ib = one(b, "events.txt", (t, d) => inputs(t, d).join("\n"));
    const inew = one(n, "events.txt", (t, d) => inputs(t, d).join("\n"));
    if (ib.size !== 1 || inew.size !== 1 || [...ib][0] !== [...inew][0]) {
      problems.push(`${key}: the input sent differs (events.txt without times)`);
    }
    const kb = one(b, "command.txt", commandKey);
    const kn = one(n, "command.txt", commandKey);
    if ([...kb][0] !== [...kn][0] || kb.size !== 1 || kn.size !== 1) {
      problems.push(`${key}: the command differs: ${[...kb].join(" | ")} vs ${[...kn].join(" | ")}`);
    }
    // Expected screens of keyed expects: byte for byte.
    for (const name of keyedExpects(b[0]!)) {
      for (const f of artefacts(name)) {
        const x = readFileOr(path.join(b[0]!, f));
        const y = readFileOr(path.join(n[0]!, f));
        if (x === undefined || y === undefined || !x.equals(y)) problems.push(`${key}: ${f} differs`);
      }
    }
  }
  return problems;
}

function readFileOr(file: string): Buffer | undefined {
  try {
    return readFileSync(file);
  } catch {
    return undefined;
  }
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const dash = args.indexOf("--");
  if (dash <= 0 || dash === args.length - 1) {
    console.error("usage: node scripts/e2e/compare.ts BASE... -- NEW...");
    process.exit(2);
  }
  const problems = compare(args.slice(0, dash), args.slice(dash + 1));
  for (const p of problems) console.log(p);
  console.log(problems.length === 0 ? "the same" : `${problems.length} differences`);
  process.exitCode = problems.length === 0 ? 0 : 1;
}
