// What a run writes as it goes: the input sent (OUTDIR/events.txt, in
// `--render-frame --events` form, for comparing with what temzdf recorded
// and for `expect`'s replay), when each phase began (OUTDIR/timing.txt),
// and the judgements (OUTDIR/expect.txt: PASS, FAIL, SKIP lines).

import { appendFileSync, writeFileSync } from "node:fs";
import path from "node:path";

export type Verdict = "PASS" | "FAIL" | "SKIP";

export class Records {
  readonly events: string;
  readonly timing: string;
  readonly expect: string;
  #t0 = performance.now();
  #timing0 = performance.now();

  constructor(outdir: string) {
    this.events = path.join(outdir, "events.txt");
    this.timing = path.join(outdir, "timing.txt");
    this.expect = path.join(outdir, "expect.txt");
    writeFileSync(this.timing, "");
  }

  /** Milliseconds since the recorded times start (`startEvents`). */
  now(): number {
    return Math.floor(performance.now() - this.#t0);
  }

  /** The recorded times start here (the first screen is up). */
  startEvents(): void {
    writeFileSync(this.events, "");
    this.#t0 = performance.now();
  }

  /** WHAT at the time given (milliseconds since the start) or now. */
  record(what: (string | number | undefined)[], at?: number): void {
    // (Absent words are left out; an empty one stays, as `echo "$*"` kept it.)
    const words = what.filter((w) => w !== undefined);
    appendFileSync(this.events, `${at ?? this.now()} ${words.join(" ")}\n`);
  }

  phase(what: string): void {
    appendFileSync(this.timing, `${Math.floor(performance.now() - this.#timing0)} ${what}\n`);
  }

  /** A judgement's line, in OUTDIR/expect.txt and on the output. */
  verdict(line: string): void {
    appendFileSync(this.expect, `${line}\n`);
    console.log(line);
  }
}
