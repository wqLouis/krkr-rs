//! Integration tests for the L2 timer registry (`engine::timer`).
//!
//! The registry is clock-free: time enters only through the [`TimerState::now_ms`]
//! field (inject without delivering) and the delivery step (inject and
//! deliver), so every
//! test is
//! deterministic. The contract under test: a timer repeats by construction
//! until removed or disabled, a late frame replays the intervals the timer
//! missed (bounded, and without letting the schedule drift), due callbacks
//! run in creation order, callbacks may re-enter the registry (create timers,
//! change intervals, remove themselves, even panic), and every stale id or
//! zero interval is a typed error — never a panic, never a silent success.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use engine::timer::{TimerCallback, TimerError, TimerState, deliver_due};

/// A callback that records the firing timer's raw id into `log`.
fn recorder(log: &Rc<RefCell<Vec<u64>>>) -> TimerCallback {
    let log = Rc::clone(log);
    Box::new(move |id| log.borrow_mut().push(id.value()))
}

/// A callback that counts its invocations and never fails.
fn counter() -> (TimerCallback, Rc<Cell<u64>>) {
    let hits = Rc::new(Cell::new(0u64));
    let inner = Rc::clone(&hits);
    (Box::new(move |_| inner.set(inner.get() + 1)), hits)
}

// ---------------------------------------------------------------------------
// Scheduling basics
// ---------------------------------------------------------------------------

#[test]
fn create_interval_fires_one_interval_from_now_then_repeats() {
    let mut timers = TimerState::new();
    let log = Rc::new(RefCell::new(Vec::new()));

    timers.now_ms = 1_000;
    let id = timers.create_interval(10, recorder(&log)).expect("create");

    // Not due before the first deadline, due exactly on it.
    assert_eq!(deliver_due(&mut timers, 1_009), 0);
    assert_eq!(deliver_due(&mut timers, 1_010), 1);
    assert_eq!(log.borrow().as_slice(), &[id.value()]);

    // Repeats on the interval: nothing between deadlines, one fire each.
    assert_eq!(deliver_due(&mut timers, 1_019), 0);
    assert_eq!(deliver_due(&mut timers, 1_020), 1);

    // A late frame replays the intervals it missed: the deadline was 1_030
    // and the frame landed at 1_100, so the timer fires eight times and
    // keeps its place in the schedule.
    assert_eq!(deliver_due(&mut timers, 1_100), 8);
    assert_eq!(deliver_due(&mut timers, 1_101), 0);
    assert_eq!(deliver_due(&mut timers, 1_110), 1);
    assert_eq!(log.borrow().len(), 11);
}

#[test]
fn disabling_drops_the_deadline_and_reabling_arms_from_now() {
    let mut timers = TimerState::new();
    let (callback, hits) = counter();

    // `create_interval` arms at the injected now + interval (now = 0).
    let id = timers.create_interval(10, callback).expect("create");
    timers.set_enabled(id, false).expect("disable");
    assert_eq!(
        deliver_due(&mut timers, 1_000_000),
        0,
        "a disarmed timer never fires"
    );

    // Re-enabling arms from the injected now, not from any stale deadline.
    timers.now_ms = 5_000;
    timers.set_enabled(id, true).expect("enable");
    assert_eq!(deliver_due(&mut timers, 5_009), 0);
    assert_eq!(deliver_due(&mut timers, 5_010), 1);
    assert_eq!(hits.get(), 1);

    // Disabling keeps the callback: a later enable fires again.
    timers.set_enabled(id, false).expect("disable");
    assert_eq!(deliver_due(&mut timers, 10_000), 0);
    timers.now_ms = 20_000;
    timers.set_enabled(id, true).expect("enable");
    assert_eq!(deliver_due(&mut timers, 20_010), 1);
    assert_eq!(hits.get(), 2);
}

#[test]
fn set_interval_rearms_an_armed_timer_from_now_and_stores_while_disarmed() {
    let mut timers = TimerState::new();
    let (callback, hits) = counter();
    let id = timers.create_interval(100, callback).expect("create");

    assert_eq!(deliver_due(&mut timers, 100), 1);

    // The re-arm origin is the injected now (L3's property-write
    // timestamp), not the previous deadline and not the last poll.
    timers.now_ms = 200;
    timers.set_interval(id, 20).expect("set interval");
    assert_eq!(deliver_due(&mut timers, 219), 0);
    assert_eq!(deliver_due(&mut timers, 220), 1);

    // A disarmed timer just stores the interval; the next enable arms with
    // the new value.
    timers.set_enabled(id, false).expect("disable");
    timers.set_interval(id, 5).expect("store");
    assert_eq!(deliver_due(&mut timers, 10_000), 0);
    timers.now_ms = 300;
    timers.set_enabled(id, true).expect("enable");
    assert_eq!(deliver_due(&mut timers, 304), 0);
    assert_eq!(deliver_due(&mut timers, 305), 1);
    assert_eq!(hits.get(), 3);
}

#[test]
fn a_repeating_timer_fires_until_it_is_removed() {
    let mut timers = TimerState::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    let id = timers.create_interval(10, recorder(&log)).expect("create");

    // 101 fires — an unsuppressed repeating timer must never disable
    // itself: it fires every interval until the owner removes (cancels) or
    // disables it.
    for i in 1..=101u64 {
        assert_eq!(
            deliver_due(&mut timers, i * 10),
            1,
            "fire {i} at {} ms",
            i * 10
        );
    }
    assert_eq!(log.borrow().len(), 101);

    timers.remove(id).expect("cancel");
    assert_eq!(deliver_due(&mut timers, 1_000_000), 0);
    assert_eq!(log.borrow().len(), 101, "a removed timer never fires again");
}

#[test]
fn due_callbacks_run_in_creation_order() {
    let mut timers = TimerState::new();
    timers.now_ms = 500;
    let log = Rc::new(RefCell::new(Vec::new()));
    let first = timers.create_interval(10, recorder(&log)).expect("first");
    let second = timers.create_interval(10, recorder(&log)).expect("second");
    let third = timers.create_interval(10, recorder(&log)).expect("third");

    assert_eq!(deliver_due(&mut timers, 510), 3);
    assert_eq!(
        *log.borrow(),
        vec![first.value(), second.value(), third.value()],
        "simultaneous timers must fire in creation order"
    );
}

// ---------------------------------------------------------------------------
// Typed errors
// ---------------------------------------------------------------------------

#[test]
fn zero_intervals_and_stale_ids_are_typed_errors() {
    let mut timers = TimerState::new();

    // A zero interval is not representable in L2: it would be due on every
    // delivery. L3 maps a script `interval <= 0` onto not arming instead.
    assert_eq!(
        timers.create_interval(0, Box::new(|_| {})),
        Err(TimerError::InvalidInterval)
    );

    let id = timers
        .create_interval(10, Box::new(|_| {}))
        .expect("create");
    assert_eq!(timers.set_interval(id, 0), Err(TimerError::InvalidInterval));

    // Removed ids are typed errors for every mutator (`TimerId` is
    // `#[non_exhaustive]`, so a ghost is made by cancelling a real timer).
    timers.remove(id).expect("remove");
    assert_eq!(
        timers.set_interval(id, 10),
        Err(TimerError::UnknownTimer(id))
    );
    assert_eq!(
        timers.set_enabled(id, true),
        Err(TimerError::UnknownTimer(id))
    );
    assert_eq!(timers.remove(id), Err(TimerError::UnknownTimer(id)));
    assert_eq!(deliver_due(&mut timers, u64::MAX), 0);
}

#[test]
fn now_ms_injects_time_without_delivering() {
    let mut timers = TimerState::new();
    let (callback, hits) = counter();
    let id = timers.create_interval(10, callback).expect("create");

    // `now_ms` past the deadline must neither fire nor reschedule: only
    // the delivery fires, and the deadline is untouched.
    timers.now_ms = 1_000_000;
    assert_eq!(deliver_due(&mut timers, 9), 0);
    assert_eq!(deliver_due(&mut timers, 10), 1);
    assert_eq!(hits.get(), 1);
    timers.remove(id).expect("remove");
}

// ---------------------------------------------------------------------------
// Re-entrancy: callbacks mutate the registry mid-delivery
// ---------------------------------------------------------------------------

#[test]
fn a_callback_removing_itself_leaves_consistent_state() {
    let mut timers = TimerState::new();
    // SAFETY: `timers` lives on this stack frame; `registry` is only
    // dereferenced from inside a callback the delivery invokes,
    // strictly sequentially with the delivery's own accesses (the documented
    // single-threaded re-entrancy contract). Borrowck accepts the raw
    // pointer because the `&mut` that produced it ends at this statement.
    let registry = &mut timers as *mut TimerState;
    let (callback, hits) = counter();
    let wrapped: TimerCallback = {
        let mut callback = callback;
        Box::new(move |id| {
            callback(id);
            // SAFETY: see above — the registry is mid-delivery, which
            // expects exactly this re-entrant shape.
            unsafe { (*registry).remove(id) }.expect("remove self");
        })
    };

    let id = timers.create_interval(10, wrapped).expect("create");
    assert_eq!(deliver_due(&mut timers, 10), 1);
    assert_eq!(hits.get(), 1);

    // The entry is gone: removing again is a typed error, and no later
    // advance can ever fire it again.
    assert_eq!(timers.remove(id), Err(TimerError::UnknownTimer(id)));
    assert_eq!(deliver_due(&mut timers, 10_000), 0);
    assert_eq!(hits.get(), 1);
}

#[test]
fn a_callback_creating_a_timer_joins_the_schedule() {
    let mut timers = TimerState::new();
    // SAFETY: see `a_callback_removing_itself_leaves_consistent_state`.
    let registry = &mut timers as *mut TimerState;
    let log = Rc::new(RefCell::new(Vec::new()));
    let created = Rc::new(Cell::new(false));

    let outer_log = Rc::clone(&log);
    let created_flag = Rc::clone(&created);
    let outer_callback: TimerCallback = Box::new(move |id| {
        outer_log.borrow_mut().push(id.value());
        if !created_flag.get() {
            created_flag.set(true);
            // SAFETY: see above; creating a timer from inside a running
            // callback is the documented re-entrant shape.
            unsafe { (*registry).create_interval(10, recorder(&outer_log)) }
                .expect("create inner timer");
        }
    });
    let outer = timers
        .create_interval(10, outer_callback)
        .expect("create outer");

    // The outer fires alone; the inner timer joins at the injected now
    // (the delivery's `now_ms`), so its first deadline is one interval out.
    assert_eq!(deliver_due(&mut timers, 10), 1);
    assert_eq!(deliver_due(&mut timers, 20), 2);
    assert_eq!(deliver_due(&mut timers, 30), 2);
    let log = log.borrow();
    assert_eq!(
        log[..2],
        [outer.value(), outer.value()],
        "the outer timer must not be double-invoked by the nested creation"
    );
    assert_eq!(
        log[2],
        outer.value() + 1,
        "the inner timer's id follows the outer's"
    );
    assert_eq!(
        log[3..],
        [outer.value(), outer.value() + 1],
        "creation order is preserved in later rounds: outer, then the \
         inner timer it created"
    );
    assert_eq!(log.len(), 5);
    drop(log);
    assert!(created.get());
}

#[test]
fn a_callback_changing_its_own_interval_reschedules() {
    let mut timers = TimerState::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    // SAFETY: see `a_callback_removing_itself_leaves_consistent_state`.
    let registry = &mut timers as *mut TimerState;
    let rescheduled = Rc::new(Cell::new(false));
    let flag = Rc::clone(&rescheduled);
    let log_out = Rc::clone(&log);
    let callback: TimerCallback = Box::new(move |id| {
        log_out.borrow_mut().push(id.value());
        if !flag.get() {
            flag.set(true);
            // Re-arm from the delivery's `now_ms` (10): the next fire moves
            // from 20 to 60.
            // SAFETY: see above.
            unsafe { (*registry).set_interval(id, 50) }.expect("set interval");
        }
    });
    let id = timers.create_interval(10, callback).expect("create");

    assert_eq!(deliver_due(&mut timers, 10), 1);
    assert_eq!(
        deliver_due(&mut timers, 20),
        0,
        "the interval change must reschedule"
    );
    assert_eq!(deliver_due(&mut timers, 60), 1);
    assert_eq!(log.borrow().len(), 2);
    timers.remove(id).expect("remove");
}

#[test]
fn a_peer_disabled_from_a_callback_is_cancelled_before_it_fires() {
    let mut timers = TimerState::new();
    // SAFETY: see `a_callback_removing_itself_leaves_consistent_state`.
    let registry = &mut timers as *mut TimerState;
    let log = Rc::new(RefCell::new(Vec::new()));
    let peer_slot: Rc<Cell<Option<engine::timer::TimerId>>> = Rc::new(Cell::new(None));
    let disabled_once = Rc::new(Cell::new(false));

    // The first timer disables the second while both are due in the same
    // delivery: the second's pending fire must be cancelled.
    let slot = Rc::clone(&peer_slot);
    let once = Rc::clone(&disabled_once);
    let killer_log = Rc::clone(&log);
    let killer = timers
        .create_interval(
            10,
            Box::new(move |_| {
                killer_log.borrow_mut().push(0u64);
                if !once.get() {
                    once.set(true);
                    if let Some(peer) = slot.get() {
                        // SAFETY: see above.
                        unsafe { (*registry).set_enabled(peer, false) }.expect("disable peer");
                    }
                }
            }),
        )
        .expect("create killer");
    let peer = timers
        .create_interval(10, recorder_marker(&log))
        .expect("peer");
    peer_slot.set(Some(peer));
    assert_ne!(killer.value(), peer.value());

    assert_eq!(
        deliver_due(&mut timers, 10),
        1,
        "only the killer fires this round"
    );
    assert_eq!(*log.borrow(), vec![0u64]);

    // The killer keeps repeating; the peer stays disarmed.
    assert_eq!(deliver_due(&mut timers, 20), 1);
    assert_eq!(log.borrow().len(), 2);

    // Re-enabling the peer brings it back into the rounds.
    timers.now_ms = 30;
    timers.set_enabled(peer, true).expect("re-enable peer");
    // The killer's deadline was 30 and the frame landed at 40, so the killer
    // replays the interval it missed and fires twice; the peer fires once.
    assert_eq!(deliver_due(&mut timers, 40), 3);
    assert_eq!(*log.borrow(), vec![0u64, 0u64, 0u64, 0u64, 1u64]);
}

/// A callback that records a fixed `1` marker (the peer's tag).
fn recorder_marker(log: &Rc<RefCell<Vec<u64>>>) -> TimerCallback {
    let log = Rc::clone(log);
    Box::new(move |_| log.borrow_mut().push(1u64))
}

#[test]
fn a_panicking_callback_unwinds_over_consistent_state() {
    let mut timers = TimerState::new();
    let (callback, hits) = counter();
    let panic_next = Rc::new(Cell::new(true));
    let should_panic = Rc::clone(&panic_next);
    let wrapped: TimerCallback = {
        let mut callback = callback;
        Box::new(move |id| {
            callback(id);
            if should_panic.get() {
                should_panic.set(false);
                panic!("boom");
            }
        })
    };
    let id = timers.create_interval(10, wrapped).expect("create");

    // Silence the panic hook for the duration (process-global; restored
    // immediately so parallel tests keep their output).
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        deliver_due(&mut timers, 10)
    }));
    std::panic::set_hook(previous_hook);

    // The panic propagates out of the delivery — L2 does not swallow it.
    assert!(result.is_err(), "the callback's panic must not be caught");

    // ...but the registry is consistent: the callback was restored, the
    // schedule kept running, and the next deadline fires normally.
    assert_eq!(hits.get(), 1);
    assert_eq!(deliver_due(&mut timers, 20), 1);
    assert_eq!(deliver_due(&mut timers, 30), 1);
    assert_eq!(hits.get(), 3);
    timers.remove(id).expect("remove");
}

// ---------------------------------------------------------------------------
// The late frame: the missed intervals are replayed
// ---------------------------------------------------------------------------

#[test]
fn a_late_frame_replays_the_missed_intervals_without_drift() {
    let mut timers = TimerState::new();
    let (callback, hits) = counter();
    // The timer is armed at the injected now (0), so its first deadline is
    // one interval out: 10 ms.
    timers.create_interval(10, callback).expect("create");

    // The frame lands 35 ms past that deadline (at 45 ms). The intervals
    // the timer missed are the ones at 20, 30 and 40 ms, and the fire that
    // was due at 10 ms joins them: four fires in this one frame.
    //
    // reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the timer
    // thread computes n = (curtick - NextTick) / Interval, increments it,
    // and triggers the timer n times, re-arming from the deadline it
    // missed: NextTick + n * Interval.
    assert_eq!(deliver_due(&mut timers, 45), 4);
    assert_eq!(hits.get(), 4);

    // The next deadline is the old deadline plus four intervals (10 + 40),
    // so the schedule did not drift: the timer is due again at 50 ms, not
    // at 55 ms.
    assert_eq!(deliver_due(&mut timers, 49), 0);
    assert_eq!(deliver_due(&mut timers, 50), 1);
    assert_eq!(hits.get(), 5);
}

#[test]
fn a_backlog_beyond_the_cap_fires_once_and_rearms_from_now() {
    let mut timers = TimerState::new();
    let (callback, hits) = counter();
    timers.create_interval(10, callback).expect("create");

    // Forty missed intervals are still replayed: the deadline was 10 ms,
    // the frame lands at 400 ms, so n = (400 - 10) / 10 + 1 = 40 fires, and
    // the next deadline is the old one plus forty intervals.
    //
    // reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the catch-up
    // count is n = (curtick - NextTick) / Interval + 1.
    assert_eq!(deliver_due(&mut timers, 400), 40);
    assert_eq!(hits.get(), 40);
    assert_eq!(deliver_due(&mut timers, 409), 0);
    assert_eq!(deliver_due(&mut timers, 410), 1);
    assert_eq!(hits.get(), 41);

    // One interval more is past the cap: the backlog is discarded, the
    // timer fires once and the next deadline starts from now.
    //
    // reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — n > 40 discards
    // the rest: Trigger(1) and SetNextTick(curtick + Interval).
    let (callback, hits) = counter();
    let mut timers = TimerState::new();
    timers.create_interval(10, callback).expect("create");
    assert_eq!(deliver_due(&mut timers, 411), 1);
    assert_eq!(hits.get(), 1);
    assert_eq!(deliver_due(&mut timers, 420), 0);
    assert_eq!(deliver_due(&mut timers, 421), 1);
    assert_eq!(hits.get(), 2);
}

#[test]
fn a_schedule_write_during_a_catch_up_cancels_the_remaining_fires() {
    let mut timers = TimerState::new();
    // SAFETY: see `a_callback_removing_itself_leaves_consistent_state`.
    let registry = &mut timers as *mut TimerState;
    let hits = Rc::new(Cell::new(0u64));
    let once = Rc::new(Cell::new(false));
    let hits_ref = Rc::clone(&hits);
    let flag = Rc::clone(&once);
    let callback: TimerCallback = Box::new(move |id| {
        hits_ref.set(hits_ref.get() + 1);
        if !flag.get() {
            flag.set(true);
            // The reschedule cancels the timer's remaining fires, the way a
            // cancelled pending event is dropped.
            //
            // reference/cpp/core/utils/impl/TimerImpl.cpp:73-120 — a
            // schedule write cancels the queued events and re-derives the
            // next tick.
            //
            // SAFETY: see above.
            unsafe { (*registry).set_interval(id, 100) }.expect("set interval");
        }
    });
    timers.create_interval(10, callback).expect("create");

    // The frame lands at 45 ms: four fires are due, and the first one
    // reschedules the timer, so only the first fire happens.
    assert_eq!(deliver_due(&mut timers, 45), 1);
    assert_eq!(hits.get(), 1);

    // The reschedule re-armed the timer at the delivery's now (45) plus
    // the new interval, so it is due at 145 ms.
    assert_eq!(deliver_due(&mut timers, 144), 0);
    assert_eq!(deliver_due(&mut timers, 145), 1);
    assert_eq!(hits.get(), 2);
}
