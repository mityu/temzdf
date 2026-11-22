// Write crates/temzdf-script/ts-types/temzdf-all.d.ts (what `temzdf
// print-types` prints): the declarations written by hand (temzdf-core.d.ts,
// temzdf-actions.d.ts) and those of the modules scripts import
// (`temzdf/NAME`, the table MODULES in crates/temzdf-script/build.rs),
// bundled from their TypeScript by dts-bundle-generator, each in a
// `declare module "temzdf/NAME" { ... }` showing only what the module
// exports. `cargo xtask check` checks that the committed file is what this
// writes.
//
// Usage: node scripts/ts-types.ts [OUT]   (default: the committed file)
//
// dts-bundle-generator and TypeScript 6 are devDependencies of
// crates/temzdf-script/runtime/package.json, found in its node_modules
// (`npm ci`), else in $TEMZDF_NODE_MODULES (Nix's); the bundler is run
// with this Node, so no shell or `.bin` shim is involved.
// It keeps an import as it is only for a library, a module found in a
// node_modules directory: so `temzdf` (the declarations by hand) and the
// runtime are put under target/ts-types/node_modules/temzdf here, and
// `temzdf/NAME` resolves there, while the entries are read where they are
// (a library's files would be read as sources, not as declarations).
//
// Node 22.18 or later runs the TypeScript as it is (erasable syntax only,
// type-checked by `tsc -p scripts`).

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

const root = dirname(import.meta.dirname);
const types = "crates/temzdf-script/ts-types";
const out = process.argv[2] === undefined ? join(root, types, "temzdf-all.d.ts") : resolve(process.argv[2]);
process.chdir(root);

function fail(message: string): never {
  process.stderr.write(`${message}\n`);
  process.exit(1);
}

/** FILE's lines, as sed and grep see them (a last line without a newline is one too). */
function lines(file: string): string[] {
  const ls = readFileSync(file, "utf8").split("\n");
  if (ls[ls.length - 1] === "") ls.pop();
  return ls;
}
const text = (ls: string[]) => ls.map((l) => `${l}\n`).join("");

// dts-bundle-generator's command: its package's `bin`, run with this Node.
const bundler = (() => {
  const dirs = ["crates/temzdf-script/runtime/node_modules"];
  if (process.env.TEMZDF_NODE_MODULES) dirs.push(process.env.TEMZDF_NODE_MODULES);
  for (const dir of dirs) {
    const pkg = join(dir, "dts-bundle-generator", "package.json");
    if (existsSync(pkg)) {
      const bin: string | Record<string, string> = JSON.parse(readFileSync(pkg, "utf8")).bin;
      return resolve(dirname(pkg), typeof bin === "string" ? bin : bin["dts-bundle-generator"]);
    }
  }
  return fail("no dts-bundle-generator: run npm ci in crates/temzdf-script/runtime");
})();
const work = join(root, "target/ts-types");
const runtime = join(root, "crates/temzdf-script/runtime");
rmSync(work, { recursive: true, force: true });
mkdirSync(join(work, "node_modules/temzdf"), { recursive: true });
mkdirSync(join(work, "out"));

// The modules: the ("NAME", "ENTRY") lines of build.rs's MODULES.
const modules: { name: string; entry: string }[] = [];
{
  let inTable = false;
  for (const line of lines("crates/temzdf-script/build.rs")) {
    if (!inTable) {
      inTable = line.startsWith("const MODULES");
      continue;
    }
    if (line.startsWith("];")) break;
    const m = /^ *\("([^"]*)", "([^"]*)"\),$/.exec(line);
    if (m) modules.push({ name: m[1], entry: m[2] });
  }
}
if (modules.length === 0) fail("no modules in crates/temzdf-script/build.rs");

const start = 'declare module "temzdf" {';
// `temzdf` as a library: the bodies of the declarations' `declare module
// "temzdf"` blocks; what comes before the block (globals) stays global.
function body(file: string): string[] {
  const kept: string[] = [];
  let inBlock = false;
  for (const line of lines(file)) {
    if (inBlock) {
      kept.push(line);
      if (line === "}") inBlock = false;
    } else if (line === start) {
      kept.push(line);
      inBlock = true;
    }
  }
  return kept.slice(1, -1);
}
// The globals, referred to from there: the bundler's programs start from
// the entries alone (no `files`), and the npm packages' types use them too
// (zod's URL).
writeFileSync(
  join(work, "node_modules/temzdf/index.d.ts"),
  text([
    '/// <reference path="../../globals.d.ts" />',
    ...body(`${types}/temzdf-core.d.ts`),
    ...body(`${types}/temzdf-actions.d.ts`),
  ]),
);
{
  const core = lines(`${types}/temzdf-core.d.ts`);
  const i = core.indexOf(start);
  writeFileSync(join(work, "globals.d.ts"), text(i < 0 ? core : core.slice(0, i)));
}
// A junction on Windows: it needs no privilege there, unlike a symbolic link.
symlinkSync(runtime, join(work, "node_modules/temzdf/runtime"), process.platform === "win32" ? "junction" : "dir");
writeFileSync(
  join(work, "node_modules/temzdf/package.json"),
  '{ "name": "temzdf", "version": "0.0.0", "types": "index.d.ts" }\n',
);

const outFile = (name: string) => join(work, "out", `${name.replaceAll("/", "-")}.d.ts`);
const paths: Record<string, string[]> = { temzdf: ["./node_modules/temzdf/index.d.ts"] };
for (const m of modules) paths[`temzdf/${m.name}`] = [`./node_modules/temzdf/runtime/${m.entry}`];
const tsconfig = {
  compilerOptions: {
    strict: true,
    target: "es2022",
    module: "esnext",
    moduleResolution: "bundler",
    allowImportingTsExtensions: true,
    noEmit: true,
    types: [],
    lib: ["es2022"],
    paths,
  },
  files: ["./globals.d.ts", ...modules.map((m) => join(runtime, m.entry))],
};
writeFileSync(join(work, "tsconfig.json"), `${JSON.stringify(tsconfig, null, 2)}\n`);
const bundle = {
  compilationOptions: { preferredConfigPath: join(work, "tsconfig.json") },
  entries: modules.map((m) => ({
    filePath: join(runtime, m.entry),
    outFile: outFile(m.name),
    output: { exportReferencedTypes: false, noBanner: true },
    libraries: { importedLibraries: ["temzdf"] },
  })),
};
writeFileSync(join(work, "bundle.json"), `${JSON.stringify(bundle, null, 2)}\n`);
const run = spawnSync(process.execPath, [bundler, "--silent", "--config", join(work, "bundle.json")], {
  encoding: "utf8",
  maxBuffer: 64 * 1024 * 1024,
});
const log = `${run.stdout ?? ""}${run.stderr ?? ""}`;
writeFileSync(join(work, "bundle.log"), log);
if (run.status !== 0) fail(run.error ? String(run.error) : log.trimEnd());

// One `declare module` per module: inside one, declarations are ambient
// already (`declare` is an error there), and `export {}` is implied.
let result =
  `// Generated by scripts/ts-types.ts from ${types}/temzdf-core.d.ts,\n` +
  `// ${types}/temzdf-actions.d.ts and the shipped modules (do not edit):\n` +
  "// the scripting API of temzdf, as `temzdf print-types` prints it.\n" +
  "\n" +
  readFileSync(`${types}/temzdf-core.d.ts`, "utf8") +
  "\n" +
  readFileSync(`${types}/temzdf-actions.d.ts`, "utf8");
for (const m of modules) {
  const ls = lines(outFile(m.name))
    .filter((l) => l !== "export {};")
    .map((l) => l.replace(/^(export )?declare /, "$1"));
  const first = ls.findIndex((l) => l !== "");
  result += `\ndeclare module "temzdf/${m.name}" {\n${text(first < 0 ? [] : ls.slice(first))}}\n`;
}
writeFileSync(out, result);
