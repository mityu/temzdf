// Starting a run in the Windows VM (scripts/e2e.sh -d windows as it was):
// connecting (SSH, one connection for the run; QMP), the guest's directory
// for the run made anew, temzdf-gui started on the desktop and its window
// dragged to the middle of the screen, and the end of it all. "Windows in
// a VM" in doc/dev/testing.md says why each step is so.

import { existsSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { Lines } from "../lines.ts";
import { magick, size } from "../magick.ts";
import { type Run, StartError, tail } from "../run.ts";
import { Guest, gdir, Qmp, type WinState, windows } from "./windows.ts";

export async function startWindows(r: Run): Promise<void> {
  // COMMAND's temzdf-gui in the guest, and its temzdf.
  r.guiBin = r.argv[0]!.replaceAll("\\", "/");
  r.temzdf = `${path.posix.dirname(r.guiBin)}/temzdf.exe`;
  const cfg = path.join(r.runtimeDir, "ssh_config");
  const sshConfig = await r.procs.exec("scripts/windows-vm.sh", ["ssh-config"]);
  writeFileSync(r.path("guest.log"), sshConfig.stderr);
  if (sshConfig.code !== 0) {
    throw new StartError(
      `e2e: no Windows VM ${process.env.TEMZDF_WINDOWS_VM || "temzdf-windows"} running (scripts/windows-vm.sh start)`,
    );
  }
  writeFileSync(cfg, sshConfig.stdout);
  const host = /^Host\s+(\S+)/m.exec(sshConfig.stdout)?.[1] ?? "";
  // One connection for the run (each SSH handshake takes a while).
  const sshOpts = [
    "-F",
    cfg,
    "-o",
    "ControlMaster=auto",
    "-o",
    `ControlPath=${path.join(r.runtimeDir, "ssh")}`,
    "-o",
    "ControlPersist=yes",
  ];
  await r.procs.run("ssh", [...sshOpts, "-MNf", host]);
  r.procs.defer(() => r.procs.exec("ssh", [...sshOpts, "-O", "exit", host]));
  const qmpSocket = (await r.procs.run("scripts/windows-vm.sh", ["qmp-socket"])).trim();
  const qmp = await Qmp.connect(qmpSocket, r.signal);
  r.procs.defer(() => qmp.close());
  const ps = r.procs.start(
    "ssh",
    ["-T", ...sshOpts, host, "powershell -NoLogo -NoProfile -NonInteractive -Command -"],
    { stdio: ["pipe", "pipe", "ignore"] },
  );
  const guest = new Guest(r.procs, sshOpts, host, new Lines(ps));
  r.win32 = { qmp, guest, screen: { w: 0, h: 0 }, clipFile: path.join(r.runtimeDir, "clip.txt") };
  r.be = windows(r.procs, r.win32);
  // What the run left in the guest: the GUI, its sessions, the tasks; and
  // the guest's logs, into OUTDIR (as on the other displays).
  r.procs.defer(async () => {
    await guest
      .exec(
        `taskkill /F /IM temzdf-gui.exe >$null 2>&1; foreach ($l in @(& '${r.temzdf}' ls 2>$null)) { & '${r.temzdf}' kill-session ($l -split ' ')[0] >$null 2>&1 }; schtasks /delete /tn e2e-gui /f >$null 2>&1; schtasks /delete /tn e2e-clip /f >$null 2>&1`,
      )
      .catch(() => {});
    await fetchLogs(r);
    guest.ps.close();
  });
  // Sessions of the run in its own directories (they end with it), for
  // the GUI (gui.cmd) and the commands here alike.
  await guest.run(
    `[Console]::OutputEncoding = [Text.Encoding]::UTF8; $env:XDG_RUNTIME_DIR = '${gdir}/run'; $env:XDG_STATE_HOME = '${gdir}/state'`,
  );
  const screen = path.join(r.runtimeDir, "screen.png");
  await r.be.shot(screen);
  const s = await size(screen, r.signal);
  r.win32.screen = { w: s.w, h: s.h };
  r.screen = r.win32.screen;
  // What an earlier run left (it ended without its cleanup).
  await guest.run(
    `taskkill /F /IM temzdf-gui.exe >$null 2>&1; if (Test-Path ${gdir}/run) { foreach ($l in @(& '${r.temzdf}' ls 2>$null)) { & '${r.temzdf}' kill-session ($l -split ' ')[0] >$null 2>&1 } }; Remove-Item -Recurse -Force ${gdir} -ErrorAction SilentlyContinue; foreach ($d in 'run', 'state', 'files', 'out') { New-Item -ItemType Directory -Force ${gdir}/$d >$null }; $LASTEXITCODE = 0`,
  );
  const clip = path.join(r.runtimeDir, "clip.ps1");
  writeFileSync(
    clip,
    `# scripts/e2e -d windows: the clipboard of the desktop, run there; get
# writes it to clip.txt, set takes clip-in.txt (UTF-8 without a BOM).
param($what)
$utf8 = New-Object Text.UTF8Encoding $false
if ($what -eq 'get') {
  $t = Get-Clipboard -Raw
  if ($null -eq $t) { $t = '' }
  [IO.File]::WriteAllText('${gdir}/clip.tmp', $t, $utf8)
  Move-Item -Force ${gdir}/clip.tmp ${gdir}/clip.txt
} else {
  Set-Clipboard -Value ([IO.File]::ReadAllText('${gdir}/clip-in.txt', $utf8))
  Set-Content ${gdir}/clip.done ''
}
`,
  );
  await guest.copyIn(clip, `${gdir}/clip.ps1`);
}

/** The guest's logs and what temzdf-gui recorded, into OUTDIR. */
async function fetchLogs(r: Run): Promise<void> {
  const g = r.win32!.guest;
  await g.copyOut(`${gdir}/terminal.log`, r.path("terminal.log")).catch(() => {});
  await g.copyOut(`${gdir}/input.txt`, r.path("input.txt")).catch(() => {});
  await r.procs
    .exec("scp", ["-q", "-r", ...g.sshOpts, `${g.host}:${gdir}/state/temzdf`, r.path("state/")])
    .catch(() => {});
}

/** The GUI on the desktop: a batch file with the run's environment
 * (gui.cmd), started by a scheduled task; COMMAND's arguments that name
 * files here are copied in. The batch file waits for the GUI, then writes
 * its exit status (exit.txt); the terminal's process is an SSH command here
 * waiting for that. Then its window dragged by its title bar to the middle
 * of the screen (which focuses it). */
export async function launchWindows(r: Run): Promise<number> {
  const st = r.win32!;
  const g = st.guest;
  let gargs = "";
  for (const a of r.argv.slice(1)) {
    let arg = a;
    if (existsSync(a)) {
      arg = `${gdir}/files/${path.basename(a)}`;
      await g.copyIn(a, arg);
    }
    gargs += ` "${arg}"`;
  }
  const win = gdir.replaceAll("/", "\\");
  let cmd = "@echo off\r\n";
  cmd += `set XDG_RUNTIME_DIR=${win}\\run\r\nset XDG_STATE_HOME=${win}\\state\r\n`;
  cmd += `set TEMZDF_RECORD_INPUT=${win}\\input.txt\r\nset TEMZDF_GUI_GEOMETRY=${win}\\geometry.txt\r\n`;
  if (process.env.TEMZDF_PIXELS || r.opts.pixels)
    cmd += `set TEMZDF_PIXELS=${r.opts.pixels ?? process.env.TEMZDF_PIXELS}\r\n`;
  cmd += `"${r.guiBin.replaceAll("/", "\\")}"${gargs} > ${win}\\terminal.log 2>&1\r\n`;
  cmd += `echo %ERRORLEVEL%> ${win}\\exit.txt\r\n`;
  const local = path.join(r.runtimeDir, "gui.cmd");
  writeFileSync(local, cmd);
  await g.copyIn(local, `${gdir}/gui.cmd`);
  // The pointer out of the way, where the harness knows it: off the window
  // to come (at the screen's right edge); the actions move it in.
  await r.be.move(st.screen.w - 1, Math.floor(st.screen.h / 2));
  await g.onDesktop("gui", `cmd.exe /c ${win}\\gui.cmd`);
  const waiter = r.procs.start(
    "ssh",
    [...g.sshOpts, g.host, `while (-not (Test-Path ${gdir}/exit.txt)) { Start-Sleep -Milliseconds 200 }`],
    {
      stdio: ["ignore", "ignore", "ignore"],
    },
  );
  r.termPid = waiter.pid ?? 0;
  // The drawing area, once the window is there (it took more than 5 s).
  const geometry = async () => {
    const out = await g.run(`Get-Content ${gdir}/geometry.txt`);
    const [x, y, w, h, s] = out.trim().split(/\s+/).map(Number);
    return { x: x!, y: y!, w: w!, h: h!, s: s! };
  };
  let geo: Awaited<ReturnType<typeof geometry>> | undefined;
  if (await g.wait(`${gdir}/geometry.txt`, 60)) geo = await geometry().catch(() => undefined);
  if (!geo || Number.isNaN(geo.s)) {
    await fetchLogs(r);
    throw new StartError(
      `e2e: no window of temzdf-gui within 60 s; the end of its output:\n${tail(r.path("terminal.log"), 20)}`,
    );
  }
  r.origin = { x: geo.x, y: geo.y };
  r.win = { w: geo.w, h: geo.h, scale: geo.s };
  r.rec.phase("window");
  // Windows opens each new window a little lower and further right than
  // the last, and one a scheduled task opens does not get the focus: its
  // title bar (a little above the drawing area) dragged puts the drawing
  // area in the middle of the screen and gives it the focus.
  const tx = Math.floor((st.screen.w - geo.w) / 2);
  const ty = Math.floor((st.screen.h - geo.h) / 2);
  const gx = geo.x + Math.floor(geo.w / 2);
  const gy = geo.y - Math.floor(10 * geo.s + 0.5);
  await r.be.move(gx, gy);
  await r.be.button("down", 1);
  for (let i = 1; i <= 10; i++) {
    await r.be.move(gx + Math.trunc(((tx - geo.x) * i) / 10), gy + Math.trunc(((ty - geo.y) * i) / 10));
  }
  await r.be.button("up", 1);
  // Away again, off the window (it never was over the drawing area).
  await r.be.move(st.screen.w - 1, Math.floor(st.screen.h / 2));
  for (let i = 0; i < 50; i++) {
    geo = await geometry();
    if (geo.x === tx && geo.y === ty) break;
    await r.sleep(0.1);
  }
  if (geo.x !== tx || geo.y !== ty) {
    throw new StartError(`e2e: the window did not move to ${tx},${ty} (it is at ${geo.x},${geo.y})`);
  }
  r.origin = { x: geo.x, y: geo.y };
  return 5;
}

/** A screenshot of the drawing area only (the desktop around it has a
 * clock): what wait_ready and wait_still watch on Windows. */
export async function watchShot(r: Run, file: string): Promise<void> {
  const full = `${file}.full.png`;
  await r.be.shot(full);
  await magick(
    [full, "-crop", `${r.win.w}x${r.win.h}+${r.origin.x}+${r.origin.y}`, "+repage", "-strip", file],
    r.signal,
  );
  rmSync(full, { force: true });
}

/** `expect` on Windows: temzdf-gui --render-frame in the guest, at the
 * window's scale, its picture copied back. Whether it was. */
export async function renderGuiInGuest(r: Run, name: string): Promise<boolean> {
  const g = r.win32!.guest;
  const session = (await g.run(`& '${r.temzdf}' ls`)).split("\n")[0]?.split(" ")[0] ?? "";
  r.rec.phase(`expect ${name}`);
  const renderer = process.env.E2E_GUI_RENDERER || r.guiBin;
  // PowerShell waits for a GUI program only when its output is piped.
  const ok = (
    await g.exec(
      `& '${renderer}' --render-frame ${gdir}/out/${name}.expected.png -s '${session}' --size ${r.win.w}x${r.win.h} --scale ${r.win.scale} 2>&1 | Out-File -Encoding utf8 ${gdir}/out/${name}.expected.log`,
    )
  ).ok;
  if (
    ok &&
    (await g.copyOut(`${gdir}/out/${name}.expected.png`, r.path(`${name}.expected.png`)).then(
      () => true,
      () => false,
    ))
  ) {
    return true;
  }
  await g.copyOut(`${gdir}/out/${name}.expected.log`, r.path(`${name}.expected.log`)).catch(() => {});
  await r.verdict("FAIL", name, `--render-frame failed (see ${name}.expected.log)`);
  return false;
}

export type { WinState };
