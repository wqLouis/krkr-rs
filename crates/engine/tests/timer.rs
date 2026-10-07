//! Integration tests for the L2 timer registry (`engine::timer`).
//!
//! The registry is clock-free: every test drives it with an explicit
//! `now_ms`, so scheduling is deterministic.

use engine::timer::{TimerConfig, TimerError, TimerFire, TimerMode, TimerState};

fn repeat(interval_ms: u64, enabled: bool) -> TimerConfig {
    TimerConfig::new(interval_ms, enabled, TimerMode::Repeat, 1)
}

#[test]
fn create_and_configure_round_trips() {
    let mut timers = TimerState::new();
    assert!(timers.is_empty());

    let id = timers
        .create(TimerConfig::new(100, false, TimerMode::Repeat, 3))
        .expect("create");
    assert_eq!(timers.len(), 1);
    assert!(timers.contains(id));
    assert_eq!(timers.interval_ms(id), Some(100));
    assert_eq!(timers.is_enabled(id), Some(false));
    assert_eq!(timers.mode(id), Some(TimerMode::Repeat));
    assert_eq!(timers.capacity(id), Some(3));
    assert_eq!(timers.count(id), Some(0));
    assert_eq!(timers.next_fire_ms(id), Some(0));

    timers.set_capacity(id, 5).expect("set capacity");
    assert_eq!(timers.capacity(id), Some(5));

    timers.remove(id).expect("remove");
    assert!(timers.is_empty());
    assert_eq!(timers.remove(id), Err(TimerError::UnknownTimer(id)));
}

#[test]
fn disabled_timer_never_fires() {
    let mut timers = TimerState::new();
    let id = timers.create(repeat(10, false)).expect("create");
    assert!(timers.advance(1_000_000).is_empty());
    assert_eq!(timers.count(id), Some(0));
    assert!(!timers.any_enabled());
}

#[test]
fn repeat_timer_fires_and_rearms_from_now() {
    let mut timers = TimerState::new();
    let id = timers.create(repeat(10, true)).expect("create");

    // An enabled timer is due on the first advance.
    assert_eq!(timers.advance(0), vec![TimerFire::new(id, 1)]);
    assert_eq!(timers.next_fire_ms(id), Some(10));

    // A late frame fires once and re-arms from `now` (no catch-up burst).
    assert_eq!(timers.advance(100), vec![TimerFire::new(id, 2)]);
    assert_eq!(timers.next_fire_ms(id), Some(110));
    assert!(timers.any_enabled());
}

#[test]
fn one_shot_disables_after_firing() {
    let mut timers = TimerState::new();
    let id = timers
        .create(TimerConfig::new(5, true, TimerMode::OneShot, 1))
        .expect("create");

    assert_eq!(timers.advance(0).len(), 1);
    assert_eq!(timers.is_enabled(id), Some(false));
    assert!(timers.advance(1000).is_empty());
    assert!(!timers.any_enabled());
}

#[test]
fn set_enabled_and_interval_rearm_from_now() {
    let mut timers = TimerState::new();
    let id = timers.create(repeat(100, false)).expect("create");

    timers.set_enabled(id, true, 50).expect("enable");
    assert_eq!(timers.next_fire_ms(id), Some(150));
    assert!(timers.advance(149).is_empty());
    assert_eq!(timers.advance(150).len(), 1);

    // Changing the interval of an enabled timer re-arms from `now`.
    timers.set_interval(id, 20, 200).expect("set interval");
    assert_eq!(timers.next_fire_ms(id), Some(220));

    timers.set_enabled(id, false, 300).expect("disable");
    assert!(timers.advance(10_000).is_empty());
}

#[test]
fn invalid_config_is_a_typed_error() {
    let mut timers = TimerState::new();
    let mut zero_interval = TimerConfig::default();
    zero_interval.interval_ms = 0;
    assert_eq!(
        timers.create(zero_interval),
        Err(TimerError::InvalidInterval)
    );
    let mut zero_capacity = TimerConfig::default();
    zero_capacity.capacity = 0;
    assert_eq!(
        timers.create(zero_capacity),
        Err(TimerError::InvalidCapacity)
    );

    let id = timers.create(TimerConfig::default()).expect("create");
    assert_eq!(
        timers.set_interval(id, 0, 0),
        Err(TimerError::InvalidInterval)
    );
    assert_eq!(timers.set_capacity(id, 0), Err(TimerError::InvalidCapacity));
}

#[test]
fn several_timers_report_in_id_order() {
    let mut timers = TimerState::new();
    let first = timers.create(repeat(10, true)).expect("first");
    let second = timers.create(repeat(5, true)).expect("second");

    let fired = timers.advance(0);
    assert_eq!(
        fired,
        vec![TimerFire::new(first, 1), TimerFire::new(second, 1),]
    );
}
