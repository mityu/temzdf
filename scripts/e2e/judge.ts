// Judging a run's screens: `expect` in its three forms (the standalone
// viewer against `temzdf standalone pdf --render-frame`, a session against
// `temzdf render-session`, the GUI against `temzdf-gui --render-frame`),
// the cells the terminal draws itself against its own drawing of them
// (drawReferences), rows of screenshots (screenRestored), and the waits
// for a screen. Moved from scripts/e2e.sh as it was; its comments say why.

import { copyFileSync, existsSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { temzdfAlive } from "./host.ts";
import { compareAE, cropComposite, geometry, magick } from "./magick.ts";
import { until } from "./procs.ts";
import { type Geometry, type Run, renameOver, sameBytes } from "./run.ts";

/** The text the terminal shows (kitty, WezTerm); "skip" where it cannot tell. */
export async function readScreen(r: Run): Promise<{ text: string } | "skip" | "fail"> {
  if (r.opts.mux) return "skip";
  switch (r.opts.term) {
    case "kitty": {
      const res = await r.procs.exec("kitten", [
        "@",
        "--to",
        `unix:${r.kittySocket}`,
        "get-text",
        "--extent",
        "screen",
      ]);
      return res.code === 0 ? { text: res.stdout } : "fail";
    }
    case "wezterm": {
      if (r.opts.display === "macos") {
        // Started through `open`: its socket is in its own runtime
        // directory, and the CLI cannot tell the focused pane of a window in
        // the background.
        const env = { ...r.env, WEZTERM_UNIX_SOCKET: `${process.env.HOME}/.local/share/wezterm/gui-sock-${r.termPid}` };
        const list = await r.procs.exec("wezterm", ["cli", "list", "--format", "json"], { env });
        if (list.code !== 0) return "fail";
        const pane = (JSON.parse(list.stdout) as { pane_id: number }[])[0]?.pane_id;
        const res = await r.procs.exec("wezterm", ["cli", "get-text", "--pane-id", String(pane)], { env });
        return res.code === 0 ? { text: res.stdout } : "fail";
      }
      // The GUI's own socket: without it the CLI starts a mux server of its own.
      const dir = path.join(r.runtimeDir, "wezterm");
      let socks: string[] = [];
      try {
        socks = readdirSync(dir)
          .filter((f) => f.startsWith("gui-sock-"))
          .sort()
          .map((f) => path.join(dir, f));
      } catch {}
      const res = await r.procs.exec("wezterm", ["cli", "get-text"], {
        env: { ...r.env, WEZTERM_UNIX_SOCKET: socks[0] ?? path.join(dir, "gui-sock-*") },
      });
      return res.code === 0 ? { text: res.stdout } : "fail";
    }
    default:
      return "skip";
  }
}

/** Whether the terminal shows TEXT ("skip" where it cannot tell). */
export async function screenContains(r: Run, text: string): Promise<boolean | "skip"> {
  const s = await readScreen(r);
  if (s === "skip") return "skip";
  if (s === "fail") return false;
  return s.text.includes(text);
}

/** A screen's text without trailing blanks, or blank lines at the end. */
export function screenLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.at(-1) === "") lines.pop();
  const out = lines.map((l) => l.replace(/\s+$/, ""));
  while (out.length > 0 && out.at(-1) === "") out.pop();
  return out;
}

/** Compares the expected and actual text (when the terminal tells it):
 * NAME.text.diff on a mismatch; the failure, if any. */
async function compareText(r: Run, name: string): Promise<string | undefined> {
  const s = await readScreen(r);
  if (s === "skip" || s === "fail") return undefined;
  writeFileSync(r.path(`${name}.actual.txt`), s.text);
  const want = screenLines(readFileSync(r.path(`${name}.expected.txt`), "utf8"));
  const got = screenLines(s.text);
  const a = r.path(`${name}.text.want`);
  const b = r.path(`${name}.text.got`);
  writeFileSync(a, want.map((l) => `${l}\n`).join(""));
  writeFileSync(b, got.map((l) => `${l}\n`).join(""));
  const diff = await r.procs.exec("diff", [a, b]);
  rmSync(a, { force: true });
  rmSync(b, { force: true });
  if (diff.code === 0) {
    rmSync(r.path(`${name}.text.diff`), { force: true });
    return undefined;
  }
  writeFileSync(r.path(`${name}.text.diff`), diff.stdout);
  return `the text differs (see ${name}.text.diff)`;
}

/** Pixels compared: ACTUAL against EXPECTED, the difference into DIFF; the
 * failure, if any. */
async function comparePixels(r: Run, name: string): Promise<string | undefined> {
  let n: number;
  try {
    n = await compareAE(
      r.path(`${name}.actual.png`),
      r.path(`${name}.expected.png`),
      r.path(`${name}.diff.png`),
      r.signal,
    );
  } catch (e) {
    r.signal.throwIfAborted();
    return `the comparison failed: ${e instanceof Error ? e.message : e}`;
  }
  if (n === 0) {
    rmSync(r.path(`${name}.diff.png`), { force: true });
    return undefined;
  }
  return `${n} pixels differ (see ${name}.diff.png)`;
}

/** `expect NAME [KEYS]` (scripts/e2e.sh's header says what each form judges). */
export async function expect(r: Run, name: string, keys?: string): Promise<void> {
  if (r.opts.term === "gui") return expectGui(r, name);
  if (r.sessionMode) return expectSession(r, name);
  let input: string;
  if (keys !== undefined) {
    input = `--keys=${keys}`;
  } else {
    // Everything sent so far, then the moment of the screenshot.
    const events = readLines(r.rec.events);
    if (r.opts.mux && events.some((l) => !/^[0-9]+ (key|forward) /.test(l))) {
      return r.verdict("FAIL", name, `mouse actions are not judged inside ${r.opts.mux}`);
    }
    // temzdf's own record of what it handled (its clock, the terminal's
    // coordinates), ended as long after its last input as the screenshot
    // came after the last one sent.
    const recorded = r.path("input.txt");
    if (!existsSync(recorded)) {
      return r.verdict("FAIL", name, "temzdf recorded no input (TEMZDF_RECORD_INPUT)");
    }
    const now = r.rec.now();
    const sent = Number(events.at(-1)?.split(" ")[0] ?? 0) || 0;
    const last = Number(readLines(recorded).at(-1)?.split(" ")[0] ?? 0) || 0;
    const replay = r.path(`${name}.events.txt`);
    copyFileSync(recorded, replay);
    writeFileSync(replay, `${last + now - sent} end\n`, { flag: "a" });
    input = `--events=${replay}`;
  }
  await shot(r, name);
  const g = r.geometry();
  if (!g) return r.verdict("FAIL", name, "no geometry in temzdf.log");
  r.rec.phase(`expect ${name}`);
  const s = r.standalone!;
  const renderer = process.env.E2E_RENDERER || s.renderer;
  const linear = (r.opts.term === "kitty" || r.opts.term === "ghostty") && !r.opts.mux ? ["--linear-blending"] : [];
  const res = await r.procs.exec(renderer, [
    "standalone",
    "pdf",
    ...s.options,
    ...linear,
    "--render-frame",
    r.path(`${name}.expected.png`),
    "--size",
    `${g.cols}x${g.rows}`,
    "--cell",
    `${g.cw}x${g.ch}`,
    "--text",
    r.path(`${name}.expected.txt`),
    "--mask",
    r.path(`${name}.mask.png`),
    "--ansi",
    r.path(`${name}.ansi`),
    "--cells",
    r.path(`${name}.cells.png`),
    input,
    s.file,
  ]);
  writeFileSync(r.path(`${name}.expected.log`), res.stderr);
  if (res.code !== 0) return r.verdict("FAIL", name, `--render-frame failed (see ${name}.expected.log)`);
  r.rec.phase(`expected ${name}`);
  // Its cells are judged at the end, against this terminal's own drawing.
  if (!r.opts.mux) r.refs.push({ name, ...g });
  await judgeGrid(r, name, g);
}

/** The session's form of expect: the screen against `temzdf
 * render-session` attached to the session now, as a client like the
 * terminal's. */
async function expectSession(r: Run, name: string): Promise<void> {
  if (r.opts.mux) return r.verdict("FAIL", name, `sessions are not judged inside ${r.opts.mux}`);
  await shot(r, name);
  const session = process.env.E2E_SESSION || (await firstSession(r));
  // The backend may come with its settings: "Composited { ... } over Direct".
  const log = readFileSync(r.path("state/temzdf/temzdf.log"), "utf8");
  const m = [
    ...log.matchAll(/geometry ([0-9]+)x([0-9]+) cells of ([0-9]+)x([0-9]+) px with ([A-Za-z]+).* over ([A-Za-z]+)/g),
  ].at(-1);
  if (!m) return r.verdict("FAIL", name, "no geometry in temzdf.log");
  const g: Geometry = { cols: Number(m[1]), rows: Number(m[2]), cw: Number(m[3]), ch: Number(m[4]) };
  const [backend, medium] = [m[5]!, m[6]!];
  // The hints the terminal client sent (TtyFrontend::hints).
  const flags: string[] = [];
  if (backend !== "Layered") flags.push("--exact-scaling");
  if (medium !== "Direct") flags.push("--fast-uploads");
  if (r.opts.term === "kitty" || r.opts.term === "ghostty") flags.push("--linear-blending");
  r.rec.phase(`expect ${name}`);
  const res = await r.procs.exec(process.env.E2E_RENDERER || r.temzdf, [
    "render-session",
    session,
    r.path(`${name}.expected.png`),
    "--size",
    `${g.cols}x${g.rows}`,
    "--cell",
    `${g.cw}x${g.ch}`,
    "--text",
    r.path(`${name}.expected.txt`),
    "--mask",
    r.path(`${name}.mask.png`),
    ...flags,
    "--ansi",
    r.path(`${name}.ansi`),
    "--cells",
    r.path(`${name}.cells.png`),
  ]);
  writeFileSync(r.path(`${name}.expected.log`), res.stderr);
  if (res.code !== 0) return r.verdict("FAIL", name, `render-session failed (see ${name}.expected.log)`);
  r.rec.phase(`expected ${name}`);
  r.refs.push({ name, ...g });
  await judgeGrid(r, name, g);
}

/** The pixels (outside the cells the terminal draws itself: NAME.mask.png)
 * and the text of a screenshot against the expected screen. */
async function judgeGrid(r: Run, name: string, g: Geometry): Promise<void> {
  const grid = { w: g.cols * g.cw, h: g.rows * g.ch, x: r.origin.x, y: r.origin.y };
  // Pixels where the images show: the cells the terminal draws go black on
  // both sides.
  await cropComposite(
    r.path(`${name}.png`),
    grid,
    [r.path(`${name}.mask.png`)],
    r.path(`${name}.actual.png`),
    r.signal,
  );
  await asTerminalShows(r, r.path(`${name}.expected.png`));
  await magick(
    [r.path(`${name}.expected.png`), r.path(`${name}.mask.png`), "-composite", r.path(`${name}.expected.png`)],
    r.signal,
  );
  await r.maskWindowEdges(name, grid.w, grid.h);
  const failed = [await comparePixels(r, name), await compareText(r, name)].filter(Boolean);
  if (failed.length === 0) await r.verdict("PASS", name);
  else await r.verdict("FAIL", name, failed.join("; "));
}

/** The GUI's form of expect: the window against an offscreen render of
 * the same session at this moment. */
async function expectGui(r: Run, name: string): Promise<void> {
  await shot(r, name);
  if (r.opts.display === "windows") {
    if (!(await r.renderGuiInGuest(name))) return;
  } else {
    const session = await firstSession(r);
    r.rec.phase(`expect ${name}`);
    const res = await r.procs.exec(
      process.env.E2E_GUI_RENDERER || r.guiBin,
      ["--render-frame", r.path(`${name}.expected.png`), "-s", session, "--size", `${r.win.w}x${r.win.h}`],
      { env: { ...r.cleanEnv, VK_DRIVER_FILES: process.env.TEMZDF_LAVAPIPE_ICD ?? "" } },
    );
    writeFileSync(r.path(`${name}.expected.log`), res.stderr);
    if (res.code !== 0) return r.verdict("FAIL", name, `--render-frame failed (see ${name}.expected.log)`);
  }
  r.rec.phase(`expected ${name}`);
  await magick(
    [
      r.path(`${name}.png`),
      "-crop",
      geometry({ w: r.win.w, h: r.win.h, x: r.origin.x, y: r.origin.y }),
      "+repage",
      r.path(`${name}.actual.png`),
    ],
    r.signal,
  );
  await r.maskWindowEdges(name, r.win.w, r.win.h);
  await maskPointer(r, name);
  const failed = await comparePixels(r, name);
  if (failed) await r.verdict("FAIL", name, failed);
  else await r.verdict("PASS", name);
}

/** The run's only session (or its first). */
async function firstSession(r: Run): Promise<string> {
  const ls = await r.procs.exec(r.temzdf, ["ls"]);
  return ls.stdout.split("\n")[0]?.split(" ")[0] ?? "";
}

/** Where the screenshots show the pointer and the harness knows where it
 * is (the Windows backend): a square around it goes black on both sides of
 * NAME's comparison, 48 px at scale 1 with the hot spot 16 px in from its
 * top-left (Windows' standard pointers fit). */
async function maskPointer(r: Run, name: string): Promise<void> {
  const p = r.be.pointer?.();
  if (!p) return;
  const x = p.x - r.origin.x;
  const y = p.y - r.origin.y;
  const a = Math.ceil(16 * r.win.scale - 1e-9);
  const b = Math.ceil(32 * r.win.scale - 1e-9);
  if (!(x + b > 0 && y + b > 0 && x - a < r.win.w && y - a < r.win.h)) return;
  for (const f of ["actual", "expected"]) {
    const file = r.path(`${name}.${f}.png`);
    await magick(
      [file, "-fill", "black", "-draw", `rectangle ${x - a},${y - a} ${x + b - 1},${y + b - 1}`, file],
      r.signal,
    );
  }
}

/** Ghostty on macOS shows images' pixels as Display P3 values: the
 * expected screen goes through the same (doc/terminals/terminal-compat.md). */
async function asTerminalShows(r: Run, file: string): Promise<void> {
  if (!(r.opts.display === "macos" && r.opts.term === "ghostty")) return;
  await magick(
    [
      file,
      "-strip",
      "-profile",
      "/System/Library/ColorSync/Profiles/Display P3.icc",
      "-profile",
      "/System/Library/ColorSync/Profiles/sRGB Profile.icc",
      "-strip",
      file,
    ],
    r.signal,
  );
}

/** `shot NAME`: OUTDIR/NAME.png. */
export function shot(r: Run, name: string): Promise<void> {
  return r.be.shot(r.path(`${name}.png`));
}

function readLines(file: string): string[] {
  try {
    return readFileSync(file, "utf8").split("\n").filter(Boolean);
  } catch {
    return [];
  }
}

/** Until temzdf in the terminal has written a complete frame since it last
 * showed one that was not, at most LIMIT seconds. */
export async function waitComplete(r: Run, limit = 10): Promise<void> {
  if (!r.temzdfInTerminal) return;
  const log = r.path("state/temzdf/temzdf.log");
  const done = await until(
    () => {
      const lines = readLines(log).filter((l) => /temzdf: (incomplete frame shown|complete frame written)/.test(l));
      return /complete frame written/.test(lines.at(-1) ?? "");
    },
    limit,
    0.05,
    r.signal,
  );
  if (done) {
    r.rec.phase("complete");
  } else {
    console.error(`e2e: no complete frame written within ${limit}s`);
    r.rec.phase("complete (timed out)");
  }
}

/** Until the screen stands still (unchanged across three screenshots 0.3 s
 * apart), at most LIMIT seconds; past that a FAIL wait_still line. */
export async function waitStill(r: Run, limit = 20): Promise<void> {
  const deadline = performance.now() + limit * 1000;
  const a = r.path("still.a.png");
  const b = r.path("still.b.png");
  let still = 0;
  await r.watchShot(a);
  while (performance.now() < deadline) {
    await r.sleep(0.3);
    await r.watchShot(b);
    if (sameBytes(a, b)) {
      if (++still === 2) {
        rmSync(a, { force: true });
        rmSync(b, { force: true });
        r.rec.phase("still");
        return;
      }
    } else {
      still = 0;
    }
    renameOver(b, a);
  }
  rmSync(a, { force: true });
  rmSync(b, { force: true });
  r.rec.phase("still (timed out)");
  // Not thrown: the line fails the run at its end.
  await r.verdict("FAIL", "wait_still", `the screen did not stand still within ${limit}s`);
}

/** Until FILE exists, at most S seconds. */
function waitFile(r: Run, file: string, s: number): Promise<boolean> {
  return until(() => existsSync(file), s, 0.05, r.signal);
}

/** Judges each expect's cells once the actions are done: ends temzdf, has
 * the shell left behind draw NAME.ansi on a cleared screen, and compares
 * the screenshot taken at the expect with that (NAME.cells). */
export async function drawReferences(r: Run): Promise<void> {
  if (r.refs.length === 0) return;
  if (!r.hasShell) {
    for (const ref of r.refs) {
      await r.verdict("SKIP", `${ref.name}.cells`, "no shell after the command (sh -c '...; exec sh')");
    }
    return;
  }
  r.rec.phase("references");
  const alive = () => temzdfAlive(r.outdir, r.procs);
  if (r.sessionMode) {
    const ls = await r.procs.exec(r.temzdf, ["ls"]);
    for (const line of ls.stdout.split("\n").filter(Boolean)) {
      await r.procs.exec(r.temzdf, ["kill-session", line.split(" ")[0]!]);
    }
  } else {
    // Esc closes the input line, the sidebar, the selection and the
    // search, then the pane (the viewer ends).
    outer: for (let i = 0; i < 6; i++) {
      if (!(await alive())) break;
      await r.be.key("Escape");
      for (let k = 0; k < 10; k++) {
        if (!(await alive())) break outer;
        await r.sleep(0.05);
      }
    }
  }
  for (let i = 0; i < 100 && (await alive()); i++) await r.sleep(0.05);
  if (await alive()) {
    for (const ref of r.refs) await r.verdict("FAIL", `${ref.name}.cells`, "temzdf did not end");
    return;
  }
  // Short names to type: the shell runs in the run's directory, and `r
  // NAME` draws NAME.ansi on a cleared screen, then says so (NAME.ok), and
  // waits (no prompt over the cells until the next one).
  let dir = r.outdir;
  if (dir.startsWith(`${process.cwd()}/`)) dir = dir.slice(process.cwd().length + 1);
  rmSync(r.path("shell.ok"), { force: true });
  await r.typeText(`cd ${dir} && r() { clear; cat $1.ansi; : >$1.ok; read x; } && : >shell.ok\n`);
  if (!(await waitFile(r, r.path("shell.ok"), 10))) {
    await shot(r, "no-shell");
    for (const ref of r.refs) await r.verdict("FAIL", `${ref.name}.cells`, "the shell did not answer");
    return;
  }
  for (const ref of r.refs) {
    const { name } = ref;
    // The terminal's grid now: another size cannot draw the same cells.
    const g = r.geometry();
    if (!g || `${g.cols} ${g.rows} ${g.cw} ${g.ch}` !== `${ref.cols} ${ref.rows} ${ref.cw} ${ref.ch}`) {
      await r.verdict("SKIP", `${name}.cells`, "the terminal changed size since");
      continue;
    }
    // Pixels of the cells compared (opaque in the mask).
    const cells = Number(
      (
        await magick(
          [r.path(`${name}.cells.png`), "-alpha", "extract", "-format", "%[fx:round(mean*w*h)]", "info:"],
          r.signal,
        )
      ).trim(),
    );
    if (!(cells > 0)) continue;
    r.rec.phase(`reference ${name}`);
    rmSync(r.path(`${name}.ok`), { force: true });
    await r.typeText(`r ${name}\n`);
    if (!(await waitFile(r, r.path(`${name}.ok`), 10))) {
      await r.verdict("FAIL", `${name}.cells`, `the shell did not draw ${name}.ansi`);
      continue;
    }
    await waitStill(r, 10);
    await shot(r, `${name}.ref`);
    r.rec.phase(`referenced ${name}`);
    await r.be.key("Return");
    // Both inside the cells compared (elsewhere black), from the grid's
    // top-left, as expect crops.
    const grid = geometry({ w: ref.cols * ref.cw, h: ref.rows * ref.ch, x: r.origin.x, y: r.origin.y });
    for (const i of [name, `${name}.ref`]) {
      const tmp = r.path(`${i}.cells.png.tmp.png`);
      await magick(
        [
          r.path(`${i}.png`),
          "-crop",
          grid,
          "+repage",
          "(",
          r.path(`${name}.cells.png`),
          "-alpha",
          "extract",
          ")",
          "-compose",
          "multiply",
          "-composite",
          tmp,
        ],
        r.signal,
      );
      const wmask = r.path(`${name}.window-mask.png`);
      if (existsSync(wmask)) await magick([tmp, wmask, "-compose", "over", "-composite", tmp], r.signal);
    }
    renameSync(r.path(`${name}.cells.png.tmp.png`), r.path(`${name}.cells.actual.png`));
    renameSync(r.path(`${name}.ref.cells.png.tmp.png`), r.path(`${name}.cells.ref.png`));
    let n: number | string;
    try {
      n = await compareAE(
        r.path(`${name}.cells.actual.png`),
        r.path(`${name}.cells.ref.png`),
        r.path(`${name}.cells.diff.png`),
        r.signal,
      );
    } catch (e) {
      r.signal.throwIfAborted();
      n = `the comparison failed (${e instanceof Error ? e.message : e});`;
    }
    if (n === 0) {
      rmSync(r.path(`${name}.cells.diff.png`), { force: true });
      await r.verdict("PASS", `${name}.cells`, `(${Math.floor(cells / (ref.cw * ref.ch))} cells)`);
    } else {
      await r.verdict(
        "FAIL",
        `${name}.cells`,
        `${n} pixels differ from this terminal's drawing of ${name}.ansi (see ${name}.cells.diff.png)`,
      );
    }
  }
}

/** Cell row ROW of screenshot NAME into FILE (on macOS the window's edges
 * masked: rowMask). */
async function cellRow(r: Run, g: Geometry, name: string, row: number, file: string): Promise<void> {
  const mask = r.rowMask ? [r.rowMask, "-composite"] : [];
  await magick(
    [
      r.path(`${name}.png`),
      "-crop",
      geometry({ w: g.cols * g.cw, h: g.rows * g.ch, x: r.origin.x, y: r.origin.y }),
      "+repage",
      ...mask,
      "-crop",
      geometry({ w: g.cols * g.cw, h: g.ch, x: 0, y: row * g.ch }),
      "+repage",
      "-strip",
      file,
    ],
    r.signal,
  );
}

/** Whether two images are the same, as expect compares. */
async function sameImage(r: Run, a: string, b: string): Promise<boolean> {
  try {
    return (await compareAE(a, b, "null:", r.signal)) === 0;
  } catch {
    r.signal.throwIfAborted();
    return false;
  }
}

async function rowOf(r: Run, g: Geometry, shotName: string, ref: string, k: number): Promise<number | undefined> {
  const want = r.path(".row-want.png");
  const got = r.path(".row-got.png");
  await cellRow(r, g, ref, k, want);
  for (let row = 0; row < g.rows; row++) {
    await cellRow(r, g, shotName, row, got);
    if (await sameImage(r, want, got)) return row;
  }
  return undefined;
}

/** Whether screenshot SHOT, taken after leaving temzdf, shows REF's row K
 * (the message), nothing below it but the prompt, and above it what BEFORE
 * showed up to its row like REF's row BK, or nothing without BEFORE. */
export async function screenRestored(
  r: Run,
  shotName: string,
  ref: string,
  k: number,
  before?: string,
  bk?: number,
): Promise<boolean> {
  const g = r.geometry();
  if (!g) return false;
  if (r.opts.display === "macos" && !r.rowMask) {
    // Not .rows: screencapture writes no file whose name starts with a dot.
    await r.windowEdgesMask("rows", g.cols * g.cw, g.rows * g.ch);
    r.rowMask = r.path("rows.window-mask.png");
  }
  const a = r.path(".row-a.png");
  const b = r.path(".row-b.png");
  const blank = r.path(".row-blank.png");
  const m = await rowOf(r, g, shotName, ref, k);
  if (m === undefined) {
    console.error(`screen_restored: no row of ${shotName}.png is ${ref}.png's row ${k}`);
    return false;
  }
  // The reference's last row: a row of nothing in this terminal.
  await cellRow(r, g, ref, g.rows - 1, blank);
  if (before !== undefined) {
    const mb = await rowOf(r, g, before, ref, bk ?? 0);
    if (mb === undefined) {
      console.error(`screen_restored: no row of ${before}.png is ${ref}.png's row ${bk}`);
      return false;
    }
    for (let row = 0; row <= mb; row++) {
      await cellRow(r, g, before, row, a);
      await cellRow(r, g, shotName, row, b);
      if (!(await sameImage(r, a, b))) {
        console.error(`screen_restored: row ${row} of ${shotName}.png is not as in ${before}.png`);
        return false;
      }
    }
  } else {
    // Empty above the message (temzdf was the first command).
    for (let row = 0; row < m; row++) {
      await cellRow(r, g, shotName, row, b);
      if (!(await sameImage(r, blank, b))) {
        console.error(`screen_restored: row ${row} of ${shotName}.png is not empty`);
        return false;
      }
    }
  }
  // Below the message and the prompt after it: nothing.
  for (let row = m + 2; row < g.rows; row++) {
    await cellRow(r, g, shotName, row, b);
    if (!(await sameImage(r, blank, b))) {
      console.error(`screen_restored: row ${row} of ${shotName}.png is not empty`);
      return false;
    }
  }
  return true;
}

/** Whether screenshot SHOT shows what `expect REF` saw (REF.actual.png),
 * outside REF's mask (and the window's edges on macOS). */
export async function sameScreen(r: Run, shotName: string, ref: string): Promise<boolean> {
  const g = r.geometry();
  if (!g) return false;
  if (!existsSync(r.path(`${ref}.actual.png`)) || !existsSync(r.path(`${ref}.mask.png`))) return false;
  const actual = r.path(`${shotName}.actual.png`);
  await cropComposite(
    r.path(`${shotName}.png`),
    { w: g.cols * g.cw, h: g.rows * g.ch, x: r.origin.x, y: r.origin.y },
    [r.path(`${ref}.mask.png`)],
    actual,
    r.signal,
  );
  const wmask = r.path(`${ref}.window-mask.png`);
  if (existsSync(wmask)) await magick([actual, wmask, "-composite", actual], r.signal);
  // The comparison goes to SHOT.diff.png, kept on a mismatch.
  const diff = r.path(`${shotName}.diff.png`);
  let n: number;
  try {
    n = await compareAE(actual, r.path(`${ref}.actual.png`), diff, r.signal);
  } catch {
    r.signal.throwIfAborted();
    return false;
  }
  if (n === 0) rmSync(diff, { force: true });
  return n === 0;
}

/** `draw_lines NAME LINE...`: the shell draws the lines (rows 0, 1, ...),
 * a reference for screenRestored. */
export async function drawLines(r: Run, name: string, lines: string[]): Promise<void> {
  let cmd = "clear; printf '%s\\n'";
  for (const line of lines) cmd += ` '${line}'`;
  await r.typeText(`${cmd}\n`);
  await r.sleep(1);
  await shot(r, name);
}
