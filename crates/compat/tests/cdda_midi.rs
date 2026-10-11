//! Integration tests for the `CDDASoundBuffer` / `MIDISoundBuffer`
//! native classes.
//!
//! The reference compiles both classes' live surface out
//! (`ENABLE_CDDA`/`TVP_ENABLE_MIDI` are defined nowhere in `reference/`), so
//! every method is an arity-checked no-op and every property reports the
//! disabled build's value. The tests pin each value per class: the classes
//! differ only in `volume` (CD-DA leaves the result `void`, MIDI returns `0`).

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

/// The disabled-build value of every property, per class.
#[test]
fn disabled_build_defaults_are_pinned_per_class() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var cdda = new CDDASoundBuffer(); var midi = new MIDISoundBuffer();");

    // reference/cpp/core/sound/CDDAIntf.cpp:170-234: `position`/`paused`/
    // `totalTime` getters are "not yet implemented" and leave `*result`
    // untouched.
    assert_eq!(env.eval_ok("cdda.position"), TjsValue::Void);
    assert_eq!(env.eval_ok("cdda.paused"), TjsValue::Void);
    assert_eq!(env.eval_ok("cdda.totalTime"), TjsValue::Void);
    // reference/cpp/core/sound/CDDAIntf.cpp:236-259: the `looping` getter body
    // is inside `#ifdef ENABLE_CDDA`, which is undefined.
    assert_eq!(env.eval_ok("cdda.looping"), TjsValue::Void);
    // reference/cpp/core/sound/CDDAIntf.cpp:261-285: the `volume` getter body
    // is likewise compiled out.
    assert_eq!(env.eval_ok("cdda.volume"), TjsValue::Void);
    // reference/cpp/core/sound/CDDAIntf.cpp:287-313: `#else *result = 0;`.
    assert_eq!(env.eval_int("cdda.volume2"), 0);
    // reference/cpp/core/sound/SoundBufferBaseIntf.cpp:58-75: a buffer that
    // never left `ssUnload` reports "unload".
    assert_eq!(
        env.eval_ok("cdda.status"),
        TjsValue::String("unload".into())
    );

    // reference/cpp/core/sound/MIDIIntf.cpp:171-235.
    assert_eq!(env.eval_ok("midi.position"), TjsValue::Void);
    assert_eq!(env.eval_ok("midi.paused"), TjsValue::Void);
    assert_eq!(env.eval_ok("midi.totalTime"), TjsValue::Void);
    // reference/cpp/core/sound/MIDIIntf.cpp:237-260.
    assert_eq!(env.eval_ok("midi.looping"), TjsValue::Void);
    // reference/cpp/core/sound/MIDIIntf.cpp:262-282: the getter is *not*
    // guarded and calls `GetVolume()`, which the disabled `tTJSNI_MIDISoundBuffer`
    // overrides to return `0` (reference/cpp/core/sound/win32/MIDIImpl.h).
    assert_eq!(env.eval_int("midi.volume"), 0);
    // reference/cpp/core/sound/MIDIIntf.cpp:284-310: `#else *result = 0;`.
    assert_eq!(env.eval_int("midi.volume2"), 0);
    assert_eq!(
        env.eval_ok("midi.status"),
        TjsValue::String("unload".into())
    );
}

/// Every property setter's body is inside the disabled `#ifdef`, so an
/// assignment is accepted and changes nothing.
#[test]
fn property_setters_are_inert() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var cdda = new CDDASoundBuffer(); var midi = new MIDISoundBuffer();");

    // reference/cpp/core/sound/CDDAIntf.cpp:170-313 and MIDIIntf.cpp:171-310:
    // the setters are no-ops in the disabled build.
    env.run(
        "cdda.volume = 50000; cdda.volume2 = 25000; cdda.position = 250; \
         cdda.looping = true; cdda.paused = true; cdda.totalTime = 999;",
    );
    env.run(
        "midi.volume = 50000; midi.volume2 = 25000; midi.position = 250; \
         midi.looping = true; midi.paused = true; midi.totalTime = 999;",
    );

    assert_eq!(env.eval_ok("cdda.volume"), TjsValue::Void);
    assert_eq!(env.eval_int("cdda.volume2"), 0);
    assert_eq!(env.eval_ok("cdda.position"), TjsValue::Void);
    assert_eq!(env.eval_ok("cdda.looping"), TjsValue::Void);
    assert_eq!(env.eval_ok("cdda.paused"), TjsValue::Void);
    assert_eq!(env.eval_ok("cdda.totalTime"), TjsValue::Void);

    assert_eq!(env.eval_int("midi.volume"), 0);
    assert_eq!(env.eval_int("midi.volume2"), 0);
    assert_eq!(env.eval_ok("midi.position"), TjsValue::Void);
    assert_eq!(env.eval_ok("midi.looping"), TjsValue::Void);
    assert_eq!(env.eval_ok("midi.paused"), TjsValue::Void);
    assert_eq!(env.eval_ok("midi.totalTime"), TjsValue::Void);
}

/// `status` is read-only in the reference (`TJS_DENY_NATIVE_PROP_SETTER`,
/// `CDDAIntf.cpp:315-327`, `MIDIIntf.cpp:312-324`).
#[test]
fn status_is_read_only() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var s = new MIDISoundBuffer(); var c = new CDDASoundBuffer();");
    assert!(
        !env.error("s.status = 'play'").is_empty(),
        "MIDISoundBuffer.status must reject writes"
    );
    assert!(
        !env.error("c.status = 'play'").is_empty(),
        "CDDASoundBuffer.status must reject writes"
    );
}

/// Every method is an arity-checked no-op that returns `void` and never
/// touches a decoder or device.
#[test]
fn disabled_methods_are_no_ops() {
    let _lock = vm_lock();
    let env = Env::new();

    for (index, class) in ["CDDASoundBuffer", "MIDISoundBuffer"]
        .into_iter()
        .enumerate()
    {
        let var = format!("s{index}");
        env.run(&format!("var {var} = new {class}();"));

        // reference/cpp/core/sound/CDDAIntf.cpp:59-132 and MIDIIntf.cpp:59-132:
        // `open`/`play`/`stop`/`fade`/`stopFade` fall through to `TJS_S_OK`.
        assert_eq!(env.eval_ok(&format!("{var}.open('track')")), TjsValue::Void);
        assert_eq!(env.eval_ok(&format!("{var}.play()")), TjsValue::Void);
        assert_eq!(env.eval_ok(&format!("{var}.stop()")), TjsValue::Void);
        assert_eq!(env.eval_ok(&format!("{var}.stopFade()")), TjsValue::Void);
        assert_eq!(env.eval_ok(&format!("{var}.fade(0, 50)")), TjsValue::Void);
        assert_eq!(
            env.eval_ok(&format!("{var}.fade(0, 50, 10)")),
            TjsValue::Void
        );
        // The event forwarders do not require a caller argument.
        assert_eq!(
            env.eval_ok(&format!("{var}.onStatusChanged()")),
            TjsValue::Void
        );
        assert_eq!(
            env.eval_ok(&format!("{var}.onFadeCompleted()")),
            TjsValue::Void
        );
        // The no-ops leave the buffer unloaded.
        assert_eq!(
            env.eval_ok(&format!("{var}.status")),
            TjsValue::String("unload".into())
        );
    }
}

/// `open` and `fade` still enforce the reference's argument counts
/// (`TJS_E_BADPARAMCOUNT`), unlike `play`/`stop`/`stopFade`.
#[test]
fn open_and_fade_require_their_arguments() {
    let _lock = vm_lock();
    let env = Env::new();

    for (index, class) in ["CDDASoundBuffer", "MIDISoundBuffer"]
        .into_iter()
        .enumerate()
    {
        let var = format!("s{index}");
        env.run(&format!("var {var} = new {class}();"));
        // reference/cpp/core/sound/CDDAIntf.cpp:62-73 / MIDIIntf.cpp:62-74.
        assert!(
            env.error(&format!("{var}.open()")).contains("required"),
            "open() should require an argument"
        );
        // reference/cpp/core/sound/CDDAIntf.cpp:99-119 / MIDIIntf.cpp:100-120.
        assert!(
            env.error(&format!("{var}.fade(0)")).contains("fade"),
            "fade(0) should require a second argument"
        );
    }
}

/// `MIDISoundBuffer.midiOut` is the reference's disabled static: an arity
/// check then `TJS_S_OK` (`reference/cpp/core/sound/win32/MIDIImpl.cpp:978-993`).
#[test]
fn midi_out_is_a_no_op_when_midi_is_disabled() {
    let _lock = vm_lock();
    let env = Env::new();

    assert_eq!(env.eval_ok("MIDISoundBuffer.midiOut(0)"), TjsValue::Void);
    assert!(env.error("MIDISoundBuffer.midiOut()").contains("midiOut"));
}
