//! Script-OOP dispatch tests: `extends` / `super` / overrides / `objthis`.
//!
//! Every frame-body and host seam that invokes a member **by name** must look
//! the member up *on the object* (the reference event system posts to the
//! object, whose class chain resolves a script subclass's override before the
//! native default — `reference/cpp/core/utils/TimerIntf.cpp:85` posts
//! `"onTimer"` at `Owner`). These tests instantiate real script subclasses of
//! the native classes and prove that:
//!
//! * a subclass override runs instead of the native member (the live-game
//!   `OnceTimer extends Timer` failure), including when the override relies
//!   on state stored by `super.Native(...)`;
//! * `super.Native(...)` captures the *subclass* instance as `objthis`, so the
//!   retained dispatch target resolves back through the subclass;
//! * `Timer.capacity` follows the reference queue-depth semantics
//!   (`reference/cpp/core/utils/TimerIntf.cpp:69-92`): `capacity > 0`
//!   (including the default 6) keeps a timer repeating, `0` is the
//!   reference "no limit" sentinel, and `< 0` never delivers while the
//!   timer stays armed and enabled;
//! * `invalidate` of a subclass instance tears the native payload down in the
//!   right order (the dispatch stops, `isvalid` is false, no crash).

mod common;

use std::cell::RefCell;

use common::vm_lock;
use compat::CompatHost;
use engine::FrameClock;
use engine::Scene;
use engine::Storage;
use engine::input::InputState;
use engine::timer::{TimerState, deliver_due};
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached scene, timer registry and input state.
///
/// The boxes are declared after `_host`, so they outlive the host's raw
/// pointers (fields drop in declaration order).
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Read back by the layer test to confirm the subclass instance really
    /// landed in the L2 scene.
    scene: Box<Scene>,
    /// The attached timer registry; tests move its injected time directly so a
    /// poll never depends on the wall clock.
    timer: RefCell<Box<TimerState>>,
    _input: Box<InputState>,
    /// Only [`Env::with_storage`] mounts a storage (the KAG scenario test);
    /// declared after `_host` so it outlives the binding's raw pointer.
    _storage: Option<Box<Storage>>,
    /// The temp game dir, removed after the storage drops.
    _game_dir: Option<std::path::PathBuf>,
    /// The engine's frame clock: the transition's phase follows it.
    clock: RefCell<Box<FrameClock>>,
}

impl Env {
    fn new() -> Env {
        Env::build(false)
    }

    /// A bound Env with a mounted L2 storage holding a `scenario.ks`.
    fn with_storage() -> Env {
        Env::build(true)
    }

    fn build(with_storage: bool) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut input = Box::new(InputState::new());
        let mut clock = Box::new(FrameClock::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene, timer registry, input state and clock
        // outlive the host (they are declared after `_host`), matching
        // `attach_*`.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
            host.services_mut()
                .attach_clock(clock.as_mut() as *mut FrameClock);
        }
        let (storage, game_dir) = if with_storage {
            let mut dir = std::env::temp_dir();
            dir.push(format!(
                "krkr-extend-dispatch-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("create temp game dir");
            std::fs::write(
                dir.join("scenario.ks"),
                b"*start\n; a label and a tag\n[l]\n",
            )
            .expect("write scenario.ks");
            let mut storage = Box::new(
                Storage::mount_archives(
                    fs::FileSystem::host(&dir),
                    dir.clone(),
                    std::iter::empty::<&str>(),
                )
                .expect("mount storage"),
            );
            // SAFETY: boxed and declared after `_host`, so it outlives the
            // binding (same contract as the other attachments).
            unsafe { host.services_mut().attach_storage(&mut *storage) };
            (Some(storage), Some(dir))
        } else {
            (None, None)
        };
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            scene,
            timer: RefCell::new(timer),
            _input: input,
            _storage: storage,
            _game_dir: game_dir,
            clock: RefCell::new(clock),
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "extend-dispatch-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "extend-dispatch-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_ok(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "extend-dispatch-test")
            .map_err(|error| error.to_string())
    }

    /// Move the attached timer registry's injected time forward by `ms`.
    fn advance_time(&self, ms: u64) {
        let mut timer = self.timer.borrow_mut();
        timer.now_ms = timer.now_ms.saturating_add(ms);
    }

    /// Deliver the timers due at the registry's injected time.
    ///
    /// The registry's pump belongs to the engine; a test that owns the
    /// registry drives it directly.
    fn deliver_due(&self) {
        let mut timer = self.timer.borrow_mut();
        let now = timer.now_ms;
        deliver_due(timer.as_mut(), now);
    }

    /// Run one frame-body `timer_poll` step at a virtual 16 ms frame step;
    /// returns whether any timer is still enabled afterwards (the poll's own
    /// convention).
    fn poll_timer(&self) -> bool {
        self.advance_time(16);
        self.deliver_due();
        let mut ctx = self._host.ctx().expect("bound host");
        compat::timer_poll(&mut ctx).expect("timer poll")
    }

    /// Run one frame-body `async_trigger_poll` step.
    fn poll_async(&self) {
        let mut ctx = self._host.ctx().expect("bound host");
        compat::async_trigger_poll(&mut ctx).expect("async trigger poll");
    }
}

// ---------------------------------------------------------------------------
// Timer: the live-game `OnceTimer extends Timer` pattern
// ---------------------------------------------------------------------------

/// The game pattern: `class OnceTimer extends Timer` keeps state stored by
/// `super.Timer(...)` and overrides `onTimer`. The frame-body dispatch must
/// reach the *script override* (not the native `Timer::onTimer`), through the
/// **subclass instance** retained as `objthis` in the native constructor, and
/// the override must be able to call back into the stored owner.
///
/// The harness's poll advances 16 ms and the timer's interval is 1 ms, so
/// the registry replays the sixteen intervals the frame covered: the timer
/// fires sixteen times and the capacity bounds the `onTimer` postings still
/// outstanding for it, so the script sees six of them.
///
/// reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the catch-up count
/// is n = (curtick - NextTick) / Interval + 1.
/// reference/cpp/core/utils/TimerIntf.cpp:69-111 — `Fire(n)` posts
/// min(n, cap - count) events.
/// reference/cpp/core/utils/TimerIntf.cpp:17,32 — the default capacity is
/// TVP_DEFAULT_TIMER_CAPACITY = 6.
#[test]
fn timer_subclass_override_wins_over_the_native_on_timer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var hits = 0; var cb = 0; var ends = 0; \
         class OnceTimer extends Timer { \
             var _owner = null; \
             var _endFunc = null; \
             function OnceTimer(owner, time, endFunc) { \
                 super.Timer(owner, ''); \
                 interval = time; \
                 _owner = owner; \
                 _endFunc = endFunc; \
             } \
             function onTimer() { \
                 hits++; \
                 if (_owner) _owner(); \
             } \
         } \
         var ot = new OnceTimer(function(){ cb++; }, 1, function(){ ends++; }); \
         ot.enabled = 1;",
    );
    env.poll_timer();

    assert_eq!(
        env.eval_int("hits"),
        6,
        "the script subclass `onTimer` must run instead of the native Timer::onTimer"
    );
    assert_eq!(
        env.eval_int("cb"),
        6,
        "the owner stored by the subclass runs once per posting (once via the \
         override, never additionally via the native ctor-callback path)"
    );
    assert_eq!(
        env.eval_int("ends"),
        0,
        "the stored `_endFunc` is untouched until the subclass calls it"
    );
    assert_eq!(env.eval_int("ot.enabled"), 1, "still repeating");
}

/// A subclass that does *not* override `onTimer` keeps the reference
/// behaviour: the member lookup falls through to the native `Timer::onTimer`,
/// which invokes the callback retained by `super.Timer(...)`.
///
/// The harness's poll advances 16 ms and the timer's interval is 1 ms, so
/// the registry replays the sixteen intervals the frame covered: the timer
/// fires sixteen times and the capacity bounds the `onTimer` postings still
/// outstanding for it, so the script sees six of them.
///
/// reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the catch-up count
/// is n = (curtick - NextTick) / Interval + 1.
/// reference/cpp/core/utils/TimerIntf.cpp:69-111 — `Fire(n)` posts
/// min(n, cap - count) events.
/// reference/cpp/core/utils/TimerIntf.cpp:17,32 — the default capacity is
/// TVP_DEFAULT_TIMER_CAPACITY = 6.
#[test]
fn timer_subclass_without_an_override_reaches_the_native_ctor_callback() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; \
         class SubTimer extends Timer { \
             function SubTimer(cb, time) { super.Timer(cb, ''); interval = time; } \
         } \
         var st = new SubTimer(function(){ n++; }, 1); \
         st.enabled = 1;",
    );
    env.poll_timer();
    assert_eq!(
        env.eval_int("n"),
        6,
        "the native onTimer invokes the ctor callback"
    );
    assert_eq!(
        env.eval_int("st.enabled"),
        1,
        "an untouched timer keeps repeating"
    );
}

/// Reference: `Capacity` bounds the number of **queued** `onTimer` events,
/// never the number of fires (`reference/cpp/core/utils/TimerIntf.cpp:69-92`):
/// nothing in the timer thread consults `Capacity` to stop a repeating
/// timer (`TimerImpl.cpp:122-146` checks only `Enabled`/`Interval`). So
/// `capacity = 1` — the game's `OnceTimer` — keeps firing every interval
/// and stays enabled; a real-game `OnceTimer` is stopped by its own script
/// (`cancel()` / `enabled = false`), not by the capacity.
#[test]
fn timer_capacity_one_keeps_repeating_until_cancelled() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var hits = 0; \
         class OnceTimer extends Timer { \
             function OnceTimer(time) { \
                 super.Timer(function(){}, ''); \
                 capacity = 1; \
                 interval = time; \
             } \
             function onTimer() { hits++; } \
         } \
         var o = new OnceTimer(1); \
         o.enabled = 1;",
    );
    for _ in 0..10 {
        env.poll_timer();
    }
    assert!(
        env.eval_int("hits") >= 3,
        "capacity = 1 must not stop a repeating timer (reference: capacity \
         bounds *queued* events, TimerIntf.cpp:69-92), got {}",
        env.eval_int("hits")
    );
    assert_eq!(
        env.eval_int("o.enabled"),
        1,
        "a capacity = 1 timer stays enabled while it repeats"
    );

    // The game stops its OnceTimer from script: disabling cancels the
    // schedule (`TimerImpl.cpp:270-284`) and no later poll fires.
    env.run("o.enabled = 0;");
    let hits = env.eval_int("hits");
    for _ in 0..5 {
        env.poll_timer();
    }
    assert_eq!(
        env.eval_int("hits"),
        hits,
        "a cancelled timer must not fire"
    );
    assert_eq!(env.eval_int("o.enabled"), 0);
}

/// Reference `TimerIntf.cpp:76-84`: a negative `Capacity` makes
/// `more = cap - queued <= 0`, so `Fire` posts nothing at all — the
/// callback is never invoked while the timer itself keeps running
/// (`TimerImpl.cpp:122-146` never stops on capacity). L2 models this as
/// *suppression*: armed + enabled + on schedule, never delivered.
#[test]
fn timer_capacity_negative_never_fires_while_enabled() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var t = new Timer(function(){ n++; }, ''); \
         t.capacity = -5; t.interval = 1; t.enabled = 1;",
    );
    for _ in 0..10 {
        env.poll_timer();
    }
    assert_eq!(
        env.eval_int("n"),
        0,
        "capacity < 0 must never deliver (TimerIntf.cpp:76-84)"
    );
    assert_eq!(
        env.eval_int("t.enabled"),
        1,
        "suppression gates delivery only — nothing disables the timer"
    );
}

/// The script-visible `capacity` default is the reference
/// `TVP_DEFAULT_TIMER_CAPACITY = 6`
/// (`reference/cpp/core/utils/TimerIntf.cpp:7`), and writing it round-trips.
#[test]
fn timer_capacity_reports_the_reference_default() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var t = new Timer(function(){}, ''); \
         var fresh = t.capacity; \
         t.capacity = 3; \
         var three = t.capacity; \
         t.capacity = 0; \
         var zero = t.capacity; \
         t.capacity = -5; \
         var neg = t.capacity;",
    );
    assert_eq!(env.eval_int("fresh"), 6, "reference default capacity");
    assert_eq!(env.eval_int("three"), 3);
    assert_eq!(
        env.eval_int("zero"),
        0,
        "0 is script-visible (reference sentinel)"
    );
    assert_eq!(
        env.eval_int("neg"),
        -5,
        "a negative value is stored verbatim, not clamped (TimerIntf.cpp:233)"
    );
}

/// An untouched (never-written) capacity must not stop a repeating timer:
/// the reference `Capacity` bounds *queued* `onTimer` events, not the number
/// of fires, so a plain `Timer` repeats indefinitely.
#[test]
fn timer_untouched_capacity_keeps_repeating() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var t = new Timer(function(){ n++; }, ''); \
         t.interval = 1; t.enabled = 1;",
    );
    for _ in 0..10 {
        env.poll_timer();
    }
    assert!(
        env.eval_int("n") >= 8,
        "a plain Timer must keep firing well past the default capacity of 6, got {}",
        env.eval_int("n")
    );
    assert_eq!(env.eval_int("t.enabled"), 1);
}

/// Reference `TimerIntf.cpp:77`: `Capacity == 0` reads as "no limit"
/// (`cap = Capacity == 0 ? 65535 : Capacity`). Writing `0` must therefore
/// *widen* the budget back to unbounded, not clamp it to 1.
#[test]
fn timer_capacity_zero_is_the_reference_no_limit_sentinel() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var t = new Timer(function(){ n++; }, ''); \
         t.interval = 1; t.capacity = 0; t.enabled = 1;",
    );
    for _ in 0..10 {
        env.poll_timer();
    }
    assert!(
        env.eval_int("n") >= 8,
        "capacity = 0 must mean 'no limit', got {}",
        env.eval_int("n")
    );
}

// ---------------------------------------------------------------------------
// Invalidate teardown
// ---------------------------------------------------------------------------

/// `invalidate` of a subclass instance must stop the dispatch and release the
/// retained `objthis` (reference `tTJSNI_BaseTimer::Invalidate` cancels the
/// events and drops `Owner`/`ActionOwner`), while the native payload stays
/// coherent until the object is destroyed (`timer_destroy` ordering).
///
/// The harness's poll advances 16 ms and the timer's interval is 1 ms, so
/// the registry replays the sixteen intervals the frame covered: the timer
/// fires sixteen times and the capacity bounds the `onTimer` postings still
/// outstanding for it, so the script sees six of them.
///
/// reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the catch-up count
/// is n = (curtick - NextTick) / Interval + 1.
/// reference/cpp/core/utils/TimerIntf.cpp:69-111 — `Fire(n)` posts
/// min(n, cap - count) events.
/// reference/cpp/core/utils/TimerIntf.cpp:17,32 — the default capacity is
/// TVP_DEFAULT_TIMER_CAPACITY = 6.
#[test]
fn timer_invalidate_stops_the_dispatch_and_reports_not_valid() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var hits = 0; \
         class InvTimer extends Timer { \
             function InvTimer(time) { super.Timer(function(){}, ''); interval = time; } \
             function onTimer() { hits++; } \
         } \
         var it = new InvTimer(1); \
         it.enabled = 1;",
    );
    env.poll_timer();
    assert_eq!(env.eval_int("hits"), 6);

    env.run("invalidate it;");
    assert_eq!(
        env.eval_int("isvalid it"),
        0,
        "an invalidated subclass instance reports not valid"
    );

    env.poll_timer();
    assert_eq!(
        env.eval_int("hits"),
        6,
        "an invalidated timer must not dispatch anymore"
    );
    // The payload survived the invalidate (released only by destroy): the VM
    // refuses member access on an invalidated object (the ABI's finalized-
    // object guard) instead of dereferencing freed native state.
    assert!(
        env.eval_ok("it.interval").is_err(),
        "member access on an invalidated object must be refused"
    );
}

/// A callback that invalidates **its own timer** must not be resurrected: the
/// reference removes the timer from the thread at `Invalidate`
/// (`TimerImpl.cpp:378-382`) and releases `Owner`, so the invocation in
/// progress is the last one — even though our `onTimer` glue puts the taken
/// callback back after the call returns.
#[test]
fn timer_callback_invalidate_stops_further_fires() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; \n         var t = new Timer(function(){ n++; invalidate t; }, ''); \n         t.interval = 1; t.enabled = 1;",
    );
    env.poll_timer();
    assert_eq!(env.eval_int("n"), 1, "the first due poll fires");
    let any_enabled = env.poll_timer();
    assert_eq!(
        env.eval_int("n"),
        1,
        "the callback must run exactly once — no resurrection after `invalidate`"
    );
    assert_eq!(
        env.eval_int("isvalid t"),
        0,
        "the invalidated timer reports not valid"
    );
    assert!(
        !any_enabled,
        "an invalidated timer is removed from the registry like the reference's \
         `tTVPTimerThread::Remove(this)`"
    );
}

/// A peer callback that disables a *not-yet-dispatched* timer during the same
/// poll must cancel its fire (reference `SetEnabled(false)` cancels the
/// queued event, `TimerImpl.cpp:277-281`). Due timers dispatch in id order,
/// so the first timer disables the second.
///
/// The harness's poll advances 16 ms and the timer's interval is 1 ms, so
/// the registry replays the sixteen intervals the frame covered: the timer
/// fires sixteen times and the capacity bounds the `onTimer` postings still
/// outstanding for it, so the script sees six of them.
///
/// reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the catch-up count
/// is n = (curtick - NextTick) / Interval + 1.
/// reference/cpp/core/utils/TimerIntf.cpp:69-111 — `Fire(n)` posts
/// min(n, cap - count) events.
/// reference/cpp/core/utils/TimerIntf.cpp:17,32 — the default capacity is
/// TVP_DEFAULT_TIMER_CAPACITY = 6.
#[test]
fn timer_peer_disabled_mid_dispatch_is_cancelled() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var na = 0; var nb = 0; \n         var ta = new Timer(function(){ na++; tb.enabled = 0; }, ''); \n         ta.interval = 1; \n         var tb = new Timer(function(){ nb++; }, ''); \n         tb.interval = 1; \n         ta.enabled = 1; tb.enabled = 1;",
    );
    env.poll_timer();
    assert_eq!(env.eval_int("na"), 6, "the first timer fired");
    assert_eq!(
        env.eval_int("nb"),
        0,
        "the peer-disabled timer must not fire in the same round"
    );
    env.poll_timer();
    assert_eq!(env.eval_int("na"), 12, "the first timer keeps repeating");
    assert_eq!(env.eval_int("nb"), 0, "the disabled timer stays off");
}

// ---------------------------------------------------------------------------
// AsyncTrigger
// ---------------------------------------------------------------------------

/// `async_trigger_poll` dispatches the object's `onFire` member; a script
/// subclass override must win over the native action invocation.
#[test]
fn async_trigger_subclass_on_fire_wins() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var ah = 0; var act = 0; \
         class SubTrigger extends AsyncTrigger { \
             function SubTrigger(action, name) { super.AsyncTrigger(action, name); } \
             function onFire() { ah++; } \
         } \
         var st = new SubTrigger(function(){ act++; }, ''); \
         st.trigger();",
    );
    assert_eq!(env.eval_int("ah"), 0, "trigger() defers to the poll");
    env.poll_async();
    assert_eq!(
        env.eval_int("ah"),
        1,
        "the subclass onFire override must run instead of the native action body"
    );
    assert_eq!(
        env.eval_int("act"),
        0,
        "the ctor action is only reached through the native onFire, which the override replaced"
    );
}

/// Reference `tTJSNI_AsyncTrigger::Invalidate` drops `Owner`/`ActionOwner`
/// and cancels the scheduled fires; `invalidate` of a subclass instance must
/// therefore cancel a pending `trigger()` before the next poll.
#[test]
fn async_trigger_invalidate_cancels_the_scheduled_fire() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var ah = 0; \n         class SubTrigger extends AsyncTrigger { \n             function SubTrigger(action) { super.AsyncTrigger(action, ''); } \n             function onFire() { ah++; } \n         } \n         var st = new SubTrigger(function(){}); \n         st.trigger(); \n         invalidate st;",
    );
    env.poll_async();
    assert_eq!(
        env.eval_int("ah"),
        0,
        "an invalidated trigger must not fire"
    );
    assert_eq!(env.eval_int("isvalid st"), 0);
}

// ---------------------------------------------------------------------------
// MenuItem
// ---------------------------------------------------------------------------

/// `fireClick` resolves `onClick` on the item object; a script subclass
/// override must win over the native action-owner dispatch.
///
/// Reference: `OnClick` only delivers when the item chain reaches a window
/// (`MenuItemIntf.cpp:195-214` — `while(!item->Window) ... if(!item->Window)
/// return;`), so the item is constructed **under a window** here; the
/// dispatch itself is `Target->FuncCall(0, "onClick", ...)` on the item,
/// where a script subclass override shadows the native member.
#[test]
fn menu_item_subclass_on_click_wins() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var mh = 0; var owner_called = 0; \
         var owner = %[ action: function(){ owner_called++; } ]; \
         class SubItem extends MenuItem { \
             function SubItem(o, w) { super.MenuItem(o, w); } \
             function onClick() { mh++; } \
         } \
         var w = new Window(); \
         var mi = new SubItem(owner, w); \
         mi.fireClick();",
    );
    assert_eq!(
        env.eval_int("mh"),
        1,
        "the subclass onClick override must run instead of the native dispatch"
    );
    assert_eq!(
        env.eval_int("owner_called"),
        0,
        "the native action-owner dispatch is replaced by the override"
    );
}

// ---------------------------------------------------------------------------
// Layer (event seam: BitmapLayerTreeOwner → `call_member(layer, name)`)
// ---------------------------------------------------------------------------

/// The tree-owner event seam dispatches the handler name **on the layer
/// object**, so a script subclass of `Layer` that overrides `onClick` wins.
#[test]
fn layer_subclass_on_click_wins() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var lh = 0; \
         class SubLayer extends Layer { \
             function SubLayer(w, parent) { super.Layer(w, parent); } \
             function onClick(x, y) { lh++; return true; } \
         } \
         var w = new Window(); \
         var sl = new SubLayer(w, null); \
         var owner = new BitmapLayerTreeOwner(); \
         owner.focusedLayer = sl;",
    );
    assert_eq!(
        env.eval_int("owner.fireClick(11, 22)"),
        1,
        "the subclass override consumes the event (truthy return)"
    );
    assert_eq!(
        env.eval_int("lh"),
        1,
        "the script subclass Layer.onClick must run, not the native no-op default"
    );
    // The subclass instance is the object that landed in the L2 scene, i.e.
    // `super.Layer(...)` captured the subclass instance, not the class.
    assert_eq!(env.scene.windows.len(), 1);
    assert_eq!(env.scene.layers.len(), 1);
}

// ---------------------------------------------------------------------------
// Bitmap
// ---------------------------------------------------------------------------

/// `loadAsync` retains `objthis` (the *subclass* instance) and dispatches
/// `onLoaded` through it; a script subclass override must win.
#[test]
fn bitmap_subclass_on_loaded_wins() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var oh = 0; var seen_error = -1; \
         class SubBitmap extends Bitmap { \
             function SubBitmap() { super.Bitmap(); } \
             function onLoaded(meta, async, error, message) { oh++; seen_error = error; } \
         } \
         var sb = new SubBitmap(); \
         sb.loadAsync('images/definitely-missing.img');",
    );
    assert_eq!(
        env.eval_int("oh"),
        1,
        "the subclass onLoaded override must run (no codec/storage → error path)"
    );
    assert_eq!(
        env.eval_int("seen_error"),
        1,
        "the override receives the failure report through its own arguments"
    );
}

// ---------------------------------------------------------------------------
// Layer transitions
// ---------------------------------------------------------------------------

/// The frame-body transition step must dispatch `onTransitionCompleted(dest,
/// src)` **on the layer object** (the reference fires the event synchronously
/// on the layer, `LayerIntf.cpp:8000-8015`), so a script subclass override
/// runs instead of the native no-op default.
#[test]
fn layer_subclass_on_transition_completed_wins() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var hits = 0; var sawDest = 0; var sawSrc = 0; \
         class SubLayer extends Layer { \
             function SubLayer(w, parent) { super.Layer(w, parent); } \
             function onTransitionCompleted(dest, src) { \
                 hits++; \
                 sawDest = (dest === b) ? 1 : 0; \
                 sawSrc = (src === a) ? 1 : 0; \
             } \
         } \
         var w = new Window(); \
         var a = new SubLayer(w, null); a.setImageSize(2, 2); \
         var b = new SubLayer(w, null); b.setImageSize(2, 2); \
         b.beginTransition('crossfade', true, a, %[time: 1]);",
    );
    // The transition's phase follows the engine's clock.
    env.clock.borrow_mut().advance(50.0);
    let ctx = env._host.ctx().expect("bound host");
    compat::advance_transitions(&ctx).expect("advance transitions");
    assert_eq!(
        env.eval_int("hits"),
        1,
        "the script subclass onTransitionCompleted override must run"
    );
    assert_eq!(
        env.eval_int("sawDest"),
        1,
        "argument 0 is the destination layer object (`b`)"
    );
    assert_eq!(
        env.eval_int("sawSrc"),
        1,
        "argument 1 is the source layer object (`a`)"
    );
}

// ---------------------------------------------------------------------------
// KAGParser
// ---------------------------------------------------------------------------

/// `getNextTag()` dispatches `onLabel(label, page)` **on the parser object**
/// (`kag/mod.rs` retains `objthis`), so a script subclass of `KAGParser` that
/// overrides `onLabel` runs instead of the native (absent) handler.
#[test]
fn kag_subclass_on_label_wins() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    env.run(
        "var seen = ''; \n         class SubKAG extends KAGParser { \n             function SubKAG() { super.KAGParser(); } \n             function onLabel(label, page) { seen = label; } \n         } \n         var p = new SubKAG(); \n         p.loadScenario('scenario.ks'); \n         while (p.getNextTag()) { }",
    );
    assert_eq!(
        env.eval_int("seen == '*start'"),
        1,
        "the subclass onLabel override must see the scenario label"
    );
}
