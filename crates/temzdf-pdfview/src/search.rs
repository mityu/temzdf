//! Search results as they arrive from the search worker, and navigation
//! between hits. Pure state; the worker lives in `temzdf-pdfcore`'s `workers.rs`.

use crate::pdf::{PageRect, Target};

#[derive(Clone, Debug, PartialEq)]
pub struct SearchState {
    pub query: String,
    /// Incremented per query; results from older generations are ignored.
    pub generation: u64,
    /// Hits per page, in reading order.
    pub hits: Vec<Vec<PageRect>>,
    pages_done: usize,
    /// The hit the user navigated to: (page, index on that page).
    pub current: Option<(usize, usize)>,
}

impl SearchState {
    pub fn new(pages: usize) -> Self {
        Self { query: String::new(), generation: 0, hits: vec![Vec::new(); pages], pages_done: 0, current: None }
    }

    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    /// Start a new query; returns its generation.
    pub fn start(&mut self, query: &str) -> u64 {
        self.generation += 1;
        self.query = query.to_owned();
        self.hits.iter_mut().for_each(Vec::clear);
        self.pages_done = 0;
        self.current = None;
        self.generation
    }

    pub fn clear(&mut self) {
        self.start("");
    }

    /// Record the hits of one page. Returns false for stale generations.
    pub fn add(&mut self, generation: u64, page: usize, rects: Vec<PageRect>) -> bool {
        if generation != self.generation || page >= self.hits.len() {
            return false;
        }
        self.hits[page] = rects;
        self.pages_done += 1;
        true
    }

    pub fn is_complete(&self) -> bool {
        self.pages_done >= self.hits.len()
    }

    pub fn total(&self) -> usize {
        self.hits.iter().map(Vec::len).sum()
    }

    /// 1-based ordinal of the current hit among all hits.
    pub fn current_ordinal(&self) -> Option<usize> {
        let (p, i) = self.current?;
        Some(self.hits[..p].iter().map(Vec::len).sum::<usize>() + i + 1)
    }

    pub fn target(&self, hit: (usize, usize)) -> Option<Target> {
        let r = self.hits.get(hit.0)?.get(hit.1)?;
        Some(Target { page: hit.0, y: Some(r.y0) })
    }

    /// The first hit at or after `pos` (forward) or before it (backward), wrapping.
    pub fn find_from(&self, pos: Target, forward: bool) -> Option<(usize, usize)> {
        let all: Vec<(usize, usize, f32)> = self
            .hits
            .iter()
            .enumerate()
            .flat_map(|(p, rs)| rs.iter().enumerate().map(move |(i, r)| (p, i, r.y0)))
            .collect();
        let here = (pos.page, pos.y.unwrap_or(0.0));
        let after = |h: &&(usize, usize, f32)| (h.0, h.2) >= here;
        let pick = if forward {
            all.iter().find(after).or_else(|| all.first())
        } else {
            all.iter().rev().find(|h| (h.0, h.2) < here).or_else(|| all.last())
        };
        pick.map(|h| (h.0, h.1))
    }

    /// Move from the current hit to the next or previous one, wrapping around.
    pub fn step(&mut self, forward: bool) -> Option<(usize, usize)> {
        let (p, i) = self.current?;
        let n = self.hits.len();
        let next = if forward {
            if i + 1 < self.hits[p].len() {
                Some((p, i + 1))
            } else {
                (1..=n).map(|d| (p + d) % n).find(|&q| !self.hits[q].is_empty()).map(|q| (q, 0))
            }
        } else if i > 0 {
            Some((p, i - 1))
        } else {
            (1..=n).map(|d| (p + n - d) % n).find(|&q| !self.hits[q].is_empty()).map(|q| (q, self.hits[q].len() - 1))
        };
        self.current = next;
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(y: f32) -> PageRect {
        PageRect { x0: 10.0, y0: y, x1: 50.0, y1: y + 10.0 }
    }

    #[test]
    fn ordering_and_wrapping() {
        let mut s = SearchState::new(3);
        let g = s.start("x");
        assert!(s.add(g, 0, vec![rect(100.0), rect(300.0)]));
        assert!(s.add(g, 2, vec![rect(50.0)]));
        assert!(!s.add(g - 1, 1, vec![rect(1.0)]), "stale generation");
        assert!(!s.is_complete());
        assert!(s.add(g, 1, vec![]));
        assert!(s.is_complete());
        assert_eq!(s.total(), 3);

        s.current = s.find_from(Target { page: 0, y: Some(200.0) }, true);
        assert_eq!(s.current, Some((0, 1)));
        assert_eq!(s.current_ordinal(), Some(2));
        assert_eq!(s.step(true), Some((2, 0)), "skips the empty page");
        assert_eq!(s.step(true), Some((0, 0)), "wraps");
        assert_eq!(s.step(false), Some((2, 0)), "wraps backwards");
        assert_eq!(s.find_from(Target { page: 0, y: Some(200.0) }, false), Some((0, 0)));
        assert_eq!(s.find_from(Target { page: 2, y: Some(60.0) }, true), Some((0, 0)), "wraps to the start");
    }
}
