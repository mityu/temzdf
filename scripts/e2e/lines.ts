// A helper process spoken to in lines: a command a line on its standard
// input, an answer a line on its output (wlinput, `termctl serve`, the
// guest's PowerShell). One command at a time.

import type { ChildProcess } from "node:child_process";
import { appendFileSync } from "node:fs";
import { createInterface, type Interface } from "node:readline";

/** E2E_INPUT_LOG: when each command to an input helper (wlinput, termctl,
 * QMP) went out, for comparing the timing of input. */
export function inputLog(who: string, line: string): void {
  const log = process.env.E2E_INPUT_LOG;
  if (log) appendFileSync(log, `${Date.now() / 1000} ${who} ${line}\n`);
}

export class Lines {
  readonly #child: ChildProcess;
  readonly #rl: Interface;
  readonly #queue: string[] = [];
  #waiting: ((line: string | undefined) => void) | undefined;
  #closed = false;

  constructor(child: ChildProcess) {
    this.#child = child;
    this.#rl = createInterface({ input: child.stdout!, crlfDelay: Number.POSITIVE_INFINITY });
    this.#rl.on("line", (line) => {
      if (this.#waiting) {
        const w = this.#waiting;
        this.#waiting = undefined;
        w(line);
      } else {
        this.#queue.push(line);
      }
    });
    this.#rl.on("close", () => {
      this.#closed = true;
      this.#waiting?.(undefined);
      this.#waiting = undefined;
    });
  }

  send(line: string): void {
    this.#child.stdin!.write(`${line}\n`);
  }

  /** The next line, or undefined when the output ended or S seconds passed
   * (or the signal fired, which throws). */
  next(s: number, signal: AbortSignal): Promise<string | undefined> {
    signal.throwIfAborted();
    const queued = this.#queue.shift();
    if (queued !== undefined) return Promise.resolve(queued);
    if (this.#closed) return Promise.resolve(undefined);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#waiting = undefined;
        resolve(undefined);
      }, s * 1000);
      const abort = () => {
        clearTimeout(timer);
        this.#waiting = undefined;
        reject(signal.reason);
      };
      signal.addEventListener("abort", abort, { once: true });
      this.#waiting = (line) => {
        clearTimeout(timer);
        signal.removeEventListener("abort", abort);
        resolve(line);
      };
    });
  }

  /** Sends a command and expects `ok` (wlinput, termctl). */
  async ok(line: string, who: string, signal: AbortSignal): Promise<void> {
    // Nothing goes out after an abort.
    signal.throwIfAborted();
    inputLog(who, line);
    this.send(line);
    const answer = await this.next(30, signal);
    if (answer !== "ok") throw new Error(`${who}: ${answer ?? "no answer"} (${line})`);
  }

  close(): void {
    this.#child.stdin?.end();
  }
}
