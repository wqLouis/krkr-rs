//! Monotonic clock and fixed-step tick source.
//!
//! Both types are plain values owned by the caller (usually the event loop,
//! or the higher layer driving it) with no globals and no mutable statics
//!. The [`Clock`] is a read-only wall-clock view; the [`Ticker`] decides
//! when the next frame is due so frame pacing lives in L1.

use std::time::{Duration, Instant};

/// Smallest frame interval the ticker will honor.
///
/// A zero interval would make [`Ticker::poll`] return `true` on every wake and
/// busy-loop the event loop, so it is normalized up to this value.
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(1);

/// Largest frame interval the ticker will honor.
///
/// Clamping keeps `now + interval` far from the `Instant` overflow so the
/// deadline arithmetic stays well-defined for a pathological configuration.
const MAX_FRAME_INTERVAL: Duration = Duration::from_secs(3600);

/// A monotonic wall-clock source.
///
/// The clock is created once and reports the milliseconds elapsed since then.
/// It is also the L1 implementation of the [`api::host::Clock`] contract
/// so higher layers can observe time without owning a platform timer.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    start: Instant,
}

impl Clock {
    /// Creates a clock whose origin is the current instant.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Returns the milliseconds elapsed since the clock was created.
    pub fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Returns the duration elapsed since the clock was created.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

impl api::host::Clock for Clock {
    fn now_ms(&self) -> u64 {
        Clock::now_ms(self)
    }
}

/// A fixed-step frame pacer.
///
/// The event loop initializes the ticker when the window is created and asks
/// it on every idle turn whether a frame is due; [`Ticker::deadline`] gives
/// the instant the loop should next wake at. This keeps the frame period and
/// the `WaitUntil` scheduling in one place in L1.
#[derive(Clone, Copy, Debug)]
pub struct Ticker {
    interval: Duration,
    next: Instant,
}

impl Ticker {
    /// Creates a ticker whose first frame is due at `now`.
    ///
    /// `interval` is clamped into `[1 ms, 1 h]`: a zero interval would make
    /// the loop busy-spin and an absurdly large one could overflow the
    /// deadline arithmetic.
    pub fn new(interval: Duration, now: Instant) -> Self {
        Self {
            interval: interval.clamp(MIN_FRAME_INTERVAL, MAX_FRAME_INTERVAL),
            next: now,
        }
    }

    /// Returns the instant at which the next frame becomes due.
    pub fn deadline(&self) -> Instant {
        self.next
    }

    /// Re-bases the next deadline on `now`.
    pub fn reset(&mut self, now: Instant) {
        self.next = now;
    }

    /// Returns `true` and advances the deadline when a frame is due at `now`.
    ///
    /// The deadline advances by one interval from `now` (not from the previous
    /// deadline), so a late wake-up does not cause a burst of catch-up frames.
    /// The addition is checked; a (practically unreachable) overflow leaves
    /// the deadline at `now` rather than panicking.
    pub fn poll(&mut self, now: Instant) -> bool {
        if now >= self.next {
            self.next = now.checked_add(self.interval).unwrap_or(now);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{MAX_FRAME_INTERVAL, MIN_FRAME_INTERVAL, Ticker};

    #[test]
    fn poll_returns_true_only_on_and_after_the_deadline() {
        let start = Instant::now();
        let interval = Duration::from_millis(16);
        let mut ticker = Ticker::new(interval, start);

        assert_eq!(ticker.deadline(), start);
        assert!(ticker.poll(start));
        assert_eq!(ticker.deadline(), start + interval);
        assert!(!ticker.poll(start + Duration::from_millis(15)));
        assert!(ticker.poll(start + interval));
        assert_eq!(ticker.deadline(), start + 2 * interval);
    }

    #[test]
    fn a_late_wake_up_re_bases_the_deadline_without_catch_up() {
        // reference/cpp/core/visual/impl/VSyncTimingThread.cpp:114,141-142
        // One wake presents one frame, so a wake that arrives a long way past
        // the deadline must not produce a burst of frames.
        let start = Instant::now();
        let interval = Duration::from_millis(16);
        let mut ticker = Ticker::new(interval, start);
        let late = start + Duration::from_secs(10);

        assert!(ticker.poll(late));
        assert_eq!(ticker.deadline(), late + interval);
        assert!(
            !ticker.poll(late),
            "the deadline is re-based on the late wake, not on the missed one"
        );
    }

    #[test]
    fn the_interval_is_clamped_to_the_documented_bounds() {
        let start = Instant::now();

        let mut zero = Ticker::new(Duration::ZERO, start);
        assert!(zero.poll(start));
        assert_eq!(zero.deadline(), start + MIN_FRAME_INTERVAL);

        let mut huge = Ticker::new(Duration::MAX, start);
        assert!(huge.poll(start));
        assert_eq!(huge.deadline(), start + MAX_FRAME_INTERVAL);
    }
}
