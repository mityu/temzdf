// `temzdf/process`: running commands and finding them, asynchronously.
// The server does it natively (crates/temzdf-script/src/native_process.rs),
// each command on a thread of its own: no shell (`argv[0]` is the command,
// found on PATH as `exepath` finds it), output of at most 64 MiB each,
// killed when the configuration is loaded again or the session ends.
// Await them in a dispatched task, or in waitResolved's for keys to wait.

/** Why a command could not run or was stopped, by class. A relative `cwd`
 * throws a `TypeError` instead, an aborted `signal` its reason. */
export class ProcessError extends Error {}
/** The command is not on PATH (nor where its name says). */
class NotFound extends ProcessError {
  override name = "NotFound";
}
class PermissionDenied extends ProcessError {
  override name = "PermissionDenied";
}
/** It ran longer than its `timeout` (and was killed). */
class TimedOut extends ProcessError {
  override name = "TimedOut";
}
/** It wrote more than 64 MiB to stdout or stderr (and was killed). */
class TooLarge extends ProcessError {
  override name = "TooLarge";
}
class Other extends ProcessError {
  override name = "Other";
}

/** The error classes: `e instanceof errors.NotFound`. */
export const errors = { NotFound, PermissionDenied, TimedOut, TooLarge, Other } as const;

export interface RunOptions {
  /** Where it runs, an absolute path (default: the session server's working directory). */
  cwd?: string;
  /** Variables added to the server's environment (a `PATH` here is where the command is looked for). */
  env?: Record<string, string>;
  /** Written to its standard input, then closed (default: an empty input). */
  stdin?: string | Uint8Array;
  /** Milliseconds after which it is killed and the call fails with `errors.TimedOut`. */
  timeout?: number;
  /** Aborting it kills the command and rejects with the signal's reason. */
  signal?: AbortSignal;
}

/** How a command ended, and what it wrote. */
export interface RunResult {
  /** Its exit code; undefined when a signal ended it. */
  code: number | undefined;
  /** The signal that ended it (Unix), else undefined. */
  signal: number | undefined;
  /** Whether it exited with 0. */
  success: boolean;
  /** Its output as bytes: `new TextDecoder().decode(r.stdout)` for text. */
  stdout: Uint8Array;
  stderr: Uint8Array;
}

type Native = {
  exepath(name: string): Promise<unknown>;
  run(spec: string, stdin: Uint8Array | undefined): { id: number; done: Promise<unknown> };
  kill(id: number): void;
};
const native = (globalThis as unknown as { __temzdfProcess: Native }).__temzdfProcess;

/** A rejection of the server's (`{ kind, message }` as JSON) as its class. */
function failure(name: string, e: unknown): unknown {
  if (typeof e !== "string") return e;
  const { kind, message } = JSON.parse(e) as { kind: string; message: string };
  if (kind === "NotAbsolute") return new TypeError(`${name}: ${message}`);
  const C: typeof ProcessError = (errors as Record<string, typeof ProcessError>)[kind] ?? Other;
  return new C(`${name}: ${message}`);
}

function name(fn: string, value: unknown): string {
  if (typeof value !== "string") throw new TypeError(`${fn}: name: expected a string, got ${typeof value}`);
  return value;
}

/** Where command `name` is, as `run` would find it: its absolute path, or
 * undefined (Vim's `exepath()`). A name with a directory in it is that file;
 * on Windows PATHEXT's extensions are tried. */
export async function exepath(command: string): Promise<string | undefined> {
  try {
    return (JSON.parse((await native.exepath(name("exepath", command))) as string) as string | null) ?? undefined;
  } catch (e) {
    throw failure("exepath", e);
  }
}

/** Whether command `name` can be run (Vim's `executable()`). */
export async function executable(command: string): Promise<boolean> {
  return (await exepath(name("executable", command))) !== undefined;
}

/** Run `argv` (the command, then its arguments; no shell) to its end. A
 * command that fails is not an error: see `code` and `success`. */
export async function run(argv: readonly string[], options: RunOptions = {}): Promise<RunResult> {
  if (!Array.isArray(argv) || argv.length === 0 || !argv.every((a) => typeof a === "string")) {
    throw new TypeError("run: argv: expected a non-empty list of strings");
  }
  const { cwd, env = {}, stdin, timeout, signal } = options ?? {};
  if (cwd !== undefined && typeof cwd !== "string") throw new TypeError("run: cwd: expected a string");
  if (typeof env !== "object" || env === null || !Object.values(env).every((v) => typeof v === "string")) {
    throw new TypeError("run: env: expected { name: value } of strings");
  }
  if (timeout !== undefined && !(typeof timeout === "number" && timeout >= 0 && Number.isFinite(timeout))) {
    throw new TypeError("run: timeout: expected milliseconds");
  }
  let input: Uint8Array | undefined;
  if (typeof stdin === "string") input = new TextEncoder().encode(stdin);
  else if (stdin instanceof Uint8Array) input = stdin;
  else if (stdin !== undefined) throw new TypeError("run: stdin: expected a string or a Uint8Array");
  signal?.throwIfAborted();
  const spec = {
    program: argv[0],
    args: argv.slice(1),
    cwd,
    env: Object.entries(env),
    timeoutMs: timeout === undefined ? undefined : Math.ceil(timeout),
  };
  const started = native.run(JSON.stringify(spec), input);
  let onAbort: (() => void) | undefined;
  const aborted = new Promise<never>((_, reject) => {
    onAbort = () => {
      native.kill(started.id);
      reject(signal?.reason);
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
  try {
    const raw = (await Promise.race([started.done, aborted])) as {
      code?: number | null;
      signal?: number | null;
      stdout: ArrayBuffer;
      stderr: ArrayBuffer;
    };
    const code = raw.code ?? undefined;
    return {
      code,
      signal: raw.signal ?? undefined,
      success: code === 0,
      stdout: new Uint8Array(raw.stdout),
      stderr: new Uint8Array(raw.stderr),
    };
  } catch (e) {
    throw signal?.aborted ? e : failure("run", e);
  } finally {
    if (onAbort) signal?.removeEventListener("abort", onAbort);
  }
}
