//! A watchdog for the loops that run test sessions (`temzdf test`, the
//! server's js_tests): the loop names the step it is in, and a step that
//! lasts too long is reported (which step, for how long) and the process
//! aborted, rather than hanging until CI's timeout. Every step of those
//! loops returns within milliseconds, so one that does not is stuck.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// How often the watchdog looks.
const LOOK: Duration = Duration::from_millis(200);

/// Watches the steps of one loop; stops watching when dropped.
pub struct Watchdog {
    shared: Arc<Shared>,
}

struct Shared {
    steps: &'static [&'static str],
    /// The step now (an index into `steps`).
    step: AtomicUsize,
    /// When it began, in milliseconds since `start`.
    since: AtomicU64,
    start: Instant,
    done: AtomicBool,
}

impl Shared {
    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl Watchdog {
    /// Watch a loop called `what` whose steps are `steps` (it starts in the
    /// first), aborting when one lasts more than `limit`.
    pub fn new(what: String, steps: &'static [&'static str], limit: Duration) -> Self {
        assert!(!steps.is_empty(), "a watchdog needs steps");
        let shared = Arc::new(Shared {
            steps,
            step: AtomicUsize::new(0),
            since: AtomicU64::new(0),
            start: Instant::now(),
            done: AtomicBool::new(false),
        });
        let watched = shared.clone();
        let spawned = std::thread::Builder::new().name("temzdf-watchdog".into()).spawn(move || {
            while !watched.done.load(Ordering::SeqCst) {
                std::thread::sleep(LOOK);
                let since = watched.since.load(Ordering::SeqCst);
                let lasted = Duration::from_millis(watched.elapsed_ms().saturating_sub(since));
                if lasted > limit && !watched.done.load(Ordering::SeqCst) {
                    let step = watched.steps[watched.step.load(Ordering::SeqCst)];
                    // Straight to the process's stderr: a test harness's
                    // capture would go with the process.
                    let _ = writeln!(
                        std::io::stderr(),
                        "watchdog: {what}: stuck in {step} for {:.1} s; aborting",
                        lasted.as_secs_f64()
                    );
                    std::process::abort();
                }
            }
        });
        // Without the thread, nothing is watched (the loop runs as before).
        drop(spawned);
        Self { shared }
    }

    /// The loop is in step `i` of its steps now.
    pub fn step(&self, i: usize) {
        let s = &self.shared;
        s.since.store(s.elapsed_ms(), Ordering::SeqCst);
        s.step.store(i.min(s.steps.len() - 1), Ordering::SeqCst);
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.shared.done.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_within_the_limit_are_left_alone() {
        let w = Watchdog::new("ok".into(), &["a", "b"], Duration::from_millis(500));
        for i in 0..6 {
            w.step(i % 2);
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
