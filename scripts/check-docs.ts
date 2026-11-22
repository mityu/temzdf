// Check references to documents, so moving or renaming one cannot leave
// stale paths behind. Looks at the files git tracks (committed or added;
// `git add -N` a new file to include it) and the untracked ones it does
// not ignore, except the skills under .claude/ (most are vendored and name
// files relative to themselves), this file and Cargo.lock:
//
//   1. every path under doc/ written anywhere (code comments, scripts,
//      docs: `doc/design/session-server.md`, `doc/survey/`) exists;
//   2. every relative Markdown link, [text](path), in a .md file exists,
//      resolved from that file's directory (anchors are not checked);
//   3. every Markdown file named in backticks in a .md file (`todo.md`,
//      `crates/tuitest/README.md`) exists relative to the repository root,
//      as the conventions want paths written (AGENTS.md);
//   4. every document under doc/ is linked from doc/README.md, the index.
//
// It does not check URLs, headings or anchors, or whether a document's
// content is still true.
//
// Usage: node scripts/check-docs.ts   (prints each problem as FILE:LINE: ...;
//                                      exits 1 if there was one)
//
// Node's standard library only (no npm packages), so that the documents'
// CI runs it without `npm ci`; Node 22.18 or later runs the TypeScript as
// it is (erasable syntax only, type-checked by `tsc -p scripts`).

import { execFileSync } from "node:child_process";
import { existsSync, lstatSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";

process.chdir(dirname(import.meta.dirname));

/** What `git ls-files` lists for PATHSPECS: tracked and untracked, not ignored. */
function gitFiles(...pathspecs: string[]): string[] {
  const out = execFileSync(
    "git",
    ["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", ...pathspecs],
    {
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    },
  );
  return out.split("\0").filter((f) => f !== "");
}

/** FILE's lines, or nothing when it is gone, not a regular file or binary (as `git grep` skips them). */
function lines(file: string): string[] {
  try {
    if (!lstatSync(file).isFile()) return [];
  } catch {
    return [];
  }
  const data = readFileSync(file);
  if (data.subarray(0, 8000).includes(0)) return [];
  return data.toString("utf8").split("\n");
}

/** Each match of PATTERN (global) in FILES: [file, 1-based line, match]. */
function* grep(files: string[], pattern: RegExp): Generator<[string, number, string]> {
  for (const file of files) {
    const ls = lines(file);
    for (let i = 0; i < ls.length; i++) {
      for (const m of ls[i].matchAll(pattern)) yield [file, i + 1, m[0]];
    }
  }
}

const skipped = (f: string) => f.startsWith(".claude/skills/");
const all = gitFiles(".")
  .filter((f) => !skipped(f) && f !== "scripts/check-docs.ts" && f !== "Cargo.lock")
  .sort();
const md = all.filter((f) => f.endsWith(".md"));

let bad = false;
function problem(message: string): void {
  console.log(message);
  bad = true;
}

// 1. Paths under doc/ (globs such as doc/** are not paths).
for (const [file, line, path] of grep(all, /(?<![A-Za-z0-9_./-])doc\/[A-Za-z0-9_./-]*[A-Za-z0-9_/]/g)) {
  if (path.includes("*")) continue;
  if (!existsSync(path)) problem(`${file}:${line}: ${path} does not exist`);
}

// 2. Relative Markdown links in Markdown files.
for (const [file, line, link] of grep(md, /\]\([^) \t\n\v\f\r]+\)/g)) {
  const target = link.slice(2, -1).split("#")[0];
  if (target === "" || target.includes("://") || target.startsWith("mailto:")) continue;
  const dir = dirname(file);
  if (!existsSync(join(dir, target))) problem(`${file}:${line}: link to ${target}, not found from ${dir}/`);
}

// 3. Markdown files named in backticks in Markdown files.
for (const [file, line, quoted] of grep(md, /`[A-Za-z0-9_./-]+\.md`/g)) {
  const name = quoted.slice(1, -1);
  if (!existsSync(name))
    problem(`${file}:${line}: \`${name}\` does not exist (paths are relative to the repository root)`);
}

// 4. Every document is in the index.
let index = "";
try {
  index = readFileSync("doc/README.md", "utf8");
} catch {
  problem("doc/README.md: missing (the index of doc/)");
}
for (const doc of gitFiles("doc/*.md")) {
  if (doc === "doc/README.md") continue;
  if (!index.includes(`](${doc.slice("doc/".length)})`)) problem(`doc/README.md: does not link to ${doc}`);
}

process.exit(bad ? 1 : 0);
