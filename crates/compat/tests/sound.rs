//! Integration tests for the `WaveSoundBuffer` native class.
//!
//! The tests here exercise the script-visible surface that does not need an
//! L1 codec: construction, the property round-trips (including the
//! `0..=100000` volume/pan scale and the millisecond `position`), the static
//! members, the object-valued `flags`/`labels`/`filters` getters and the
//! typed "no storage / no decoder" error paths. The full `open` → `play` →
//! poll → event flow is covered by the unit tests inside
//! `crates/compat/src/natives/sound.rs`, because the process-wide binding and
//! the private `sound` module are not reachable from an integration test and
//! `compat` cannot link a codec.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The engine is boxed and the host is declared **before** it so that (a) the
/// binding's stored engine address stays valid when `Env` is moved (the `Box`
/// keeps the `Tjs2Engine` on the heap) and (b) the host's `Drop` removes the
/// process-wide binding before the VM is destroyed.
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
            .eval(expression, "sound-test")
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
            .exec_script(script, "sound-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

#[test]
fn constructor_and_default_properties_match_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("var s = new WaveSoundBuffer();");

    // The reference ctor: volume/volume2 100000, pan 0, unloaded status.
    assert_eq!(env.eval_int("s.volume"), 100_000);
    assert_eq!(env.eval_int("s.volume2"), 100_000);
    assert_eq!(env.eval_int("s.pan"), 0);
    assert_eq!(env.eval_int("s.looping"), 0);
    assert_eq!(env.eval_int("s.paused"), 0);
    assert_eq!(env.eval_int("s.position"), 0);
    assert_eq!(env.eval_int("s.samplePosition"), 0);
    assert_eq!(env.eval_int("s.totalTime"), 0);
    assert_eq!(env.eval_int("s.frequency"), 0);
    assert_eq!(env.eval_int("s.bits"), 0);
    assert_eq!(env.eval_int("s.channels"), 0);
    assert_eq!(env.eval_int("s.useVisBuffer"), 0);
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("unload".into()));
    assert_eq!(env.eval_real("s.posX"), 0.0);
    assert_eq!(env.eval_real("s.posY"), 0.0);
    assert_eq!(env.eval_real("s.posZ"), 0.0);

    // `flags` is the 16-slot conditional-loop array; `filters` is a stable
    // array; `labels` is an empty dictionary until a track is opened.
    assert_eq!(env.eval_int("s.flags.length"), 16);
    assert_eq!(env.eval_int("s.filters.length"), 0);
    assert!(matches!(env.eval_ok("s.labels"), TjsValue::Object));
    // Reading `filters` twice yields the same object (reference identity).
    assert_eq!(env.eval_int("s.filters === s.filters"), 1);
}

#[test]
fn volume_and_pan_round_trip_on_the_script_scale() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    env.run("s.volume = 50000; s.volume2 = 25000; s.pan = -75000;");
    assert_eq!(env.eval_int("s.volume"), 50_000);
    assert_eq!(env.eval_int("s.volume2"), 25_000);
    assert_eq!(env.eval_int("s.pan"), -75_000);

    // The reference clamps at both ends of the 0..=100000 scale.
    env.run("s.volume = 200000; s.volume2 = -5; s.pan = 200000;");
    assert_eq!(env.eval_int("s.volume"), 100_000);
    assert_eq!(env.eval_int("s.volume2"), 0);
    assert_eq!(env.eval_int("s.pan"), 100_000);
    env.run("s.pan = -200000;");
    assert_eq!(env.eval_int("s.pan"), -100_000);
}

#[test]
fn position_is_milliseconds_and_3d_position_round_trips() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    env.run("s.position = 250;");
    assert_eq!(env.eval_int("s.position"), 250);

    env.run("s.setPos(1.5, -2.0, 3.25);");
    assert!((env.eval_real("s.posX") - 1.5).abs() < 1e-6);
    assert!((env.eval_real("s.posY") + 2.0).abs() < 1e-6);
    assert!((env.eval_real("s.posZ") - 3.25).abs() < 1e-6);
    env.run("s.posX = 4.0; s.posY = 5.0; s.posZ = 6.0;");
    assert!((env.eval_real("s.posX") - 4.0).abs() < 1e-6);
    assert!((env.eval_real("s.posY") - 5.0).abs() < 1e-6);
    assert!((env.eval_real("s.posZ") - 6.0).abs() < 1e-6);
}

#[test]
fn looping_paused_and_use_vis_buffer_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    env.run("s.looping = true; s.paused = true; s.useVisBuffer = true;");
    assert_eq!(env.eval_int("s.looping"), 1);
    assert_eq!(env.eval_int("s.paused"), 1);
    assert_eq!(env.eval_int("s.useVisBuffer"), 1);
    env.run("s.looping = false; s.paused = false; s.useVisBuffer = false;");
    assert_eq!(env.eval_int("s.looping"), 0);
    assert_eq!(env.eval_int("s.paused"), 0);
    assert_eq!(env.eval_int("s.useVisBuffer"), 0);
}

#[test]
fn static_global_members_are_on_the_class() {
    let _lock = vm_lock();
    let env = Env::new();

    assert_eq!(env.eval_int("WaveSoundBuffer.globalVolume"), 100_000);
    env.run("WaveSoundBuffer.globalVolume = 25000;");
    assert_eq!(env.eval_int("WaveSoundBuffer.globalVolume"), 25_000);

    assert_eq!(env.eval_int("WaveSoundBuffer.globalFocusMode"), 0);
    env.run("WaveSoundBuffer.globalFocusMode = 2;");
    assert_eq!(env.eval_int("WaveSoundBuffer.globalFocusMode"), 2);

    // `freeDirectSound` is a static no-op.
    assert_eq!(
        env.eval_ok("WaveSoundBuffer.freeDirectSound()"),
        TjsValue::Void
    );
}

#[test]
fn event_forwarders_and_get_vis_buffer_are_callable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    assert_eq!(env.eval_ok("s.onStatusChanged('play')"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.onFadeCompleted()"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.onLabel('mid')"), TjsValue::Void);
    // L3 captures no visualization buffer, so the count is 0.
    assert_eq!(env.eval_int("s.getVisBuffer(0, 16, 1)"), 0);
}

#[test]
fn open_without_a_storage_reports_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    let message = env.error("s.open('tone.wav')");
    assert!(
        message.contains("no storage is attached"),
        "unexpected message: {message}"
    );
    // The failed open leaves the buffer unloaded rather than lying.
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("unload".into()));
}

#[test]
fn open_requires_a_storage_name() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    let message = env.error("s.open()");
    assert!(
        message.contains("storage name"),
        "unexpected message: {message}"
    );
}

#[test]
fn play_stop_and_stop_fade_are_callable_on_an_unloaded_buffer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new WaveSoundBuffer();");

    assert_eq!(env.eval_ok("s.play()"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.stop()"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.stopFade()"), TjsValue::Void);
}
