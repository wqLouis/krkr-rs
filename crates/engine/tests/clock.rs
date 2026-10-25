//! The frame clock: the engine's own time base.
//!
//! The clock is the value the frame advances and the value the script reads
//! through `System.getTickCount`.

use engine::FrameClock;

/// A clock that accumulates the frame deltas keeps the fractional
/// millisecond, so sixty 16.6 ms frames reach 996 ms and not 960 ms.
///
/// reference/cpp/core/utils/impl/TimerImpl.cpp:113,128-146
/// Each timer check re-arms from the current tick, so a frame delta must not
/// discard its sub-millisecond remainder.
#[test]
fn a_clock_keeps_the_fractional_millisecond_across_deltas() {
    let mut clock = FrameClock::new();
    for _ in 0..60 {
        clock.advance(16.6);
    }
    assert!(
        (clock.now_f64() - 996.0).abs() < 0.01,
        "60 x 16.6 ms must reach 996 ms, got {}",
        clock.now_f64()
    );
    assert_eq!(clock.now_ms(), 996, "the tick count is whole milliseconds");
}

/// A negative delta counts as zero: the clock never runs backwards.
#[test]
fn a_negative_delta_does_not_rewind_the_clock() {
    let mut clock = FrameClock::new();
    clock.advance(10.0);
    assert_eq!(clock.advance(-5.0), 10, "a negative delta counts as zero");
    assert_eq!(clock.now_f64(), 10.0, "the clock did not rewind");
}

/// A clock that is set lands on the exact value: the fractional remainder an
/// earlier delta left behind is gone.
#[test]
fn a_set_clock_replaces_the_fractional_remainder() {
    let mut clock = FrameClock::new();
    clock.advance(10.6);
    assert!((clock.now_f64() - 10.6).abs() < 1e-3);

    clock.set(40);
    assert_eq!(clock.now_f64(), 40.0, "the clock is exactly the set value");
    assert_eq!(clock.now_ms(), 40);
}

/// The clock saturates at the integer ceiling instead of wrapping.
#[test]
fn a_saturating_delta_keeps_the_clock_monotonic() {
    let mut clock = FrameClock::new();
    assert_eq!(clock.advance(f64::MAX), u64::MAX);
    assert_eq!(clock.advance(f64::MAX), u64::MAX, "the clock stays capped");
    assert_eq!(clock.now_f64(), u64::MAX as f64);
}
