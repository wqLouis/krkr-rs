//! Integration tests for the `getSample.dll` plugin surface on
//! `WaveSoundBuffer`.
//!
//! The plugin attaches the legacy `getSample` function and the new-style
//! `sampleValue` / `sampleCount` / `sampleAhead` properties plus
//! `setDefaultCounts` / `setDefaultAheads` / `finalize` to the already
//! registered `WaveSoundBuffer` class. The L2 `getVisBuffer` that the plugin
//! drives captures no visualization buffer in this port (see
//! `natives/sound.rs`), so the numeric sample paths are exercised through
//! their observable surface: the argument coercion, the property defaults and
//! the implicit `useVisBuffer = true`, while the reference reductions
//! themselves are unit-tested in the plugin module over synthetic buffers.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The engine is boxed and the host is declared **before** it so the
/// process-wide binding outlives the VM in declaration order.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
        }
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "get-sample-test")
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

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "get-sample-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

/// The `WaveSoundBufferAdd` defaults are `sampleCount = 100` /
/// `sampleAhead = 0` (`reference/cpp/plugins/getSample.cpp:151-152`); the
/// constructor that the instance-getter hook runs on first use
/// (`getSample.cpp:155-165`) seeds them and writes `useVisBuffer = 1`
/// (`getSample.cpp:79-81`).
#[test]
fn new_style_defaults_match_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    // The reference `WaveSoundBufferAdd` defaults.
    assert_eq!(env.eval_int("s.sampleCount"), 100);
    assert_eq!(env.eval_int("s.sampleAhead"), 0);
    // Reading the property runs the reference constructor, which enables the
    // visualization buffer.
    assert_eq!(env.eval_int("s.useVisBuffer"), 1);
}

/// `sampleCount`/`sampleAhead` are per-object read/write properties
/// (`reference/cpp/plugins/getSample.cpp:131-140,173-176`) backed by the
/// instance's `counts`/`aheads`, which the constructor seeds from the
/// *current* defaults (`getSample.cpp:74`) — one object's writes never reach
/// another, and a first touch initializes both fields from the defaults.
#[test]
fn sample_count_and_ahead_round_trip_per_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var a = new WaveSoundBuffer(); var b = new WaveSoundBuffer();");

    env.run("a.sampleCount = 16; a.sampleAhead = 3;");
    assert_eq!(env.eval_int("a.sampleCount"), 16);
    assert_eq!(env.eval_int("a.sampleAhead"), 3);

    // The second object is untouched by the first object's writes.
    assert_eq!(env.eval_int("b.sampleCount"), 100);
    assert_eq!(env.eval_int("b.sampleAhead"), 0);

    // Binding `sampleCount` first initializes both fields from the defaults,
    // then overwrites the one being set.
    env.run("var c = new WaveSoundBuffer(); c.sampleAhead = 9;");
    assert_eq!(env.eval_int("c.sampleCount"), 100);
    assert_eq!(env.eval_int("c.sampleAhead"), 9);
}

/// `setDefaultCounts`/`setDefaultAheads` change the static defaults
/// (`reference/cpp/plugins/getSample.cpp:145-152,178-181`) that only
/// *subsequently* constructed instances read (`getSample.cpp:74`); the
/// functions are class members, so an instance can reach them too.
#[test]
fn set_default_counts_and_aheads_apply_to_later_objects_only() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var before = new WaveSoundBuffer(); var seen = before.sampleCount;");

    env.run("WaveSoundBuffer.setDefaultCounts(7); WaveSoundBuffer.setDefaultAheads(4);");
    env.run("var after = new WaveSoundBuffer();");

    assert_eq!(env.eval_int("after.sampleCount"), 7);
    assert_eq!(env.eval_int("after.sampleAhead"), 4);
    // An object whose state was initialized earlier keeps the old defaults.
    assert_eq!(env.eval_int("seen"), 100);
    assert_eq!(env.eval_int("before.sampleCount"), 100);
    assert_eq!(env.eval_int("before.sampleAhead"), 0);

    // The functions are reachable from an instance too.
    env.run("after.setDefaultCounts(12); after.setDefaultAheads(2);");
    env.run("var third = new WaveSoundBuffer();");
    assert_eq!(env.eval_int("third.sampleCount"), 12);
    assert_eq!(env.eval_int("third.sampleAhead"), 2);
}

/// `sampleValue` is a read-only real (`reference/cpp/plugins/getSample.cpp:103-124,172`):
/// it zeroes the buffer, calls the object's `getVisBuffer`
/// (`getSample.cpp:106`) and returns `(peak/32768)²`. Reading it (or any
/// other plugin member) constructs the per-object state, whose constructor
/// writes `useVisBuffer = 1` (`getSample.cpp:79-81`).
#[test]
fn sample_value_is_a_real_and_implicitly_enables_the_visualization_buffer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    // L2 captures no visualization buffer, so `getVisBuffer` reports 0 samples
    // and the reduction stays at 0.0; the result is still a real.
    assert_eq!(env.eval_real("s.sampleValue"), 0.0);
    assert_eq!(env.eval_int("s.useVisBuffer"), 1);

    // A fresh object gets `useVisBuffer` enabled only when a getSample member
    // touches it.
    env.run("var t = new WaveSoundBuffer();");
    assert_eq!(env.eval_int("t.useVisBuffer"), 0);
    let _ = env.eval_real("t.sampleValue");
    assert_eq!(env.eval_int("t.useVisBuffer"), 1);
}

/// The legacy `getSample(n)` defaults to 100 samples
/// (`reference/cpp/plugins/getSample.cpp:18`), only fills a result for
/// `n > 0` (`getSample.cpp:19` — a non-positive count leaves the call
/// `void`) and returns the integer mean of the non-negative samples
/// (`getSample.cpp:30-37`).
#[test]
fn legacy_get_sample_returns_an_integer_average() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    // With no argument the reference uses 100 samples; the empty visualization
    // buffer reduces to 0.
    assert_eq!(env.eval_int("s.getSample()"), 0);
    assert_eq!(env.eval_int("s.getSample(10)"), 0);

    // A non-positive count leaves the result void (the reference only fills
    // `result` when `n > 0`).
    assert_eq!(env.eval_ok("s.getSample(0)"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.getSample(-3)"), TjsValue::Void);
}

/// The plugin only *attaches* members to the existing `WaveSoundBuffer`
/// class (`reference/cpp/plugins/getSample.cpp:167-176`), so the class's
/// own playback state is untouched by the per-object parameter writes.
#[test]
fn sample_members_coexist_with_the_rest_of_the_wave_surface() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer(); s.sampleCount = 4; s.sampleAhead = 1;");

    // The plugin's writes do not disturb the core `WaveSoundBuffer` state.
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("unload".into()));
    assert_eq!(env.eval_int("s.totalTime"), 0);
    assert_eq!(env.eval_int("s.sampleCount"), 4);
    assert_eq!(env.eval_int("s.sampleAhead"), 1);
}

/// ncbind coerces arguments through `tTJSVariant::operator tjs_int`, which
/// truncates reals (`reference/cpp/core/tjs2/tjsVariant.h:933`), matching
/// the `int` parameters of `setDefaultCounts`/`setDefaultAheads`
/// (`reference/cpp/plugins/getSample.cpp:145-149`).
#[test]
fn set_default_members_truncate_their_real_arguments() {
    let _lock = vm_lock();
    let env = Env::new();

    // Real arguments are truncated toward zero like the reference `(int)` cast.
    env.run("WaveSoundBuffer.setDefaultCounts(5.9); WaveSoundBuffer.setDefaultAheads(-2.7);");
    env.run("var s = new WaveSoundBuffer();");
    assert_eq!(env.eval_int("s.sampleCount"), 5);
    assert_eq!(env.eval_int("s.sampleAhead"), -2);
}

/// A missing argument is `TJS_E_BADPARAMCOUNT` in the reference:
/// `NCB_ATTACH_FUNCTION` wraps the C++ function in ncbind's `doInvoke`,
/// which rejects calls shorter than the signature
/// (`reference/cpp/core/plugin/ncbind.hpp:1178`,
/// `reference/cpp/plugins/getSample.cpp:178-181`); the reference renders
/// that error as "Invalid argument count"
/// (`reference/cpp/core/tjs2/tjsError.h:268`). This port raises a typed
/// error naming the member instead.
#[test]
fn set_default_members_require_their_argument() {
    let _lock = vm_lock();
    let env = Env::new();

    assert!(
        env.error("WaveSoundBuffer.setDefaultCounts()")
            .contains("requires 1 argument")
    );
    assert!(
        env.error("WaveSoundBuffer.setDefaultAheads()")
            .contains("requires 1 argument")
    );
    // The defaults were never touched by the failed calls.
    env.run("var t = new WaveSoundBuffer();");
    assert_eq!(env.eval_int("t.sampleCount"), 100);
    assert_eq!(env.eval_int("t.sampleAhead"), 0);
}

/// `finalize` is the port's per-object teardown hook: the core invokes a
/// `finalize` member during object finalisation, immediately before the
/// native instances are invalidated
/// (`reference/cpp/core/tjs2/tjsObject.cpp:389-401`) — the point where the
/// reference releases its `WaveSoundBufferAdd` instance. Afterwards the
/// next access re-seeds the state from the current defaults, exactly like
/// the instance-getter hook rebuilding it (`getSample.cpp:155-165`).
/// (The reference registers an *empty* `finalize`
/// (`reference/cpp/core/plugin/ncbind.hpp:1869`); a manual call on a live
/// object re-seeds the state rather than doing nothing.)
#[test]
fn finalize_reseeds_the_per_object_state() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("WaveSoundBuffer.setDefaultCounts(5); WaveSoundBuffer.setDefaultAheads(-2);");
    env.run("var t = new WaveSoundBuffer(); t.sampleCount = 3;");
    assert_eq!(env.eval_ok("t.finalize()"), TjsValue::Void);
    assert_eq!(env.eval_int("t.sampleCount"), 5);
    assert_eq!(env.eval_int("t.sampleAhead"), -2);
}

/// The reference `getSample` calls `objthis->getVisBuffer` and propagates
/// the failure (`reference/cpp/plugins/getSample.cpp:26-35`). Called on the
/// class object rather than an instance there is no playback state, and the
/// plugin reports that error instead of silently returning a value.
#[test]
fn get_sample_on_the_class_object_surfaces_the_get_vis_buffer_error() {
    let _lock = vm_lock();
    let env = Env::new();

    // The reference `getSample` calls `objthis->getVisBuffer`. Called on the
    // class object rather than an instance there is no playback state, and the
    // plugin reports that error instead of silently returning a value.
    let message = env.error("WaveSoundBuffer.getSample(4)");
    assert!(
        message.contains("getSample"),
        "unexpected message: {message}"
    );
}

/// Both sample paths reach `getVisBuffer` **through the object** — the
/// legacy `objthis->FuncCall(0, "getVisBuffer", …)`
/// (`reference/cpp/plugins/getSample.cpp:26`) and the new-style
/// `objthis->FuncCall(0, "getVisBuffer", …)` (`getSample.cpp:106`) — so a
/// script member installed on the instance wins over the class's own
/// method, and its return value feeds the reductions.
#[test]
fn get_vis_buffer_is_reached_through_the_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var s = new WaveSoundBuffer(); var calls = 0; \
         s.getVisBuffer = function(buf, count, channel) { calls += 1; return 0; };",
    );

    // The legacy path invokes the override once and reduces its buffer.
    assert_eq!(env.eval_int("s.getSample(4)"), 0);
    assert_eq!(env.eval_int("calls"), 1);

    // The new-style path does too.
    assert_eq!(env.eval_real("s.sampleValue"), 0.0);
    assert_eq!(env.eval_int("calls"), 2);
}
