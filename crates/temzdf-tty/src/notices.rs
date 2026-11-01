//! A terminal client's own floating messages, for a terminal without the
//! kitty graphics protocol (only this client cannot show images, so the
//! session does not say it): why, from the start until the next key, and
//! which panes have images it cannot show, once per pane until its images
//! go and come back, at most one message per [`INTERVAL`] (panes that turn
//! up meanwhile are named together in the next one).

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use temzdf_scene::{Color, Region, TextGrid};

/// The least time between two messages about panes.
pub const INTERVAL: Duration = Duration::from_secs(5);
/// How long a message about panes stays.
pub const SHOWN_FOR: Duration = Duration::from_secs(4);

pub struct Notices {
    /// Why there are no images, until the next key.
    reason: Option<String>,
    /// The message about panes shown now, and until when.
    shown: Option<(String, Instant)>,
    /// Panes whose images were noticed (until they have none again).
    noticed: HashSet<u32>,
    /// Panes to name in the next message.
    waiting: BTreeSet<u32>,
    last: Option<Instant>,
    /// The first frame: its panes are covered by the reason.
    started: bool,
}

impl Notices {
    /// Messages for a terminal without images for `reason`.
    pub fn new(reason: &str) -> Self {
        Self {
            reason: Some(format!("{reason}: PDF panes and images are not shown")),
            shown: None,
            noticed: HashSet::new(),
            waiting: BTreeSet::new(),
            last: None,
            started: false,
        }
    }

    /// A key was pressed: the reason goes. Returns whether something went.
    pub fn key(&mut self) -> bool {
        self.reason.take().is_some()
    }

    /// The panes of a new frame. Returns whether the messages changed.
    pub fn frame(&mut self, regions: &[Region], now: Instant) -> bool {
        let with: HashSet<u32> = regions.iter().filter(|r| r.images).map(|r| r.pane).collect();
        self.noticed.retain(|p| with.contains(p));
        self.waiting.retain(|p| with.contains(p));
        for p in with {
            if self.noticed.insert(p) && self.started {
                self.waiting.insert(p);
            }
        }
        self.started = true;
        self.tick(now)
    }

    /// Time passed. Returns whether the messages changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.shown.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.shown = None;
            changed = true;
        }
        if !self.waiting.is_empty() && self.last.is_none_or(|t| now >= t + INTERVAL) {
            let panes: Vec<String> = std::mem::take(&mut self.waiting).iter().map(u32::to_string).collect();
            let text = match panes.as_slice() {
                [one] => format!("Images in pane {one} are not shown (no kitty graphics protocol support)"),
                many => {
                    format!("Images in panes {} are not shown (no kitty graphics protocol support)", many.join(", "))
                }
            };
            self.shown = Some((text, now + SHOWN_FOR));
            self.last = Some(now);
            changed = true;
        }
        changed
    }

    /// When [`Notices::tick`] has something to do next.
    pub fn deadline(&self) -> Option<Instant> {
        let release = (!self.waiting.is_empty()).then(|| self.last.map(|t| t + INTERVAL)).flatten();
        [self.shown.as_ref().map(|(_, until)| *until), release].into_iter().flatten().min()
    }

    /// The messages as boxes at the top right of a `cols` x `rows` screen,
    /// from row `top`.
    pub fn grids(&self, cols: u16, rows: u16, top: u16) -> Vec<TextGrid> {
        let mut messages = Vec::new();
        if let Some(r) = &self.reason {
            messages.push((r.as_str(), Color::Indexed(1)));
        }
        if let Some((s, _)) = &self.shown {
            messages.push((s.as_str(), Color::Indexed(4)));
        }
        temzdf_scene::notice::boxes(&messages, cols as usize, rows as usize, top as usize)
    }

    /// The texts shown now (for tests).
    #[cfg(test)]
    fn texts(&self) -> Vec<String> {
        self.reason.iter().chain(self.shown.iter().map(|(s, _)| s)).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temzdf_scene::{Rect, RegionKind};

    fn region(pane: u32, images: bool) -> Region {
        Region { pane, rect: Rect::new(0, 0, 10, 10), kind: RegionKind::Terminal, images }
    }

    #[test]
    fn the_reason_stays_until_a_key() {
        let mut n = Notices::new("no graphics here");
        let t = Instant::now();
        // The first frame's panes: the reason covers them.
        n.frame(&[region(1, true)], t);
        assert_eq!(n.texts(), ["no graphics here: PDF panes and images are not shown"]);
        assert_eq!(n.grids(80, 24, 0).len(), 1, "a box");
        assert!(n.key());
        assert!(!n.key());
        assert!(n.texts().is_empty() && n.grids(80, 24, 0).is_empty());
    }

    #[test]
    fn panes_are_named_once_until_their_images_go_and_come_back() {
        let mut n = Notices::new("r");
        n.key();
        let t = Instant::now();
        n.frame(&[region(1, false)], t);
        assert!(n.frame(&[region(1, true)], t));
        assert_eq!(n.texts(), ["Images in pane 1 are not shown (no kitty graphics protocol support)"]);
        // The same images again: nothing new; the message fades.
        assert!(!n.frame(&[region(1, true)], t + Duration::from_secs(1)));
        assert_eq!(n.deadline(), Some(t + SHOWN_FOR));
        assert!(n.tick(t + SHOWN_FOR));
        assert!(n.texts().is_empty());
        // Gone and back: named again (after the interval).
        n.frame(&[region(1, false)], t + Duration::from_secs(6));
        assert!(n.frame(&[region(1, true)], t + Duration::from_secs(7)));
        assert_eq!(n.texts().len(), 1);
    }

    #[test]
    fn panes_turning_up_within_the_interval_are_named_together() {
        let mut n = Notices::new("r");
        n.key();
        let t = Instant::now();
        n.frame(&[], t);
        n.frame(&[region(1, true)], t);
        let soon = t + Duration::from_secs(1);
        assert!(!n.frame(&[region(1, true), region(3, true)], soon), "too soon");
        assert!(!n.frame(&[region(1, true), region(3, true), region(5, true)], soon));
        assert_eq!(n.deadline(), Some(t + SHOWN_FOR), "the message fades first");
        n.tick(t + SHOWN_FOR);
        assert_eq!(n.deadline(), Some(t + INTERVAL));
        assert!(n.tick(t + INTERVAL));
        assert_eq!(n.texts(), ["Images in panes 3, 5 are not shown (no kitty graphics protocol support)"]);
        // A pane whose images went before its turn is left out.
        let later = t + INTERVAL + Duration::from_secs(1);
        n.frame(&[region(7, true)], later);
        n.frame(&[], later);
        n.tick(t + INTERVAL * 2 + SHOWN_FOR);
        assert!(n.texts().is_empty(), "{:?}", n.texts());
    }
}
