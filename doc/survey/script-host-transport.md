# Script host round trips: three forms measured

Measured 2026-11-07 for the text editor framework
(`doc/design/text-editor.md`), where editors are scripts and one key can
issue tens of primitive commands, plus synchronous events. Question: what
does a command cost between a mapped JavaScript function and the server,
for the script host as it is and two in-process alternatives, also on a
constrained CPU? Code: `experiments/script-host-bench/`.

## Forms

- **A, today**: the real script host (`temzdf_script::run`, what
  `temzdf script-host` runs) in a child process over pipes. The bench plays
  the server as `crates/temzdf-server/src/session/script.rs` does: a reader
  thread feeding a channel, replies written from the main thread, postcard
  framing (`temzdf_protocol::send`/`recv`). The shipped defaults are loaded
  as in a session.
- **B**: `rquickjs` in the server's process: `__host.request` is a native
  function.
- **C**: QuickJS (`rquickjs` built for `wasm32-wasip1`) in `wasmtime` 50 in
  the server's process, `__host.request` a host import; Cranelift, epoch
  interruption on (a ticker every 10 ms, the deadline callback continuing).

All three run the same JavaScript shape as `crates/temzdf-script/src/prelude.js`:
a command stringifies its token and payload to JSON, the native side parses
them and answers with JSON (`"normal"`, as `current-mode`), and the reply is
`JSON.parse`d. B and C leave out the server's command dispatch, so they are
lower bounds; A includes everything up to the server's reply.

One **key** is one call of a mapped function issuing `k` commands (1, 10,
50), timed from the call to its end; 200 warm-up keys, then 2000 (1000 in
the repeats). Every form checks that the server side served exactly `k`
commands per key.

## Running it

```sh
nix-shell experiments/script-host-bench/shell.nix --run experiments/script-host-bench/run.sh
```

`shell.nix` brings a Rust toolchain with `wasm32-wasip1` and nixpkgs' WASI
clang (`RQUICKJS_SYS_NO_WASI_SDK=1`, so rquickjs-sys does not download the
WASI SDK). It is not pinned, as the numbers depend on the machine anyway;
and it must not use `builtins.getFlake` on the repository, which copies the
directory, `target/` included (115 GB here), into the store on every
evaluation (this filled the disk once). `run.sh` writes
`target/script-host-bench/results.csv`. Conditions use `systemd-run --user
--scope` (cgroup v2, cpu controller delegated, no root).

## Machine

i7-14700 (8 P-cores with two threads each: CPUs 0-15; 12 E-cores: CPUs
16-27), Linux 6.18.60, governor `powersave` with
`balance_performance`. Frequency scaling makes repeated runs differ by up to
two or three times (below: medians of one full run, then the spread of
three repeats); the orders of magnitude are stable.

## Results

Median per key in µs (p99 in parentheses), from the full run:

| Condition | Form | k = 1 | k = 10 | k = 50 |
|---|---|---|---|---|
| no limit | A | 19 (30) | 124 (169) | 578 (937) |
| | B | 5 (6) | 21 (23) | 102 (108) |
| | C | 6 (8) | 49 (54) | 240 (254) |
| one P-core | A | 23 (68) | 157 (324) | 731 (2529) |
| | B | 3 (3) | 21 (24) | 107 (138) |
| | C | 18 (27) | 150 (170) | 731 (763) |
| one E-core | A | 61 (1918) | 340 (4836) | 1495 (31573) |
| | B | 4 (4) | 29 (32) | 128 (142) |
| | C | 17 (25) | 143 (154) | 723 (777) |
| one P-core, 50% quota | A | 44 (783) | 229 (2467) | 628 (48202) |
| | B | 5 (6) | 21 (40) | 103 (148) |
| | C | 6 (7) | 47 (82) | 227 (495) |
| one P-core, 25% quota | A | 23 (57) | 103 (244) | 768 (76426) |
| | B | 3 (5) | 21 (36) | 103 (203) |
| | C | 17 (31) | 141 (385) | 712 (75848) |

Repeats (no limit and one P-core, three each), median at k = 50: A 528-1573,
B 104-314, C 225-255. Per command that is about **A 11-30 µs, B 2-6 µs,
C 4.5-15 µs** (C's 15 in some runs of the full table, 4.5-5 in all
repeats, apparently the clock).

Under a quota the maxima of every form reach 47-82 ms: the cgroup throttles
for the rest of each 100 ms period once the quota is spent, whatever the
form. A, taking longer per key, meets it more often (its p99).

Other figures (full run):

- Plain JavaScript (a 10⁶-iteration loop with string work): B 88-378 ms, C
  68-588 ms across conditions; within the noise of each other. QuickJS in
  WebAssembly under Cranelift is not markedly slower than native here.
- Startup: A 6-21 ms (process start and loading the defaults), B 0.3-0.5 ms,
  C 0.5-1.3 ms after compiling the module, which takes **1.2-2.0 s** (7.9 s
  at 25% quota) with Cranelift.

## Compiled or interpreted WebAssembly (added 2026-11-07)

Form C compiles the guest (QuickJS, the interpreter of the scripts) to
machine code with Cranelift once, when the module is loaded; the scripts
themselves are interpreted by QuickJS either way. **D** runs the same guest
on wasmtime's Pulley interpreter instead (`Config::target("pulley64")`): the
guest is compiled to Pulley bytecode, which wasmtime interprets. A second
full run (`--forms abcd`, same conditions), median per key in µs (p99):

| Condition | Form | k = 1 | k = 10 | k = 50 | 10⁶-loop JS |
|---|---|---|---|---|---|
| no limit | C | 6 (6) | 47 (50) | 226 (231) | 55 ms |
| | D | 185 (222) | 1539 (1734) | 7631 (7984) | 656 ms |
| one P-core | C | 6 (6) | 46 (50) | 227 (233) | 55 ms |
| | D | 185 (216) | 1539 (1733) | 7640 (7977) | 657 ms |
| one E-core | C | 5 (6) | 46 (49) | 226 (231) | 55 ms |
| | D | 185 (226) | 1543 (1738) | 7629 (8002) | 656 ms |
| one P-core, 50% | C | 6 (7) | 46 (224) | 226 (1108) | 116 ms |
| | D | 190 (1017) | 1597 (55814) | 7902 (72158) | 1698 ms |
| one P-core, 25% | C | 6 (40) | 49 (361) | 258 (75638) | 307 ms |
| | D | 202 (75364) | 3562 (82879) | 94020 (102504) | — |

(A and B in this run, k = 50 median: A 595-2468 µs, B 102-143 µs. The
E-core rows match the P-core ones in this run, unlike the first; the clock
again.)

- Interpreting costs about **30 times** per key and **12 times** for plain
  JavaScript: an interpreter (Pulley) running an interpreter (QuickJS).
  At k = 50 a key takes 7.6 ms unconstrained, half a 60 Hz frame, and
  94 ms at a 25% quota.
- Pulley does not save the startup either: the guest still goes through
  Cranelift (to Pulley bytecode, 0.8 s here), and instantiating took
  4.6 ms against C's 0.5 ms.
- Precompiling C at build time keeps C's per-key numbers (the same machine
  code is produced) and removes the compile at startup: loading a
  precompiled module took 0.3 ms, and a binary with wasmtime's runtime but
  no Cranelift grew by about 2.8 MB plus the 3.1 MB precompiled guest
  (measured with stripped release builds; Cranelift in the binary would add
  about 13.5 MB instead).
- The sandbox does not depend on interpreting: Cranelift's code keeps
  WebAssembly's rules (bounds via guard pages or checks, indirect calls
  through typed tables, stack limits), and faults come back as traps.
  Precompiled modules must come from the build itself (loading one is
  `unsafe`: it is machine code).

## Reading

- A key issuing 50 commands costs at most about 1.5 ms at the median in
  every form and condition, a tenth of a 60 Hz frame. The transport does
  not rule out the framework's design (many primitives per key, three
  synchronous events per key: `InvokePre`, `TextChanged`, `InvokePost`).
- **A** (today) is the slowest and the most sensitive: one core or a slow
  core makes the server and the host take turns (process switches), and its
  tails grow (E-core p99 at k = 50: 32 ms). It is still within budget for
  ordinary keys.
- **B** is the fastest (5-10 times A per command) but gives up the
  isolation the separate process was chosen for: a crash in QuickJS's C
  code takes the session, with its terminals, down.
- **C** is 2-3 times faster than A and keeps isolation inside the process:
  faults in the guest are traps returned as errors, endless loops stop
  through epoch interruption, memory can be capped; what it cannot contain
  is a bug in wasmtime itself or a panic in the server's own host
  functions. Its costs: compiling the module (1-2 s, so it must be cached or
  precompiled, e.g. `Module::serialize`), the WASI C toolchain in the build,
  and wasmtime as a large dependency.
- A CPU quota hurts all forms alike (throttling stalls of tens of
  milliseconds); no transport helps there.

No decision yet; this is the input for choosing the script host's form when
the editor framework is designed.
