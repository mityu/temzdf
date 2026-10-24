//! Kinetic scrolling for discrete wheel input.
//!
//! Terminals deliver wheel input as discrete steps. Each step is treated as an
//! impulse that moves a target offset; the displayed offset eases towards the
//! target with exponential decay, so motion looks continuous. Rapid steps in the
//! same direction accelerate (flicks). See `doc/input.md`.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct ScrollConfig {
    /// Pixels per wheel step before acceleration.
    pub step: f64,
    /// Time constant of the easing: after this long, ~63% of the remaining
    /// distance is covered.
    pub tau: Duration,
    /// Steps closer together than this count as one flick.
    pub flick_window: Duration,
    /// Extra step multiplier per consecutive flick step.
    pub acceleration: f64,
    /// Cap on the multiplier.
    pub max_multiplier: f64,
}

impl Default for ScrollConfig {
    fn default() -> Self {
        Self {
            step: 60.0,
            tau: Duration::from_millis(60),
            flick_window: Duration::from_millis(80),
            acceleration: 0.2,
            max_multiplier: 4.0,
        }
    }
}

/// One scroll axis.
#[derive(Clone, Debug)]
pub struct KineticScroll {
    pub config: ScrollConfig,
    offset: f64,
    target: f64,
    min: f64,
    max: f64,
    last_tick: Option<Instant>,
    last_step: Option<(Instant, f64)>,
    streak: u32,
}

impl KineticScroll {
    pub fn new(config: ScrollConfig) -> Self {
        Self {
            config,
            offset: 0.0,
            target: 0.0,
            min: 0.0,
            max: f64::INFINITY,
            last_tick: None,
            last_step: None,
            streak: 0,
        }
    }

    /// Current (displayed) offset.
    pub fn offset(&self) -> f64 {
        self.offset
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    /// Limit the offset to `min..=max` (e.g. document height minus viewport).
    pub fn set_bounds(&mut self, min: f64, max: f64) {
        self.min = min;
        self.max = max.max(min);
        self.target = self.target.clamp(self.min, self.max);
        self.offset = self.offset.clamp(self.min, self.max);
    }

    /// One wheel step: `direction` is +1.0 (down/right) or -1.0 (up/left).
    pub fn wheel(&mut self, direction: f64, now: Instant) {
        let c = self.config;
        match self.last_step {
            Some((t, d)) if d.signum() == direction.signum() && now.duration_since(t) <= c.flick_window => {
                self.streak += 1;
            }
            Some((_, d)) if d.signum() != direction.signum() => {
                // Reversal: drop the distance still pending in the old direction.
                self.target = self.offset;
                self.streak = 0;
            }
            _ => self.streak = 0,
        }
        let multiplier = (1.0 + self.streak as f64 * c.acceleration).min(c.max_multiplier);
        self.last_step = Some((now, direction));
        self.scroll_by(direction * c.step * multiplier, now);
    }

    /// Animate by `delta` pixels.
    pub fn scroll_by(&mut self, delta: f64, now: Instant) {
        if !self.is_animating() {
            self.last_tick = Some(now);
        }
        self.target = (self.target + delta).clamp(self.min, self.max);
    }

    /// Move immediately, without animation (dragging, jumps).
    pub fn jump_to(&mut self, offset: f64) {
        self.offset = offset.clamp(self.min, self.max);
        self.target = self.offset;
        self.last_tick = None;
    }

    pub fn is_animating(&self) -> bool {
        self.offset != self.target
    }

    /// Advance the animation to `now`. Returns true if the offset changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.is_animating() {
            self.last_tick = None;
            return false;
        }
        let dt = self.last_tick.map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
        self.last_tick = Some(now);
        let k = 1.0 - (-dt.as_secs_f64() / self.config.tau.as_secs_f64()).exp();
        let before = self.offset;
        self.offset += (self.target - self.offset) * k;
        if (self.target - self.offset).abs() < 0.5 {
            self.offset = self.target;
        }
        self.offset != before
    }
}

impl Default for KineticScroll {
    fn default() -> Self {
        Self::new(ScrollConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn eases_to_target_and_stops() {
        let t0 = Instant::now();
        let mut s = KineticScroll::default();
        s.wheel(1.0, t0);
        assert_eq!(s.target(), 60.0);
        let mut last = 0.0;
        for i in 1..=60 {
            s.tick(ms(t0, i * 16));
            assert!(s.offset() >= last, "monotonic");
            last = s.offset();
        }
        assert_eq!(s.offset(), 60.0);
        assert!(!s.is_animating());
        assert!(!s.tick(ms(t0, 2000)));
    }

    #[test]
    fn flick_accelerates_and_reversal_resets() {
        let t0 = Instant::now();
        let mut s = KineticScroll::default();
        for i in 0..5 {
            s.wheel(1.0, ms(t0, i * 20));
        }
        // 60 * (1 + 1.2 + 1.4 + 1.6 + 1.8)
        assert!((s.target() - 420.0).abs() < 1e-9);
        s.tick(ms(t0, 120));
        let here = s.offset();
        s.wheel(-1.0, ms(t0, 130));
        assert!((s.target() - (here - 60.0)).abs() < 1e-9);
    }

    #[test]
    fn respects_bounds() {
        let t0 = Instant::now();
        let mut s = KineticScroll::default();
        s.set_bounds(0.0, 100.0);
        s.wheel(-1.0, t0);
        assert_eq!(s.target(), 0.0);
        s.jump_to(500.0);
        assert_eq!(s.offset(), 100.0);
    }
}
