//! OOP-adjacent L2 semantics: script-`class`-driven misuse of the timer
//! registry and the scene graph.
//!
//! These are the shapes real game scripts produce (`class OnceTimer extends
//! Timer { ... }` stopped by its own `cancel()`, a script write gating a
//! live timer, `invalidate`-ing a layer from under an active transition,
//! dropping an object while it is armed). The contract under test: every
//! stale id is a **typed miss** (a `false` result or a typed error) —
//! never a panic, never a hang, never a silent success — and
//! cancellation/disarming/`shutdown` are honoured by the state machines
//! that store them.

use std::cell::RefCell;
use std::rc::Rc;

use engine::timer::{TimerError, TimerState};
use engine::trans::Trans;
use engine::{BitmapState, Scene};

// ---------------------------------------------------------------------------
// Cancellation and gating (the `OnceTimer` / `OnceCall` family)
// ---------------------------------------------------------------------------

/// The live bug this suite exists for: the game arms a repeating timer from
/// `class OnceTimer extends Timer { ... capacity = 1; ... }` and — verified
/// in a real-game trace — is stopped by its own script `cancel()`, not by
/// any L2 fire budget. L2 has no budget: a repeating timer keeps firing
/// until it is removed or disabled, and cancelling one member of a timer
/// family never touches its siblings.
#[test]
fn once_timer_family_is_stopped_by_cancellation_not_by_a_budget() {
    let mut timers = TimerState::new();
    let cancelled = timers
        .create_interval(1_000, Box::new(|_| {}))
        .expect("create");
    let survivor = timers
        .create_interval(1_000, Box::new(|_| {}))
        .expect("create");

    for now in [1_000u64, 2_000, 3_000] {
        assert_eq!(timers.advance(now), 2, "both fire at {now} ms");
    }

    // The script cancels one member mid-flight: every mutator of its id is
    // a typed miss.
    timers.remove(cancelled).expect("cancel");
    assert_eq!(
        timers.set_interval(cancelled, 500),
        Err(TimerError::UnknownTimer(cancelled))
    );
    assert_eq!(
        timers.set_enabled(cancelled, true),
        Err(TimerError::UnknownTimer(cancelled))
    );
    assert_eq!(
        timers.remove(cancelled),
        Err(TimerError::UnknownTimer(cancelled))
    );

    // ...the sibling keeps firing well past the old six-fire budget and
    // stays armed until it is cancelled too.
    for (i, now) in [4_000u64, 5_000, 6_000, 7_000, 8_000, 9_000, 10_000]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            timers.advance(now),
            1,
            "survivor fire {} at {now} ms",
            5 + i
        );
    }
    timers.remove(survivor).expect("cancel survivor");
    assert_eq!(timers.advance(100_000), 0);
}

/// Script-shaped gating round trip (what L3 does when the reference would
/// never post: a negative `capacity` or `interval <= 0` keeps the L2 timer
/// *unarmed*, and a later valid write arms it for real): disarming drops
/// the pending deadline outright — no advance can fire the timer, however
/// far time jumps — re-arming starts a fresh schedule from the injected
/// now, and the callback itself is untouched throughout.
#[test]
fn disarming_mid_flight_gates_delivery_and_rearming_starts_fresh() {
    let mut timers = TimerState::new();
    let hits = Rc::new(RefCell::new(0u64));
    let inner = Rc::clone(&hits);
    let id = timers
        .create_interval(10, Box::new(move |_| *inner.borrow_mut() += 1))
        .expect("create");

    assert_eq!(timers.advance(10), 1);
    assert_eq!(timers.advance(20), 1);
    assert_eq!(*hits.borrow(), 2);

    // The script gates the timer mid-flight; the deadline is gone, not
    // merely ignored.
    timers.set_enabled(id, false).expect("disarm");
    for now in [30u64, 40, 50, 10_000] {
        assert_eq!(timers.advance(now), 0, "fired at {now} ms");
    }
    assert_eq!(*hits.borrow(), 2);

    // An interval write while disarmed only stores; re-arming then starts a
    // fresh schedule from the injected now — no backlog of missed fires,
    // then the ordinary cadence with the same callback.
    timers.set_interval(id, 5).expect("store");
    timers.set_now(20_000);
    timers.set_enabled(id, true).expect("re-arm");
    assert_eq!(timers.advance(20_004), 0);
    assert_eq!(timers.advance(20_005), 1);
    assert_eq!(timers.advance(20_010), 1);
    assert_eq!(*hits.borrow(), 4);
}

// ---------------------------------------------------------------------------
// A timer removed mid-flight
// ---------------------------------------------------------------------------

/// The frame body removes a timer between arming and firing (script object
/// destroyed). The removed id must never be reported again and every
/// mutator must be a typed error.
#[test]
fn timer_removed_mid_flight_is_a_typed_miss_never_a_fire() {
    let mut timers = TimerState::new();
    let doomed = timers
        .create_interval(10, Box::new(|_| {}))
        .expect("create");
    let survivor = timers
        .create_interval(10, Box::new(|_| {}))
        .expect("create");

    // Remove before the first due tick: only the survivor fires.
    timers.remove(doomed).expect("remove");
    assert_eq!(timers.advance(10), 1);
    assert_eq!(timers.advance(20), 1);

    // Typed errors for every mutator of the removed id — never a panic.
    assert_eq!(
        timers.set_interval(doomed, 10),
        Err(TimerError::UnknownTimer(doomed))
    );
    assert_eq!(
        timers.set_enabled(doomed, true),
        Err(TimerError::UnknownTimer(doomed))
    );
    assert_eq!(timers.remove(doomed), Err(TimerError::UnknownTimer(doomed)));

    // Removing a timer *after* `advance` reported it (dispatch-time removal)
    // is safe: the next advance never mentions it again.
    timers.remove(survivor).expect("remove survivor");
    for now in [30u64, 40, 50] {
        assert_eq!(timers.advance(now), 0);
    }
}

// ---------------------------------------------------------------------------
// A layer invalidated while a transition is active
// ---------------------------------------------------------------------------

/// `invalidate` runs from inside script code while a `trans` is mid-flight
/// (the classic scene teardown during a fade). The invalidated id must
/// become a typed miss everywhere the frame body looks — and the transition
/// machine, which owns only bitmaps, must keep stepping to completion
/// against the surviving targets.
#[test]
fn layer_invalidated_while_transition_is_active_leaves_only_typed_misses() {
    let mut scene = Scene::default();
    let win = scene.add_window("game", (320, 240));
    let bg = scene.add_layer(win, None); // first layer = primary
    let victim = scene.add_layer(win, Some(bg));
    let bm = scene.add_bitmap(4, 4, vec![255; 4 * 4 * 4]);
    let victim_state = scene.layer_mut(victim).unwrap();
    victim_state.bitmap = Some(bm);
    victim_state.visible = true;
    victim_state.rect = engine::Rect::new(0, 0, 4, 4);
    scene.set_focus(victim);
    let order_before = scene.window_layer_order(win);
    assert!(order_before.contains(&victim));
    // The victim is the front hit at (1, 1) while it is alive.
    assert_eq!(scene.layer_at(bg, 1, 1, false, false), Some(victim));

    // A cross-fade is armed and half-way when the script invalidates the
    // layer the transition was going to replace.
    let mut trans = Trans::cross_fade(100.0).expect("trans");
    trans.step(50.0);
    assert!(!trans.is_complete());

    scene.invalidate_layer(victim);

    // The transition keeps running against bitmap-owned state...
    trans.step(50.0);
    assert!(trans.is_complete());
    let mut dst = BitmapState::new(4, 4, vec![0; 4 * 4 * 4]);
    let src1 = BitmapState::new(4, 4, vec![0; 4 * 4 * 4]);
    let src2 = BitmapState::new(4, 4, vec![255; 4 * 4 * 4]);
    trans
        .apply(&mut dst, &src1, &src2, (0, 0, 4, 4))
        .expect("apply after invalidation");

    // ...while the invalidated id is a typed miss in every frame-body query.
    assert!(scene.layer(victim).is_none());
    assert!(!scene.node_visible(victim));
    assert!(!scene.node_enabled(victim));
    assert!(!scene.node_focusable(victim));
    assert_eq!(scene.absolute_offset(victim), None);
    assert_eq!(scene.layer_at(bg, 0, 0, false, false), None);
    assert!(!scene.window_layer_order(win).contains(&victim));

    // No dangling ids survive: window roots, focus and primary slot are
    // clean, and the released MainImage is gone too.
    let window = scene.window(win).expect("window");
    assert!(!window.layers.contains(&victim));
    assert_eq!(window.primary_layer, Some(bg));
    assert_eq!(window.focused_layer, None);
    assert!(
        scene.bitmap(bm).is_none(),
        "released MainImage must be gone"
    );
    assert!(!scene.bitmap_referenced_by_layer(bm));

    // A repeated invalidate is a typed no-op, not a panic.
    scene.invalidate_layer(victim);
    assert!(scene.layer(victim).is_none());
}

// ---------------------------------------------------------------------------
// `Scene::window_layer_order` never returns a shutdown layer
// ---------------------------------------------------------------------------

/// A `LayerState::shutdown` record (what `invalidate` marks before the
/// record is dropped, and what a caller may mark directly) must never reach
/// the frame body through `window_layer_order` — neither as a root nor
/// through the child index — and `node_visible` must treat it as inert.
#[test]
fn window_layer_order_never_returns_a_shutdown_layer() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (320, 240));
    let root = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(root));
    let sibling = scene.add_layer(win, None);
    assert_eq!(scene.window_layer_order(win), vec![root, child, sibling]);

    // A shutdown child: dropped from the parent's subtree walk.
    scene.layer_mut(child).unwrap().shutdown = true;
    assert_eq!(scene.window_layer_order(win), vec![root, sibling]);
    assert!(!scene.node_visible(child), "shutdown layer is inert");
    assert!(!scene.node_enabled(child));

    // A shutdown root in the window's own root list: dropped from the roots
    // (its whole subtree disappears with it, shutdown child or not).
    scene.layer_mut(root).unwrap().shutdown = true;
    assert_eq!(scene.window_layer_order(win), vec![sibling]);

    // Recovery: clearing the flag restores the exact original order.
    scene.layer_mut(root).unwrap().shutdown = false;
    scene.layer_mut(child).unwrap().shutdown = false;
    assert_eq!(scene.window_layer_order(win), vec![root, child, sibling]);

    // The real `invalidate` path: the record leaves the table, so the id is
    // a typed miss and can never reappear in the order.
    scene.invalidate_layer(child);
    assert!(scene.layer(child).is_none());
    assert!(!scene.window_layer_order(win).contains(&child));
    assert_eq!(scene.window_layer_order(win), vec![root, sibling]);

    // Unknown window ids are a typed miss, not a panic.
    assert!(scene.window_layer_order(u32::MAX).is_empty());
    assert_eq!(scene.window_layer_order(win).len(), 2);
}

/// Stale ids across the whole query surface: every lookup returns a typed
/// miss for an id that was never registered or has been destroyed.
#[test]
fn stale_ids_are_typed_misses_across_the_query_surface() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let stale_layer = scene.add_layer(win, None);
    scene.invalidate_layer(stale_layer);
    assert!(scene.remove_window(win), "the window existed");

    let ghost = u32::MAX;
    assert!(scene.layer(ghost).is_none());
    assert!(scene.window(ghost).is_none());
    assert!(scene.bitmap(ghost).is_none());
    assert!(scene.font(ghost).is_none());
    assert!(!scene.node_visible(ghost));
    assert!(!scene.node_enabled(ghost));
    assert!(!scene.node_focusable(ghost));
    assert!(!scene.is_primary_layer(ghost));
    assert_eq!(scene.absolute_offset(ghost), None);
    assert_eq!(scene.layer_at(ghost, 0, 0, false, false), None);
    assert_eq!(scene.next_focusable(ghost), None);
    assert_eq!(scene.prev_focusable(ghost), None);
    assert_eq!(scene.ordered_children(ghost), Vec::<u32>::new());
    assert!(scene.window_layer_order(ghost).is_empty());
    assert_eq!(scene.main_image_pitch(ghost), 0);
    assert_eq!(scene.province_pixel(ghost, 0, 0), 0);
    assert!(!scene.remove_bitmap(ghost));
    assert!(!scene.release_bitmap(ghost));
    assert_eq!(scene.set_focus(ghost), (None, None));

    // Mutators on a stale id are typed misses / no-ops, never panics.
    assert!(scene.layer_mut(ghost).is_none());
    assert!(scene.layer(stale_layer).is_none());
    scene.invalidate_layer(ghost);
    scene.destroy_layer(ghost);
    scene.remove_layer(ghost);
}
