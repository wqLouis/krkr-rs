//! Receiver-misrouting discriminator for the `WaveSoundBuffer` native family.
//!
//! # Mechanism under test
//!
//! Native dispatch has exactly one receiver channel: the `objthis` the VM
//! passes to `PropGet`/`PropSet`/`FuncCall`. The VM derives it as
//! `clo.ObjThis ? clo.ObjThis : ra[-1].AsObjectNoAddRef()` in
//! `SetPropertyDirect` (vendored `crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1458-1467`,
//! identical to `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1450-1459`) and
//! in `CallFunctionDirect` (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:2161-2199`).
//! `ra[-1]` is the executing function's `this`
//! (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:710`), and
//! `tTJSVariantClosure::PropSet` forwards
//! `ObjThis ? ObjThis : (objthis ? objthis : Object)`
//! (`crates/tjs/cpp/tjs2/tjsVariant.h:281-290`).
//!
//! A `with(obj)` block stores the *value* of `obj` in a frame register
//! (`T_WITHDOT_PROXY` returns `data.RefRegister`,
//! `crates/tjs/cpp/tjs2/tjsInterCodeGen.cpp:2523-2534`; `EnterWithCode`
//! writes it at `:3309-3330`). Every `.member` inside the block is compiled
//! against that register. If the register holds a variant whose `ObjThis` is
//! not the intended object — e.g. `null`, or an owner bound by a class-member
//! copy (`RegisterObjectMember` calls `ChangeClosureObjThis(Dest)`
//! unconditionally, `crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:2760-2767`;
//! `tTJSNativeClass::FuncCall` does it for any member whose `ObjThis` is
//! null, `crates/tjs/cpp/tjs2/tjsNative.cpp:341-356`) — then the write's
//! receiver falls back to the enclosing function's `this`.
//!
//! The sound instance families resolve their payload **only** from that
//! `objthis`:
//!
//! * `tjs2_dispatch_native_instance_method` looks the payload up via
//!   `objthis->NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)`
//!   (`crates/tjs/cpp/tjs2_abi.cpp:1136`), the same lookup the reference
//!   `TJS_GET_NATIVE_INSTANCE` performs (`reference/cpp/core/tjs2/tjsNative.h:298-308`);
//! * `tjs2_dispatch_native_instance_property_set` at
//!   `crates/tjs/cpp/tjs2_abi.cpp:1327`;
//! * the Rust `channel_setter!`/`channel_int_getter!`/`channel_real_getter!`
//!   macros ignore `_objthis` and use `instance` (the payload)
//!   (`crates/compat/src/natives/sound.rs:910-996`).
//!
//! So a wrong-but-same-class receiver writes the wrong mixer channel
//! **silently**. Reading `volume` back is reading the L2 `SoundChannel` state:
//! `sound_volume_get` returns `channel.volume()`
//! (`crates/compat/src/natives/sound.rs:1906-1908`) and `sound_volume_set`
//! writes it (`:2284-2286`).
//!
//! The **static** members are immune to this bug by construction: they are
//! registered with `TJS_STATICMEMBER` and the static dispatch drops
//! `objthis` (`crates/tjs/cpp/tjs2_abi.cpp:569` for methods, `:718` for
//! properties; reference `TJS_END_NATIVE_STATIC_PROP_DECL`,
//! `reference/cpp/core/tjs2/tjsNative.h:444-451`).
//!
//! # What this file proves
//!
//! The tests below were first written as a discriminator for the
//! receiver-misrouting hypothesis; running them against the current tree
//! **disproves** it for the sound family. Every instance member routes to
//! the object the write names, for the direct, parameter, subclass,
//! class-field, `with`-read, `with`-call, `with`-from-a-timer and
//! `property volume` override shapes. The family's only exposure is the
//! generic one-channel receiver, and no sound script shape exercised here
//! loses the receiver.
//!
//! Two **script-shape gotchas** surfaced while building the repros and are
//! pinned here (both reference-faithful, not compat bugs):
//!
//! * inside a subclass scope, a bare `WaveSoundBuffer` resolves through the
//!   this-proxy to the instance's *copied class-name constructor member*
//!   (the reference registers the constructor non-static,
//!   `reference/cpp/core/tjs2/tjsNative.h:400-418`, so
//!   `tTJSNativeClass::FuncCall` copies it, `tjsNative.cpp:344-370`). A
//!   `new WaveSoundBuffer(...)` there therefore calls `CreateNew` on the
//!   constructor function and raises `TJS_E_NOTIMPL`; the correct spelling
//!   for a *second* buffer is `new global.WaveSoundBuffer(...)`.
//! * a class-qualified write to a static clears its `TJS_SYMBOL_STATIC` flag
//!   (`tjsObject.cpp:1483-1485`, reference-identical), so "an instance write
//!   shadows the static" must be checked **before** any class-qualified
//!   write.
//!
//! Reference behaviour pinned by each assertion is cited inline.
//!
//! # Commands
//!
//! `rustfmt --edition 2024 --check crates/compat/tests/dispatch_sound.rs`
//! (the orchestrator runs the cargo gate).

mod common;

use std::time::Duration;

use common::vm_lock;
use compat::CompatHost;
use engine::timer::TimerState;
use tjs::{Tjs2Engine, TjsValue};

/// Script label for diagnostics.
const SCRIPT: &str = "dispatch-sound-test";

/// A bound VM + host + attached timer registry.
///
/// No storage/decoder is needed: a fresh `WaveSoundBuffer` already owns its
/// mixer channel (`sound_ctor` spawns one,
/// `crates/compat/src/natives/sound.rs:1355-1375`), so `volume` reads/writes
/// observe real L2 state. `_timer` drops after `_host` (declaration order),
/// so the host's raw pointer stays valid for its whole life.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _timer: Box<TimerState>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut timer = Box::new(TimerState::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed timer registry outlives the host binding; fields
        // drop in declaration order, so `_timer` is released after `_host`.
        unsafe {
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _timer: timer,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, SCRIPT)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    /// One frame-body `timer_poll` step (the timer-owned callback seam).
    fn poll_timer(&self) -> bool {
        let mut ctx = self._host.ctx().expect("bound host");
        compat::timer_poll(&mut ctx).expect("timer poll")
    }
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

// ---------------------------------------------------------------------------
// 1. Direct instances / parameter passing — the immune baseline
// ---------------------------------------------------------------------------

/// The `with` proxy's fallback receiver with **direct** native instances.
///
/// `new WaveSoundBuffer()` reaches `tTJSNativeClass::CreateNew`, whose
/// `FuncCall(0, nullptr, …)` copies the native members onto the object and
/// binds each object-valued member's `ObjThis` to the object
/// (`reference/cpp/core/tjs2/tjsNative.cpp:305-375`, in particular the
/// `val.ChangeClosureObjThis(Dest)` at `:353`), and the `new` result is stored
/// as `tTJSVariant(dsp, dsp)`
/// (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:2138`). So `b` carries
/// `ObjThis == b` and `with(b)` must reach `b`'s channel, never `a`'s — even
/// though the write runs with `this == a`.
#[test]
fn with_write_from_a_different_this_routes_to_the_direct_instance_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var a = new WaveSoundBuffer(null); \
         var b = new WaveSoundBuffer(null); \
         a.volume = 11111; \
         b.volume = 22222; \
         var write = function(v) { with(b) { .volume = v; } }; \
         (write incontextof a)(33333);",
    );
    assert_eq!(
        env.eval_int("b.volume"),
        33333,
        "with(b) must write b's L2 channel (sound_volume_set, sound.rs:2284)"
    );
    assert_eq!(
        env.eval_int("a.volume"),
        11111,
        "the enclosing `this` (a) must be untouched: the direct-instance closure \
         has ObjThis == b (tjsInterCodeExec.cpp:2138)"
    );
}

/// A subclass method writing into an **argument** (`target`). Function
/// parameters carry the argument variant through unchanged, so the `with`
/// register keeps `target`'s `ObjThis`.
#[test]
fn with_write_from_a_subclass_method_routes_to_a_parameter_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class Host extends WaveSoundBuffer { \
             function Host(c) { super.WaveSoundBuffer(c); } \
             function writeInto(target, v) { with(target) { .volume = v; } } \
         } \
         var host = new Host(null); \
         var target = new WaveSoundBuffer(null); \
         host.volume = 11111; \
         target.volume = 22222; \
         host.writeInto(target, 33333);",
    );
    assert_eq!(
        env.eval_int("target.volume"),
        33333,
        "the write must reach the parameter target's channel"
    );
    assert_eq!(
        env.eval_int("host.volume"),
        11111,
        "the subclass `this` must stay untouched"
    );
}

/// A subclass member assigned in the **constructor** and then used as the
/// `with` target. The constructor assignment stores the `new` variant
/// `(obj, obj)` via `tTJSCustomObject::PropSetByVS`'s `CopyRef`
/// (`crates/tjs/cpp/tjs2/tjsObject.cpp:1447-1505`), so the register keeps the
/// member object's own `ObjThis`.
#[test]
fn with_write_routes_to_a_constructor_assigned_member() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class Host extends WaveSoundBuffer { \
             var _snd; \
             function Host(c) { super.WaveSoundBuffer(c); _snd = new global.WaveSoundBuffer(null); } \
             function writeInto(v) { with(_snd) { .volume = v; } } \
         } \
         var host = new Host(null); \
         host._snd.volume = 22222; \
         host.writeInto(33333);",
    );
    assert_eq!(
        env.eval_int("host._snd.volume"),
        33333,
        "the constructor-assigned member's own channel must receive the write"
    );
}

/// The canonical cross-instance shape: a same-class **foreign** receiver holds
/// the write target as a member assigned at runtime, and a method of the
/// foreign receiver performs the `with` write. The enclosing function's
/// `this` (`foreign`) is a valid same-class receiver, so a receiver leak
/// would be silent rather than a loud wrong-class error — making this the
/// sharpest discriminator. It routes correctly.
///
/// Correct behaviour: `target`'s channel gets `33333`; `foreign` keeps its own.
#[test]
fn with_write_from_a_foreign_subclass_receiver_routes_to_the_member_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubSound extends WaveSoundBuffer { \
             var _target = null; \
             var _observed = -1; \
             function SubSound(c) { super.WaveSoundBuffer(c); } \
             function writeIt(v) { with(_target) { .volume = v; } } \
             function readIt() { with(_target) { _observed = .volume; } } \
         } \
         var target = new SubSound(null); \
         var foreign = new SubSound(null); \
         target.volume = 11111; \
         foreign.volume = 22222; \
         foreign._target = target; \
         foreign.writeIt(33333);",
    );
    assert_eq!(
        env.eval_int("target.volume"),
        33333,
        "the with-write must reach the member object (`target`), not `foreign`"
    );
    assert_eq!(
        env.eval_int("foreign.volume"),
        22222,
        "the foreign receiver's own channel must be untouched"
    );

    // The read counterpart uses the same one receiver channel.
    env.run(
        "target.volume = 12345; \
         foreign.volume = 54321; \
         foreign.readIt();",
    );
    assert_eq!(
        env.eval_int("foreign._observed"),
        12345,
        "the with-read must yield the member object's channel volume"
    );
}

// ---------------------------------------------------------------------------
// 2. Subclass object-valued class field
// ---------------------------------------------------------------------------

/// A script subclass whose object-valued **class field** is the `with`
/// target, written from a method running with a different `this` (the
/// subclass instance).
///
/// The class body's field initializer stores the `new` variant `(obj, obj)`
/// through `VM_SPDS` (`crates/tjs/cpp/tjs2/tjsInterCodeGen.cpp:2850-2888`),
/// so the class-field object keeps its own `ObjThis` and the native `volume`
/// setter reaches the field object's mixer channel.
#[test]
fn with_write_routes_to_a_class_field_object_not_the_enclosing_instance() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class Host extends WaveSoundBuffer { \
             var _snd = new global.WaveSoundBuffer(null); \
             function Host(c) { super.WaveSoundBuffer(c); } \
             function writeInto(v) { with(_snd) { .volume = v; } } \
         } \
         var host = new Host(null); \
         host._snd.volume = 22222; \
         var host_volume_before = host.volume; \
         host.writeInto(33333);",
    );
    assert_eq!(
        env.eval_int("host._snd.volume"),
        33333,
        "the class-field object is the with-target: its channel must be written \
         (reference `with` semantics, test_with.tjs)"
    );
    assert_eq!(
        env.eval_int("host.volume"),
        env.eval_int("host_volume_before"),
        "the enclosing instance's channel must NOT change: a wrong receiver here \
         is the silent same-class misroute"
    );
}

/// The same class-field shape, but the `with` target is a script **property
/// override** subclass, matching the game's `system/sound.tjs`
/// `property volume { setter(v){ super.volume = v; } }`.
///
/// The override's `super.volume = v` re-enters native dispatch with the
/// setter's `this`; here that is the field object, so the native write lands
/// on the field object's channel.
#[test]
fn with_write_through_a_script_volume_override_routes_to_the_field_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class Sound extends WaveSoundBuffer { \
             property volume { \
                 setter(v) { super.volume = v; } \
                 getter() { return super.volume; } \
             } \
             function Sound(c) { super.WaveSoundBuffer(c); } \
         } \
         class Host extends WaveSoundBuffer { \
             var _snd = new global.Sound(null); \
             function Host(c) { super.WaveSoundBuffer(c); } \
             function writeInto(v) { with(_snd) { .volume = v; } } \
         } \
         var host = new Host(null); \
         host._snd.volume = 22222; \
         var host_volume_before = host.volume; \
         host.writeInto(23456);",
    );
    assert_eq!(
        env.eval_int("host._snd.volume"),
        23456,
        "the override's `super.volume = v` must reach the field object's channel"
    );
    assert_eq!(
        env.eval_int("host.volume"),
        env.eval_int("host_volume_before"),
        "the enclosing instance's channel must NOT change (silent misroute)"
    );
}

// ---------------------------------------------------------------------------
// 3. Reads and method calls — the same one receiver channel
// ---------------------------------------------------------------------------

/// Reads use the same receiver channel as writes
/// (`tTJSInterCodeContext::GetProperty` /
/// `crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1478-1485`). Reading `.volume`
/// through `with(target)` must return the target's L2 channel volume
/// (`sound_volume_get`, `crates/compat/src/natives/sound.rs:1906`).
#[test]
fn with_read_routes_to_the_with_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class Host extends WaveSoundBuffer { \
             function Host(c) { super.WaveSoundBuffer(c); } \
             function readFrom(target) { with(target) { return .volume; } } \
         } \
         var host = new Host(null); \
         host.volume = 11111; \
         var target = new WaveSoundBuffer(null); \
         target.volume = 22222; \
         var got = host.readFrom(target);",
    );
    assert_eq!(
        env.eval_int("got"),
        22222,
        "with(target) {{ .volume }} must read the target's channel, not the \
         enclosing instance's"
    );
}

/// A method call for contrast: `CallFunctionDirect` uses the same
/// `clo.ObjThis ? clo.ObjThis : ra[-1]` fallback
/// (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:2186-2192`), but the audit
/// reports calls keep the right receiver. `fade(0, 50)` completes inside the
/// call (time below the 60 ms beat,
/// `reference/cpp/core/sound/SoundBufferBaseIntf.cpp:168-170`), so the target
/// channel's `volume` jumps to `0` — an observable native side effect.
#[test]
fn with_method_call_routes_to_the_with_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class Host extends WaveSoundBuffer { \
             function Host(c) { super.WaveSoundBuffer(c); } \
             function fadeTarget(target) { with(target) { .fade(0, 50); } } \
         } \
         var host = new Host(null); \
         var target = new WaveSoundBuffer(null); \
         host.volume = 100000; \
         target.volume = 100000; \
         host.fadeTarget(target);",
    );
    assert_eq!(
        env.eval_int("target.volume"),
        0,
        "with(target) {{ .fade(0, 50) }} must drive the target's channel to 0"
    );
    assert_eq!(
        env.eval_int("host.volume"),
        100000,
        "the enclosing instance's channel must stay at its default"
    );
}

// ---------------------------------------------------------------------------
// 4. Statics — receiver-independent by registration
// ---------------------------------------------------------------------------

/// `WaveSoundBuffer.globalVolume` is registered with `TJS_STATICMEMBER`
/// (`reference/cpp/core/sound/WaveIntf.cpp:1592-1607`,
/// `TJS_END_NATIVE_STATIC_PROP_DECL`; the flag is documented "member is not
/// registered to the object", `crates/tjs/cpp/tjs2/tjsInterface.h:29-31`),
/// and the compat static dispatcher drops `objthis`
/// (`crates/tjs/cpp/tjs2_abi.cpp:718`), writing the shared mixer's global
/// volume (`crates/compat/src/natives/sound.rs:2486-2516`). The class member's
/// copy is skipped by the native member copy
/// (`if(!(flags & TJS_STATICMEMBER))`,
/// `crates/tjs/cpp/tjs2/tjsNative.cpp:344-356`), so an instance-qualified
/// write must land on an own shadow member and leave the engine state alone.
///
/// **Order matters.** `tTJSCustomObject::PropSetByVS` *clears*
/// `TJS_SYMBOL_STATIC` on every write whose flags lack `TJS_STATICMEMBER`
/// (`crates/tjs/cpp/tjs2/tjsObject.cpp:1483-1485`; it is reference-identical),
/// and a class-qualified write goes through `SetPropertyDirect` with
/// `TJS_MEMBERENSURE` only (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1458-1467`).
/// So a class-qualified write to a static un-statics it for later instances —
/// a reference quirk, not a compat bug. The instance/shadow check must
/// therefore run **before** any class-qualified write.
#[test]
fn static_global_volume_is_receiver_independent() {
    let _lock = vm_lock();
    let env = Env::new();

    // Fresh engine: the static is still marked static, so the instance write
    // shadows instead of reaching the shared mixer (reference
    // `WaveImpl.cpp:1888`: the default is 100000).
    env.run("var b = new WaveSoundBuffer(null); b.globalVolume = 11111;");
    assert_eq!(
        env.eval_int("WaveSoundBuffer.globalVolume"),
        100_000,
        "an instance-qualified write on a still-static member must shadow, not \
         touch the engine (tjsNative.cpp:344-356)"
    );
    assert_eq!(
        env.eval_int("b.globalVolume"),
        11111,
        "the instance-qualified write must land on b's own shadow member"
    );

    // The class-qualified write reaches the class's static dispatch, which
    // drops objthis (tjs2_abi.cpp:718) and writes the shared mixer.
    env.run("WaveSoundBuffer.globalVolume = 54321;");
    assert_eq!(
        env.eval_int("WaveSoundBuffer.globalVolume"),
        54321,
        "the class-qualified static write must reach the shared mixer \
         (sound.rs:2486-2516)"
    );
}

// ---------------------------------------------------------------------------
// 5. The game's property override, without `with`
// ---------------------------------------------------------------------------

/// The `system/sound.tjs` shape in isolation: `property volume { setter(v){
/// super.volume = v; } }`. On a correctly received instance the setter runs
/// with `this` == the instance and `super.volume = v` reaches the instance's
/// channel. This is the case the task measured to work.
#[test]
fn script_volume_override_setter_reaches_the_instance_channel() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class Sound extends WaveSoundBuffer { \
             property volume { \
                 setter(v) { super.volume = v; } \
                 getter() { return super.volume; } \
             } \
             function Sound(c) { super.WaveSoundBuffer(c); } \
         } \
         var s = new Sound(null); \
         s.volume = 12345;",
    );
    assert_eq!(
        env.eval_int("s.volume"),
        12345,
        "the override's super.volume must reach the instance's channel"
    );
}

// ---------------------------------------------------------------------------
// 6. Timer-invoked write
// ---------------------------------------------------------------------------

/// The Title.tjs shape: a `with(target){ .volume = v }` performed from a
/// **timer-invoked function** whose enclosing `this` is a `WaveSoundBuffer`
/// subclass. The timer poll dispatches the callback through the retained
/// closure (`compat::timer_poll` -> `tjs2_call_value`/`call_member`), so the
/// write's `ra[-1]` is the subclass instance; it routes to the target.
///
/// Correct behaviour: the field object's channel gets `33333`; the enclosing
/// instance keeps its default.
#[test]
fn with_write_from_a_timer_callback_routes_to_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class Host extends WaveSoundBuffer { \
             var _snd = new global.WaveSoundBuffer(null); \
             function Host(c) { super.WaveSoundBuffer(c); } \
             function writeInto(v) { with(_snd) { .volume = v; } } \
         } \
         var host = new Host(null); \
         host._snd.volume = 22222; \
         var t = new Timer(function() { host.writeInto(33333); }, ''); \
         t.interval = 1; \
         t.enabled = 1;",
    );
    sleep(10);
    assert!(
        env.poll_timer(),
        "the timer must still be enabled after one poll"
    );
    assert_eq!(
        env.eval_int("host._snd.volume"),
        33333,
        "the timer-invoked with-write must reach the field object's channel"
    );
    assert_eq!(
        env.eval_int("host.volume"),
        100000,
        "the enclosing instance's channel must NOT receive the timer write"
    );
}
