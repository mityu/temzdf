# Checking concurrent code: model checkers, sanitizers, simulation

Question (2026-11-10): which tools find concurrency bugs like the one the
tile schedule had, and can a model be derived from the Rust code itself
and model-checked?

Status: research only, no decision and nothing adopted. Mostly from
memory. Three points were checked on the web on that date: Kani's lack of
thread support, Miri's GenMC mode, and lockbud. The rest needs checking
before it is relied on.

## The bug that prompted it

Fixed in `a6b3219` (Markdown strips) and `eebca8d` (PDF tiles).

- Each frame replaced the worker pool's queue with its new work alone
  (`WorkPool::replace`, `crates/temzdf-scene/src/tiles.rs`).
- Work in flight was not asked for again.
- So a job an earlier frame had queued, but no worker had started yet,
  was dropped. Nothing asked for it until something else redrew. With the
  Markdown strips, the pane stayed blank.
- It happened when the workers were idle while work was queued, as just
  after the threads start.

This is a logic race, not a data race: every access was under the queue's
`Mutex`. Whether work was lost depended only on the order of three kinds of
event:

1. a frame replaces the queue;
2. a worker takes a job;
3. a worker delivers a result.

The fix is `WorkPool::replace_wanted`: a frame names the queued work it
still wants (`Want::Queued`, `Need::Queued`).

## Tools that run the code under a controlled scheduler

| Tool | What it does | Fit here |
|---|---|---|
| loom | Runs a test under every interleaving the C11 memory model allows (bounded, with partial-order reduction); `std::sync` replaced by `loom::sync` under `cfg(loom)` | `WorkPool` itself (queue, `Condvar`, `stop`); tests must stay small (a few threads, a few steps) |
| shuttle (AWS) | The same substitution; random (and PCT) schedules instead of all of them | Larger tests than loom; finds likely bugs, proves nothing |
| Miri | Detects data races and deadlocks where every thread is blocked, and weak-memory effects; `-Zmiri-many-seeds` varies the schedule | Already in `scripts/check.sh`. It would not see this bug: there was no data race, and the test thread waits on a channel, so not every thread is blocked |
| Miri's GenMC mode | Miri runs the program repeatedly until it has explored every execution the Rust memory model allows (GenMC, a stateless model checker) | Promising, but work in progress (its GenMC document, under Sources): Linux hosts only, loops must be bounded by hand, no Stacked/Tree Borrows, `cargo miri test` not supported in some versions |
| ThreadSanitizer (`-Zsanitizer=thread`, nightly) | Data races in executions that actually ran | Fast, but only the schedules that happened; no data race here |

## Tools that derive a model from the code

| Tool | What it does | Fit here |
|---|---|---|
| Kani (AWS) | Bounded model checking of MIR through CBMC: an assertion holds for every input | Single-threaded only: its documentation puts concurrency out of scope and treats atomics as sequential |
| RustMC | GenMC extended to Rust programs (found in the search, not looked into) | To be looked at if GenMC fits |
| Verus, Creusot, Prusti | Deductive verification from annotations (contracts, invariants); Verus has shared-memory concurrency (tokenized state machines) | Can prove it in principle; far too much annotation for this code |
| lockbud, RAPx (static analysis) | Double locks, conflicting lock order, `Condvar` misuse; lockbud also has an atomicity-violation detector (lockbud is tied to one nightly toolchain) | Deadlock-shaped bugs; not lost work |

## The other direction: write a model, check the code against it

Trace validation:

- Write a small specification, for example in TLA+.
- Record the events the real program emits.
- Check that the trace is a behaviour the specification allows.

Microsoft's CCF and etcd's raft use it.

## What fits this code

The session's side is single-threaded, and the bug lay in the order of
the three events above, not in fine-grained interleavings. So
**deterministic simulation testing** fits best. FoundationDB, TigerBeetle
and madsim work this way. The code itself is the model: no `std::sync`
substitution, and every failure replays exactly.

- Make the pool's execution pluggable. In tests, the test steps it by
  hand: "a worker takes a job" and "a worker delivers a result" become
  explicit steps.
- Run sequences of frame, take and deliver steps through the real wiring
  (`claim` in `crates/temzdf-pdfview/src/host.rs`, `md_dispatch` in
  `crates/temzdf-server/src/session/markdown.rs`):
  - every short sequence, exhaustively;
  - longer ones at random (proptest).
- After each sequence, let the work finish and check two things:
  - every tile arrived, and the cache is idle;
  - the next frame needs no tile.

loom or shuttle would then cover `WorkPool` itself. Suggested order:
simulation first, loom on the pool if it is needed.

## Sources

- Miri-GenMC documentation, in the Miri repository
  (<https://rust.googlesource.com/miri/+/HEAD/doc/genmc.md>).
- Kani, Rust feature support:
  <https://model-checking.github.io/kani/rust-feature-support.html>.
- lockbud: <https://github.com/BurtonQin/lockbud>; paper "Understanding
  and Detecting Real-World Safety Issues in Rust" (TSE 2024).
