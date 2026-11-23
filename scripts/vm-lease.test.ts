// The hand-held VM lock (scripts/lib/vm-lease.sh) and its use in
// scripts/windows-vm.sh. Never touches a VM or the real lock: every test has
// its own directory and two made-up holders, a and b.

import assert from "node:assert/strict";
import { type ChildProcess, spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";

const root = resolve(import.meta.dirname, "..");
// The VM scripts run on a Unix host (the gate runs these tests on Windows
// too): there, no signals to send a process group and no running a script
// by its shebang.
const notOnWindows = process.platform === "win32" ? "signals and shebangs: Unix only" : false;

interface Env {
  dir: string;
  lock: string;
  env: (holder: string) => NodeJS.ProcessEnv;
}

function fresh(): Env {
  const dir = mkdtempSync(join(tmpdir(), "vm-lease-"));
  const lock = join(dir, "state", "vm.lock");
  const env = (holder: string): NodeJS.ProcessEnv => ({
    ...process.env,
    TEMZDF_VM_LOCK: lock,
    TEMZDF_VM_HOLDER: holder,
    TEMZDF_VM_LOCK_POLL: "0.1",
    TEMZDF_WINDOWS_STATE: join(dir, "windows"),
  });
  return { dir, lock, env };
}

// `bash -c SCRIPT` with the library sourced, as holder `holder`.
function lib(e: Env, holder: string, script: string, ...args: string[]) {
  return spawnSync("bash", ["-c", `source scripts/lib/vm-lease.sh; ${script}`, "_", ...args], {
    cwd: root,
    env: e.env(holder),
    encoding: "utf8",
  });
}

const cli = (e: Env, holder: string, ...args: string[]) => lib(e, holder, 'lease_cli "$@"', ...args);

function withDir(fn: (e: Env) => void | Promise<void>) {
  return async () => {
    const e = fresh();
    try {
      await fn(e);
    } finally {
      rmSync(e.dir, { recursive: true, force: true });
    }
  };
}

const lockA = ["lock", "--as", "alice", "--why", "windows e2e"];

test(
  "a lock held by another holder is refused and named",
  withDir((e) => {
    assert.equal(cli(e, "a", ...lockA).status, 0);
    const r = cli(e, "b", "lock", "--as", "bob", "--why", "other");
    assert.equal(r.status, 3);
    assert.match(r.stderr, /alice/);
    assert.match(r.stderr, /windows e2e/);
    assert.match(r.stderr, /unlock --force/);
    // Taking it again is fine and leaves it as it was.
    const before = readFileSync(e.lock, "utf8");
    assert.equal(cli(e, "a", "lock", "--as", "other-name", "--why", "again").status, 0);
    assert.equal(readFileSync(e.lock, "utf8"), before);
  }),
);

test(
  "unlock by another holder is refused, unlock --force is not",
  withDir((e) => {
    cli(e, "a", ...lockA);
    assert.equal(cli(e, "b", "unlock").status, 3);
    assert.ok(existsSync(e.lock));
    assert.equal(cli(e, "b", "unlock", "--force").status, 0);
    assert.ok(!existsSync(e.lock));
    assert.equal(cli(e, "b", "unlock").status, 0);
  }),
);

test(
  "unlock by the holder removes the lock and no temporary file stays",
  withDir((e) => {
    cli(e, "a", ...lockA);
    assert.equal(cli(e, "a", "unlock").status, 0);
    assert.deepEqual(readdirSync(join(e.dir, "state")), []);
  }),
);

test(
  "hold releases the lock after its command and passes the status on",
  withDir((e) => {
    const ok = cli(e, "a", "hold", "--as", "alice", "--why", "x", "--", "true");
    assert.equal(ok.status, 0);
    assert.ok(!existsSync(e.lock));
    const bad = cli(e, "a", "hold", "--as", "alice", "--why", "x", "--", "false");
    assert.notEqual(bad.status, 0);
    assert.ok(!existsSync(e.lock));
    // The command runs under the lock.
    const r = cli(e, "a", "hold", "--as", "alice", "--why", "x", "--", "bash", "-c", 'test -e "$TEMZDF_VM_LOCK"');
    assert.equal(r.status, 0);
  }),
);

test(
  "hold inside a held lock keeps it; hold against another's lock is refused",
  withDir((e) => {
    cli(e, "a", ...lockA);
    assert.equal(cli(e, "a", "hold", "--as", "alice", "--why", "x", "--", "true").status, 0);
    assert.ok(existsSync(e.lock));
    const r = cli(e, "b", "hold", "--as", "bob", "--why", "x", "--", "touch", join(e.dir, "ran"));
    assert.equal(r.status, 3);
    assert.ok(!existsSync(join(e.dir, "ran")));
  }),
);

test(
  "hold releases the lock when sent TERM",
  { skip: notOnWindows },
  withDir(async (e) => {
    // Its own process group, so that the command gets the signal as well, as
    // with Ctrl+C.
    const child: ChildProcess = spawn(
      "bash",
      ["-c", "source scripts/lib/vm-lease.sh; lease_cli hold --as alice --why x -- sleep 30"],
      { cwd: root, env: e.env("a"), detached: true, stdio: "ignore" },
    );
    const done = new Promise<void>((r) => child.on("exit", () => r()));
    for (let i = 0; i < 100 && !existsSync(e.lock); i++) await new Promise((r) => setTimeout(r, 50));
    assert.ok(existsSync(e.lock));
    await new Promise((r) => setTimeout(r, 300));
    process.kill(-(child.pid as number), "SIGTERM");
    await done;
    // The subshell releases it after the outer shell is gone.
    for (let i = 0; i < 100 && existsSync(e.lock); i++) await new Promise((r) => setTimeout(r, 50));
    assert.ok(!existsSync(e.lock));
  }),
);

test(
  "lock --wait takes the lock once the holder releases it",
  withDir(async (e) => {
    cli(e, "a", ...lockA);
    const b = spawn("bash", ["-c", "source scripts/lib/vm-lease.sh; lease_cli lock --as bob --why next --wait"], {
      cwd: root,
      env: e.env("b"),
      stdio: ["ignore", "ignore", "pipe"],
    });
    let err = "";
    b.stderr?.on("data", (d) => {
      err += d;
    });
    const done = new Promise<number | null>((r) => b.on("exit", (c) => r(c)));
    await new Promise((r) => setTimeout(r, 500));
    assert.match(err, /alice/);
    assert.match(err, /waiting/);
    cli(e, "a", "unlock");
    assert.equal(await done, 0);
    assert.match(readFileSync(e.lock, "utf8"), /^name=bob$/m);
    assert.equal(cli(e, "b", "unlock").status, 0);
  }),
);

test(
  "lease_require passes for the holder only",
  withDir((e) => {
    const req = (h: string) => lib(e, h, "lease_require");
    const none = req("a");
    assert.equal(none.status, 3);
    assert.match(none.stderr, /lock --as NAME --why TEXT/);
    assert.match(none.stderr, /hold --as NAME --why TEXT -- CMD/);
    cli(e, "a", ...lockA);
    assert.equal(req("a").status, 0);
    const other = req("b");
    assert.equal(other.status, 3);
    assert.match(other.stderr, /alice/);
  }),
);

test(
  "lease_status says free, the holder, and (you)",
  withDir((e) => {
    assert.match(lib(e, "a", "lease_status").stdout, /free/);
    cli(e, "a", ...lockA);
    const mine = lib(e, "a", "lease_status").stdout;
    assert.match(mine, /alice: windows e2e/);
    assert.match(mine, /\(you\)/);
    assert.doesNotMatch(lib(e, "b", "lease_status").stdout, /\(you\)/);
  }),
);

test(
  "the lock does not contain the worktree path",
  withDir((e) => {
    // The id comes from the worktree itself here.
    const env = { ...e.env("unused") };
    delete env.TEMZDF_VM_HOLDER;
    const r = spawnSync("bash", ["-c", 'source scripts/lib/vm-lease.sh; lease_cli "$@"', "_", ...lockA], {
      cwd: root,
      env,
      encoding: "utf8",
    });
    assert.equal(r.status, 0);
    const text = readFileSync(e.lock, "utf8");
    assert.ok(!text.includes(root));
    assert.match(text, /^id=[0-9a-f]{16}$/m);
  }),
);

test(
  "windows-vm.sh refuses VM commands without the lock and creates nothing",
  { skip: notOnWindows },
  withDir((e) => {
    for (const args of [["run", "x"], ["ssh-config"], ["start"], ["qmp-socket"]]) {
      const r = spawnSync("scripts/windows-vm.sh", args, { cwd: root, env: e.env("a"), encoding: "utf8" });
      assert.equal(r.status, 3, `${args.join(" ")}: ${r.stderr}`);
      assert.match(r.stderr, /lock --as NAME --why TEXT/);
    }
    assert.ok(!existsSync(join(e.dir, "windows")));
  }),
);

test(
  "windows-vm.sh status says the lock is free",
  { skip: notOnWindows },
  withDir((e) => {
    const r = spawnSync("scripts/windows-vm.sh", ["status"], { cwd: root, env: e.env("a"), encoding: "utf8" });
    assert.match(r.stdout, /^lock: free$/m);
  }),
);
