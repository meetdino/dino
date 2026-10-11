//! Output tokens per second across every session, live: what the sidebar shows while agents write.
//! Beside it, the new input per second: what agents send that the prompt cache doesn't already
//! hold (fresh input and what's newly written to the cache), counted exactly as each answer says,
//! as soon as it says it (Anthropic's `message_start`, else as the answer ends). Each call sends
//! the whole conversation again, mostly read back from the cache; those reads aren't counted.
//!
//! An answer says how many tokens it wrote only as it ends (Anthropic's last `message_delta`, a
//! Responses API's `response.completed`), so counting those alone would show nothing through a
//! long answer and all of it in the second it ends. While an answer streams, each chunk counts as
//! the tokens its bytes likely are, from the tokens per streamed byte of the answers before it;
//! as it ends, what it said it wrote less what was counted is counted then. Every answer adds up
//! to its own count exactly; only when within it its tokens are counted is a guess.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Seconds averaged over.
pub const WINDOW: u64 = 10;
/// One slot more than the window: the second being counted isn't in it yet.
const SLOTS: usize = WINDOW as usize + 1;
/// Tokens per streamed byte before any answer has said: Anthropic's text deltas carry about 100
/// bytes of event around a few tokens each.
const FIRST_GUESS: f64 = 1.0 / 30.0;
/// An answer's weight in the tokens per byte, against the ones before it.
const LEARN: f64 = 0.2;

pub struct Rate {
    inner: Mutex<Inner>,
    /// Woken as an answer starts or ends, so a wait for a change can sleep while none streams.
    wake: Condvar,
    origin: Instant,
}

/// Thousandths of tokens counted in each second (since `origin`): (that second, its count).
#[derive(Default)]
struct Slots([(u64, i64); SLOTS]);

struct Inner {
    output: Slots,
    input: Slots,
    /// Tokens and streamed bytes of the answers so far, older ones weighing less.
    tokens: f64,
    bytes: f64,
    /// Answers streaming now.
    streaming: usize,
}

/// One answer's part: its bytes so far, and what's been counted for them.
#[derive(Default)]
pub(crate) struct Live {
    bytes: u64,
    counted: i64,
    began: bool,
    /// New input tokens counted so far.
    input: u64,
}

impl Default for Rate {
    fn default() -> Self {
        Rate { inner: Mutex::new(Inner { output: Slots::default(), input: Slots::default(), tokens: 0.0, bytes: 0.0, streaming: 0 }), wake: Condvar::new(), origin: Instant::now() }
    }
}

impl Inner {
    fn per_byte(&self) -> f64 {
        if self.bytes > 0.0 { self.tokens / self.bytes } else { FIRST_GUESS }
    }

    /// Both rates over the window before `second`, and whether either can still change.
    fn rates(&self, second: u64) -> (u32, u32, bool) {
        let (output, a) = self.output.window(second);
        let (input, b) = self.input.window(second);
        (Rate::rounded(output), Rate::rounded(input), a || b)
    }
}

impl Slots {
    fn add(&mut self, second: u64, milli: i64) {
        let slot = &mut self.0[(second % SLOTS as u64) as usize];
        if slot.0 != second {
            *slot = (second, 0);
        }
        slot.1 += milli;
    }

    /// Thousandths of tokens in the `WINDOW` whole seconds before `second`, and whether those or
    /// `second` itself counted anything (the rate can still change).
    fn window(&self, second: u64) -> (i64, bool) {
        let first = second.saturating_sub(WINDOW);
        let counted = self.0.iter().filter(|(s, m)| *s >= first && *s <= second && *m != 0);
        counted.fold((0, false), |(sum, _), (s, m)| (sum + if *s < second { *m } else { 0 }, true))
    }
}

impl Rate {
    fn second(&self) -> u64 {
        self.origin.elapsed().as_secs()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `n` bytes of a streamed answer came.
    pub(crate) fn chunk(&self, live: &mut Live, n: usize) {
        let second = self.second();
        let mut g = self.lock();
        if !live.began {
            live.began = true;
            g.streaming += 1;
            self.wake.notify_all();
        }
        live.bytes += n as u64;
        let milli = (n as f64 * g.per_byte() * 1000.0).round() as i64;
        live.counted += milli;
        g.output.add(second, milli);
    }

    /// The answer has said it read `input` new tokens so far: what's more than counted counts now.
    pub(crate) fn input(&self, live: &mut Live, input: u64) {
        if input <= live.input {
            return;
        }
        let second = self.second();
        let mut g = self.lock();
        g.input.add(second, (input - live.input) as i64 * 1000);
        live.input = input;
        self.wake.notify_all();
    }

    /// The answer ended, having written `output` tokens and read `input` new ones (0 if it never
    /// said).
    pub(crate) fn ended(&self, live: &mut Live, output: u64, input: u64) {
        self.input(live, input);
        let second = self.second();
        let mut g = self.lock();
        if live.began {
            g.streaming -= 1;
        }
        g.output.add(second, output as i64 * 1000 - live.counted);
        // Learned from answers big enough that their first and last events don't make them up.
        if live.bytes >= 2048 && output > 0 {
            g.tokens = g.tokens * (1.0 - LEARN) + output as f64 * LEARN;
            g.bytes = g.bytes * (1.0 - LEARN) + live.bytes as f64 * LEARN;
        }
        *live = Live::default();
        self.wake.notify_all();
    }

    /// Output and new input tokens per second over the last `WINDOW` whole seconds.
    pub fn per_second(&self) -> (u32, u32) {
        let (output, input, _) = self.lock().rates(self.second());
        (output, input)
    }

    fn rounded(milli: i64) -> u32 {
        (milli.max(0) as f64 / 1000.0 / WINDOW as f64).round() as u32
    }

    /// The rates (output, new input) once they aren't `seen`, or after `max` regardless. Looks once
    /// a second while answers stream or the window still holds some; otherwise sleeps until one
    /// starts or ends. With no input `seen` (a client older than it), only the output is compared.
    pub fn changed(&self, seen: Option<u32>, seen_input: Option<u32>, max: Duration) -> (u32, u32) {
        let until = Instant::now() + max;
        let mut g = self.lock();
        loop {
            let elapsed = self.origin.elapsed();
            let (output, input, any) = g.rates(elapsed.as_secs());
            let now = Instant::now();
            if seen != Some(output) || seen_input.is_some_and(|s| s != input) || now >= until {
                return (output, input);
            }
            let left = until - now;
            let wait = if g.streaming == 0 && !any { left } else { (Duration::from_secs(elapsed.as_secs() + 1) - elapsed).min(left) };
            g = self.wake.wait_timeout(g, wait).map_or_else(|e| e.into_inner().0, |r| r.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_answer_adds_up_to_what_it_said() {
        let rate = Rate::default();
        let mut live = Live::default();
        for _ in 0..100 {
            rate.chunk(&mut live, 300);
        }
        let streamed = rate.lock().output.window(u64::MAX / 2).0;
        assert_eq!(streamed, 0, "outside the window");
        let now = rate.second();
        let (milli, any) = rate.lock().output.window(now + 1);
        assert!(any && milli == 1000 * 1000, "30 000 bytes at 1/30 is 1000 tokens: {milli}");
        rate.ended(&mut live, 1200, 0);
        assert_eq!(rate.lock().output.window(now + 1).0, 1200 * 1000, "the answer's own count, exactly");
        assert_eq!(rate.lock().streaming, 0);
        // Learned: 1200 tokens in 30 000 bytes.
        assert!((rate.lock().per_byte() - 0.04).abs() < 1e-9);
        assert_eq!(Rate::rounded(1200 * 1000), 120);
    }

    #[test]
    fn an_answer_not_streamed_counts_as_it_ends() {
        let rate = Rate::default();
        rate.ended(&mut Live::default(), 50, 0);
        assert_eq!(rate.lock().output.window(rate.second() + 1).0, 50_000);
        assert_eq!(rate.lock().streaming, 0, "never began");
        assert!((rate.lock().per_byte() - FIRST_GUESS).abs() < 1e-12, "nothing to learn from");
    }

    #[test]
    fn a_wait_returns_on_a_change_and_sleeps_while_nothing_streams() {
        let rate = std::sync::Arc::new(Rate::default());
        let started = Instant::now();
        assert_eq!(rate.changed(Some(0), Some(0), Duration::from_millis(50)), (0, 0));
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert_eq!(rate.changed(Some(7), Some(0), Duration::from_secs(5)), (0, 0), "differs from what was seen: at once");
        assert_eq!(rate.changed(Some(0), Some(7), Duration::from_secs(5)), (0, 0), "the input too");
        let r = rate.clone();
        let waiting = std::thread::spawn(move || r.changed(Some(0), Some(0), Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(50));
        rate.ended(&mut Live::default(), 1000, 0);
        // Counted in this second: the window has it once the second is over.
        assert_eq!(waiting.join().unwrap(), (100, 0));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn new_input_counts_once_as_soon_as_an_answer_says_it() {
        let rate = Rate::default();
        let mut live = Live::default();
        let now = rate.second();
        // `message_start` says it read 2000 new tokens; later events say it again.
        rate.input(&mut live, 2000);
        rate.input(&mut live, 2000);
        assert_eq!(rate.lock().input.window(now + 1).0, 2000 * 1000, "counted once, before the answer ends");
        rate.ended(&mut live, 10, 2500);
        assert_eq!(rate.lock().input.window(now + 1).0, 2500 * 1000, "what the end says more counts then");
        assert_eq!(Rate::rounded(2500 * 1000), 250);
    }

    #[test]
    fn an_older_client_waits_on_the_output_alone() {
        let rate = Rate::default();
        rate.input(&mut Live::default(), 5000);
        let started = Instant::now();
        // The input in the window differs from nothing seen; the output (0) is as seen: it waits.
        assert_eq!(rate.changed(Some(0), None, Duration::from_millis(50)).0, 0);
        assert!(started.elapsed() >= Duration::from_millis(50));
    }
}
