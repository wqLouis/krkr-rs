//! Integration tests for the platform clock and fixed-step ticker.

use std::time::{Duration, Instant};

use platform::{Clock, Ticker};

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
fn ticker_normalizes_a_zero_interval() {
    let start = Instant::now();
    let mut ticker = Ticker::new(Duration::ZERO, start);

    assert!(ticker.poll(start));
    assert!(
        ticker.deadline() > start,
        "a zero interval must be raised to a positive minimum"
    );
}

#[test]
fn ticker_accepts_a_huge_interval_without_panicking() {
    let start = Instant::now();
    let mut ticker = Ticker::new(Duration::MAX, start);

    assert!(ticker.poll(start));
    assert!(ticker.deadline() >= start);
    let later = start + Duration::from_secs(1);
    assert!(
        !ticker.poll(later),
        "a clamped large interval is not due yet"
    );
}

#[test]
fn clock_reports_monotonic_milliseconds() {
    let clock = Clock::new();
    let first = clock.now_ms();
    assert!(first <= clock.now_ms(), "the clock is monotonic");
    assert!(clock.elapsed() < Duration::from_secs(1));
}
