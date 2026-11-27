// The COMMAND a scenario runs in the terminal, and what the harness reads
// from it: whether it runs temzdf (and with --clean or --config), whether a
// shell is left after it (`sh -c 'CMD; exec sh'`), the session or the
// standalone viewer, and the headless render `expect` compares with. One
// path for both a scenario module's command and `SCENARIO -- COMMAND`: the
// module's is turned into the same argument list first (`commandLine`).

/** Quotes a word for POSIX sh (single quotes; nothing is special inside). */
export function shQuote(word: string): string {
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(word)) return word;
  return `'${word.replaceAll("'", `'\\''`)}'`;
}

/** Splits a shell word list as `xargs` does (quotes and backslashes, no
 * expansions): the words of `sh -c`'s text before its first `;`. */
export function splitWords(text: string): string[] {
  const words: string[] = [];
  let cur = "";
  let inWord = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i]!;
    if (c === "'" || c === '"') {
      const end = text.indexOf(c, i + 1);
      if (end < 0) throw new Error(`unmatched ${c} in ${text}`);
      cur += text.slice(i + 1, end);
      inWord = true;
      i = end;
    } else if (c === "\\" && i + 1 < text.length) {
      cur += text[++i];
      inWord = true;
    } else if (/\s/.test(c)) {
      if (inWord) words.push(cur);
      cur = "";
      inWord = false;
    } else {
      cur += c;
      inWord = true;
    }
  }
  if (inWord) words.push(cur);
  return words;
}

/** Whether COMMAND runs temzdf: a word naming temzdf or temzdf-gui (also
 * inside `sh -c '...'`; Windows' too: C:\...\temzdf-gui.exe), not any path
 * with "temzdf" in it. */
export function runsTemzdf(argv: readonly string[]): boolean {
  return /(^|[ /'"\\])temzdf(-gui)?(\.exe)?( |$)/m.test(argv.join(" "));
}

/** Throws unless a COMMAND that runs temzdf passes --clean or --config FILE
 * (scenarios see the shipped defaults or a configuration of the
 * repository's, not the user's config.ts). */
export function checkConfig(argv: readonly string[]): void {
  const all = argv.join(" ");
  if (runsTemzdf(argv) && !all.includes("--clean") && !all.includes("--config ")) {
    throw new Error("COMMAND runs temzdf without --clean or --config (it would load your config.ts)");
  }
}

const isEnvWord = (w: string): boolean => w === "env" || w.includes("=");

export interface Parsed {
  /** The words of what runs (env assignments and `sh -c` taken off). */
  words: string[];
  /** A shell is left after the command (`sh -c '...; exec sh'`). */
  hasShell: boolean;
}

/** COMMAND's words as the harness reads them: past `env VAR=...`, and
 * inside `sh -c 'CMD...; exec sh'` CMD's words (quotes as the shell reads
 * them). */
export function parse(argv: readonly string[]): Parsed {
  let args = [...argv];
  while (args.length > 0 && isEnvWord(args[0]!)) args.shift();
  let hasShell = false;
  if (args.length >= 3 && (args[0] === "sh" || args[0]!.endsWith("/sh")) && args[1] === "-c") {
    const text = args[2]!;
    hasShell = /;[ \t\n]*exec sh[ \t\n]*$/.test(text);
    const semi = text.indexOf(";");
    args = splitWords(semi < 0 ? text : text.slice(0, semi));
    while (args.length > 0 && isEnvWord(args[0]!)) args.shift();
  }
  return { words: args, hasShell };
}

export interface Standalone {
  /** temzdf standalone pdf [--page N] [--scale S] [--config FILE]: what
   * `expect` runs with --render-frame (the renderer may be E2E_RENDERER). */
  renderer: string;
  options: string[];
  file: string;
}

/** For `expect` in the standalone viewer: the app's document and view
 * options (scripts/e2e.sh took them from the command's words). */
export function standalone(words: readonly string[]): Standalone | undefined {
  if (words.length === 0) return undefined;
  const options: string[] = [];
  for (let i = 1; i < words.length - 1; i++) {
    const w = words[i]!;
    if (w === "--page" || w === "--scale" || w === "--config") options.push(w, words[i + 1]!);
  }
  return { renderer: words[0]!, options, file: words[words.length - 1]! };
}

/** The temzdf binary a session COMMAND runs (`.../temzdf` followed by a
 * space or the end), if any. */
export function sessionTemzdf(argv: readonly string[]): string | undefined {
  return /[^ '"]*\/temzdf( |$)/.exec(argv.join(" "))?.[0].trim();
}

/** A session (not the standalone viewer, not the GUI): `expect` attaches to it. */
export function isSession(term: string, argv: readonly string[]): boolean {
  return term !== "gui" && parse(argv).words[1] !== "standalone" && runsTemzdf(argv);
}

/** What a scenario module declares as its command. */
export interface CommandSpec {
  /** The program and its arguments. */
  command: string[];
  /** Variables for the command (inside `sh -c` when a shell follows); a
   * function of the run's OUTDIR when one names a file there. */
  env?: Record<string, string> | ((outdir: string) => Record<string, string>);
  /** Leave a shell after the command: `sh -c 'VAR=... CMD...; exec sh'`. */
  shellAfter?: boolean;
  /** Variables for the shell too (`env VAR=... sh -c '...'`). */
  outerEnv?: Record<string, string>;
}

/** A module's command as the argument list scripts/e2e.sh was given. */
export function commandLine(spec: CommandSpec, outdir = ""): string[] {
  const vars = typeof spec.env === "function" ? spec.env(outdir) : (spec.env ?? {});
  const env = Object.entries(vars).map(([k, v]) => `${k}=${v}`);
  const outer = Object.entries(spec.outerEnv ?? {}).map(([k, v]) => `${k}=${v}`);
  let line: string[];
  if (spec.shellAfter) {
    // An assignment's name stays bare (quoted it would be a command).
    const assignments = Object.entries(vars).map(([k, v]) => `${k}=${shQuote(v)}`);
    line = ["sh", "-c", `${[...assignments, ...spec.command.map(shQuote)].join(" ")}; exec sh`];
  } else line = env.length > 0 ? ["env", ...env, ...spec.command] : [...spec.command];
  return outer.length > 0 ? ["env", ...outer, ...line] : line;
}
