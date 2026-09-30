//! Hybrid logical clocks: when a setting was changed, comparable across devices whose clocks
//! disagree a little. Wall time first, a counter for changes within the same millisecond or after
//! seeing a later remote stamp, and the device id to break exact ties.
//!
//! Kulkarni et al., "Logical Physical Clocks" (2014); the shape follows
//! <https://jaredforsyth.com/posts/hybrid-logical-clocks/>.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// How far ahead of the server's clock a stamp may be before the server refuses it. A device
/// whose clock runs days ahead would otherwise win every conflict until then.
pub const MAX_SKEW_MS: u64 = 10 * 60 * 1000;

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
        let next = match &self.last {
            Some(l) if l.wall_ms >= wall_ms => Hlc { wall_ms: l.wall_ms, counter: l.counter + 1, device: self.device.clone() },
            _ => Hlc { wall_ms, counter: 0, device: self.device.clone() },
        };
        self.last = Some(next.clone());
        next
    }

    /// Takes in a stamp seen from another device at `wall_ms`, so later local stamps come after it.
    pub fn observe(&mut self, remote: &Hlc, wall_ms: u64) {
        let top = [self.last.as_ref().map_or(0, |l| l.wall_ms), remote.wall_ms, wall_ms].into_iter().max().unwrap_or(0);
        let counter = |h: Option<&Hlc>| h.filter(|h| h.wall_ms == top).map(|h| h.counter + 1);
        let c = counter(self.last.as_ref()).max(counter(Some(remote))).unwrap_or(0);
        self.last = Some(Hlc { wall_ms: top, counter: c, device: self.device.clone() });
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
        a.observe(&remote, 1000);
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
}
