//! Modes, the matcher that turns typed keys into mappings, and the mode
//! history (doc/keymaps.md).
//!
//! A mode has properties: its mappings, what unmapped keys do (drop them or
//! pass them to the pane), whether it takes counts, and for a transient
//! mode (a submode) how long it stays. A key may have several mappings in a
//! mode: the first whose condition holds runs, in order of priority, then
//! the last registered first. A mapping whose condition is false is not
//! there. Conditions are the script's functions, evaluated by the caller
//! when the matcher asks ([`Step::NeedConditions`]) with the context of the
//! first key of a sequence, and kept for the rest of it.
//!
//! The matcher looks keys up in layers ([`Layers`]): the current mode, and
//! below a transient mode with `keepalive: "always"` the modes under it.
//! Sequences resolve as in Vim: a key that starts a longer mapping waits (up
//! to `timeoutlen`, unless the shorter mapping is `nowait`); a sequence that
//! matches nothing runs the longest mapped prefix (or passes on its first
//! key unmapped) and the rest is looked at again.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use termpix_core::input::{KeyCode, KeyEvent};

use super::notation::Stroke;

/// A condition: the script function deciding whether a mapping exists
/// (`when`) or a submode stays (`keepalive`).
pub type Cond = u32;

/// What a mode does with keys it has no mapping for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unmapped {
    Drop,
    Passthrough,
}

/// How long a transient mode stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Keepalive {
    /// While its own mappings take the keys; another key leaves it and is
    /// handled again below.
    #[default]
    Mapped,
    /// Always: keys it does not map are looked up in the modes below.
    Always,
    /// While the condition holds (asked at each sequence's first key).
    Func(Cond),
}

/// One mapping of a key sequence.
#[derive(Clone, Debug)]
pub struct Mapping<T> {
    pub target: T,
    /// The condition under which it exists; none: always.
    pub when: Option<Cond>,
    pub nowait: bool,
    pub desc: Option<String>,
    /// Mapping the same keys with the same namespace again replaces it;
    /// without one, mappings are added.
    pub namespace: Option<String>,
    /// Higher first; equal: the last registered first.
    pub priority: i32,
    order: u64,
}

impl<T> Mapping<T> {
    pub fn new(target: T) -> Self {
        Self { target, when: None, nowait: false, desc: None, namespace: None, priority: 0, order: 0 }
    }
}

/// A mode's own properties; `None`: the mode below decides (the default
/// when there is none).
#[derive(Clone, Debug)]
pub struct ModeDef<T> {
    pub unmapped: Option<Unmapped>,
    pub count: Option<bool>,
    /// A submode: sits over the mode it was entered from.
    pub transient: bool,
    pub keepalive: Keepalive,
    /// A transient mode is left after this long without keys.
    pub timeout: Option<Duration>,
    /// Each sequence's mappings in the order they are tried.
    pub maps: HashMap<Vec<Stroke>, Vec<Mapping<T>>>,
}

impl<T> Default for ModeDef<T> {
    fn default() -> Self {
        Self {
            unmapped: None,
            count: None,
            transient: false,
            keepalive: Keepalive::default(),
            timeout: None,
            maps: HashMap::new(),
        }
    }
}

/// Every mode, by name.
#[derive(Clone, Debug)]
pub struct Modes<T> {
    defs: HashMap<String, ModeDef<T>>,
    /// Registration counter: later mappings come first.
    next: u64,
}

impl<T> Default for Modes<T> {
    fn default() -> Self {
        Self { defs: HashMap::new(), next: 0 }
    }
}

impl<T: Clone> Modes<T> {
    pub fn define(&mut self, name: &str) -> &mut ModeDef<T> {
        self.defs.entry(name.to_owned()).or_default()
    }

    pub fn get(&self, name: &str) -> Option<&ModeDef<T>> {
        self.defs.get(name)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.defs.contains_key(name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.defs.keys().cloned().collect();
        v.sort();
        v
    }

    /// Add `m` for `keys` in `mode`; with a namespace it replaces the
    /// mapping of the same keys and namespace.
    pub fn map(&mut self, mode: &str, keys: Vec<Stroke>, mut m: Mapping<T>) {
        m.order = self.next;
        self.next += 1;
        let list = self.define(mode).maps.entry(keys).or_default();
        if m.namespace.is_some() {
            list.retain(|o| o.namespace != m.namespace);
        }
        list.push(m);
        list.sort_by_key(|o| (std::cmp::Reverse(o.priority), std::cmp::Reverse(o.order)));
    }

    /// Remove `keys`' mappings in `mode`: those of `namespace`, or all.
    pub fn unmap(&mut self, mode: &str, keys: &[Stroke], namespace: Option<&str>) {
        let Some(d) = self.defs.get_mut(mode) else { return };
        if let Some(list) = d.maps.get_mut(keys) {
            list.retain(|o| namespace.is_some_and(|n| o.namespace.as_deref() != Some(n)));
            if list.is_empty() {
                d.maps.remove(keys);
            }
        }
    }

    /// `mode`'s mappings (of `namespace` when given), sorted by keys and
    /// then in the order they are tried.
    pub fn mappings(&self, mode: &str, namespace: Option<&str>) -> Vec<(Vec<Stroke>, Mapping<T>)> {
        let Some(d) = self.defs.get(mode) else { return Vec::new() };
        let mut keys: Vec<&Vec<Stroke>> = d.maps.keys().collect();
        keys.sort_by_key(|k| k.iter().map(|s| s.notation()).collect::<String>());
        keys.into_iter()
            .flat_map(|k| {
                d.maps[k]
                    .iter()
                    .filter(|m| namespace.is_none_or(|n| m.namespace.as_deref() == Some(n)))
                    .map(move |m| (k.clone(), m.clone()))
            })
            .collect()
    }
}

/// Where the matcher looks keys up: the modes, nearest first, whether
/// digits are counts, and the rule for leaving a transient mode on top.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layers {
    pub modes: Vec<String>,
    pub count: bool,
    /// The first mode is transient and stays while its own mappings take
    /// the keys (`Mapped`) or while the condition holds (`Func`).
    pub keep: Option<Keepalive>,
}

impl Layers {
    /// One mode, no submode rules (tests, plain lookups).
    pub fn of(mode: &str, count: bool) -> Self {
        Self { modes: vec![mode.to_owned()], count, keep: None }
    }
}

/// The result of a lookup under conditions.
enum Found<T> {
    Yes(T, bool),
    No,
    /// A condition was not evaluated.
    Unknown,
}

/// Whether mapping `m` exists, given the conditions known.
fn enabled<T>(m: &Mapping<T>, conds: &HashMap<Cond, bool>) -> Option<bool> {
    match m.when {
        None => Some(true),
        Some(c) => conds.get(&c).copied(),
    }
}

impl<T: Clone> Modes<T> {
    /// The mapping for exactly `keys` in `layers` (`only`: the first mode
    /// alone): the first that exists.
    fn lookup(&self, layers: &[String], keys: &[Stroke], conds: &HashMap<Cond, bool>) -> Found<T> {
        for mode in layers {
            let Some(list) = self.defs.get(mode).and_then(|d| d.maps.get(keys)) else { continue };
            for m in list {
                match enabled(m, conds) {
                    Some(true) => return Found::Yes(m.target.clone(), m.nowait),
                    Some(false) => {}
                    None => return Found::Unknown,
                }
            }
        }
        Found::No
    }

    /// Whether a mapping longer than `keys` that exists starts with them.
    fn has_longer(&self, layers: &[String], keys: &[Stroke], conds: &HashMap<Cond, bool>) -> bool {
        layers.iter().filter_map(|mode| self.defs.get(mode)).any(|d| {
            d.maps.iter().any(|(k, list)| {
                k.len() > keys.len() && k.starts_with(keys) && list.iter().any(|m| enabled(m, conds) == Some(true))
            })
        })
    }

    /// Conditions not known yet of mappings that could take a sequence
    /// starting with `first` (any of its readings).
    fn unknown_conditions(&self, layers: &[String], first: Stroke, conds: &HashMap<Cond, bool>) -> Vec<Cond> {
        let starts: Vec<Stroke> = readings(&[first]).into_iter().map(|r| r[0]).collect();
        let mut out: Vec<Cond> = layers
            .iter()
            .filter_map(|mode| self.defs.get(mode))
            .flat_map(|d| d.maps.iter())
            .filter(|(k, _)| k.first().is_some_and(|s| starts.contains(s)))
            .flat_map(|(_, list)| list.iter().filter_map(|m| m.when))
            .filter(|c| !conds.contains_key(c))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        out.sort_unstable();
        out
    }

    /// The mapping for the typed `keys`, trying their [`readings`].
    fn lookup_typed(
        &self,
        layers: &[String],
        keys: &[Stroke],
        conds: &HashMap<Cond, bool>,
    ) -> Option<(T, bool, Vec<Stroke>)> {
        readings(keys).into_iter().find_map(|r| match self.lookup(layers, &r, conds) {
            Found::Yes(t, n) => Some((t, n, r)),
            Found::No | Found::Unknown => None,
        })
    }

    /// Whether any reading of `keys` starts a longer mapping.
    fn has_longer_typed(&self, layers: &[String], keys: &[Stroke], conds: &HashMap<Cond, bool>) -> bool {
        readings(keys).iter().any(|r| self.has_longer(layers, r, conds))
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
    /// Evaluate these conditions ([`Matcher::set_conditions`]), then step
    /// again.
    NeedConditions(Vec<Cond>),
    /// Leave the transient mode on top, then step again: the key is looked
    /// at again below.
    Leave,
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
    /// Conditions evaluated for the sequence being typed.
    conds: HashMap<Cond, bool>,
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
        self.conds.clear();
    }

    /// Results of conditions the matcher asked for.
    pub fn set_conditions(&mut self, results: impl IntoIterator<Item = (Cond, bool)>) {
        self.conds.extend(results);
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

    /// The next decision for keys in `layers`. Call until it says
    /// [`Step::Wait`]; mappings that change the mode take effect for the
    /// keys after them.
    pub fn step<T: Clone>(&mut self, modes: &Modes<T>, layers: &Layers, timeout: Duration, now: Instant) -> Step<T> {
        if self.since.is_some_and(|t| now >= t + timeout) {
            self.expired = true;
        }
        loop {
            if self.expired {
                self.expired = false;
                self.since = None;
                if let Some(step) = self.resolve(modes, layers) {
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
            let ls = &layers.modes;
            if self.pending.is_empty() {
                // A sequence starts: what may take it, under which conditions.
                let mut unknown = modes.unknown_conditions(ls, k, &self.conds);
                if let Some(Keepalive::Func(c)) = layers.keep
                    && !self.conds.contains_key(&c)
                {
                    unknown.push(c);
                }
                if !unknown.is_empty() {
                    self.input.push_front((k, ev, remap));
                    return Step::NeedConditions(unknown);
                }
            }
            // Counts: digits, `0` only after another digit, unless mapped.
            if self.pending.is_empty()
                && layers.count
                && let KeyCode::Char(c @ '0'..='9') = k.code
                && k.mods == termpix_core::input::Modifiers::NONE
                && (c != '0' || self.count.is_some())
                && (self.count.is_some()
                    || (modes.lookup_typed(ls, &[k], &self.conds).is_none()
                        && !modes.has_longer_typed(ls, &[k], &self.conds)))
            {
                let d = c as usize - '0' as usize;
                self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(d));
                self.conds.clear();
                continue;
            }
            if self.pending.is_empty() {
                let leave = match layers.keep {
                    Some(Keepalive::Mapped) => {
                        modes.lookup_typed(&ls[..1], &[k], &self.conds).is_none()
                            && !modes.has_longer_typed(&ls[..1], &[k], &self.conds)
                    }
                    Some(Keepalive::Func(c)) => self.conds.get(&c) == Some(&false),
                    _ => false,
                };
                if leave {
                    // Looked at again below; a count typed so far stays.
                    self.input.push_front((k, ev, remap));
                    return Step::Leave;
                }
            }
            self.pending.push((k, ev));
            let keys = self.pending();
            let exact = modes.lookup_typed(ls, &keys, &self.conds);
            let longer = modes.has_longer_typed(ls, &keys, &self.conds);
            if let Some((target, nowait, keys)) = exact
                && (!longer || nowait)
            {
                self.since = None;
                self.pending.clear();
                self.conds.clear();
                return Step::Run { target, count: self.count.take(), keys };
            }
            if longer {
                self.since.get_or_insert(now);
            } else {
                self.since = None;
                if let Some(step) = self.resolve(modes, layers) {
                    return step;
                }
            }
        }
    }

    /// Pending keys that cannot grow: run the longest mapped prefix, or pass
    /// the first key on unmapped; the rest goes back to the input.
    fn resolve<T: Clone>(&mut self, modes: &Modes<T>, layers: &Layers) -> Option<Step<T>> {
        if self.pending.is_empty() {
            return None;
        }
        let mut items = std::mem::take(&mut self.pending);
        let keys: Vec<Stroke> = items.iter().map(|(k, _)| *k).collect();
        let found = (1..=keys.len())
            .rev()
            .find_map(|n| modes.lookup_typed(&layers.modes, &keys[..n], &self.conds).map(|(t, _, read)| (n, t, read)));
        self.conds.clear();
        if let Some((n, target, read)) = found {
            let rest = items.split_off(n);
            self.requeue(rest);
            return Some(Step::Run { target, count: self.count.take(), keys: read });
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

    /// Every mode of the history, the current one first, the root last.
    pub fn modes(&self) -> impl Iterator<Item = &str> {
        self.stack.iter().rev().map(String::as_str).chain(std::iter::once(self.root.as_str()))
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

    fn k(s: &str) -> Vec<Stroke> {
        parse(s).unwrap()
    }

    fn add(m: &mut Modes<&'static str>, mode: &str, keys: &str, target: &'static str) {
        m.map(mode, k(keys), Mapping::new(target));
    }

    fn modes() -> Modes<&'static str> {
        let mut m = Modes::default();
        add(&mut m, "base", "j", "down");
        add(&mut m, "base", "g", "g");
        add(&mut m, "base", "gg", "top");
        add(&mut m, "base", "0", "fit");
        add(&mut m, "base", "x", "close");
        m.map("base", k("<C-b>"), Mapping { nowait: true, ..Mapping::new("prefix") });
        add(&mut m, "base", "<C-b>%", "split");
        add(&mut m, "term", "ab", "ab");
        m
    }

    /// Feed keys typed at `t0 + ms` in `layers` and collect what the
    /// matcher does; conditions are answered from `conds`.
    fn run_in(
        m: &Modes<&'static str>,
        layers: &Layers,
        conds: &[(Cond, bool)],
        keys: &[(&str, u64)],
        until: u64,
    ) -> Vec<String> {
        let t0 = Instant::now();
        let mut matcher = Matcher::default();
        let mut out = Vec::new();
        let mut layers = layers.clone();
        let mut drain = |matcher: &mut Matcher, now: Instant, out: &mut Vec<String>| {
            loop {
                match matcher.step(m, &layers, T, now) {
                    Step::Wait(_) => break,
                    Step::Run { target, count, .. } => out.push(match count {
                        Some(c) => format!("{c}{target}"),
                        None => target.to_owned(),
                    }),
                    Step::Unmapped(k, _) => out.push(format!("unmapped {}", k.notation())),
                    Step::NeedConditions(cs) => {
                        out.push(format!("ask {cs:?}"));
                        matcher.set_conditions(cs.iter().map(|c| (*c, conds.iter().any(|(d, v)| d == c && *v))));
                    }
                    Step::Leave => {
                        out.push("leave".into());
                        layers.modes.remove(0);
                        layers.keep = None;
                    }
                }
            }
        };
        for (s, ms) in keys {
            for key in k(s) {
                matcher.push(&key.event());
            }
            drain(&mut matcher, t0 + Duration::from_millis(*ms), &mut out);
        }
        drain(&mut matcher, t0 + Duration::from_millis(until), &mut out);
        out
    }

    fn run(m: &Modes<&'static str>, mode: &str, keys: &[(&str, u64)], until: u64) -> Vec<String> {
        run_in(m, &Layers::of(mode, mode == "base"), &[], keys, until)
    }

    #[test]
    fn sequences_wait_and_time_out_as_in_vim() {
        let m = modes();
        assert_eq!(run(&m, "base", &[("gg", 0)], 10), ["top"]);
        assert_eq!(run(&m, "base", &[("g", 0)], 500), Vec::<String>::new(), "still waiting");
        assert_eq!(run(&m, "base", &[("g", 0)], 1001), ["g"], "the shorter mapping after the timeout");
        assert_eq!(run(&m, "base", &[("g", 0), ("j", 10)], 20), ["g", "down"], "a key that does not continue");
        assert_eq!(run(&m, "base", &[("q", 0)], 10), ["unmapped q"]);
        assert_eq!(run(&m, "base", &[("<C-b>", 0)], 10), ["prefix"], "nowait runs the shorter mapping at once");
    }

    #[test]
    fn unmatched_sequences_pass_their_first_key_on() {
        let m = modes();
        // In a pass-through mode the caller sends unmapped keys to the pane.
        assert_eq!(run(&m, "term", &[("ac", 0)], 10), ["unmapped a", "unmapped c"]);
        assert_eq!(run(&m, "term", &[("ab", 0)], 10), ["ab"]);
        assert_eq!(run(&m, "term", &[("a", 0)], 1001), ["unmapped a"], "timed out");
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
    fn several_mappings_of_a_key_take_turns() {
        let mut m: Modes<&str> = Modes::default();
        add(&mut m, "n", "x", "first");
        add(&mut m, "n", "x", "second");
        assert_eq!(run(&m, "n", &[("x", 0)], 10), ["second"], "the last registered first");
        m.map("n", k("x"), Mapping { priority: 1, ..Mapping::new("urgent") });
        add(&mut m, "n", "x", "third");
        assert_eq!(run(&m, "n", &[("x", 0)], 10), ["urgent"], "a higher priority first");
        assert_eq!(m.mappings("n", None).len(), 4, "without a namespace mappings are added");
        // A namespace replaces its own mapping only.
        let ns = |t, n: &str| Mapping { namespace: Some(n.into()), ..Mapping::new(t) };
        m.map("n", k("y"), ns("a", "default"));
        m.map("n", k("y"), ns("b", "user"));
        m.map("n", k("y"), ns("c", "default"));
        let ys: Vec<&str> =
            m.mappings("n", None).iter().filter(|(k2, _)| *k2 == k("y")).map(|(_, m)| m.target).collect();
        assert_eq!(ys, ["c", "b"]);
        assert_eq!(m.mappings("n", Some("user")).len(), 1);
        // unmap: a namespace's, or all of the key's.
        m.unmap("n", &k("y"), Some("default"));
        assert_eq!(run(&m, "n", &[("y", 0)], 10), ["b"]);
        m.unmap("n", &k("x"), None);
        assert_eq!(run(&m, "n", &[("x", 0)], 10), ["unmapped x"]);
    }

    #[test]
    fn mappings_exist_only_while_their_condition_holds() {
        let mut m: Modes<&str> = Modes::default();
        let when = |t, c| Mapping { when: Some(c), ..Mapping::new(t) };
        add(&mut m, "n", "<Tab>", "plain");
        m.map("n", k("<Tab>"), when("popup", 7));
        m.map("n", k("gt"), when("next-tab", 8));
        add(&mut m, "n", "g", "g");
        let layers = Layers::of("n", false);
        assert_eq!(run_in(&m, &layers, &[(7, true)], &[("<Tab>", 0)], 10), ["ask [7]", "popup"]);
        assert_eq!(run_in(&m, &layers, &[(7, false)], &[("<Tab>", 0)], 10), ["ask [7]", "plain"], "the next one");
        // Only conditions of mappings that could take the key are asked.
        assert_eq!(run_in(&m, &layers, &[], &[("q", 0)], 10), ["unmapped q"]);
        // A disabled longer mapping does not make the key wait.
        assert_eq!(run_in(&m, &layers, &[(8, false)], &[("g", 0)], 10), ["ask [8]", "g"]);
        assert_eq!(run_in(&m, &layers, &[(8, true)], &[("g", 0)], 10), ["ask [8]"], "waits for gt");
        assert_eq!(run_in(&m, &layers, &[(8, true)], &[("gt", 0)], 10), ["ask [8]", "next-tab"]);
        // Asked once per sequence: a second sequence asks again.
        assert_eq!(
            run_in(&m, &layers, &[(7, true)], &[("<Tab><Tab>", 0)], 10),
            ["ask [7]", "popup", "ask [7]", "popup"]
        );
    }

    #[test]
    fn transient_modes_leave_or_fall_through() {
        let mut m = modes();
        add(&mut m, "resize", "+", "taller");
        let layers =
            |keep, modes: &[&str]| Layers { modes: modes.iter().map(|s| (*s).to_owned()).collect(), count: true, keep };
        // keepalive "mapped": its keys stay; another leaves, handled below.
        let mapped = layers(Some(Keepalive::Mapped), &["resize", "base"]);
        assert_eq!(run_in(&m, &mapped, &[], &[("++j", 0)], 10), ["taller", "taller", "leave", "down"]);
        assert_eq!(run_in(&m, &mapped, &[], &[("3+", 0)], 10), ["3taller"], "a count stays in");
        // "always": the modes below take what it does not map.
        let always = layers(None, &["resize", "base"]);
        assert_eq!(run_in(&m, &always, &[], &[("+j+", 0)], 10), ["taller", "down", "taller"]);
        // A function: asked at each sequence's first key.
        let func = layers(Some(Keepalive::Func(5)), &["resize", "base"]);
        assert_eq!(run_in(&m, &func, &[(5, false)], &[("+", 0)], 10), ["ask [5]", "leave", "unmapped +"]);
        assert_eq!(run_in(&m, &func, &[(5, true)], &[("+", 0)], 10), ["ask [5]", "taller"]);
    }

    #[test]
    fn keys_fed_without_mappings_and_cancel() {
        let m = modes();
        let base = Layers::of("base", true);
        let mut matcher = Matcher::default();
        let now = Instant::now();
        matcher.feed_front(&k("j"), false);
        assert_eq!(matcher.step(&m, &base, T, now), Step::Unmapped(k("j")[0], None));
        matcher.push(&k("3")[0].event());
        matcher.push(&k("g")[0].event());
        assert!(matches!(matcher.step(&m, &base, T, now), Step::Wait(Some(_))));
        assert_eq!((matcher.count(), matcher.pending().len()), (Some(3), 1));
        matcher.cancel();
        assert_eq!((matcher.count(), matcher.pending().len()), (None, 0));
        assert_eq!(matcher.step(&m, &base, T, now), Step::Wait(None));
    }

    #[test]
    fn typed_keys_try_their_own_form_then_the_fallback() {
        let mut m: Modes<&str> = Modes::default();
        add(&mut m, "m", "<Tab>", "tab");
        add(&mut m, "m", "A", "big-a");
        let typed = |m: &Modes<&'static str>, s: &str| run(m, "m", &[(s, 0)], 10);
        assert_eq!(typed(&m, "<C-i>"), ["tab"], "<C-i> does what <Tab> does");
        assert_eq!(typed(&m, "<S-a>"), ["big-a"], "Shift+a does what A does");
        add(&mut m, "m", "<C-i>", "ctrl-i");
        add(&mut m, "m", "<S-a>", "shift-a");
        assert_eq!(typed(&m, "<C-i>"), ["ctrl-i"], "unless mapped itself");
        assert_eq!(typed(&m, "<Tab>"), ["tab"]);
        assert_eq!(typed(&m, "<S-a>"), ["shift-a"]);
        assert_eq!(typed(&m, "A"), ["big-a"], "Caps Lock A stays A");
        // Sequences: the reading with the fewest fallbacks wins.
        add(&mut m, "m", "<Tab>x", "tab-x");
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
        assert_eq!(h.modes().collect::<Vec<_>>(), ["d", "c", "b", "normal"]);
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
