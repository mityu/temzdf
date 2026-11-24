// What a scenario drives its run with (scenario.ts hands it over). Every
// helper works through the run's abort signal: after a timeout or Ctrl+C
// the next one throws instead of sending input, and what waits (a pause, a
// command, a wait for the screen) ends at once. Coordinates are pixels of
// the terminal area, or of the GUI's drawing area; MODS are letters: C
// Ctrl, S Shift, A Alt; key names are XKB names as xdotool takes them
// (`q`, `ctrl+f`, `shift+g`, `Return`).

import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import * as judge from "./judge.ts";
import { magick } from "./magick.ts";
import type { Display, Term } from "./options.ts";
import type { ExecOptions, Result } from "./procs.ts";
import { type Geometry, type Run, Skip } from "./run.ts";

export interface Helpers {
  /** Where screenshots and logs go (absolute). */
  readonly outdir: string;
  readonly term: Term;
  readonly display: Display;
  /** The terminal's (or the GUI's) process. */
  readonly termPid: number;
  /** Where the terminal area starts on the screen. */
  readonly origin: { x: number; y: number };

  /** Saves OUTDIR/NAME.png. */
  shot(name: string): Promise<void>;
  /** N wheel steps (N < 0 scrolls up) at X,Y (default: centre), 30 ms apart
   * (a flick: they accelerate). */
  wheel(n: number, x?: number, y?: number, mods?: string): Promise<void>;
  /** Left-drag vertically from Y1 to Y2. */
  drag(x: number, y1: number, y2: number): Promise<void>;
  /** Left-drag in STEPS moves (default 10), 30 ms apart. */
  dragto(x1: number, y1: number, x2: number, y2: number, steps?: number): Promise<void>;
  /** A click (button 1 by default). */
  click(x: number, y: number, btn?: number, mods?: string): Promise<void>;
  /** Presses a button there (and MODS); `move` drags, `up` releases. */
  down(x: number, y: number, btn?: number, mods?: string): Promise<void>;
  up(btn?: number): Promise<void>;
  /** The pointer to X,Y (hover, or drag while `down`). */
  move(x: number, y: number): Promise<void>;
  /** Keys, one after another (`keys("q")`, `keys("ctrl+b", "percent")`). */
  keys(...keys: string[]): Promise<void>;
  /** Literal text (MS between keys); newlines as Return. */
  type(text: string, ms?: number): Promise<void>;
  /** Keys held across other actions, and their release (not recorded). */
  hold(...keys: string[]): Promise<void>;
  release(...keys: string[]): Promise<void>;
  /** The clipboard's text; undefined when it cannot be read. */
  clipboard(): Promise<string | undefined>;
  setClipboard(text: string): Promise<void>;
  /** Opens a plain window of another program; its PID. */
  otherWindow(): Promise<number>;
  /** Gives PID's window the focus (`termPid`: the terminal's). */
  focusWindow(pid: number): Promise<void>;
  /** Ends a process (SIGTERM). */
  kill(pid: number): void;
  pause(seconds: number): Promise<void>;
  /** Until temzdf in the terminal has written a complete frame (every
   * visible page rendered) since it last showed one that was not, at most S
   * seconds (default 10); nothing for the GUI. */
  waitComplete(seconds?: number): Promise<void>;
  /** Until the screen stands still (three screenshots 0.3 s apart), at
   * most S seconds (default 20; past that FAIL wait_still). */
  waitStill(seconds?: number): Promise<void>;
  /** Saves what the screen shows now (what waitStill watches) for
   * `waitChange(NAME)`. */
  mark(name: string): Promise<void>;
  /** Until the screen is no longer as `mark(NAME)` saw it (screenshots
   * 0.3 s apart), at most S seconds (default 10; past that FAIL
   * wait_change). For something that goes by itself, such as a notice
   * fading: mark while it shows, wait for the change, then waitStill. */
  waitChange(name: string, seconds?: number): Promise<void>;
  /** A SyncTeX forward search from another process (`temzdf --forward
   * LINE[:COLUMN]:FILE PDF`), recorded for `expect` like the input; throws
   * when the command fails (wrap it in `check` to judge it). */
  forward(spec: string, pdf: string): Promise<void>;
  /** (-I) Switches the input method. */
  ime(state: "on" | "off"): Promise<void>;
  /** Saves OUTDIR/NAME.png and judges it (a line of OUTDIR/expect.txt;
   * mismatches in NAME.diff.png and NAME.text.diff):
   *
   * - `expect(NAME, KEYS)`, in the standalone viewer: against the screen
   *   `temzdf standalone pdf --render-frame` draws for the same geometry
   *   after KEYS (Vim feedkeys notation, e.g. "3G" or "/needle<CR>"; the
   *   whole sequence since startup; "" for the first screen).
   * - `expect(NAME)` there: the same after all the input temzdf handled
   *   since startup, replayed with its timing (--render-frame --events of
   *   what temzdf recorded through TEMZDF_RECORD_INPUT, so the terminal's
   *   own delivery times and coordinates count): mouse actions too. Pause
   *   before it until animations end. Mouse actions are not judged inside
   *   multiplexers (they report cells).
   *
   *   Both for COMMAND = temzdf standalone pdf [--page N] [--scale S]
   *   [--config FILE] FILE (the headless side loads that configuration
   *   too; else --clean) and terminals drawing from the window's top-left
   *   (kitty, WezTerm, Ghostty without decorations). The images are
   *   compared by pixels outside the cells the terminal draws itself (the
   *   status line; NAME.mask.png), and in kitty and WezTerm the text too
   *   (NAME.expected.txt).
   * - In a session (`temzdf --clean -s NAME ...`): against `temzdf
   *   render-session` attached to the session at that moment (its only
   *   one, or $E2E_SESSION), the images and the text as above. Not inside
   *   multiplexers.
   * - With the GUI (-t gui): the window against `temzdf-gui --render-frame`
   *   attached to the same session at that moment (the window's drawing,
   *   not the session's input handling).
   *
   * The first two forms and sessions (not inside multiplexers) also write
   * the cells the terminal draws itself as bytes (NAME.ansi) and which of
   * them to judge (NAME.cells.png). After the scenario, temzdf is ended
   * (Esc until the viewer quits; a session's killed) and the shell COMMAND
   * leaves behind (`sh -c '...; exec sh'`) draws each NAME.ansi on a
   * cleared screen (NAME.ref.png, once it stands still): NAME.png inside
   * NAME.cells.png against it is PASS or FAIL NAME.cells (the attributes
   * and colours of those cells, and their text in Ghostty too), SKIP
   * without such a shell or when the terminal changed size since. */
  expect(name: string, keys?: string): Promise<void>;
  /** A judgement of what a screenshot cannot judge: FN returning true (or
   * nothing) passes, false or throwing fails, "skip" or throwing Skip
   * skips (this terminal cannot tell). */
  check(name: string, fn: () => unknown): Promise<void>;
  /** The shell draws LINE... (rows 0, 1, ...) on a cleared screen, saved as
   * OUTDIR/NAME.png: a reference for screenRestored. */
  drawLines(name: string, ...lines: string[]): Promise<void>;
  /** Whether screenshot SHOT, taken after leaving temzdf, shows REF's row
   * K, nothing below it but the prompt, and above it what BEFORE showed up
   * to its row like REF's row BK (or nothing without BEFORE). */
  screenRestored(shot: string, ref: string, k: number, before?: string, bk?: number): Promise<boolean>;
  /** Whether screenshot SHOT shows what `expect REF` saw, outside the
   * cells the terminal draws itself. */
  sameScreen(shot: string, ref: string): Promise<boolean>;
  /** Whether the screen shows TEXT ("skip" where the terminal cannot tell:
   * kitty and WezTerm can, outside multiplexers). */
  screenContains(text: string): Promise<boolean | "skip">;
  /** The terminal's cell grid, as temzdf last logged it. */
  readGeometry(): Geometry | undefined;
  /** Runs a program with arguments (no shell) in the harness's
   * environment; its output. Throws when it fails, which stops the
   * scenario (as a failed command did in an action file), or fails the
   * judgement inside `check`. */
  run(argv: string[], opts?: ExecOptions): Promise<string>;
  /** As `run`, but its exit status and output, whatever the status. */
  exec(argv: string[], opts?: ExecOptions): Promise<Result>;
  /** ImageMagick with arguments; its output. */
  magick(args: string[]): Promise<string>;
  /** Files (relative paths from the repository's root). */
  fs: {
    read(path: string): string;
    exists(path: string): boolean;
    write(path: string, text: string): void;
    copy(from: string, to: string): void;
    rename(from: string, to: string): void;
    remove(path: string): void;
    mkdir(path: string): void;
    /** Sets a file's times to now (`touch`). */
    touch(path: string): void;
    /** FROM's first N bytes into TO (`head -c N FROM > TO`). */
    head(from: string, n: number, to: string): void;
  };
}

export { Skip };

/** The helpers of run R. */
export function helpers(r: Run): Helpers {
  const live = () => r.signal.throwIfAborted();
  return {
    outdir: r.outdir,
    term: r.opts.term,
    display: r.opts.display,
    termPid: r.termPid,
    origin: r.origin,
    shot: (name) => judge.shot(r, name),
    wheel: (n, x, y, mods) => r.wheel(n, x, y, mods),
    drag: (x, y1, y2) => r.dragto(x, y1, x, y2),
    dragto: (x1, y1, x2, y2, steps) => r.dragto(x1, y1, x2, y2, steps),
    click: (x, y, btn, mods) => r.click(x, y, btn, mods),
    down: (x, y, btn, mods) => r.down(x, y, btn, mods),
    up: (btn) => r.up(btn),
    move: (x, y) => r.move(x, y),
    keys: (...keys) => r.keys(keys),
    type: (text, ms) => r.typeText(text, ms),
    async hold(...keys) {
      for (const k of keys) await r.be.keydown(k);
    },
    async release(...keys) {
      for (const k of keys) await r.be.keyup(k);
    },
    clipboard: () => r.be.clipboard(),
    setClipboard: (text) => r.be.setClipboard(text),
    otherWindow: () => r.be.otherWindow(),
    focusWindow: (pid) => r.be.focus(pid),
    kill(pid) {
      live();
      try {
        process.kill(pid);
      } catch {}
    },
    pause: (s) => r.sleep(s),
    waitComplete: (s) => judge.waitComplete(r, s),
    waitStill: (s) => judge.waitStill(r, s),
    mark: (name) => judge.mark(r, name),
    waitChange: (name, s) => judge.waitChange(r, name, s),
    async forward(spec, pdf) {
      await r.procs.run(r.forwardTemzdf, ["--forward", spec, pdf]);
      // Recorded once sent (when temzdf has it), as LINE COLUMN FILE.
      const [line, a, b] = spec.split(":");
      if (b === undefined) r.rec.record(["forward", line, 0, a]);
      else r.rec.record(["forward", line, a, b]);
    },
    async ime(state) {
      await r.procs.exec("fcitx5-remote", [state === "on" ? "-o" : "-c"]);
    },
    expect: (name, keys) => judge.expect(r, name, keys),
    async check(name, fn) {
      live();
      let result: unknown;
      try {
        result = await fn();
      } catch (e) {
        r.signal.throwIfAborted();
        if (e instanceof Skip) result = "skip";
        else return r.verdict("FAIL", name, e instanceof Error ? e.message : String(e));
      }
      if (result === "skip") await r.verdict("SKIP", name, `${r.opts.term} cannot tell`);
      else if (result === false) await r.verdict("FAIL", name, "false");
      else await r.verdict("PASS", name);
    },
    drawLines: (name, ...lines) => judge.drawLines(r, name, lines),
    screenRestored: (shot, ref, k, before, bk) => judge.screenRestored(r, shot, ref, k, before, bk),
    sameScreen: (shot, ref) => judge.sameScreen(r, shot, ref),
    screenContains: (text) => judge.screenContains(r, text),
    readGeometry: () => r.geometry(),
    run: (argv, opts) => r.procs.run(argv[0]!, argv.slice(1), opts),
    exec: (argv, opts) => r.procs.exec(argv[0]!, argv.slice(1), opts),
    magick: (args) => magick(args, r.signal),
    fs: {
      read: (p) => readFileSync(p, "utf8"),
      exists: (p) => existsSync(p),
      write(p, text) {
        live();
        writeFileSync(p, text);
      },
      copy(from, to) {
        live();
        copyFileSync(from, to);
      },
      rename(from, to) {
        live();
        renameSync(from, to);
      },
      remove(p) {
        live();
        rmSync(p, { force: true });
      },
      mkdir(p) {
        live();
        mkdirSync(p, { recursive: true });
      },
      touch(p) {
        live();
        const now = new Date();
        if (existsSync(p)) utimesSync(p, now, now);
        else writeFileSync(p, "");
      },
      head(from, n, to) {
        live();
        // Written in place (the same file, cut short), as the shell's `>`.
        writeFileSync(to, readFileSync(from).subarray(0, n));
      },
    },
  };
}
