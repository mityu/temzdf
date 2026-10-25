//! Modes, the matcher that turns typed keys into mappings, and the mode
//! history.
//!
//! A mode has properties: its mappings, what unmapped keys do (drop them or
//! pass them to the pane) and whether it takes counts. `inherit` takes
//! another mode's properties, live: lookups walk up to the parent when a key
//! is pressed, so the order of definitions does not matter. A mode can unmap
//! an inherited key. A mode and its ancestors form one table for key
//! sequences, resolved as in Vim: a key that starts a longer mapping waits
//! (up to `timeoutlen`, unless the shorter mapping is `nowait`); a sequence
//! that matches nothing runs the longest mapped prefix (or passes on its
//! first key unmapped) and the rest is looked at again.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use termpix_core::input::{KeyCode, KeyEvent};

use super::notation::Stroke;

/// What a mode does with keys it has no mapping for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unmapped {
    Drop,
    Passthrough,
}

#[derive(Clone, Debug)]
pub enum Entry<T> {
    Map {
        target: T,
        nowait: bool,
        desc: Option<String>,
    },
    /// Hides an inherited mapping.
    Unmap,
}

/// A mode's own properties; `None` takes the parent's (or the default).
#[derive(Clone, Debug)]
pub struct ModeDef<T> {
    pub inherit: Option<String>,
    pub unmapped: Option<Unmapped>,
    pub count: Option<bool>,
    pub maps: HashMap<Vec<Stroke>, Entry<T>>,
}

impl<T> Default for ModeDef<T> {
    fn default() -> Self {
        Self { inherit: None, unmapped: None, count: None, maps: HashMap::new() }
    }
}

/// Every mode, by name.
#[derive(Clone, Debug)]
pub struct Modes<T> {
    defs: HashMap<String, ModeDef<T>>,
}

impl<T> Default for Modes<T> {
    fn default() -> Self {
        Self { defs: HashMap::new() }
    }
}

impl<T: Clone> Modes<T> {
    pub fn define(&mut self, name: &str) -> &mut ModeDef<T> {
        self.defs.entry(name.to_owned()).or_default()
    }

    pub fn exists(&self, name: &str) -> bool {
        self.defs.contains_key(name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.defs.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn map(&mut self, mode: &str, keys: Vec<Stroke>, target: T, nowait: bool, desc: Option<String>) {
        self.define(mode).maps.insert(keys, Entry::Map { target, nowait, desc });
    }

    pub fn unmap(&mut self, mode: &str, keys: Vec<Stroke>) {
        self.define(mode).maps.insert(keys, Entry::Unmap);
    }

    /// The mode and its ancestors, nearest first (a cycle or an unknown
    /// parent ends the chain; [`Modes::check`] reports them).
    fn chain(&self, mode: &str) -> Vec<&ModeDef<T>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut name = Some(mode);
        while let Some(n) = name {
            if !seen.insert(n) {
                break;
            }
            let Some(d) = self.defs.get(n) else { break };
            out.push(d);
            name = d.inherit.as_deref();
        }
        out
    }

    pub fn unmapped(&self, mode: &str) -> Unmapped {
        self.chain(mode).iter().find_map(|d| d.unmapped).unwrap_or(Unmapped::Drop)
    }

    pub fn takes_count(&self, mode: &str) -> bool {
        self.chain(mode).iter().find_map(|d| d.count).unwrap_or(false)
    }

    /// The mapping for exactly `keys` (the nearest entry decides).
    pub fn lookup(&self, mode: &str, keys: &[Stroke]) -> Option<(T, bool)> {
        for d in self.chain(mode) {
            match d.maps.get(keys) {
                Some(Entry::Map { target, nowait, .. }) => return Some((target.clone(), *nowait)),
                Some(Entry::Unmap) => return None,
                None => {}
            }
        }
        None
    }

    /// Whether a mapping longer than `keys` starts with them.
    pub fn has_longer(&self, mode: &str, keys: &[Stroke]) -> bool {
        self.mappings(mode).into_iter().any(|(k, _)| k.len() > keys.len() && k.starts_with(keys))
    }

    /// The mode's effective mappings (its own and inherited ones it does not
    /// hide), with their descriptions.
    pub fn mappings(&self, mode: &str) -> Vec<(Vec<Stroke>, Option<String>)> {
        let mut seen: HashSet<&Vec<Stroke>> = HashSet::new();
        let mut out = Vec::new();
        for d in self.chain(mode) {
            for (k, e) in &d.maps {
                if seen.insert(k)
                    && let Entry::Map { desc, .. } = e
                {
                    out.push((k.clone(), desc.clone()));
                }
            }
        }
        out
    }

    /// Unknown parents and inheritance cycles.
    pub fn check(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (name, d) in &self.defs {
            if let Some(p) = &d.inherit
                && !self.defs.contains_key(p)
            {
                problems.push(format!("mode {name:?} inherits unknown mode {p:?}"));
            }
            let mut seen = HashSet::new();
            let mut n = Some(name.as_str());
            while let Some(m) = n {
                if !seen.insert(m) {
                    problems.push(format!("mode {name:?} inherits itself (through {m:?})"));
                    break;
                }
                n = self.defs.get(m).and_then(|d| d.inherit.as_deref());
            }
        }
        problems.sort();
        problems
    }
}

/// What the matcher decided.
#[derive(Clone, Debug, PartialEq)]
pub enum Step<T> {
    /// Nothing to do now: no input, or waiting for more keys until `.0`.
    Wait(Option<Instant>),
    Run {
        target: T,
        count: Option<usize>,
        keys: Vec<Stroke>,
    },
    /// A key without a mapping, for the mode's unmapped behaviour, with the
    /// event as typed (when it was typed rather than fed).
    Unmapped(Stroke, Option<KeyEvent>),
}

/// The ways to read typed `keys` (each key as itself or as its fallback),
/// in the order mappings are tried: fewest fallbacks first, then keys
/// earlier in the sequence kept as typed. Long runs of keys with fallbacks
/// try only all-typed and all-fallback.
pub fn readings(keys: &[Stroke]) -> Vec<Vec<Stroke>> {
    let alts: Vec<usize> = (0..keys.len()).filter(|&i| keys[i].fallback().is_some()).collect();
    if alts.is_empty() {
        return vec![keys.to_vec()];
    }
    let masks: Vec<u32> =
        if alts.len() > 8 { vec![0, (1u32 << alts.len()) - 1] } else { (0..(1u32 << alts.len())).collect() };
    let mut out: Vec<(u32, u32, Vec<Stroke>)> = masks
        .into_iter()
        .map(|mask| {
            let mut v = keys.to_vec();
            for (bit, &i) in alts.iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    v[i] = keys[i].fallback().expect("has a fallback");
                }
            }
            (mask.count_ones(), mask, v)
        })
        .collect();
    // Bit i is the i-th key with a fallback: among equal counts a larger mask
    // has its fallbacks later, keeping earlier keys as typed.
    out.sort_by_key(|(n, mask, _)| (*n, std::cmp::Reverse(*mask)));
    out.into_iter().map(|(_, _, v)| v).collect()
}

impl<T: Clone> Modes<T> {
    /// The mapping for the typed `keys`, trying their [`readings`].
    pub fn lookup_typed(&self, mode: &str, keys: &[Stroke]) -> Option<(T, bool, Vec<Stroke>)> {
        readings(keys).into_iter().find_map(|r| self.lookup(mode, &r).map(|(t, n)| (t, n, r)))
    }

    /// Whether any reading of `keys` starts a longer mapping.
    pub fn has_longer_typed(&self, mode: &str, keys: &[Stroke]) -> bool {
        readings(keys).iter().any(|r| self.has_longer(mode, r))
    }
}

/// Typed keys on their way to mappings.
#[derive(Debug, Default)]
pub struct Matcher {
    /// Keys not looked at yet, as typed, and whether mappings apply.
    input: VecDeque<(Stroke, Option<KeyEvent>, bool)>,
    pending: Vec<(Stroke, Option<KeyEvent>)>,
    since: Option<Instant>,
    count: Option<usize>,
    /// The pending keys timed out: resolve them with what they match.
    expired: bool,
}

impl Matcher {
    /// A typed key.
    pub fn push(&mut self, ev: &KeyEvent) {
        self.input.push_back((Stroke::of(ev), Some(ev.clone()), true));
    }

    /// Keys fed ahead of what is queued, as Vim's `feedkeys` inserts
    /// typeahead.
    pub fn feed_front(&mut self, keys: &[Stroke], remap: bool) {
        for k in keys.iter().rev() {
            self.input.push_front((*k, None, remap));
        }
    }

    fn requeue(&mut self, keys: Vec<(Stroke, Option<KeyEvent>)>) {
        for (k, ev) in keys.into_iter().rev() {
            self.input.push_front((k, ev, true));
        }
    }

    pub fn has_input(&self) -> bool {
        !self.input.is_empty()
    }

    /// Drop pending keys, the count and queued input (`<C-c>`).
    pub fn cancel(&mut self) {
        self.input.clear();
        self.pending.clear();
        self.since = None;
        self.count = None;
        self.expired = false;
    }

    pub fn pending(&self) -> Vec<Stroke> {
        self.pending.iter().map(|(k, _)| *k).collect()
    }

    pub fn count(&self) -> Option<usize> {
        self.count
    }

    pub fn deadline(&self, timeout: Duration) -> Option<Instant> {
        self.since.map(|t| t + timeout)
    }

    /// The next decision for `mode`. Call until it says [`Step::Wait`];
    /// mappings that change the mode take effect for the keys after them.
    pub fn step<T: Clone>(&mut self, modes: &Modes<T>, mode: &str, timeout: Duration, now: Instant) -> Step<T> {
        if self.since.is_some_and(|t| now >= t + timeout) {
            self.expired = true;
        }
        loop {
            if self.expired {
                self.expired = false;
                self.since = None;
                if let Some(step) = self.resolve(modes, mode) {
                    return step;
                }
                continue;
            }
            let Some((k, ev, remap)) = self.input.pop_front() else {
                return Step::Wait(self.deadline(timeout));
            };
            if !remap {
                // Fed without mappings: settle what is pending first.
                if !self.pending.is_empty() {
                    self.input.push_front((k, ev, remap));
                    self.expired = true;
                    continue;
                }
                self.count = None;
                return Step::Unmapped(k, ev);
            }
            // Counts: digits, `0` only after another digit, unless mapped.
            if self.pending.is_empty()
                && modes.takes_count(mode)
                && let KeyCode::Char(c @ '0'..='9') = k.code
                && k.mods == termpix_core::input::Modifiers::NONE
                && (c != '0' || self.count.is_some())
                && (self.count.is_some()
                    || (modes.lookup_typed(mode, &[k]).is_none() && !modes.has_longer_typed(mode, &[k])))
            {
                let d = c as usize - '0' as usize;
                self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(d));
                continue;
            }
            self.pending.push((k, ev));
            let keys = self.pending();
            let exact = modes.lookup_typed(mode, &keys);
            let longer = modes.has_longer_typed(mode, &keys);
            if let Some((target, nowait, keys)) = exact
                && (!longer || nowait)
            {
                self.since = None;
                self.pending.clear();
                return Step::Run { target, count: self.count.take(), keys };
            }
            if longer {
                self.since.get_or_insert(now);
            } else {
                self.since = None;
                if let Some(step) = self.resolve(modes, mode) {
                    return step;
                }
            }
        }
    }

    /// Pending keys that cannot grow: run the longest mapped prefix, or pass
    /// the first key on unmapped; the rest goes back to the input.
    fn resolve<T: Clone>(&mut self, modes: &Modes<T>, mode: &str) -> Option<Step<T>> {
        if self.pending.is_empty() {
            return None;
        }
        let mut items = std::mem::take(&mut self.pending);
        let keys: Vec<Stroke> = items.iter().map(|(k, _)| *k).collect();
        for n in (1..=keys.len()).rev() {
            if let Some((target, _, read)) = modes.lookup_typed(mode, &keys[..n]) {
                let rest = items.split_off(n);
                self.requeue(rest);
                return Some(Step::Run { target, count: self.count.take(), keys: read });
            }
        }
        let rest = items.split_off(1);
        self.requeue(rest);
        self.count = None;
        let (k, ev) = items.pop().expect("one pending key");
        Some(Step::Unmapped(k, ev))
    }
}

/// The modes entered with `switch-mode`, oldest first; the operating mode is
/// always below them.
#[derive(Debug)]
pub struct History {
    stack: VecDeque<String>,
    root: String,
    pub cap: usize,
}

impl History {
    pub fn new(root: &str, cap: usize) -> Self {
        Self { stack: VecDeque::new(), root: root.to_owned(), cap }
    }

    pub fn current(&self) -> &str {
        self.stack.back().map_or(&self.root, String::as_str)
    }

    /// `switch-mode`: the oldest entries go beyond the cap.
    pub fn push(&mut self, mode: &str) {
        self.stack.push_back(mode.to_owned());
        while self.stack.len() > self.cap.max(1) {
            self.stack.pop_front();
        }
    }

    /// `exit-current-mode`: back to the previous mode (the operating mode
    /// when the history is empty).
    pub fn pop(&mut self) {
        self.stack.pop_back();
    }

    /// The focus moved: the history starts again from the pane's mode.
    pub fn reset(&mut self, mode: &str) {
        self.stack.clear();
        if mode != self.root {
            self.stack.push_back(mode.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::notation::parse;

    const T: Duration = Duration::from_millis(1000);

    fn modes() -> Modes<&'static str> {
        let mut m = Modes::default();
        let k = |s| parse(s).unwrap();
        let d = m.define("base");
        d.unmapped = Some(Unmapped::Drop);
        d.count = Some(true);
        m.map("base", k("j"), "down", false, None);
        m.map("base", k("g"), "g", false, None);
        m.map("base", k("gg"), "top", false, None);
        m.map("base", k("0"), "fit", false, None);
        m.map("base", k("x"), "close", false, None);
        let c = m.define("child");
        c.inherit = Some("base".into());
        m.map("child", k("<C-b>"), "prefix", true, None);
        m.map("child", k("<C-b>%"), "split", false, None);
        m.unmap("child", k("x"));
        let t = m.define("term");
        t.unmapped = Some(Unmapped::Passthrough);
        m.map("term", k("ab"), "ab", false, None);
        m
    }

    /// Feed keys typed at `t0 + ms` and collect what the matcher does.
    fn run(m: &Modes<&'static str>, mode: &str, keys: &[(&str, u64)], until: u64) -> Vec<String> {
        let t0 = Instant::now();
        let mut matcher = Matcher::default();
        let mut out = Vec::new();
        let drain = |matcher: &mut Matcher, now: Instant, out: &mut Vec<String>| {
            loop {
                match matcher.step(m, mode, T, now) {
                    Step::Wait(_) => break,
                    Step::Run { target, count, .. } => out.push(match count {
                        Some(c) => format!("{c}{target}"),
                        None => target.to_owned(),
                    }),
                    Step::Unmapped(k, _) => out.push(format!("unmapped {}", k.notation())),
                }
            }
        };
        for (s, ms) in keys {
            for k in parse(s).unwrap() {
                matcher.push(&k.event());
            }
            drain(&mut matcher, t0 + Duration::from_millis(*ms), &mut out);
        }
        drain(&mut matcher, t0 + Duration::from_millis(until), &mut out);
        out
    }

    #[test]
    fn sequences_wait_and_time_out_as_in_vim() {
        let m = modes();
        assert_eq!(run(&m, "base", &[("gg", 0)], 10), ["top"]);
        assert_eq!(run(&m, "base", &[("g", 0)], 500), Vec::<String>::new(), "still waiting");
        assert_eq!(run(&m, "base", &[("g", 0)], 1001), ["g"], "the shorter mapping after the timeout");
        assert_eq!(run(&m, "base", &[("g", 0), ("j", 10)], 20), ["g", "down"], "a key that does not continue");
        assert_eq!(run(&m, "base", &[("q", 0)], 10), ["unmapped q"]);
    }

    #[test]
    fn unmatched_sequences_pass_their_first_key_on() {
        let m = modes();
        // In a pass-through mode the caller sends unmapped keys to the pane.
        assert_eq!(run(&m, "term", &[("ac", 0)], 10), ["unmapped a", "unmapped c"]);
        assert_eq!(run(&m, "term", &[("ab", 0)], 10), ["ab"]);
        assert_eq!(run(&m, "term", &[("a", 0)], 1001), ["unmapped a"], "timed out");
        assert_eq!(m.unmapped("term"), Unmapped::Passthrough);
    }

    #[test]
    fn counts() {
        let m = modes();
        assert_eq!(run(&m, "base", &[("3j", 0)], 10), ["3down"]);
        assert_eq!(run(&m, "base", &[("10j", 0)], 10), ["10down"], "0 after a digit counts");
        assert_eq!(run(&m, "base", &[("0", 0)], 10), ["fit"], "a lone 0 is its mapping");
        assert_eq!(run(&m, "base", &[("2q", 0)], 10), ["unmapped q"], "an unmapped key drops the count");
        assert_eq!(run(&m, "term", &[("3", 0)], 10), ["unmapped 3"], "no counts in pass-through modes");
    }

    #[test]
    fn inheritance_is_live_and_unmap_hides() {
        let mut m = modes();
        assert_eq!(run(&m, "child", &[("j", 0)], 10), ["down"], "inherited");
        assert_eq!(run(&m, "child", &[("x", 0)], 10), ["unmapped x"], "unmapped in the child");
        assert_eq!(run(&m, "base", &[("x", 0)], 10), ["close"]);
        assert!(m.takes_count("child") && m.unmapped("child") == Unmapped::Drop, "properties inherited");
        // A mapping added to the parent later counts in the child.
        m.map("base", parse("k").unwrap(), "up", false, None);
        assert_eq!(run(&m, "child", &[("k", 0)], 10), ["up"]);
        // nowait runs the shorter mapping at once.
        assert_eq!(run(&m, "child", &[("<C-b>", 0)], 10), ["prefix"]);
        assert!(m.mappings("child").iter().all(|(k, _)| k != &parse("x").unwrap()));
    }

    #[test]
    fn keys_fed_without_mappings_and_cancel() {
        let m = modes();
        let mut matcher = Matcher::default();
        let now = Instant::now();
        matcher.feed_front(&parse("j").unwrap(), false);
        assert_eq!(matcher.step(&m, "base", T, now), Step::Unmapped(parse("j").unwrap()[0], None));
        matcher.push(&parse("3").unwrap()[0].event());
        matcher.push(&parse("g").unwrap()[0].event());
        assert!(matches!(matcher.step(&m, "base", T, now), Step::Wait(Some(_))));
        assert_eq!((matcher.count(), matcher.pending().len()), (Some(3), 1));
        matcher.cancel();
        assert_eq!((matcher.count(), matcher.pending().len()), (None, 0));
        assert_eq!(matcher.step(&m, "base", T, now), Step::Wait(None));
    }

    #[test]
    fn checks_report_cycles_and_unknown_parents() {
        let mut m: Modes<&str> = Modes::default();
        m.define("a").inherit = Some("b".into());
        m.define("b").inherit = Some("a".into());
        m.define("c").inherit = Some("nope".into());
        let p = m.check();
        assert!(p.iter().any(|s| s.contains("\"c\" inherits unknown mode \"nope\"")), "{p:?}");
        assert!(p.iter().any(|s| s.contains("inherits itself")), "{p:?}");
        assert_eq!(m.unmapped("a"), Unmapped::Drop, "a cycle still resolves");
    }

    #[test]
    fn typed_keys_try_their_own_form_then_the_fallback() {
        let mut m: Modes<&str> = Modes::default();
        let k = |s| parse(s).unwrap();
        m.map("m", k("<Tab>"), "tab", false, None);
        m.map("m", k("A"), "big-a", false, None);
        let typed = |m: &Modes<&'static str>, s: &str| run(m, "m", &[(s, 0)], 10);
        assert_eq!(typed(&m, "<C-i>"), ["tab"], "<C-i> does what <Tab> does");
        assert_eq!(typed(&m, "<S-a>"), ["big-a"], "Shift+a does what A does");
        m.map("m", k("<C-i>"), "ctrl-i", false, None);
        m.map("m", k("<S-a>"), "shift-a", false, None);
        assert_eq!(typed(&m, "<C-i>"), ["ctrl-i"], "unless mapped itself");
        assert_eq!(typed(&m, "<Tab>"), ["tab"]);
        assert_eq!(typed(&m, "<S-a>"), ["shift-a"]);
        assert_eq!(typed(&m, "A"), ["big-a"], "Caps Lock A stays A");
        // Sequences: the reading with the fewest fallbacks wins.
        m.map("m", k("<Tab>x"), "tab-x", false, None);
        assert_eq!(typed(&m, "<C-i>x"), ["tab-x"]);
        assert_eq!(readings(&k("<S-a><C-i>"))[0], k("<S-a><C-i>"));
        assert_eq!(readings(&k("<S-a><C-i>"))[1], k("<S-a><Tab>"), "keep earlier keys as typed");
    }

    #[test]
    fn history() {
        let mut h = History::new("normal", 3);
        assert_eq!(h.current(), "normal");
        for m in ["a", "b", "c", "d"] {
            h.push(m);
        }
        assert_eq!(h.current(), "d");
        h.pop();
        h.pop();
        h.pop();
        assert_eq!(h.current(), "normal", "a was dropped by the cap; normal is below");
        h.pop();
        assert_eq!(h.current(), "normal");
        h.reset("pdf");
        assert_eq!(h.current(), "pdf");
        h.pop();
        assert_eq!(h.current(), "normal");
    }
}
