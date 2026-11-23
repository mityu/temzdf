// ImageMagick (`magick`) for the e2e harness: the shapes the judging uses,
// typed, and anything else as raw arguments. Every call takes the run's
// abort signal (a timeout or Ctrl+C ends the process).

import { execFile } from "node:child_process";

export interface Rect {
  w: number;
  h: number;
  x: number;
  y: number;
}

/** `WxH+X+Y`, as -crop takes it. */
export const geometry = (r: Rect): string => `${r.w}x${r.h}+${r.x}+${r.y}`;

/** Runs `magick ARGS`; its standard output. Throws when it fails. */
export function magick(args: string[], signal?: AbortSignal): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile("magick", args, { signal, maxBuffer: 64 << 20 }, (err, stdout, stderr) => {
      if (err) reject(new Error(`magick ${args.join(" ")}: ${stderr.trim() || err.message}`));
      else resolve(stdout);
    });
  });
}

/** IN cropped to R (and +repage), then composited with each of OVER in
 * turn (default compose: their opaque pixels over it), into OUT. */
export async function cropComposite(
  input: string,
  r: Rect,
  over: string[],
  out: string,
  signal?: AbortSignal,
): Promise<void> {
  await magick([input, "-crop", geometry(r), "+repage", ...over.flatMap((o) => [o, "-composite"]), out], signal);
}

/** IN with each of OVER composited over it in turn, into OUT (OUT may be IN). */
export async function composite(input: string, over: string[], out: string, signal?: AbortSignal): Promise<void> {
  await magick([input, ...over.flatMap((o) => [o, "-composite"]), out], signal);
}

/** The result of `magick compare -metric AE -fuzz 3%`: how many pixels
 * differ. Exit 1 is "they differ" (the count is on standard error), exit 2
 * a failure (missing file, different sizes), which throws. */
export function compareAE(a: string, b: string, diff: string, signal?: AbortSignal): Promise<number> {
  return new Promise((resolve, reject) => {
    execFile(
      "magick",
      ["compare", "-quiet", "-metric", "AE", "-fuzz", "3%", a, b, diff],
      { signal },
      (err, _stdout, stderr) => {
        const code = err ? (typeof err.code === "number" ? err.code : -1) : 0;
        // The count (possibly "12 (0.0001)") leads its standard error.
        const n = Number.parseFloat(stderr.trim().split(/\s+/)[0] ?? "");
        if ((code === 0 || code === 1) && Number.isFinite(n)) resolve(n);
        else reject(new Error(`magick compare ${a} ${b}: ${stderr.trim() || err?.message}`));
      },
    );
  });
}

/** `magick identify -format FORMAT FILE`. */
export async function identify(format: string, file: string, signal?: AbortSignal): Promise<string> {
  return magick(["identify", "-format", format, file], signal);
}

/** Width and height of an image. */
export async function size(file: string, signal?: AbortSignal): Promise<{ w: number; h: number }> {
  const [w, h] = (await identify("%w %h", file, signal)).trim().split(" ").map(Number);
  return { w: w!, h: h! };
}

/** How many colours an image has (`%k`): 1 for a screen of one colour. */
export async function colours(file: string, signal?: AbortSignal): Promise<number> {
  return Number((await magick([file, "-format", "%k", "info:"], signal)).trim());
}

/** A gradient image of W x H from one colour to another (`gradient:red-blue`). */
export async function gradient(w: number, h: number, from: string, to: string, out: string, signal?: AbortSignal) {
  await magick(["-size", `${w}x${h}`, `gradient:${from}-${to}`, out], signal);
}
