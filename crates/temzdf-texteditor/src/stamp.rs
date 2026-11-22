//! Values that tell when they may have changed, for caches of what is
//! drawn from them: a stamp unique in the process (one counter for all),
//! taken anew on every mutable access. Equal stamps mean the same value
//! (a clone keeps its original's until either changes); a cache keyed by
//! them can only miss more often than needed, never show a stale value.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn fresh() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// `T` and its stamp: reading it goes through `Deref`, changing it through
/// `DerefMut`, which takes a new stamp (whether or not it then changes).
#[derive(Clone, Debug)]
pub struct Stamped<T> {
    value: T,
    stamp: u64,
}

impl<T> Stamped<T> {
    pub fn new(value: T) -> Self {
        Self { value, stamp: fresh() }
    }

    /// Unique to this value as it is now.
    pub fn stamp(&self) -> u64 {
        self.stamp
    }
}

impl<T: Default> Default for Stamped<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Deref for Stamped<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> DerefMut for Stamped<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.stamp = fresh();
        &mut self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mutable_access_takes_a_stamp_no_other_value_has() {
        let mut a = Stamped::new(vec![1]);
        let b = Stamped::new(vec![1]);
        assert_ne!(a.stamp(), b.stamp(), "equal values made apart");
        let c = a.clone();
        assert_eq!(a.stamp(), c.stamp(), "a clone is the same value");
        let before = a.stamp();
        assert_eq!(a.len(), 1);
        assert_eq!(a.stamp(), before, "reading keeps it");
        a.push(2);
        assert!(a.stamp() != before && a.stamp() != c.stamp() && a.stamp() != b.stamp());
    }
}
