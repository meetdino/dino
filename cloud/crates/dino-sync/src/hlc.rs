//! Hybrid logical clocks: when a setting was changed, comparable across devices whose clocks
//! disagree a little. Wall time first, a counter for changes within the same millisecond or after
//! seeing a later remote stamp, and the device id to break exact ties.
//!
//! The device id is chosen by the device that wrote the stamp, so a device can always win exact
//! ties. That's harmless: a signed-in device can write any value anyway.
//!
//! Kulkarni et al., "Logical Physical Clocks" (2014); the shape follows
//! <https://jaredforsyth.com/posts/hybrid-logical-clocks/>.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// How far ahead of the server's clock a stamp may be before the server refuses it, and of a
/// device's clock before the device refuses it. A device whose clock runs days ahead would
/// otherwise win every conflict until then, and a stamp near `u64::MAX` would win forever.
pub const MAX_SKEW_MS: u64 = 10 * 60 * 1000;

/// A remote stamp refused for being further ahead of this device's clock than `MAX_SKEW_MS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FutureStamp {
    pub ahead_ms: u64,
}

impl std::fmt::Display for FutureStamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stamped {} ms ahead of this device's clock", self.ahead_ms)
    }
}

impl std::error::Error for FutureStamp {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hlc {
    /// Milliseconds since the Unix epoch.
    pub wall_ms: u64,
    pub counter: u32,
    pub device: String,
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.wall_ms, self.counter, &self.device).cmp(&(other.wall_ms, other.counter, &other.device))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hlc {
    /// How far `self` is ahead of `now_ms`, when it's further than `MAX_SKEW_MS`.
    pub fn too_far_ahead(&self, now_ms: u64) -> Option<u64> {
        let ahead = self.wall_ms.saturating_sub(now_ms);
        (ahead > MAX_SKEW_MS).then_some(ahead)
    }
}

/// One device's clock. Every stamp it hands out is later than every stamp it has made or seen.
#[derive(Debug, Clone)]
pub struct Clock {
    device: String,
    last: Option<Hlc>,
}

impl Clock {
    pub fn new(device: impl Into<String>) -> Self {
        Self { device: device.into(), last: None }
    }

    /// Picks up where a saved clock left off, so stamps keep increasing across restarts.
    pub fn resume(device: impl Into<String>, last: Option<Hlc>) -> Self {
        Self { device: device.into(), last }
    }

    pub fn last(&self) -> Option<&Hlc> {
        self.last.as_ref()
    }

    /// A stamp for a change made here at `wall_ms`.
    pub fn now(&mut self, wall_ms: u64) -> Hlc {
        let (wall_ms, counter) = match &self.last {
            Some(l) if l.wall_ms >= wall_ms => after(l.wall_ms, l.counter),
            _ => (wall_ms, 0),
        };
        let next = Hlc { wall_ms, counter, device: self.device.clone() };
        self.last = Some(next.clone());
        next
    }

    /// Takes in a stamp seen from another device at `wall_ms`, so later local stamps come after it.
    /// Refuses (and ignores) a stamp further ahead of `wall_ms` than `MAX_SKEW_MS`: taking it would
    /// drag every later local stamp that far into the future.
    pub fn observe(&mut self, remote: &Hlc, wall_ms: u64) -> Result<(), FutureStamp> {
        if let Some(ahead_ms) = remote.too_far_ahead(wall_ms) {
            return Err(FutureStamp { ahead_ms });
        }
        let top = [self.last.as_ref().map_or(0, |l| l.wall_ms), remote.wall_ms, wall_ms].into_iter().max().unwrap_or(0);
        let counter = |h: Option<&Hlc>| h.filter(|h| h.wall_ms == top).map(|h| h.counter);
        let (wall_ms, counter) = match counter(self.last.as_ref()).max(counter(Some(remote))) {
            Some(c) => after(top, c),
            None => (top, 0),
        };
        self.last = Some(Hlc { wall_ms, counter, device: self.device.clone() });
        Ok(())
    }
}

/// The `(wall_ms, counter)` right after the given one: the next counter, or the next millisecond
/// when the counter is spent. (`wall_ms` saturates at `u64::MAX`, which `observe` never lets a
/// clock get near.)
fn after(wall_ms: u64, counter: u32) -> (u64, u32) {
    match counter.checked_add(1) {
        Some(c) => (wall_ms, c),
        None => (wall_ms.saturating_add(1), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_only_go_forward() {
        let mut c = Clock::new("a");
        let s1 = c.now(1000);
        let s2 = c.now(1000);
        let s3 = c.now(900); // the wall clock stepped back
        assert!(s1 < s2 && s2 < s3);
        assert_eq!((s3.wall_ms, s3.counter), (1000, 2));
    }

    #[test]
    fn a_later_remote_stamp_pulls_the_clock_along() {
        let mut a = Clock::new("a");
        let remote = Hlc { wall_ms: 5000, counter: 3, device: "b".into() };
        a.observe(&remote, 1000).unwrap();
        let s = a.now(1000);
        assert!(s > remote);
        assert_eq!((s.wall_ms, s.counter), (5000, 5));
    }

    #[test]
    fn device_breaks_an_exact_tie() {
        let x = Hlc { wall_ms: 1, counter: 0, device: "a".into() };
        let y = Hlc { wall_ms: 1, counter: 0, device: "b".into() };
        assert!(x < y);
    }

    #[test]
    fn stamps_far_in_the_future_are_caught() {
        let h = Hlc { wall_ms: 1_000_000 + MAX_SKEW_MS + 1, counter: 0, device: "a".into() };
        assert_eq!(h.too_far_ahead(1_000_000), Some(MAX_SKEW_MS + 1));
        assert_eq!(Hlc { wall_ms: 1_000_000 + MAX_SKEW_MS, ..h }.too_far_ahead(1_000_000), None);
    }

    #[test]
    fn a_stamp_far_in_the_future_is_refused_and_leaves_the_clock_alone() {
        let mut a = Clock::new("a");
        let before = a.now(1000);
        let poison = Hlc { wall_ms: u64::MAX, counter: u32::MAX, device: "evil".into() };
        assert!(matches!(a.observe(&poison, 1000), Err(FutureStamp { .. })));
        let edge = Hlc { wall_ms: 1000 + MAX_SKEW_MS + 1, counter: 0, device: "b".into() };
        assert_eq!(a.observe(&edge, 1000), Err(FutureStamp { ahead_ms: MAX_SKEW_MS + 1 }));
        assert_eq!(a.last(), Some(&before));
        let s = a.now(1000);
        assert_eq!((s.wall_ms, s.counter), (1000, 1));
    }

    #[test]
    fn a_spent_counter_moves_to_the_next_millisecond() {
        let full = Hlc { wall_ms: 1000, counter: u32::MAX, device: "a".into() };
        let mut a = Clock::resume("a", Some(full.clone()));
        let s = a.now(1000);
        assert!(s > full);
        assert_eq!((s.wall_ms, s.counter), (1001, 0));

        let mut b = Clock::new("b");
        let remote = Hlc { wall_ms: 2000, counter: u32::MAX, device: "a".into() };
        b.observe(&remote, 2000).unwrap();
        let s = b.now(2000);
        assert!(s > remote);
        assert_eq!((s.wall_ms, s.counter), (2001, 1));
    }
}
