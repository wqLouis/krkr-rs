//! Integration tests for the platform clock and fixed-step ticker.

use std::time::{Duration, Instant};

use krkr_api::host::Clock as ClockTrait;
use krkr_platform::{Clock, Ticker};

#[test]
fn ticker_fires_on_and_after_the_deadline() {
    let interval = Duration::from_millis(16);
    let start = Instant::now();
    let mut ticker = Ticker::new(interval, start);

    assert_eq!(ticker.deadline(), start);
    assert!(ticker.poll(start), "the first frame is due at the origin");
    assert_eq!(ticker.deadline(), start + interval);
    assert!(
        !ticker.poll(start + Duration::from_millis(1)),
        "a frame is not due before the next deadline"
    );

    let late = start + Duration::from_millis(40);
    assert!(ticker.poll(late), "a frame is due after the deadline");
    assert_eq!(
        ticker.deadline(),
        late + interval,
        "a late wake-up re-bases the deadline instead of bursting"
    );
}

#[test]
fn ticker_reset_rebases_the_deadline() {
    let interval = Duration::from_millis(16);
    let start = Instant::now();
    let mut ticker = Ticker::new(interval, start);

    let later = start + Duration::from_millis(100);
    ticker.reset(later);
    assert_eq!(ticker.deadline(), later);
    assert!(ticker.poll(later));
}

#[test]
fn clock_reports_monotonic_milliseconds() {
    let clock = Clock::new();
    let first = clock.now_ms();
    let trait_first = ClockTrait::now_ms(&clock);
    assert!(first <= trait_first, "the clock is monotonic");
    assert!(clock.elapsed() < Duration::from_secs(1));
}
