// Processes of an e2e run: running commands to completion and starting
// ones that stay (the display server, the terminal, the helpers that drive
// them), all bound to the run's abort signal, and their cleanup. Every
// process that stays is started in a process group of its own and ended
// with its group; cleanups registered with `defer` run last first. The
// scenario wrapper (scenario.ts) runs `cleanup` from the test's `after`;
// `killAllSync` is for an `exit` handler, the last resort.

import { type ChildProcess, execFile, type SpawnOptions, spawn } from "node:child_process";
import { once } from "node:events";
import { closeSync, openSync } from "node:fs";
import { setTimeout as sleep } from "node:timers/promises";

export interface Result {
  code: number;
  stdout: string;
  stderr: string;
}

export interface ExecOptions {
  env?: NodeJS.ProcessEnv;
  cwd?: string;
  /** Standard input. */
  input?: string;
  /** Seconds before the command is killed (it then fails). */
  timeout?: number;
}

export class Procs {
  readonly #children = new Set<ChildProcess>();
  readonly #cleanups: (() => unknown)[] = [];
  #cleaned = false;

  readonly signal: AbortSignal;
  /** The environment of what is run without one given. */
  env: NodeJS.ProcessEnv | undefined;

  constructor(signal: AbortSignal) {
    this.signal = signal;
  }

  /** What commands end on: the run's signal, and once cleanup began a
   * signal of its own (at most 60 s), as the run's has fired by then after a
   * timeout or Ctrl+C and the cleanup still has commands to run (tmux
   * kill-server, the guest's cleanup, the clipboard given back). */
  #cleanupSignal: AbortSignal | undefined;
  get #active(): AbortSignal {
    return this.#cleanupSignal ?? this.signal;
  }

  /** Runs a command to its end: its exit status and output. Throws only
   * when it cannot start or the run is aborted. */
  exec(cmd: string, args: readonly string[], opts: ExecOptions = {}): Promise<Result> {
    this.#active.throwIfAborted();
    return new Promise((resolve, reject) => {
      // Ends at once on an abort, whatever the command left holding its
      // output.
      const onAbort = () => reject(this.#active.reason);
      this.#active.addEventListener("abort", onAbort, { once: true });
      const child = execFile(
        cmd,
        args,
        {
          env: opts.env ?? this.env,
          cwd: opts.cwd,
          signal: this.#active,
          maxBuffer: 256 << 20,
          timeout: opts.timeout ? opts.timeout * 1000 : 0,
          encoding: "utf8",
        },
        (err, stdout, stderr) => {
          this.#active.removeEventListener("abort", onAbort);
          if (this.#active.aborted) return reject(this.#active.reason);
          if (err && typeof err.code !== "number" && !err.killed) return reject(err);
          resolve({ code: err ? (typeof err.code === "number" ? err.code : 1) : 0, stdout, stderr });
        },
      );
      if (opts.input !== undefined) child.stdin?.end(opts.input);
    });
  }

  /** Runs a command to its end with INPUT on its standard input and its
   * output not read: for one that leaves a process behind holding it
   * (xclip and wl-copy stay to serve the clipboard). Its exit status. */
  async feed(cmd: string, args: readonly string[], input: string, env?: NodeJS.ProcessEnv): Promise<number> {
    this.#active.throwIfAborted();
    const child = spawn(cmd, args, { env: env ?? this.env, stdio: ["pipe", "ignore", "ignore"], signal: this.#active });
    child.on("error", () => {});
    child.stdin!.end(input);
    const [code] = (await once(child, "exit", { signal: this.#active })) as [number | null];
    return code ?? 1;
  }

  /** Runs a command to its exit with its output appended to the file LOG,
   * not read: for one that goes into the background holding it (`fcitx5
   * -d`, `ssh -f`; reading its output would wait for that process). Its
   * exit status. */
  async logged(cmd: string, args: readonly string[], log: string, env?: NodeJS.ProcessEnv): Promise<number> {
    this.#active.throwIfAborted();
    const fd = openSync(log, "a");
    let child: ChildProcess;
    try {
      child = spawn(cmd, args, { env: env ?? this.env, stdio: ["ignore", fd, fd], signal: this.#active });
    } finally {
      closeSync(fd);
    }
    child.on("error", () => {});
    const [code] = (await once(child, "exit", { signal: this.#active })) as [number | null];
    return code ?? 1;
  }

  /** As `exec`, but throws when the command fails; its standard output. */
  async run(cmd: string, args: readonly string[], opts: ExecOptions = {}): Promise<string> {
    const r = await this.exec(cmd, args, opts);
    if (r.code !== 0) throw new Error(`${cmd} ${args.join(" ")}: exit ${r.code}: ${r.stderr.trim()}`);
    return r.stdout;
  }

  /** Starts a process that stays, in a group of its own, ended (with its
   * group) by `cleanup`. */
  start(cmd: string, args: readonly string[], opts: SpawnOptions = {}): ChildProcess {
    this.#active.throwIfAborted();
    const child = spawn(cmd, args, { stdio: "ignore", env: this.env, ...opts, detached: true });
    this.#children.add(child);
    child.on("error", () => {});
    return child;
  }

  /** Something to do at cleanup (after the processes started later are ended). */
  defer(fn: () => unknown): void {
    this.#cleanups.push(fn);
  }

  /** Whether a started process is still running. */
  static alive(child: ChildProcess): boolean {
    return child.exitCode === null && child.signalCode === null && child.pid !== undefined;
  }

  /** Until CHILD ends, at most S seconds: whether it did. Not bound to the
   * run's signal (cleanup waits this way after an abort too). */
  static async ended(child: ChildProcess, s: number): Promise<boolean> {
    if (!Procs.alive(child)) return true;
    const timeout = AbortSignal.timeout(s * 1000);
    try {
      await once(child, "exit", { signal: timeout });
      return true;
    } catch {
      return !Procs.alive(child);
    }
  }

  /** Ends CHILD's group: SIGTERM, then SIGKILL after S seconds. */
  async stop(child: ChildProcess, s = 2): Promise<void> {
    if (child.pid !== undefined && Procs.alive(child)) {
      kill(-child.pid, "SIGTERM");
      if (!(await Procs.ended(child, s))) kill(-child.pid, "SIGKILL");
      await Procs.ended(child, 1);
    }
    this.#children.delete(child);
  }

  /** The cleanups (last registered first) interleaved with the processes:
   * each cleanup runs, then every process still running is ended. Errors are
   * reported, not thrown. Runs once. */
  async cleanup(): Promise<void> {
    if (this.#cleaned) return;
    this.#cleaned = true;
    this.#cleanupSignal = AbortSignal.timeout(60_000);
    while (this.#cleanups.length > 0) {
      const fn = this.#cleanups.pop()!;
      try {
        await fn();
      } catch (e) {
        console.error(`e2e: cleanup: ${e instanceof Error ? e.message : e}`);
      }
    }
    await Promise.all([...this.#children].map((c) => this.stop(c)));
  }

  /** Kills every process started, now (for an `exit` handler). */
  killAllSync(): void {
    for (const c of this.#children) if (c.pid !== undefined && Procs.alive(c)) kill(-c.pid, "SIGKILL");
  }
}

function kill(pid: number, sig: NodeJS.Signals): void {
  try {
    process.kill(pid, sig);
  } catch {
    // Gone already.
  }
}

/** Sleeps S seconds, ended by the signal. */
export function pause(s: number, signal: AbortSignal): Promise<void> {
  return sleep(s * 1000, undefined, { signal });
}

/** Until COND holds, checking every STEP seconds, at most LIMIT seconds:
 * whether it did. */
export async function until(
  cond: () => boolean | Promise<boolean>,
  limit: number,
  step: number,
  signal: AbortSignal,
): Promise<boolean> {
  const deadline = performance.now() + limit * 1000;
  for (;;) {
    signal.throwIfAborted();
    if (await cond()) return true;
    if (performance.now() >= deadline) return false;
    await pause(step, signal);
  }
}
