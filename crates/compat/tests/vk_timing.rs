//! The script-visible delivery of the timer catch-up a slow frame causes.
//!
//! The engine's timer registry replays the intervals a late frame missed: a
//! timer whose deadline passed fires once per missed interval, and its next
//! deadline continues the schedule it missed. The compatibility layer turns
//! every fire into one queued `onTimer` posting, and the frame body drains
//! that queue, so the script sees one posting per replayed interval — the
//! same number of postings the reference's timer thread makes over the same
//! elapsed time, bounded by the capacity the reference applies.
//!
//! These tests pin that delivery: the script's handler runs once per missed
//! interval in the frame that replayed them, the schedule does not drift,
//! and the capacity bounds the postings the way the reference's does.

mod common;

use common::vm_lock;

/// A late frame delivers one `onTimer` event per missed interval.
///
/// The timer's first deadline is 10 ms and the frame lands at 45 ms, 35 ms
/// past it: the intervals at 20, 30 and 40 ms were missed, and the fire that
/// was due at 10 ms joins them, so the script's handler runs four times in
/// that one frame.
///
/// reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the timer thread
/// computes n = (curtick - NextTick) / Interval, increments it and triggers
/// the timer n times.
/// reference/cpp/core/utils/TimerIntf.cpp:69-111 — `Fire(n)` posts n
/// `onTimer` events.
#[test]
fn a_late_frame_delivers_one_on_timer_event_per_missed_interval() {
    let _lock = vm_lock();
    let env = common::Env::new();

    env.run(
        "var hits = 0; \
         var t = new Timer(function(){ hits++; }, ''); \
         t.interval = 10; \
         t.enabled = true;",
    );

    // The harness's poll step is 16 ms, so the frame lands at 45 ms.
    env.advance_time(29);
    env.poll_timer();
    assert_eq!(
        env.eval_int("hits"),
        4,
        "the frame must replay the three missed intervals together with the \
         fire that was due"
    );

    // The schedule did not drift: the next deadline is the deadline it
    // missed plus four intervals (10 + 40), so the timer is due at 50 ms and
    // the next frame (at 61 ms) replays one more interval.
    env.poll_timer();
    assert_eq!(
        env.eval_int("hits"),
        6,
        "the next frame must replay the one interval it missed"
    );
}

/// The capacity bounds the postings a catch-up queues.
///
/// The harness's poll advances 16 ms and the timer's interval is 1 ms, so the
/// registry hands the L3 sixteen fires. The capacity is three, so the
/// postings still outstanding for the timer are capped at three: the script
/// sees three of the sixteen.
///
/// reference/cpp/core/utils/TimerIntf.cpp:69-111 — `Fire(n)` posts
/// min(n, cap - count) events.
/// reference/cpp/core/utils/TimerIntf.cpp:17,32 — the default capacity is
/// TVP_DEFAULT_TIMER_CAPACITY = 6; a script may set another one.
#[test]
fn the_capacity_bounds_the_postings_a_catch_up_queues() {
    let _lock = vm_lock();
    let env = common::Env::new();

    env.run(
        "var hits = 0; \
         var t = new Timer(function(){ hits++; }, ''); \
         t.capacity = 3; \
         t.interval = 1; \
         t.enabled = true;",
    );

    env.poll_timer();
    assert_eq!(
        env.eval_int("hits"),
        3,
        "the capacity must bound the postings the catch-up queued"
    );
}

/// Every timer object has its own capacity budget.
///
/// The reference counts the queued events per timer object — the event is
/// posted at the timer itself and the count is taken for that same object
/// (`Owner = tjs_obj` in the constructor, and the count is taken for
/// `(Owner, Owner, "onTimer")`) — so two timers owned by the same script
/// object do not share a budget.
///
/// reference/cpp/core/utils/TimerIntf.cpp:51,69-111 — the owner of a timer is
/// the timer object, and the capacity is counted for that object.
#[test]
fn two_timers_have_separate_capacity_budgets() {
    let _lock = vm_lock();
    let env = common::Env::new();

    env.run(
        "var hitsA = 0; var hitsB = 0; \
         var a = new Timer(function(){ hitsA++; }, ''); \
         var b = new Timer(function(){ hitsB++; }, ''); \
         a.capacity = 1; a.interval = 1; a.enabled = true; \
         b.capacity = 1; b.interval = 1; b.enabled = true;",
    );

    // Each timer posts min(16 fires, its own capacity of one).
    env.poll_timer();
    assert_eq!(env.eval_int("hitsA"), 1, "the first timer posts its budget");
    assert_eq!(
        env.eval_int("hitsB"),
        1,
        "the second timer posts its own budget"
    );
}
