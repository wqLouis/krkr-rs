//! Integration tests for the `CDDASoundBuffer` / `MIDISoundBuffer`
//! native classes.
//!
//! The tests exercise the script-visible surface that does not need an L1
//! codec or a CD/MIDI device: construction, the property round-trips
//! (including the `0..=100000` volume scale), the shared playback methods on
//! an unloaded buffer and the typed deferrals (CD track selection, `midiOut`
//! and the missing decoder). The generic `open` → decode path needs a host
//! decoder, which an integration test cannot install, so only its typed error
//! paths are covered here — mirroring `tests/sound.rs`.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The engine is boxed and the host is declared **before** it so that (a) the
/// binding's stored engine address stays valid when `Env` is moved and (b) the
/// host's `Drop` removes the process-wide binding before the VM is destroyed.
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
            .eval(expression, "cdda-midi-test")
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

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "cdda-midi-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

/// The reference defaults for both classes: full volume, no loop, no pause,
/// unloaded.
fn assert_defaults(env: &Env, var: &str, class: &str) {
    env.run(&format!("var {var} = new {class}();"));
    assert_eq!(env.eval_int(&format!("{var}.volume")), 100_000);
    assert_eq!(env.eval_int(&format!("{var}.volume2")), 100_000);
    assert_eq!(env.eval_int(&format!("{var}.looping")), 0);
    assert_eq!(env.eval_int(&format!("{var}.paused")), 0);
    assert_eq!(env.eval_int(&format!("{var}.position")), 0);
    assert_eq!(env.eval_int(&format!("{var}.totalTime")), 0);
    assert_eq!(
        env.eval_ok(&format!("{var}.status")),
        TjsValue::String("unload".into())
    );
}

#[test]
fn both_constructors_expose_the_reference_defaults() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_defaults(&env, "cdda", "CDDASoundBuffer");
    assert_defaults(&env, "midi", "MIDISoundBuffer");
}

#[test]
fn properties_round_trip_on_the_script_scale() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new MIDISoundBuffer();");

    env.run(
        "s.volume = 50000; s.volume2 = 25000; s.position = 250; s.looping = true; s.paused = true;",
    );
    assert_eq!(env.eval_int("s.volume"), 50_000);
    assert_eq!(env.eval_int("s.volume2"), 25_000);
    assert_eq!(env.eval_int("s.position"), 250);
    assert_eq!(env.eval_int("s.looping"), 1);
    assert_eq!(env.eval_int("s.paused"), 1);

    // The reference clamps at both ends of the 0..=100000 scale.
    env.run("s.volume = 200000; s.volume2 = -5;");
    assert_eq!(env.eval_int("s.volume"), 100_000);
    assert_eq!(env.eval_int("s.volume2"), 0);

    // The `paused` flag toggles back off.
    env.run("s.paused = false;");
    assert_eq!(env.eval_int("s.paused"), 0);
}

#[test]
fn cdda_open_is_a_typed_cd_track_deferral() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new CDDASoundBuffer();");

    let message = env.error("s.open('D:')");
    assert!(
        message.contains("CD track selection"),
        "unexpected message: {message}"
    );
    // The failed open leaves the buffer unloaded rather than lying.
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("unload".into()));
    assert_eq!(env.eval_int("s.totalTime"), 0);
}

#[test]
fn midi_open_without_storage_reports_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new MIDISoundBuffer();");

    let message = env.error("s.open('song.mid')");
    assert!(
        message.contains("no storage is attached"),
        "unexpected message: {message}"
    );
}

#[test]
fn midi_out_is_a_typed_device_deferral() {
    let _lock = vm_lock();
    let env = Env::new();

    let message = env.error("MIDISoundBuffer.midiOut(0)");
    assert!(
        message.contains("MIDI output device"),
        "unexpected message: {message}"
    );
}

#[test]
fn open_requires_an_argument() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new MIDISoundBuffer();");

    let message = env.error("s.open()");
    assert!(
        message.contains("required"),
        "unexpected message: {message}"
    );
}

#[test]
fn shared_playback_methods_are_callable_on_an_unloaded_buffer() {
    let _lock = vm_lock();
    let env = Env::new();

    for (index, class) in ["CDDASoundBuffer", "MIDISoundBuffer"]
        .into_iter()
        .enumerate()
    {
        let var = format!("s{index}");
        env.run(&format!("var {var} = new {class}();"));

        assert_eq!(env.eval_ok(&format!("{var}.play()")), TjsValue::Void);
        assert_eq!(env.eval_ok(&format!("{var}.stop()")), TjsValue::Void);
        assert_eq!(env.eval_ok(&format!("{var}.stopFade()")), TjsValue::Void);
        assert_eq!(env.eval_ok(&format!("{var}.fade(0, 50)")), TjsValue::Void);
        assert_eq!(
            env.eval_ok(&format!("{var}.onStatusChanged('play')")),
            TjsValue::Void
        );
        assert_eq!(
            env.eval_ok(&format!("{var}.onFadeCompleted()")),
            TjsValue::Void
        );
        assert_eq!(
            env.eval_ok(&format!("{var}.status")),
            TjsValue::String("unload".into())
        );
    }
}
