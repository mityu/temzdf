//! Where a view is on content larger than itself, and how scrolling input
//! moves it: the part every pane drawing a document of its own (PDF pages,
//! a Markdown page) shares. The content's size is the pane's to say (its
//! layout, its zoom); zooming is too (a PDF scales, a Markdown page lays
//! itself out again), so the viewport only reports a zoom asked for.
//!
//! Both axes are [`KineticScroll`]s: wheel steps ease and accelerate,
//! amounts from fingers move the view exactly. See `doc/terminals/input.md`.

use std::time::Instant;

use crate::input::{Modifiers, MouseEvent, MouseEventKind, ScrollDirection, ScrollPhase, ScrollUnit};
use crate::scroll::{KineticScroll, ScrollConfig};

/// What a scrolling event did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// The view moved, or is moving.
    Scrolled,
    /// Ctrl and the wheel: zoom this many steps (in when positive).
    Zoom(i32),
    /// Not scrolling, or nothing to do with it.
    Ignored,
}

impl Input {
    /// Combine two outcomes: zooms add up, a scroll beats nothing.
    fn and(self, other: Input) -> Input {
        match (self, other) {
            (Input::Zoom(a), Input::Zoom(b)) => Input::Zoom(a + b),
            (z @ Input::Zoom(_), _) | (_, z @ Input::Zoom(_)) => z,
            (Input::Scrolled, _) | (_, Input::Scrolled) => Input::Scrolled,
            _ => Input::Ignored,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Viewport {
    pub x: KineticScroll,
    pub y: KineticScroll,
    /// The view's size (pixels).
    view: (f64, f64),
    /// The content's size (pixels, at its zoom).
    content: (f64, f64),
    /// Wheel notches not yet a whole step.
    rest: (f64, f64),
}

impl Viewport {
    pub fn new(config: ScrollConfig) -> Self {
        Self { x: KineticScroll::new(config), y: KineticScroll::new(config), ..Self::default() }
    }

    pub fn set_config(&mut self, config: ScrollConfig) {
        self.x.config = config;
        self.y.config = config;
    }

    pub fn config(&self) -> ScrollConfig {
        self.y.config
    }

    pub fn view_size(&self) -> (f64, f64) {
        self.view
    }

    pub fn content_size(&self) -> (f64, f64) {
        self.content
    }

    /// The view's size: the scrolling limits follow, a scroll on its way
    /// goes on (clamped).
    pub fn set_view(&mut self, w: f64, h: f64) {
        self.view = (w, h);
        self.update_bounds();
    }

    /// The content's size, as [`Viewport::set_view`].
    pub fn set_content(&mut self, w: f64, h: f64) {
        self.content = (w, h);
        self.update_bounds();
    }

    fn update_bounds(&mut self) {
        self.x.set_bounds(0.0, (self.content.0 - self.view.0).max(0.0));
        self.y.set_bounds(0.0, (self.content.1 - self.view.1).max(0.0));
    }

    /// Where the view is (its top-left in the content), as shown now.
    pub fn offset(&self) -> (f64, f64) {
        (self.x.offset(), self.y.offset())
    }

    /// Where the view is going.
    pub fn target(&self) -> (f64, f64) {
        (self.x.target(), self.y.target())
    }

    /// Move there now (clamped), ending any animation.
    pub fn jump_to(&mut self, x: f64, y: f64) {
        self.x.jump_to(x);
        self.y.jump_to(y);
    }

    /// Move by an amount, animated.
    pub fn scroll_by(&mut self, dx: f64, dy: f64, now: Instant) {
        if dx != 0.0 {
            self.x.scroll_by(dx, now);
        }
        if dy != 0.0 {
            self.y.scroll_by(dy, now);
        }
    }

    pub fn is_animating(&self) -> bool {
        self.x.is_animating() || self.y.is_animating()
    }

    /// Advance animations to `now`. Whether the view moved.
    pub fn tick(&mut self, now: Instant) -> bool {
        let a = self.x.tick(now);
        let b = self.y.tick(now);
        a || b
    }

    /// The content's rows `y0..y1` in sight: where they are if they are,
    /// else a third of the view from the top.
    pub fn reveal(&mut self, y0: f64, y1: f64, animate: bool, now: Instant) {
        let top = self.y.target();
        if y0 >= top && y1 <= top + self.view.1 {
            return;
        }
        let to = y0 - self.view.1 / 3.0;
        if animate {
            self.y.scroll_by(to - top, now);
        } else {
            self.y.jump_to(to);
        }
    }

    /// One wheel step. Ctrl asks to zoom (up is in); Shift turns the
    /// vertical wheel sideways.
    pub fn wheel(&mut self, dir: ScrollDirection, mods: Modifiers, now: Instant) -> Input {
        if mods.contains(Modifiers::CTRL) {
            return match dir {
                ScrollDirection::Up => Input::Zoom(1),
                ScrollDirection::Down => Input::Zoom(-1),
                _ => Input::Ignored,
            };
        }
        let shift = mods.contains(Modifiers::SHIFT);
        match (dir, shift) {
            (ScrollDirection::Down, false) => self.y.wheel(1.0, now),
            (ScrollDirection::Up, false) => self.y.wheel(-1.0, now),
            (ScrollDirection::Right, _) | (ScrollDirection::Down, true) => self.x.wheel(1.0, now),
            (ScrollDirection::Left, _) | (ScrollDirection::Up, true) => self.x.wheel(-1.0, now),
        }
        Input::Scrolled
    }

    /// An amount as whole wheel notches (down and right positive), the
    /// fractions kept for the next one. Pixels count as notches of `row`
    /// pixels.
    pub fn notches(&mut self, dx: f64, dy: f64, unit: ScrollUnit, row: f64) -> (i32, i32) {
        let (lx, ly) = match unit {
            ScrollUnit::Line => (dx, dy),
            ScrollUnit::Pixel => (dx / row.max(1.0), dy / row.max(1.0)),
        };
        self.rest.0 += lx;
        self.rest.1 += ly;
        let whole = |rest: &mut f64| {
            let n = rest.trunc();
            *rest -= n;
            n as i32
        };
        (whole(&mut self.rest.0), whole(&mut self.rest.1))
    }

    /// Pixels to move by: a wheel's ease, fingers' and the system's
    /// inertia's are followed exactly (the system has its own easing).
    /// Shift turns vertical pixels sideways.
    pub fn pixels(&mut self, dx: f64, dy: f64, phase: ScrollPhase, mods: Modifiers, now: Instant) -> Input {
        let (dx, dy) = if mods.contains(Modifiers::SHIFT) && dx == 0.0 { (dy, 0.0) } else { (dx, dy) };
        if phase == ScrollPhase::Wheel {
            self.x.scroll_by(dx, now);
            self.y.scroll_by(dy, now);
        } else {
            let (x, y) = self.offset();
            self.jump_to(x + dx, y + dy);
        }
        Input::Scrolled
    }

    /// Any scrolling event, for views with nothing else under the
    /// pointer: the wheel, notches as wheel steps (Ctrl's pixels too, so
    /// they zoom by steps), pixels as [`Viewport::pixels`]. `row` is the
    /// pixels of a notch for pixels turned into notches.
    pub fn input(&mut self, m: &MouseEvent, row: f64, now: Instant) -> Input {
        match m.kind {
            MouseEventKind::Scroll(dir) => self.wheel(dir, m.modifiers, now),
            MouseEventKind::ScrollBy { dx, dy, unit, phase } => {
                let (dx, dy) = (f64::from(dx) / 100.0, f64::from(dy) / 100.0);
                if unit == ScrollUnit::Line || m.modifiers.contains(Modifiers::CTRL) {
                    let (nx, ny) = self.notches(dx, dy, unit, row);
                    let mut out = Input::Ignored;
                    for (n, pos, neg) in [
                        (ny, ScrollDirection::Down, ScrollDirection::Up),
                        (nx, ScrollDirection::Right, ScrollDirection::Left),
                    ] {
                        let dir = if n > 0 { pos } else { neg };
                        for _ in 0..n.unsigned_abs() {
                            out = out.and(self.wheel(dir, m.modifiers, now));
                        }
                    }
                    out
                } else {
                    self.pixels(dx, dy, phase, m.modifiers, now)
                }
            }
            _ => Input::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::input::CoordUnit;

    fn view() -> Viewport {
        let mut v = Viewport::new(ScrollConfig::default());
        v.set_view(800.0, 600.0);
        v.set_content(1000.0, 5000.0);
        v
    }

    fn ev(kind: MouseEventKind, modifiers: Modifiers) -> MouseEvent {
        MouseEvent { kind, x: 0, y: 0, unit: CoordUnit::Pixel, modifiers }
    }

    fn by(dx: i32, dy: i32, unit: ScrollUnit, phase: ScrollPhase) -> MouseEventKind {
        MouseEventKind::ScrollBy { dx, dy, unit, phase }
    }

    fn settle(v: &mut Viewport, now: Instant) {
        v.tick(now + Duration::from_secs(5));
    }

    #[test]
    fn the_wheel_eases_by_its_step_and_shift_turns_it_sideways() {
        let (mut v, now) = (view(), Instant::now());
        assert_eq!(
            v.input(&ev(MouseEventKind::Scroll(ScrollDirection::Down), Modifiers::NONE), 20.0, now),
            Input::Scrolled
        );
        assert!(v.is_animating());
        assert_eq!(v.offset(), (0.0, 0.0));
        settle(&mut v, now);
        assert_eq!(v.offset(), (0.0, 60.0));
        v.input(&ev(MouseEventKind::Scroll(ScrollDirection::Down), Modifiers::SHIFT), 20.0, now);
        settle(&mut v, now);
        assert_eq!(v.offset(), (60.0, 60.0));
    }

    #[test]
    fn ctrl_asks_to_zoom_and_does_not_scroll() {
        let (mut v, now) = (view(), Instant::now());
        let up = ev(MouseEventKind::Scroll(ScrollDirection::Up), Modifiers::CTRL);
        assert_eq!(v.input(&up, 20.0, now), Input::Zoom(1));
        let down = ev(MouseEventKind::Scroll(ScrollDirection::Down), Modifiers::CTRL);
        assert_eq!(v.input(&down, 20.0, now), Input::Zoom(-1));
        // Pixels with Ctrl zoom by whole notches of a row.
        let px = ev(by(0, -4500, ScrollUnit::Pixel, ScrollPhase::Touch), Modifiers::CTRL);
        assert_eq!(v.input(&px, 20.0, now), Input::Zoom(2));
        assert!(!v.is_animating());
        assert_eq!(v.offset(), (0.0, 0.0));
    }

    #[test]
    fn notches_add_up_to_wheel_steps() {
        let (mut v, now) = (view(), Instant::now());
        let half = ev(by(0, 50, ScrollUnit::Line, ScrollPhase::Wheel), Modifiers::NONE);
        assert_eq!(v.input(&half, 20.0, now), Input::Ignored);
        settle(&mut v, now);
        assert_eq!(v.offset(), (0.0, 0.0));
        assert_eq!(v.input(&half, 20.0, now), Input::Scrolled);
        settle(&mut v, now);
        assert_eq!(v.offset(), (0.0, 60.0));
    }

    #[test]
    fn fingers_move_exactly_and_wheel_pixels_ease() {
        let (mut v, now) = (view(), Instant::now());
        v.input(&ev(by(0, 12000, ScrollUnit::Pixel, ScrollPhase::Touch), Modifiers::NONE), 20.0, now);
        assert_eq!((v.offset(), v.is_animating()), ((0.0, 120.0), false));
        v.input(&ev(by(0, 3000, ScrollUnit::Pixel, ScrollPhase::Momentum), Modifiers::NONE), 20.0, now);
        assert_eq!((v.offset(), v.is_animating()), ((0.0, 150.0), false));
        v.input(&ev(by(0, -15000, ScrollUnit::Pixel, ScrollPhase::Wheel), Modifiers::NONE), 20.0, now);
        assert!(v.is_animating());
        settle(&mut v, now);
        assert_eq!(v.offset(), (0.0, 0.0));
    }

    #[test]
    fn a_new_size_clamps_without_ending_a_scroll() {
        let (mut v, now) = (view(), Instant::now());
        v.scroll_by(0.0, 4000.0, now);
        v.tick(now + Duration::from_millis(16));
        v.set_content(1000.0, 3000.0);
        assert!(v.is_animating());
        assert_eq!(v.target(), (0.0, 2400.0));
        settle(&mut v, now);
        assert_eq!(v.offset(), (0.0, 2400.0));
        // Content smaller than the view does not scroll.
        v.set_content(100.0, 100.0);
        assert_eq!(v.offset(), (0.0, 0.0));
    }

    #[test]
    fn reveal_leaves_rows_in_sight_and_brings_others_a_third_down() {
        let (mut v, now) = (view(), Instant::now());
        v.reveal(100.0, 120.0, false, now);
        assert_eq!(v.offset(), (0.0, 0.0));
        v.reveal(1000.0, 1020.0, false, now);
        assert_eq!(v.offset(), (0.0, 800.0));
        v.reveal(200.0, 220.0, true, now);
        assert!(v.is_animating());
        settle(&mut v, now);
        assert_eq!(v.offset(), (0.0, 0.0));
    }
}
