//! Behavior parity tests for the `Timer` native class.
//!
//! Every test states the **KiriKiri (reference C++) expectation** with a
//! `reference/cpp/**` citation above it and asserts that expectation against
//! the observed script-visible behavior. Where the two differ the assertion
//! fails with the observed value, so `cargo test -p compat --test
//! timer_behavior` doubles as the observed-vs-expected report.
//!
//! Ground rules applied here:
//! * The vendored TJS2 VM (`crates/tjs/**`) is reference code: member
//!   lookup, `invalidate`/destruct ordering and the native dispatch ABI are
//!   taken as correct; divergences are attributed to
//!   `crates/compat/src/natives/visual/timer.rs` (L3 glue) or
//!   `crates/engine/src/timer.rs` (L2 registry).
//! * Time is virtual: each test moves the attached registry's injected
//!   `now_ms` forward by an explicit step and then calls the exported
//!   [`compat::timer_poll`], which advances the registry at that timestamp
//!   and drains the `onTimer` posts — no frame loop and no wall-clock wait.
//! * One [`Env`] (engine + host + attached timer registry) per test,
//!   serialized through [`common::vm_lock`].
//!
//! Note on `capacity`: the task brief asserted "a `capacity = 1` timer must
//! fire once", but the vendored reference treats `Capacity` as a bound on
//! *queued* `onTimer` events, not on the number of fires
//! (`reference/cpp/core/utils/TimerIntf.cpp:69-92`). The tests assert the
//! reference reading; see [`capacity_one_bounds_queued_events_not_fires`].

mod common;

use std::cell::RefCell;

use common::vm_lock;
use compat::{CompatHost, timer_poll};
use engine::timer::{TimerState, deliver_due};
use tjs::{Tjs2Engine, TjsValue};

/// Script label for diagnostics.
const SCRIPT: &str = "timer-behavior-test";

/// A bound VM + host + attached L2 timer registry.
///
/// `_host` is declared first so it drops first (releasing every retained
/// script object and removing the process-wide binding) before the engine
/// and the timer registry go away.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// The attached registry; tests move its injected time directly so a poll
    /// never depends on the wall clock.
    timer: RefCell<Box<TimerState>>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut timer = Box::new(TimerState::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed timer registry outlives the host (it is declared
        // after `_host`, so it drops later), matching `attach_timer`.
        unsafe {
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            timer: RefCell::new(timer),
        }
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, SCRIPT)
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    /// Read a numeric member that the reference stores either as an integer
    /// or as a fixed-point real (`Timer.interval` is a real in the
    /// reference).
    fn eval_num(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value as f64,
            TjsValue::Real(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a number"),
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    /// Run a script and keep a failure observable instead of panicking (for
    /// cases where the reference *accepts* the script and our port may raise).
    fn try_run(&self, script: &str) -> Result<(), String> {
        self.engine
            .exec_script(script, SCRIPT)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// One frame-body timer step at the current injected time: deliver the
    /// timers due now, then hand the queued `onTimer` events to the script.
    /// Returns the registry's "any timer still enabled" reading.
    fn poll(&self) -> bool {
        {
            let mut timer = self.timer.borrow_mut();
            let now = timer.now_ms;
            deliver_due(timer.as_mut(), now);
        }
        let mut ctx = self._host.ctx().expect("bound host");
        timer_poll(&mut ctx).expect("timer poll")
    }

    /// Move the attached registry's injected time forward by `ms` without
    /// polling, so a schedule write lands on this timestamp.
    fn advance(&self, ms: u64) {
        let mut timer = self.timer.borrow_mut();
        timer.now_ms = timer.now_ms.saturating_add(ms);
    }

    /// Advance `total_ms` of virtual time in `step_ms` polls; returns the last
    /// poll's reading.
    fn poll_for(&self, total_ms: u64, step_ms: u64) -> bool {
        let mut last = false;
        for _ in 0..(total_ms / step_ms) {
            self.advance(step_ms);
            last = self.poll();
        }
        last
    }
}

/// Record a mismatch of `observed` against the reference `expected`.
fn check<T: PartialEq + std::fmt::Debug>(
    problems: &mut Vec<String>,
    case: &str,
    observed: T,
    expected: T,
) {
    if observed != expected {
        problems.push(format!(
            "{case}: observed {observed:?}, expected {expected:?}"
        ));
    }
}

fn assert_no_problems(problems: Vec<String>) {
    assert!(problems.is_empty(), "\n{}\n", problems.join("\n"));
}

// ---------------------------------------------------------------------------
// 1. Constructor
// ---------------------------------------------------------------------------

/// Reference: `new Timer()` raises `TJS_E_BADPARAMCOUNT`
/// (`reference/cpp/core/utils/TimerIntf.cpp:41-42`), reached because the
/// native-constructor macro runs `tTJSNI_Timer::Construct` before the empty
/// body (`reference/cpp/core/tjs2/tjsNative.h:389-391`); the VM renders that
/// error as "Invalid argument count"
/// (`reference/cpp/core/tjs2/tjsError.h:268`).
#[test]
fn constructor_missing_argument_raises_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::new();

    let message = env
        .eval("new Timer()")
        .expect_err("`new Timer()` must raise, like the reference TJS_E_BADPARAMCOUNT");
    // The reference renders this as "Invalid argument count"
    // (tjsError.h:268); our glue raises its own typed message. The message
    // text is allowed to differ — the raising behavior must not.
    println!("observed `new Timer()` error: {message}");
}

/// Reference: the constructor stores `param[0]` as an **action closure** and
/// keeps the action *name* — defaulting to `TVPActionName == "action"`
/// (`TimerIntf.cpp:34`, `EventIntf.cpp:740`, stored at `TimerIntf.cpp:48-51`).
/// The native `onTimer` then invokes it as `callback.FuncCall("action", [ev])`
/// (`TimerIntf.cpp:156-171` + `EventIntf.h:231-234`), and the C++ macro
/// discards the `TJS_E_MEMBERNOTFOUND` that a plain function produces
/// (`reference/cpp/core/tjs2/tjsObject.cpp:1214-1226` has no default-call
/// fallback for a `ctFunction` context, `tjsInterCodeExec.cpp:2814-2831`).
/// So `new Timer(fn)` with **no** second argument silently never runs `fn`.
///
/// The first assertion proves the VM premise (`fn.action` does not exist on
/// a plain function); the second asserts the reference's silent no-op.
///
/// Our dispatch mirrors the reference: `timer_on_timer` → `invoke_action`
/// performs the member call `fn.action(event)` and *discards* the resulting
/// `MEMBERNOTFOUND` (the reference never inspects the `FuncCall` result,
/// `EventIntf.h:231-234`), so the script call succeeds, `fn` never runs and
/// nothing disables the timer.
#[test]
fn constructor_without_an_action_name_never_reaches_a_plain_function() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    // Premise: a plain function has no `action` member.
    if env
        .eval("(function(){ var f = function(){}; return f.action; })()")
        .is_ok()
    {
        problems.push(
            "premise: `f.action` resolved on a plain function, but \
             tTJSCustomObject::Find has no such member (tjsObject.cpp:1214-1226)"
                .to_string(),
        );
    }

    env.run(
        "var hitsA = 0; \
         var tA = new Timer(function(){ hitsA++; }); \
         tA.enabled = true; \
         tA.onTimer();",
    );
    check(
        &mut problems,
        "`new Timer(fn)` (no action name) callback invocations",
        env.eval_int("hitsA"),
        0,
    );
    // The reference's native onTimer returns TJS_S_OK even when the action
    // lookup failed (TimerIntf.cpp:169), so the timer stays armed.
    check(
        &mut problems,
        "`new Timer(fn)` still enabled after the failed action lookup",
        env.eval_int("tA.enabled"),
        1,
    );

    assert_no_problems(problems);
}

/// Reference: `new Timer(fn, "")` stores the empty action name
/// (`TimerIntf.cpp:48-49`), so `onTimer` falls back to a default call and
/// invokes `fn` itself — with **one argument**, the event dictionary
/// `{type: "onTimer", target: <timer>}`
/// (`EventIntf.cpp:709-736`, passed by `EventIntf.h:231-234`). The event is
/// delivered through the object's `onTimer` member with zero arguments
/// (`EventIntf.cpp:97-108`, posted with `numparams = 0` at
/// `TimerIntf.cpp:100`), which resolves to the native `Timer.onTimer`.
///
/// Our glue matches: `timer_on_timer` → `invoke_action` calls `fn` itself
/// (empty action name → `call_detached`) with the event dictionary as its
/// single argument.
#[test]
fn constructor_with_an_empty_action_name_invokes_the_callback_with_the_event() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var hitsB = 0; var evB = -1; var tgtB = -1; \
         var tB = new Timer(function(ev){ \
             hitsB++; \
             if (typeof ev == 'Object') { \
                 evB = (ev.type == 'onTimer') ? 1 : 2; \
                 tgtB = (ev.target === tB) ? 1 : 0; \
             } else { \
                 evB = 0; \
             } \
         }, ''); \
         tB.onTimer();",
    );
    check(
        &mut problems,
        "`new Timer(fn, '')` callback invocations",
        env.eval_int("hitsB"),
        1,
    );
    check(
        &mut problems,
        "`new Timer(fn, '')` callback first argument (1 = onTimer event dict)",
        env.eval_int("evB"),
        1,
    );
    check(
        &mut problems,
        "event `target` is the timer object",
        env.eval_int("tgtB"),
        1,
    );

    assert_no_problems(problems);
}

/// Reference: `new Timer(obj, "name")` stores `obj` and the action name and
/// invokes `obj.name(event)` on every fire (`TimerIntf.cpp:48-51`,
/// `TimerIntf.cpp:156-171`); `new Timer(obj)` defaults the name to
/// `"action"` and invokes `obj.action(event)` (`TimerIntf.cpp:34`,
/// `EventIntf.cpp:740`). Neither form can call `obj` *itself* — a
/// non-function object called with a default `FuncCall` returns
/// `TJS_E_INVALIDTYPE` (`reference/cpp/core/tjs2/tjsObject.cpp:1208-1212`).
///
/// Our glue performs exactly those member calls (`invoke_action`:
/// `owner.<name>(event)`, default name `"action"`) and dispatch failures
/// are logged, never raised and never disable the timer — the reference has
/// no disable-on-error path in `TimerIntf.cpp`/`TimerImpl.cpp`.
///
/// Note: a fresh timer reports *disabled* (`TimerImpl.cpp:365`, pinned by
/// [`enabled_arms_from_now_and_disabling_cancels_pending_fires`]), and
/// dispatching `onTimer` never enables one (`TimerIntf.cpp:156-171`). The
/// "stays enabled" checks below therefore arm the timers first — otherwise
/// they could not observe whether the dispatch *disabled* them (the old
/// glue's error branch did).
///
/// **Correction (action owner shape):** the auditor's original scripts used
/// `%[ fire: ... ]` *dictionaries*, which cannot host the bare counters —
/// under the vendored reference VM a bare read inside a function invoked
/// with a dictionary as `this` yields `void` (`tTJSDictionaryObject::PropGet`
/// swallows missing members, `tjsDictionary.cpp:739-743`) and `hitsC++`
/// auto-creates the member **on the dictionary**
/// (`tTJSDictionaryObject::Operation`, `tjsDictionary.cpp:764-773`), so the
/// script-global counter stays 0. That is reference behaviour, not a port
/// divergence: the reference macro reaches the member through the very same
/// `tTJSVariantClosure::FuncCall(..., objthis=nullptr)` fallback that pins
/// the effective `objthis` to the owner (`EventIntf.h:231-234` +
/// `tjsVariant.h:219-228`), i.e. a real KiriKiri run counts 0 as well. A
/// script **class instance** (`tTJSCustomObject`) returns
/// `TJS_E_MEMBERNOTFOUND` for missing members and the this-proxy falls back
/// to the global object (`tjsInterCodeExec.cpp:243-247`), so the counters
/// work — and the dispatch path under test (`owner.<name>(event)`) is
/// identical either way.
#[test]
fn constructor_with_an_object_invokes_the_named_action_member() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var hitsC = 0; var evC = -1; \
         class FireAction { \
             function FireAction() {} \
             function fire(ev) { \
                 hitsC++; \
                 if (typeof ev == 'Object') { \
                     if (ev.type == 'onTimer') { evC = 1; } else { evC = 2; } \
                 } \
             } \
         } \
         var objC = new FireAction(); \
         var tC = new Timer(objC, 'fire'); \
         tC.enabled = true; \
         tC.onTimer();",
    );
    check(
        &mut problems,
        "`new Timer(obj, 'fire')` named-action invocations",
        env.eval_int("hitsC"),
        1,
    );
    check(
        &mut problems,
        "`new Timer(obj, 'fire')` event argument seen by obj.fire",
        env.eval_int("evC"),
        1,
    );
    check(
        &mut problems,
        "timer stays enabled after dispatching to obj.fire",
        env.eval_int("tC.enabled"),
        1,
    );

    env.run(
        "var hitsD = 0; \
         class DefaultAction { \
             function DefaultAction() {} \
             function action(ev) { hitsD++; } \
         } \
         var objD = new DefaultAction(); \
         var tD = new Timer(objD); \
         tD.enabled = true; \
         tD.onTimer();",
    );
    check(
        &mut problems,
        "`new Timer(obj)` default-action (`obj.action`) invocations",
        env.eval_int("hitsD"),
        1,
    );
    check(
        &mut problems,
        "timer stays enabled after dispatching to obj.action",
        env.eval_int("tD.enabled"),
        1,
    );

    assert_no_problems(problems);
}

/// Reference: `param[0]` is coerced with `AsObjectClosure()` and stored
/// unconditionally (`TimerIntf.cpp:51`) — *any* value is an accepted
/// callback. A value that is not an object yields an empty closure, so the
/// native `onTimer` skips the invocation (`if(obj.Object)`,
/// `TimerIntf.cpp:157`) and the timer keeps running silently. Our glue
/// requires no retainable object either: a failed `retain_object_arg`
/// stores `None` — the port's empty closure — and `invoke_action` skips it
/// the same way.
#[test]
fn constructor_accepts_a_non_object_callback_without_invoking_it() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    let script = "var ntHits = 0; var nt = new Timer(42); nt.interval = 20; nt.enabled = true;";
    if let Err(error) = env.try_run(script) {
        problems.push(format!(
            "`new Timer(42)`: observed construction error {error:?}, but the reference \
             accepts any callback value (TimerIntf.cpp:51) and simply never invokes it"
        ));
    } else {
        env.poll_for(80, 10);
        check(
            &mut problems,
            "invocations of a non-object callback (reference: never, \
             TimerIntf.cpp:157 guards on obj.Object)",
            env.eval_int("ntHits"),
            0,
        );
        check(
            &mut problems,
            "non-object-callback timer still enabled",
            env.eval_int("nt.enabled"),
            1,
        );
    }

    assert_no_problems(problems);
}

// ---------------------------------------------------------------------------
// 2. interval
// ---------------------------------------------------------------------------

/// Reference: `Interval` defaults to 1000 ms
/// (`reference/cpp/core/utils/impl/TimerImpl.cpp:363`); the getter converts
/// the fixed-point value to a **double** (`TimerIntf.cpp:186-188), and the
/// setter accepts reals by scaling (`TimerIntf.cpp:196-197`), so
/// `timer.interval` is a `Real` in KiriKiri.
#[test]
fn interval_defaults_roundtrip_and_reports_a_real() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run("var t = new Timer(function(){}, '');");
    check(
        &mut problems,
        "default interval value",
        env.eval_num("t.interval"),
        1000.0,
    );
    check(
        &mut problems,
        "default interval is a Real (TimerIntf.cpp:186-188)",
        matches!(env.eval_ok("t.interval"), TjsValue::Real(_)),
        true,
    );

    env.run("t.interval = 250;");
    check(
        &mut problems,
        "interval after `= 250`",
        env.eval_num("t.interval"),
        250.0,
    );

    // Reference setter: (tjs_int64)(25.5 * 65536 + 0.5) sub-ms units →
    // 25.5 ms read back (TimerIntf.cpp:196-197 + 186-188).
    env.run("t.interval = 25.5;");
    check(
        &mut problems,
        "interval after `= 25.5` (sub-millisecond precision is kept)",
        env.eval_num("t.interval"),
        25.5,
    );

    assert_no_problems(problems);
}

/// Reference: the setter stores the scaled value verbatim
/// (`TimerIntf.cpp:196-197`), so `interval = 0` reads back as `0`, and the
/// timer thread skips any timer whose interval is 0 — it stays *enabled* but
/// never fires (`reference/cpp/core/utils/impl/TimerImpl.cpp:125`).
///
/// Our glue stores the scaled value verbatim and maps `interval <= 0` to
/// L2 *suppression*: the getter reports `0` and the timer stays enabled but
/// never fires, exactly like the reference thread's `Interval == 0` skip.
#[test]
fn interval_zero_is_kept_verbatim_and_never_fires() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var zhits = 0; \
         var zt = new Timer(function(){ zhits++; }, ''); \
         zt.interval = 0;",
    );
    check(
        &mut problems,
        "getter after `interval = 0`",
        env.eval_num("zt.interval"),
        0.0,
    );
    env.run("zt.enabled = true;");
    env.poll_for(60, 5);
    check(
        &mut problems,
        "fires with interval 0 in ~60 ms (TimerImpl.cpp:125 skips interval==0)",
        env.eval_int("zhits"),
        0,
    );
    // Enabled state is independent of the zero interval in both.
    check(
        &mut problems,
        "timer still reports enabled with interval 0",
        env.eval_int("zt.enabled"),
        1,
    );

    assert_no_problems(problems);
}

/// Reference: the setter casts through `tjs_int64` into the `tjs_uint64`
/// parameter of `SetInterval` (`TimerIntf.cpp:196-197`,
/// `TimerImpl.h:26`), so `interval = -1` stores `2^64 - 65535` sub-ms units
/// and the getter (`TimerIntf.cpp:186-188`) reports
/// `(2^64 - 65536) / 65536 = 2^48 - 1 = 281474976710655.0` — it is *not*
/// clamped to 1.
///
/// (The reference thread's `NextTick` arithmetic then wraps for such an
/// interval — `TimerImpl.cpp:128-146` — making the fire cadence
/// environment-dependent, so only the stored value is asserted here.)
///
/// Our glue reproduces the reference conversion arithmetically —
/// `(v * 65536 + 0.5) as i64 as u64` on write, `units as f64 * (1.0/65536)`
/// on read (`timer.rs` `timer_interval_set`/`timer_interval_get`) — and a
/// non-positive value additionally suppresses delivery (L2 cannot express
/// an ~8775-year interval; suppression keeps the timer armed and silent).
#[test]
fn interval_negative_is_stored_as_the_wrapped_uint64() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var nt = new Timer(function(){}, ''); \
         nt.interval = -1;",
    );
    check(
        &mut problems,
        "getter after `interval = -1`",
        env.eval_num("nt.interval"),
        281_474_976_710_655.0,
    );

    assert_no_problems(problems);
}

/// Reference: changing the interval of an **enabled** timer cancels its
/// queued events and re-arms `NextTick = now + interval`
/// (`reference/cpp/core/utils/impl/TimerImpl.cpp:289-301`), so a deadline
/// that had already passed does *not* fire immediately after the write.
/// Our L2 registry matches (`crates/engine/src/timer.rs` `set_interval`
/// re-arms from `now_ms` when enabled).
#[test]
fn interval_change_rearms_an_enabled_timer_from_now() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var rhits = 0; \
         var rt = new Timer(function(){ rhits++; }, ''); \
         rt.interval = 40; \
         rt.enabled = true;",
    );
    // Let the original deadline (now + 40) pass without polling, then move
    // the interval. If the old deadline were kept, the next poll would fire.
    env.advance(65);
    env.run("rt.interval = 40;");
    env.advance(15);
    env.poll();
    check(
        &mut problems,
        "fires right after an interval write that kept an expired deadline",
        env.eval_int("rhits"),
        0,
    );
    env.advance(40);
    env.poll();
    check(
        &mut problems,
        "fires once after the re-armed interval elapses",
        env.eval_int("rhits"),
        1,
    );

    assert_no_problems(problems);
}

// ---------------------------------------------------------------------------
// 3. enabled
// ---------------------------------------------------------------------------

/// Reference: `enabled = true` arms `NextTick = now + interval`
/// (`TimerImpl.cpp:270-284`); `enabled = false` clears the pending events
/// and the thread skips disabled timers (`TimerImpl.cpp:277-281`,
/// `TimerImpl.cpp:125`); re-enabling re-arms from *now*. Our glue matches
/// (`crates/engine/src/timer.rs` `set_enabled`).
#[test]
fn enabled_arms_from_now_and_disabling_cancels_pending_fires() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var ehits = 0; \
         var et = new Timer(function(){ ehits++; }, ''); \
         et.interval = 80; \
         et.enabled = true;",
    );
    check(
        &mut problems,
        "fresh timer reports disabled (TimerImpl.cpp:365)",
        env.eval_int("(function(){ var u = new Timer(function(){}); return u.enabled; })()"),
        0,
    );

    // Armed at now + 80: no fire halfway, fire after the deadline.
    env.advance(30);
    env.poll();
    check(
        &mut problems,
        "fires before its armed deadline",
        env.eval_int("ehits"),
        0,
    );
    env.advance(60);
    env.poll();
    check(
        &mut problems,
        "fires once after the armed deadline",
        env.eval_int("ehits"),
        1,
    );

    // Disabling cancels the (virtual) queued event: a passed deadline while
    // disabled must not deliver.
    env.run("et.enabled = false;");
    env.advance(100);
    env.poll();
    check(
        &mut problems,
        "fires while disabled",
        env.eval_int("ehits"),
        1,
    );
    check(
        &mut problems,
        "poll reports no enabled timer while disabled",
        env.poll(),
        false,
    );

    // Re-enabling re-arms from now, not from the stale deadline.
    env.run("et.enabled = true;");
    env.advance(50);
    env.poll();
    check(
        &mut problems,
        "fires before the re-armed deadline (50 ms < 80 ms)",
        env.eval_int("ehits"),
        1,
    );
    env.advance(40);
    env.poll();
    check(
        &mut problems,
        "fires once after the re-armed deadline",
        env.eval_int("ehits"),
        2,
    );

    assert_no_problems(problems);
}

/// Reference: a timer that is never enabled is skipped by the timer thread
/// (`TimerImpl.cpp:125` `!item->GetEnabled()`) and never posts anything.
/// `timer_poll` returns "any registered timer is enabled", so the reference
/// state maps to `false`.
#[test]
fn a_timer_that_is_never_enabled_never_fires() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var nhits = 0; \
         var nt = new Timer(function(){ nhits++; }, ''); \
         nt.interval = 20;",
    );
    let any_enabled = env.poll_for(60, 5);
    assert_eq!(env.eval_int("nhits"), 0, "a never-enabled timer fired");
    assert!(!any_enabled, "poll reported an enabled timer");
}

// ---------------------------------------------------------------------------
// 4. capacity
// ---------------------------------------------------------------------------

/// Reference: `Capacity` defaults to `TVP_DEFAULT_TIMER_CAPACITY = 6`
/// (`TimerIntf.cpp:17,32`); the setter stores the raw script value —
/// including `0` and negatives — and the getter reads it back verbatim
/// (`TimerIntf.cpp:222-236`, `TimerIntf.h:63`). `0` is only interpreted at
/// fire time, where it reads as `65535` ("no limit",
/// `TimerIntf.cpp:76-77`); a negative value reads as `cap = -5`, which makes
/// `more = cap - count <= 0` so no event is ever posted
/// (`TimerIntf.cpp:76-84`).
#[test]
fn capacity_reports_the_reference_default_and_stores_values_verbatim() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

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
    check(&mut problems, "default capacity", env.eval_int("fresh"), 6);
    check(
        &mut problems,
        "capacity after `= 3`",
        env.eval_int("three"),
        3,
    );
    check(
        &mut problems,
        "capacity after `= 0`",
        env.eval_int("zero"),
        0,
    );
    check(
        &mut problems,
        "capacity after `= -5` (stored verbatim, TimerIntf.cpp:233)",
        env.eval_int("neg"),
        -5,
    );

    assert_no_problems(problems);
}

/// Reference: `capacity` bounds the number of **queued** `onTimer` events,
/// never the number of fires: `Fire` posts `cap - <queued>` events per tick
/// (`TimerIntf.cpp:69-92`) and nothing in the timer thread consults
/// `Capacity` to stop a repeating timer (`TimerImpl.cpp:122-146` only checks
/// `Enabled`/`Interval`). Consequences asserted here:
///
/// * `capacity = 1` — a timer keeps firing every interval (at most one
///   event outstanding), and stays enabled;
/// * `capacity = -5` — `more <= 0`, so the callback is never invoked (while
///   the timer itself keeps running);
/// * `capacity = 0` — the "no limit" sentinel (`TimerIntf.cpp:77`), fires
///   normally.
///
/// Note: `crates/compat/tests/extend_dispatch.rs`
/// (`timer_capacity_one_keeps_repeating_until_cancelled`) encodes the same
/// reading: capacity `> 0` keeps repeating, `0` is the no-limit sentinel
/// and `< 0` suppresses delivery while the timer stays armed.
///
/// L2 models both never-fires cases — a negative capacity and a
/// non-positive interval — as *suppression*
/// (`crates/engine/src/timer.rs`), which gates delivery only: `enabled`
/// and the deadline are untouched and `advance` keeps re-arming the
/// schedule, so lifting the suppression resumes on time with no backlog.
#[test]
fn capacity_one_bounds_queued_events_not_fires() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    // capacity = 1: repeats.
    env.run(
        "var chits = 0; \
         var ct = new Timer(function(){ chits++; }, ''); \
         ct.capacity = 1; \
         ct.interval = 20; \
         ct.enabled = true;",
    );
    env.poll_for(150, 10);
    let fires = env.eval_int("chits");
    if fires < 3 {
        problems.push(format!(
            "`capacity = 1` fire count: observed {fires} fires in ~150 ms, expected >= 3 \
             (reference: capacity caps *queued* events, TimerIntf.cpp:69-92, so a \
             capacity=1 timer keeps firing every interval)"
        ));
    }
    check(
        &mut problems,
        "capacity=1 timer still enabled after firing",
        env.eval_int("ct.enabled"),
        1,
    );

    // capacity = -5: never delivers, but keeps running.
    env.run(
        "var nhits = 0; \
         var nt = new Timer(function(){ nhits++; }, ''); \
         nt.capacity = -5; \
         nt.interval = 20; \
         nt.enabled = true;",
    );
    env.poll_for(120, 10);
    check(
        &mut problems,
        "fires with `capacity = -5` (reference posts no events, TimerIntf.cpp:76-84)",
        env.eval_int("nhits"),
        0,
    );
    check(
        &mut problems,
        "capacity=-5 timer still reports enabled (nothing disables it)",
        env.eval_int("nt.enabled"),
        1,
    );

    // capacity = 0: "no limit" sentinel → fires normally.
    env.run(
        "var zhits = 0; \
         var zt = new Timer(function(){ zhits++; }, ''); \
         zt.capacity = 0; \
         zt.interval = 20; \
         zt.enabled = true;",
    );
    env.poll_for(120, 10);
    let zfires = env.eval_int("zhits");
    if zfires < 3 {
        problems.push(format!(
            "`capacity = 0` fire count: observed {zfires} fires in ~120 ms, expected >= 3 \
             (0 is the reference no-limit sentinel, TimerIntf.cpp:77)"
        ));
    }

    assert_no_problems(problems);
}

// ---------------------------------------------------------------------------
// 5. mode (and the absent count/action members)
// ---------------------------------------------------------------------------

/// Reference: `mode` is the `tTVPAsyncTriggerMode` event-priority selector —
/// `atmNormal/atmExclusive/atmAtIdle` (`reference/cpp/core/base/EventIntf.h:312`),
/// default `atmNormal == 0` (`TimerIntf.cpp:35`) — round-tripped verbatim
/// (`TimerIntf.cpp:240-253`). It is **not** a one-shot switch: the mode only
/// selects the event-posting flags (`TimerIntf.cpp:86-96`), and nothing in
/// the fire path ever stops a repeating timer because of the mode
/// (`TimerImpl.cpp:122-146`). A `mode = 1` timer therefore keeps repeating.
#[test]
fn mode_round_trips_and_is_event_priority_not_a_oneshot_switch() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var t = new Timer(function(){}, ''); \
         var mode0 = t.mode; \
         t.mode = 1; \
         var mode1 = t.mode; \
         t.mode = 2; \
         var mode2 = t.mode;",
    );
    check(&mut problems, "default mode", env.eval_int("mode0"), 0);
    check(&mut problems, "mode after `= 1`", env.eval_int("mode1"), 1);
    check(&mut problems, "mode after `= 2`", env.eval_int("mode2"), 2);

    env.run(
        "var mhits = 0; \
         var mt = new Timer(function(){ mhits++; }, ''); \
         mt.mode = 1; \
         mt.interval = 20; \
         mt.enabled = true;",
    );
    env.poll_for(120, 10);
    let fires = env.eval_int("mhits");
    if fires < 3 {
        problems.push(format!(
            "mode=1 fire count: observed {fires} fires in ~120 ms, expected >= 3 \
             (mode is event priority, not one-shot: TimerIntf.cpp:86-96)"
        ));
    }

    assert_no_problems(problems);
}

/// Reference: the `Timer` native class registers exactly the constructor,
/// `onTimer` and the four properties `interval`/`enabled`/`capacity`/`mode`
/// (`TimerIntf.cpp:137-259`; 5 reference
/// members). There is no `count` and no `action` member — reading either
/// raises a "member does not exist" error, in the reference and in our port
/// alike (our builder registers the same set).
#[test]
fn count_and_action_members_do_not_exist() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run("var t = new Timer(function(){}, '');");
    if let Ok(value) = env.eval("t.count") {
        problems.push(format!(
            "t.count: observed {value:?}, expected a member-not-found error \
             (reference Timer registers no count member, TimerIntf.cpp:137-259)"
        ));
    }
    if let Ok(value) = env.eval("t.action") {
        problems.push(format!(
            "t.action: observed {value:?}, expected a member-not-found error \
             (reference Timer registers no action member, TimerIntf.cpp:137-259)"
        ));
    }

    assert_no_problems(problems);
}

/// Reference: no `id` member exists on the reference `Timer`
/// (`TimerIntf.cpp:137-259`); `id` is our only *extra* Timer member. Reading
/// `t.id` must therefore raise.
#[test]
fn id_member_is_not_part_of_the_reference_timer() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("var t = new Timer(function(){}, '');");
    match env.eval("t.id") {
        Ok(value) => panic!(
            "reference Timer has no `id` member (TimerIntf.cpp:137-259), \
             observed {value:?}"
        ),
        Err(error) => println!("observed `t.id` error (matches the reference): {error}"),
    }
}

// ---------------------------------------------------------------------------
// 6. onTimer dispatch
// ---------------------------------------------------------------------------

/// Reference: a plain `Timer`'s `onTimer` event is posted at the timer
/// object (`TimerIntf.cpp:100`), resolved through its class chain to the
/// native `Timer.onTimer`, which invokes the constructor callback with the
/// event (`TimerIntf.cpp:156-171`). With `''` as the action name our glue
/// takes the same path (`timer_poll` → `call_member(owner, "onTimer")` →
/// `timer_on_timer` → `invoke_action` → `call_detached`), so a real timer
/// fire reaches the callback with the event dictionary (pinned in
/// [`constructor_with_an_empty_action_name_invokes_the_callback_with_the_event`];
/// here only the invocation count and timing are asserted).
#[test]
fn a_plain_timer_invokes_its_constructor_callback_on_fire() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var hits = 0; \
         var t = new Timer(function(){ hits++; }, ''); \
         t.interval = 25; \
         t.enabled = true;",
    );
    env.advance(60);
    env.poll();
    let hits = env.eval_int("hits");
    assert!(
        hits >= 1,
        "the constructor callback never ran (observed {hits} fires)"
    );
    env.advance(50);
    env.poll();
    assert!(
        env.eval_int("hits") > hits,
        "an enabled repeating timer stopped firing"
    );
}

/// Reference: the `onTimer` event is dispatched on the *object*
/// (`EventIntf.cpp:104` `Target->FuncCall(0, "onTimer", ...)`), so a script
/// subclass's override shadows the native `Timer.onTimer`
/// (`TimerIntf.cpp:156`) — the constructor callback is only invoked by the
/// native member and therefore runs **zero** times while the override is in
/// place. Our `timer_poll` dispatches through `call_member(owner, "onTimer")`
/// with the same class-chain lookup.
#[test]
fn subclass_on_timer_override_wins_and_replaces_the_ctor_callback() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var ovHits = 0; var cbHits = 0; \
         class MyTimer extends Timer { \
             function MyTimer(cb) { super.Timer(cb, ''); } \
             function onTimer() { ovHits++; } \
         } \
         var mt = new MyTimer(function(){ cbHits++; }); \
         mt.onTimer();",
    );
    check(
        &mut problems,
        "direct onTimer dispatches to the override",
        env.eval_int("ovHits"),
        1,
    );
    check(
        &mut problems,
        "direct onTimer additionally runs the ctor callback",
        env.eval_int("cbHits"),
        0,
    );

    env.run("mt.interval = 25; mt.enabled = true;");
    env.advance(60);
    env.poll();
    env.advance(40);
    env.poll();
    check(
        &mut problems,
        "override invocations after two poll waves",
        env.eval_int("ovHits") >= 3,
        true,
    );
    check(
        &mut problems,
        "ctor callback invocations while the override exists",
        env.eval_int("cbHits"),
        0,
    );
    check(
        &mut problems,
        "subclass timer still enabled",
        env.eval_int("mt.enabled"),
        1,
    );

    assert_no_problems(problems);
}

/// Reference: there is no `cancel` member on `Timer`
/// (`TimerIntf.cpp:137-259`) — reading it raises. Disabling *from inside*
/// the callback takes effect immediately: `SetEnabled(false)` runs
/// synchronously on the delivering thread and cancels queued events
/// (`TimerImpl.cpp:270-284`), and the thread re-checks `GetEnabled()` on its
/// next pass (`TimerImpl.cpp:125`), so the timer never fires again. The
/// override receives the timer object as `this` (the event is delivered to
/// the object: `EventIntf.cpp:104-105` passes `Target` as `objthis`).
#[test]
fn override_disabling_itself_stops_firing_and_cancel_does_not_exist() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var ovHits = 0; var thisOk = -1; \
         class T2 extends Timer { \
             function T2(cb) { super.Timer(cb, ''); } \
             function onTimer() { \
                 ovHits++; \
                 thisOk = (this === t2) ? 1 : 0; \
                 this.enabled = false; \
             } \
         } \
         var t2 = new T2(function(){}); \
         t2.interval = 25; \
         t2.enabled = true;",
    );
    for _ in 0..80 {
        if env.eval_int("ovHits") != 0 {
            break;
        }
        env.advance(5);
        env.poll();
    }
    check(
        &mut problems,
        "override invocations (first fire)",
        env.eval_int("ovHits") >= 1,
        true,
    );
    check(
        &mut problems,
        "`this` inside the override is the timer object",
        env.eval_int("thisOk"),
        1,
    );
    env.poll_for(90, 10);
    check(
        &mut problems,
        "further fires after the override disabled the timer",
        env.eval_int("ovHits"),
        1,
    );
    check(
        &mut problems,
        "enabled after self-disable",
        env.eval_int("t2.enabled"),
        0,
    );
    if let Ok(value) = env.eval("t2.cancel") {
        problems.push(format!(
            "t2.cancel: observed {value:?}, expected a member-not-found error \
             (reference Timer has no cancel method, TimerIntf.cpp:137-259)"
        ));
    }

    assert_no_problems(problems);
}

/// Re-entrancy: a callback that constructs (and enables) another timer from
/// inside the dispatch. The reference delivers events on the main thread
/// while the timer thread keeps running (`EventIntf.cpp:97-110`,
/// `TimerImpl.cpp:110-160`), so the nested timer joins the schedule and the
/// outer timer's callback stays registered. Our glue takes the action owner
/// out of the binding around the call and restores it (`timer.rs`
/// `invoke_action`), so it must be neither lost nor invoked twice per fire.
#[test]
fn a_callback_creating_a_timer_reenters_without_losing_the_callback() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var hits1 = 0; var hits2 = 0; var t2 = null; \
         var t1 = new Timer(function(){ \
             hits1++; \
             if (t2 == null) { \
                 t2 = new Timer(function(){ hits2++; }, ''); \
                 t2.interval = 15; \
                 t2.enabled = true; \
             } \
         }, ''); \
         t1.interval = 20; \
         t1.enabled = true;",
    );
    env.poll_for(140, 10);

    let hits1 = env.eval_int("hits1");
    let hits2 = env.eval_int("hits2");
    assert!(
        hits1 >= 2,
        "the outer callback was lost after the nested construction (observed {hits1})"
    );
    assert!(
        hits1 <= 10,
        "the outer callback was double-invoked per fire (observed {hits1} in ~140 ms)"
    );
    assert!(
        hits2 >= 2,
        "the nested timer never fired (observed {hits2})"
    );
    assert!(
        hits2 <= 15,
        "the nested callback was double-invoked per fire (observed {hits2})"
    );
}

/// Re-entrancy: a callback that invalidates its own timer.
///
/// Reference: `invalidate` runs `tTJSNI_Timer::Invalidate` immediately —
/// `tTVPTimerThread::Remove` plus event cancellation
/// (`reference/cpp/core/utils/impl/TimerImpl.cpp:378-381`) and
/// `Owner = nullptr` (`TimerIntf.cpp:58-64`). The callback completes this
/// fire, the timer is gone from the thread (so an "any enabled timer"
/// reading is `false`), and it never fires again.
///
/// Our glue matches: `timer_invalidate` drops the binding (releasing the
/// action owner and the retained timer object) *and* removes the L2 entry
/// (`timer.rs` `timer_invalidate`), so the poll reports nothing enabled,
/// the action cannot be restored after the in-flight call returns (no
/// resurrection), and no later deadline dispatches anything.
#[test]
fn a_callback_that_invalidates_itself_stops_dispatching() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var shits = 0; \
         var st = new Timer(function(){ shits++; invalidate st; }, ''); \
         st.interval = 20; \
         st.enabled = true;",
    );
    env.advance(40);
    env.poll();
    check(
        &mut problems,
        "fires on the first due poll",
        env.eval_int("shits"),
        1,
    );
    check(
        &mut problems,
        "poll after `invalidate` inside the callback (reference: timer removed \
         from the thread, TimerImpl.cpp:378-381)",
        env.poll(),
        false,
    );

    env.advance(30);
    env.poll();
    env.advance(30);
    env.poll();
    check(
        &mut problems,
        "re-invocations after self-invalidation (reference: Owner == nullptr, \
         TimerIntf.cpp:58-64)",
        env.eval_int("shits"),
        1,
    );

    // Once the script drops the object, `timer_destroy` must remove the
    // registry entry: no leak, no dangling id.
    env.run("st = null;");
    check(
        &mut problems,
        "poll after dropping the invalidated object",
        env.poll(),
        false,
    );

    assert_no_problems(problems);
}

/// Reference: an exception thrown by the action propagates out of the event
/// delivery (`EventIntf.cpp:97-110` rethrows) but never touches the timer:
/// nothing in `TimerIntf.cpp`/`TimerImpl.cpp` disables a timer because its
/// callback failed, so it keeps firing on schedule. Our glue logs the
/// dispatch failure and keeps delivering (`timer.rs` `timer_on_timer` and
/// `timer_poll`'s `onTimer dispatch failed ... the timer stays enabled`
/// branches) — there is no disable-on-error path anywhere.
#[test]
fn a_failing_callback_does_not_disable_the_timer_in_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var errHits = 0; \
         var et = new Timer(function(){ errHits++; throw 'boom'; }, ''); \
         et.interval = 20; \
         et.enabled = true;",
    );
    env.poll_for(120, 10);
    let fires = env.eval_int("errHits");
    if fires < 2 {
        problems.push(format!(
            "callback fires after a thrown error: observed {fires} in ~120 ms, \
             expected >= 2 (the reference never disables a timer because its \
             callback threw: TimerIntf.cpp:156-171 returns TJS_S_OK and no \
             reference code path links delivery errors to SetEnabled)"
        ));
    }
    check(
        &mut problems,
        "enabled after a failing callback (reference keeps it armed)",
        env.eval_int("et.enabled"),
        1,
    );

    assert_no_problems(problems);
}

// ---------------------------------------------------------------------------
// 7. Teardown
// ---------------------------------------------------------------------------

/// Reference: `invalidate timer;` runs `tTJSNI_Timer::Invalidate`
/// immediately — the timer leaves the timer thread
/// (`TimerImpl.cpp:378-381`) and its queued events are cancelled
/// (`TimerIntf.cpp:58-64`), so nothing is armed any more (`timer_poll`'s
/// "any enabled" reading must be `false`) and it can never fire again, even
/// while the script still holds the object. Dropping the object afterwards
/// must leave no registry entry behind (`timer_destroy`).
///
/// Our glue matches: `timer_invalidate` removes the binding *and* the L2
/// entry immediately (`timer.rs` `timer_invalidate`), so the poll reads
/// `false` right after the statement and no late fire can dispatch;
/// `timer_destroy` then finds nothing left to remove.
#[test]
fn invalidate_removes_the_timer_from_the_registry() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var thits = 0; \
         var tt = new Timer(function(){ thits++; }, ''); \
         tt.interval = 30; \
         tt.enabled = true; \
         invalidate tt;",
    );
    check(
        &mut problems,
        "poll right after `invalidate tt;` (reference: removed from the \
         timer thread, TimerImpl.cpp:378-381)",
        env.poll(),
        false,
    );

    // No late fire: the dispatch handles are gone in both implementations.
    env.advance(70);
    env.poll();
    env.advance(40);
    env.poll();
    check(
        &mut problems,
        "late fires after `invalidate`",
        env.eval_int("thits"),
        0,
    );
    check(
        &mut problems,
        "poll while the invalidated object is still referenced",
        env.poll(),
        false,
    );

    // Dropping the last script reference must tear the registry entry down.
    env.run("tt = null;");
    check(
        &mut problems,
        "poll after dropping the invalidated object",
        env.poll(),
        false,
    );

    assert_no_problems(problems);
}

// ---------------------------------------------------------------------------
// 8. Multiple timers
// ---------------------------------------------------------------------------

/// Reference: the timer thread iterates `List` in insertion order and posts
/// the events in that order (`TimerImpl.cpp:122-146`, `AddItem` appends at
/// `TimerImpl.cpp:222-227`); the event queue delivers same-priority events
/// in post order (`EventIntf.cpp:400-420`). Our L2 registry iterates a
/// `BTreeMap` keyed by the monotonic id (`crates/engine/src/timer.rs`
/// `advance`), which is the same creation order — so simultaneous timers
/// fire `a`, `b`, `c` deterministically, exactly once each per due window.
#[test]
fn simultaneous_timers_fire_in_creation_order() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var order = ''; \
         var ca = 0; var cb = 0; var cc = 0; \
         var ta = new Timer(function(){ ca++; order += 'a'; }, ''); \
         var tb = new Timer(function(){ cb++; order += 'b'; }, ''); \
         var tc = new Timer(function(){ cc++; order += 'c'; }, ''); \
         ta.interval = 30; tb.interval = 30; tc.interval = 30; \
         ta.enabled = true; tb.enabled = true; tc.enabled = true;",
    );
    env.advance(45);
    env.poll();

    assert_eq!(
        env.eval_ok("order"),
        TjsValue::String("abc".into()),
        "simultaneous timers must fire in creation order"
    );
    assert_eq!(env.eval_int("ca"), 1, "timer a fired more than once");
    assert_eq!(env.eval_int("cb"), 1, "timer b fired more than once");
    assert_eq!(env.eval_int("cc"), 1, "timer c fired more than once");
}

/// Reference: disabling timer B from inside timer A's callback (both due in
/// the same tick) cancels B's already-queued `onTimer` event — `SetEnabled`
/// calls `CancelEvents` (`TimerImpl.cpp:277-281`) which removes it from the
/// queue (`EventIntf.cpp:269-330`) — so B never runs in that round, while A
/// completes normally and keeps firing.
///
/// Our dispatch computes the due list up front (`advance`) and re-checks
/// `enabled` before invoking each due timer (`timer.rs` `timer_poll` loop),
/// so a timer disabled mid-round is cancelled exactly like the reference.
#[test]
fn a_peer_disabled_from_a_callback_is_cancelled_before_it_fires() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var aHits = 0; var bHits = 0; \
         var pa = new Timer(function(){ aHits++; pb.enabled = false; }, ''); \
         var pb = new Timer(function(){ bHits++; }, ''); \
         pa.interval = 20; pb.interval = 20; \
         pa.enabled = true; pb.enabled = true;",
    );
    env.advance(35);
    env.poll();
    check(
        &mut problems,
        "first callback (A) fires exactly once",
        env.eval_int("aHits"),
        1,
    );
    check(
        &mut problems,
        "B fires in the round where A disabled it mid-callback (reference: \
         SetEnabled(false) cancels B's queued event, TimerImpl.cpp:277-281)",
        env.eval_int("bHits"),
        0,
    );

    env.advance(40);
    env.poll();
    check(
        &mut problems,
        "A keeps firing on later rounds",
        env.eval_int("aHits") >= 2,
        true,
    );
    check(
        &mut problems,
        "B fires on later rounds while disabled",
        env.eval_int("bHits"),
        0,
    );

    assert_no_problems(problems);
}

/// The timer's action owner is invoked through
/// `tTJSVariantClosure::FuncCall(..., objthis = nullptr)`, which resolves the
/// receiver as `ObjThis ? ObjThis : Object` — the closure's own `ObjThis`
/// wins (`reference/cpp/core/tjs2/tjsVariant.h:219-228`; the reference's
/// `Timer.onTimer` passes `nullptr`, `reference/cpp/core/utils/TimerIntf.cpp:
/// 156-171`). A method captured as the action owner therefore runs with the
/// object it was read from as `this`.
///
/// This is the real-game `GameSceneManager` shape: the callback is the
/// `onWaitSceneChange` method, and `this._param` (a class member) must resolve
/// on the same object the constructor initialised. A method reference kept its
/// `ObjThis` across the native constructor argument marshalling, so the
/// member is the assigned `Dictionary`.
#[test]
fn timer_action_owner_runs_with_the_object_it_was_read_from() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "class SceneManager { \
             var _param; \
             var _timer; \
             function SceneManager() { \
                 _param = %[]; \
                 _timer = new Timer(onWait, ''); \
             } \
             function onWait() { \
                 thisIsManager = (this === m) ? 1 : 0; \
                 paramIsObject = (typeof _param == 'Object') ? 1 : 0; \
                 (Dictionary.assign incontextof _param)(%[a:1]); \
                 (Dictionary.clear incontextof _param)(); \
                 paramStillObject = (typeof _param == 'Object') ? 1 : 0; \
             } \
         } \
         var thisIsManager = -1; var paramIsObject = -1; var paramStillObject = -1; \
         var m = new SceneManager(); \
         m._timer.onTimer();",
    );
    check(
        &mut problems,
        "the action owner runs with the object it was read from as `this`",
        env.eval_int("thisIsManager"),
        1,
    );
    check(
        &mut problems,
        "`_param` resolves on that object (the assigned Dictionary)",
        env.eval_int("paramIsObject"),
        1,
    );
    // Reference: `incontextof` rebinds the class-fetched static method's
    // `ObjThis` (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1282-1286`);
    // the member is unchanged by the call.
    check(
        &mut problems,
        "`(Dictionary.clear incontextof _param)()` leaves `_param` an object",
        env.eval_int("paramStillObject"),
        1,
    );

    assert_no_problems(problems);
}

/// A throwing callback must not corrupt the retained action owner: the
/// reference keeps the timer enabled (`TimerIntf.cpp:156-171` returns
/// `TJS_S_OK` and no reference path links a delivery error to `SetEnabled`),
/// and the callback is still the same closure with the same `ObjThis` on the
/// next fire (`tjsVariant.h:219-228`).
///
/// This pins the audit's open question — whether `tjs2_call_value` preserves
/// the callback's receiver across an exception — with a run: the second fire
/// sees the member on the original object, not `void`.
#[test]
fn throwing_action_owner_keeps_its_receiver_on_the_next_fire() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "class Thrower { \
             var _param; \
             var _timer; \
             var _first; \
             function Thrower() { \
                 _param = %[]; \
                 _first = true; \
                 _timer = new Timer(onWait, ''); \
             } \
             function onWait() { \
                 if (_first) { _first = false; throw 'boom'; } \
                 thisIsThrower = (this === t) ? 1 : 0; \
                 paramIsObject = (typeof _param == 'Object') ? 1 : 0; \
             } \
         } \
         var thisIsThrower = -1; var paramIsObject = -1; \
         var t = new Thrower(); \
         t._timer.onTimer(); \
         t._timer.onTimer();",
    );
    check(
        &mut problems,
        "the receiver survives the first callback's exception",
        env.eval_int("thisIsThrower"),
        1,
    );
    check(
        &mut problems,
        "`_param` still resolves on the receiver after an exception",
        env.eval_int("paramIsObject"),
        1,
    );

    assert_no_problems(problems);
}

/// Reference: the timer's `onTimer` event is posted at the timer object
/// (`TimerIntf.cpp:100`) and the native `onTimer` invokes the constructor's
/// action with the event dictionary built by `TVP_ACTION_INVOKE_BEGIN(0,
/// "onTimer", objthis)` (`TimerIntf.cpp:164`, `EventIntf.h:208-215`):
/// `type == "onTimer"` and `target == the timer object`
/// (`TVPCreateEventObject`, `EventIntf.cpp:709-736`). The action closure runs
/// with its own receiver as `this` (`tTJSVariantClosure::FuncCall` prefers
/// `ObjThis` over the fallback object, `tjsVariant.h:219-228`).
///
/// This drives the **engine** path (the registry's delivery →
/// the registered callback → `dispatch_on_timer`), never a direct
/// `timer.onTimer()` call, so it pins that the timer object is still retained
/// at fire time and `target` is present in the delivered event.
#[test]
fn engine_driven_fire_delivers_type_target_and_this() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var hits = 0; var evType = 'none'; var evTarget = -1; var thisIsOwner = -1; \
         class Probe { \
             function Probe() {} \
             function onAction(ev) { \
                 hits++; \
                 evType = ev.type; \
                 evTarget = (ev.target === probeTimer) ? 1 : 0; \
                 thisIsOwner = (this === probeOwner) ? 1 : 0; \
             } \
         } \
         var probeOwner = new Probe(); \
         var probeTimer = new Timer(probeOwner.onAction, ''); \
         probeTimer.interval = 10; \
         probeTimer.enabled = true;",
    );
    // The frame lands 20 ms past the timer's first deadline (10 ms): the
    // registry replays the two intervals it missed together with the one
    // that was due, so the timer fires three times and the script sees
    // three invocations (the capacity of six does not bite).
    //
    // reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the catch-up
    // count is n = (curtick - NextTick) / Interval + 1.
    // reference/cpp/core/utils/TimerIntf.cpp:69-111 — `Fire(n)` posts
    // min(n, cap - count) events.
    env.advance(30);
    env.poll();

    check(&mut problems, "action invocations", env.eval_int("hits"), 3);
    check(
        &mut problems,
        "event `type`",
        env.eval_ok("evType"),
        TjsValue::String("onTimer".into()),
    );
    check(
        &mut problems,
        "event `target` is the timer object",
        env.eval_int("evTarget"),
        1,
    );
    check(
        &mut problems,
        "`this` inside the action is the constructor's action owner",
        env.eval_int("thisIsOwner"),
        1,
    );

    assert_no_problems(problems);
}

/// Reference: `tTJSNI_BaseTimer::Fire` posts `onTimer` with the mode's
/// priority flag — `TVP_EPT_NORMAL` / `TVP_EPT_EXCLUSIVE` / `TVP_EPT_IDLE`
/// (`TimerIntf.cpp:86-98`) — and `_TVPDeliverAllEvents2` delivers exclusive
/// events before normal, and normal before idle (`EventIntf.cpp:426-519`).
/// Delivery order is the mode priority, not creation order.
///
/// Our `timer_poll` queues the fires the L2 callbacks post and drains them by
/// `mode_priority`. Before that queue existed the fires were dispatched inside
/// `advance` in creation order, so this test — an idle timer created before an
/// exclusive one — observed `idle` first.
#[test]
fn engine_driven_fire_is_delivered_exclusive_before_idle() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut problems = Vec::new();

    env.run(
        "var order = []; \
         var idleTimer = new Timer(function(){ order.add('idle'); }, ''); \
         idleTimer.mode = 2; idleTimer.interval = 10; \
         var exclusiveTimer = new Timer(function(){ order.add('exclusive'); }, ''); \
         exclusiveTimer.mode = 1; exclusiveTimer.interval = 10; \
         idleTimer.enabled = true; \
         exclusiveTimer.enabled = true;",
    );
    // The frame lands 20 ms past the timers' first deadline (10 ms): each
    // timer fires three times (the two intervals it missed and the one
    // that was due), so the queue holds three postings per timer and the
    // capacity of six does not bite.
    //
    // reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the catch-up
    // count is n = (curtick - NextTick) / Interval + 1.
    env.advance(30);
    env.poll();

    check(
        &mut problems,
        "first delivered event",
        env.eval_ok("order[0]"),
        TjsValue::String("exclusive".into()),
    );
    check(
        &mut problems,
        "second delivered event",
        env.eval_ok("order[1]"),
        TjsValue::String("exclusive".into()),
    );
    check(
        &mut problems,
        "third delivered event",
        env.eval_ok("order[2]"),
        TjsValue::String("exclusive".into()),
    );
    check(
        &mut problems,
        "fourth delivered event",
        env.eval_ok("order[3]"),
        TjsValue::String("idle".into()),
    );

    assert_no_problems(problems);
}

/// The reference native `onTimer` has no disable-on-error path
/// (`TimerIntf.cpp:156-171`), and an `enabled = false` written by the action
/// must disarm the timer for the next poll: `SetEnabled(false)` calls
/// `CancelEvents()` and drops `NextTick` (`tTVPTimerThread::SetEnabled`,
/// `reference/cpp/core/utils/impl/TimerImpl.cpp:277-281`), and the thread then
/// skips the disabled timer (`TimerImpl.cpp:125`).
///
/// The port reaches the L2 registry through the handle the poll itself uses,
/// so a write made from inside the callback is re-entrant. This test advances
/// twice and requires exactly one invocation: if the write were lost, the
/// re-armed deadline would fire again on the second poll.
#[test]
fn self_disabling_action_is_invoked_once() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var hits = 0; \
         var t = new Timer(function(){ hits++; t.enabled = false; }, ''); \
         t.interval = 10; \
         t.enabled = true;",
    );
    env.advance(30);
    env.poll();
    env.advance(30);
    env.poll();

    assert_eq!(
        env.eval_int("hits"),
        1,
        "a self-disabled timer fired again: the action's `enabled = false` write was lost"
    );
    assert_eq!(env.eval_int("t.enabled"), 0);
}

/// The reference native timer keeps only a raw `Owner` pointer
/// (`reference/cpp/core/utils/TimerIntf.cpp:52,60`), so a script object reaches
/// reference count zero and its native destroy runs, removing the
/// `TimerBinding`. A strong `TimerBinding.owner` rooted the timer, so each
/// dropped timer leaked its binding; the state map is the observable form of
/// the "map size back to baseline" rule.
#[test]
fn dropped_timers_release_their_bindings() {
    let _lock = vm_lock();
    let env = Env::new();
    let baseline = env._host.timer_count();
    env.run(
        "for (var cycle = 0; cycle < 5; cycle++) { \
             var t = new Timer(function(){}); \
             t = null; \
         }",
    );
    // The collector must run outside the creating frame: a separate
    // execution's `doCompact` plus the direct GC pass.
    env.run("System.doCompact();");
    env.engine.do_gc().expect("idle compact");
    assert_eq!(
        env._host.timer_count(),
        baseline,
        "a dropped Timer must release its dispatch-state binding"
    );
}

/// The shipped game's `system/voice.tjs` gates its lip-sync flag on whether
/// `getSample.dll` has already been linked: its compiled top level publishes
/// `WaveSoundBuffer.enableGetSample` only when
/// `typeof WaveSoundBuffer.setDefaultAheads` is `"undefined"`, and its
/// `onFlip` timer action reads that flag before it reads the plugin's
/// `sampleValue`. The plugin's members are `NCB_ATTACH_*` registrations
/// (`reference/cpp/plugins/getSample.cpp:167-181`), so they exist only after
/// `Plugins.link` runs the module's registration; the native `onTimer` then
/// invokes the action and keeps the timer enabled
/// (`reference/cpp/core/utils/TimerIntf.cpp:156-171`).
///
/// Before the link the class has no `setDefaultAheads`, which is what makes
/// the script's probe true; after the link the flag is `1` and the action runs
/// to its `onVoiceFlip` callback instead of raising
/// `Member "enableGetSample" does not exist` on every fire.
#[test]
fn voice_tjs_publishes_its_plugin_flag_and_the_action_reads_it() {
    let _lock = vm_lock();
    let env = Env::new();

    // The plugin surface is absent until a script links it. A port that
    // registers it at startup makes the probe false and the script skips the
    // whole block, leaving the flag missing.
    assert_eq!(
        env.eval_ok("typeof WaveSoundBuffer.setDefaultAheads"),
        TjsValue::String("undefined".into()),
        "getSample members must not exist before `Plugins.link`"
    );

    // Decompiled top level of the game's `system/voice.tjs`. `CanLoadPlugin`
    // is the game's own definition from `system/Initialize.tjs`; the shipped
    // `plugin/getSample.dll` exists, so the probe is true.
    env.run(
        r#"
        function CanLoadPlugin(name) { return true; }
        global.lipFlipTime = 100;
        if (typeof WaveSoundBuffer.setDefaultAheads == "undefined") {
            WaveSoundBuffer.enableGetSample = 0;
            if (CanLoadPlugin("getSample.dll")) {
                try {
                    Plugins.link("getSample.dll");
                    WaveSoundBuffer.setDefaultAheads(0);
                    WaveSoundBuffer.enableGetSample = 1;
                } catch (e) {
                    System.inform("getSample.dll load failed: " + e.message);
                }
            }
        }
        "#,
    );

    assert_eq!(
        env.eval_ok("WaveSoundBuffer.enableGetSample"),
        TjsValue::Integer(1),
        "the script's post-link publish must be the value `onFlip` reads"
    );
    assert_ne!(
        env.eval_ok("typeof WaveSoundBuffer.setDefaultAheads"),
        TjsValue::String("undefined".into()),
        "the link must install the plugin members"
    );

    // The `onFlip` half. The real script also guards on `status == "play"`;
    // the flag read and the Timer delivery are the parts under test here.
    // `global.WaveSoundBuffer` is the spelling the compiled script uses: a
    // bare class name inside the class body resolves to the superclass
    // object, not the global class.
    env.run(
        r#"
        class VoiceProbe extends WaveSoundBuffer {
            function VoiceProbe() {
                super.WaveSoundBuffer();
                this.lipLevel1 = 0.1;
                this.lipLevel2 = 0.5;
            }
            function onFlip() {
                var level = 0;
                if (global.WaveSoundBuffer.enableGetSample) {
                    if (this.sampleValue > this.lipLevel2) {
                        level = 2;
                    } else if (this.sampleValue > this.lipLevel1) {
                        level = 1;
                    }
                }
                this.owner.onVoiceFlip(this, level);
            }
        }
        class VoiceOwner {
            function VoiceOwner() { this.flips = 0; this.level = -1; }
            function onVoiceFlip(voice, level) { this.flips++; this.level = level; }
        }
        var voiceOwner = new VoiceOwner();
        var voice = new VoiceProbe();
        voice.owner = voiceOwner;
        voice.flipTimer = new Timer(voice.onFlip, "");
        voice.flipTimer.interval = 100;
        voice.flipTimer.enabled = true;
        "#,
    );

    env.poll_for(300, 100);

    assert_eq!(
        env.eval_int("voiceOwner.flips"),
        3,
        "the timer action must reach `onVoiceFlip` on every fire"
    );
    assert_eq!(
        env.eval_int("voice.flipTimer.enabled"),
        1,
        "a dispatched action leaves the timer enabled"
    );
}

/// The registry's pump delivers the timer: the L2 registry advances, its
/// callback posts the `onTimer` event, and the delivery step hands it to the
/// script.
///
/// Reference: `tTJSNI_BaseTimer::Fire` does not call the action, it **posts**
/// `onTimer` to the owner (`reference/cpp/core/utils/TimerIntf.cpp:100`), and
/// the event loop delivers the queued events later, exclusive first and idle
/// last (`reference/cpp/core/tjs2/EventIntf.cpp:426-519`).
#[test]
fn the_registry_pump_delivers_the_timer_to_the_script() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var hits = 0; \
         var t = new Timer(function() { hits++; }, ''); \
         t.interval = 1; \
         t.enabled = true;",
    );

    // A virtual frame step is enough for the 1 ms timer.
    for _ in 0..200 {
        env.advance(16);
        env.poll();
        if env.eval_int("hits") >= 1 {
            break;
        }
    }
    assert!(env.eval_int("hits") >= 1, "the timer callback never ran");
}
