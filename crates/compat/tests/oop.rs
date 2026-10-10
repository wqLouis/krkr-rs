//! Verdict tests for script OOP: `class X extends Native`, `super`,
//! overrides, field initialization, `with` and `isvalid`.
//!
//! These tests reproduce the semantics the KiriKiri reference VM guarantees
//! (the vendored `crates/tjs/cpp/tjs2/**` is the reference source and is
//! assumed correct) and drive them through the exact seams our glue owns:
//! the ABI shim (`tjs2_abi.cpp`), the `tjs` engine bindings and the
//! `compat` dispatch (e.g. `timer_poll` -> `call_member`, the frame body's
//! `onTimer` path). The live bug that motivated this file: a game
//! `class OnceTimer extends Timer` whose native timer fired 77 times while
//! every dispatch reached the *native* `Timer::onTimer` instead of the
//! script override, and whose constructor assignments (`capacity = 1`)
//! appeared to be ignored.
//!
//! Reference semantics encoded here:
//!
//! * `tTJSInterCodeContext::CreateNew` (reference
//!   `tjsInterCodeExec.cpp`): the class body (extends-call, VM_ADDCI,
//!   VM_REGMEMBER, field initializers) runs on the fresh instance
//!   **before** the constructor member named after the class.
//! * `RegisterObjectMember` (VM_REGMEMBER) re-registers every script
//!   class member into the instance and binds each function value's
//!   closure `ObjThis` to the instance, so the class's members —
//!   overrides included — overwrite the native base's copies on the
//!   instance and detached calls keep their `this`.
//! * `tTJSNativeClass::FuncCall(membername == nullptr)`
//!   (`tjsNative.cpp:314`): the `extends Native` call adds the native
//!   class name to the instance's `ClassInstanceInfo`, attaches the
//!   native instance and copies the native members onto the object.
//!   `instanceof "Timer"` consults exactly that name list
//!   (`tTJSCustomObject::IsInstanceOf`), and `instanceof` takes a
//!   class-name *string* (an object operand stringifies to
//!   `(object %p:%p)`, `TJSObjectToString`).
//! * VM_CHKINV (`isvalid`): non-object types are always valid; an
//!   invalidated object is false; `null` is an object with a null
//!   dispatch, so `isvalid null` raises `TJSNullAccess`
//!   ("Accessing to nullptr object").
//! * `with (obj) { .member = ...; }` reads/writes through the with-object
//!   (`reference/tests/test_files/tjs2/test_with.tjs`).
//! * `Array.count` is a native property (`tjsArray.cpp`); `Dictionary`
//!   has none and `tTJSDictionaryObject::PropGet` returns **void** for a
//!   missing member instead of failing.
//!
//! Any failing assertion therefore pins a deviation to a concrete seam:
//! ABI shim, `tjs` bindings or `compat` dispatch — never the VM.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use engine::Storage;
use engine::timer::TimerState;
use fs::FileSystem;
use tjs::{RetainedValue, Tjs2Engine, TjsValue};

/// Script name reported to the VM for every exec/eval in this file.
const T: &str = "oop-test";

/// A bound VM + host + attached scene and timer registry.
///
/// Field drop order follows `tests/tree_owner.rs`: the host goes first
/// (releasing every retained script object and removing the process-wide
/// binding), then the engine, then the boxed scene and timer registry the
/// host's raw pointers refer to.
struct Env {
    host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
    _timer: Box<TimerState>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene and timer registry outlive the binding.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            host,
            engine,
            _scene: scene,
            _timer: timer,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, T)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, T)
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

    /// Numeric read for properties whose reference getter writes a Real
    /// (`TimerIntf.cpp:186-188` writes `interval` as a double).
    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a number"),
        }
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }

    /// One `FrameBody::timer_poll` step — the exact dispatch the frame
    /// body uses for every due timer.
    fn poll_once(&self) {
        let mut ctx = self.host.ctx().expect("bound compat context");
        compat::timer_poll(&mut ctx).expect("timer poll");
    }

    /// Poll until the integer expression `cond` becomes non-zero.
    fn poll_until(&self, cond: &str, tries: u32) {
        for _ in 0..tries {
            self.poll_once();
            if self.eval_int(cond) != 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("condition {cond:?} never became true after {tries} timer polls");
    }
}

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-oop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp game dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM + host + attached scene, timer registry and L2 storage on a
/// temp game directory containing one script file.
struct StorageEnv {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
    _timer: Box<TimerState>,
    _storage: Box<Storage>,
    _dir: GameDir,
}

impl StorageEnv {
    fn new(script_name: &str, script_source: &str) -> StorageEnv {
        let dir = GameDir::new();
        std::fs::write(dir.path().join(script_name), script_source).expect("write storage script");

        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(dir.path()),
                dir.path().to_path_buf(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut host = CompatHost::new();
        // SAFETY: every attached value is boxed and declared after `_host`,
        // so it outlives the binding.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut().attach_storage(&mut *storage);
        }
        host.install(&engine).expect("install compat surface");
        StorageEnv {
            _host: host,
            engine,
            _scene: scene,
            _timer: timer,
            _storage: storage,
            _dir: dir,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, T)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.engine.eval(expression, T) {
            Ok(TjsValue::Integer(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }
}

// ---------------------------------------------------------------------------
// 1. Field initializers vs constructor body
// ---------------------------------------------------------------------------

#[test]
fn field_initializer_does_not_clobber_the_constructor_assignment() {
    let _lock = vm_lock();
    let env = Env::new();

    // Reference order (`tTJSInterCodeContext::CreateNew`): the class body
    // runs field initializers on the fresh instance first, then calls the
    // constructor member — so the constructor's `x = v` must survive.
    env.run(
        "var ctorCb = function() {}; \
         class FieldTimer extends Timer { \
             var x = null; \
             var y = 42; \
             function FieldTimer(v) { super.Timer(ctorCb, ''); x = v; } \
         } \
         var ft = new FieldTimer(5);",
    );

    assert_eq!(
        env.eval_int("ft.x"),
        5,
        "the field initializer `var x = null` must run before the \
         constructor body, not after it"
    );
    assert_eq!(
        env.eval_int("ft.y"),
        42,
        "a field the constructor never touches keeps its initializer"
    );
}

// ---------------------------------------------------------------------------
// 2. Script override wins over the native base method
// ---------------------------------------------------------------------------

#[test]
fn script_override_wins_over_native_on_timer() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var log = ''; \
         var fires = 0; \
         class OverrideTimer extends Timer { \
             function OverrideTimer(cb) { super.Timer(cb, ''); } \
             function onTimer() { log += 'S'; fires++; } \
         } \
         var ot = new OverrideTimer(function() { log += 'N'; });",
    );

    // Dispatch through the same entry point `timer_poll` uses on the
    // retained owner: `engine.call_member(id, "onTimer", &[])`.
    let owner = match env
        .engine
        .eval_retained("ot", T)
        .expect("retain the timer object")
    {
        RetainedValue::Object(detached) => detached,
        RetainedValue::Value(other) => panic!("`ot` evaluated to non-object {other:?}"),
    };
    env.engine
        .call_member(owner.raw_id(), "onTimer", &[])
        .expect("dispatch onTimer");
    assert_eq!(
        env.eval_ok("log"),
        TjsValue::String("S".into()),
        "the dispatch must resolve to the script override; the native \
         Timer.onTimer (which would log 'N') must not run"
    );

    // And through the real frame path: arm the timer and poll.
    env.run("ot.interval = 1; ot.enabled = true;");
    assert_eq!(
        env.eval_int("ot.enabled"),
        1,
        "arming must reach the native"
    );
    env.poll_until("fires >= 2", 500);
    assert_eq!(
        env.eval_ok("log"),
        TjsValue::String("SS".into()),
        "every timer_poll dispatch must run the script override, never the \
         native base method or the constructor callback"
    );
}

// ---------------------------------------------------------------------------
// 3. `super.Native(...)` constructor: usable instance, captured objthis
// ---------------------------------------------------------------------------

#[test]
fn super_native_constructor_produces_a_fully_usable_instance() {
    let _lock = vm_lock();
    let env = Env::new();

    // Mirrors the game's `OnceTimer`: capacity 1, short interval, script
    // `onTimer` override, constructor callback that must never fire.
    env.run(
        "var log = ''; \
         var fires = 0; \
         class SubTimer extends Timer { \
             function SubTimer(cb) { \
                 super.Timer(cb, ''); \
                 capacity = 1; \
                 interval = 2; \
             } \
             function onTimer() { log += 'F'; fires++; } \
         } \
         var sub = new SubTimer(function() { log += 'C'; }); \
         var plain = new Timer(function() {});",
    );

    // Native members work on the subclass instance. The reference getter
    // for `interval` writes a Real (`TimerIntf.cpp:186-188`), and `capacity`
    // round-trips the verbatim script value (`TimerIntf.cpp:222-236`).
    assert_eq!(
        env.eval_real("sub.interval"),
        2.0,
        "the constructor's `interval = 2` must round-trip through the \
         native Timer property, as a Real"
    );
    assert_eq!(
        env.eval_int("sub.capacity"),
        1,
        "the constructor's `capacity = 1` must round-trip through the \
         native Timer property"
    );
    // The reference registers no `id` on Timer (`TimerIntf.cpp:137-259`);
    // D27/D28 require removing it rather than keeping an extra member.
    env.error("sub.id");

    // The native ctor captured `objthis` = the subclass instance: the
    // frame-body dispatch lands on the override, not the ctor callback.
    env.run("sub.enabled = true;");
    assert_eq!(
        env.eval_int("sub.enabled"),
        1,
        "arming must reach the native"
    );
    env.poll_until("fires >= 1", 500);
    assert_eq!(
        env.eval_ok("log"),
        TjsValue::String("F".into()),
        "the owner retained by super.Timer must be the subclass instance, \
         so onTimer dispatch reaches the override and not 'C'"
    );

    // Reference semantics (`TimerIntf.cpp:69-92`): `capacity` bounds the
    // number of *queued* `onTimer` events, never fires over a session, so a
    // `capacity = 1` timer keeps repeating until the script cancels it. (The
    // game's `OnceTimer` stops itself with `cancel()` — see
    // `timer_behavior.rs::capacity_one_bounds_queued_events_not_fires`.)
    env.poll_until("fires >= 2", 500);
    assert_eq!(
        env.eval_ok("log"),
        TjsValue::String("FF".into()),
        "the override keeps firing and the constructor callback still never runs"
    );
    assert_eq!(
        env.eval_int("sub.enabled"),
        1,
        "capacity = 1 does not disable the timer — only the script can"
    );
}

// ---------------------------------------------------------------------------
// 4. Detached method call / `this` binding
// ---------------------------------------------------------------------------

#[test]
fn detached_method_call_keeps_the_bound_this() {
    let _lock = vm_lock();
    let env = Env::new();

    // `VM_REGMEMBER` registers class members into the instance with the
    // closure ObjThis bound to it (`RegisterObjectMember` ->
    // `ChangeClosureObjThis`), and `VM_CALL` prefers `clo.ObjThis` over
    // the caller's `this` — so a detached reference still sees its owner.
    // The reference `test_class.tjs` pins the same
    // (`var objmethod = tmp.method1; objmethod();`).
    env.run(
        "var calls = 0; \
         class Holder { \
             function m() { calls++; return this === h ? this.tag : -1; } \
         } \
         var h = new Holder(); \
         h.tag = 7; \
         var f = h.m; \
         var r1 = f(); \
         var stored = h.m; \
         var r2 = stored();",
    );

    assert_eq!(
        env.eval_int("r1"),
        7,
        "var f = obj.method; f() must run with this = obj"
    );
    assert_eq!(
        env.eval_int("r2"),
        7,
        "a stored method reference (_owner()) must run"
    );
    assert_eq!(
        env.eval_int("calls"),
        2,
        "both detached calls must actually execute"
    );
}

// ---------------------------------------------------------------------------
// 5. `isvalid` across the value spectrum
// ---------------------------------------------------------------------------

#[test]
fn isvalid_matches_the_reference_chkinv_semantics() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "class Vm { function m() {} } \
         var vm = new Vm(); \
         var mref = vm.m; \
         var never;",
    );

    // Objects — script instance and script method reference — are valid.
    assert_eq!(
        env.eval_int("isvalid vm"),
        1,
        "a live script object is valid"
    );
    assert_eq!(
        env.eval_int("isvalid mref"),
        1,
        "a live script method reference is valid"
    );
    // VM_CHKINV: `Type() != tvtObject -> true`, so every non-object type
    // is valid, including void.
    assert_eq!(env.eval_int("isvalid never"), 1, "void is valid");
    assert_eq!(env.eval_int("isvalid 42"), 1, "a number is valid");
    assert_eq!(env.eval_int("isvalid \"s\""), 1, "a string is valid");

    // An invalidated object is the one false case.
    env.run("var dead = new Vm(); invalidate dead;");
    assert_eq!(
        env.eval_int("isvalid dead"),
        0,
        "an invalidated object is not valid"
    );

    // `null` is a tvtObject with a null dispatch; the closure's IsValid
    // raises TJSNullAccess ("Accessing to nullptr object") instead of
    // returning a boolean — this is why the game must guard
    // `isvalid _owner` with an explicit null check first.
    let message = env.error("isvalid null");
    assert!(
        message.contains("nullptr"),
        "`isvalid null` must raise the null-access error, got: {message}"
    );
}

// ---------------------------------------------------------------------------
// 6. `with(obj) { .member = ...; }` writes through
// ---------------------------------------------------------------------------

#[test]
fn with_statement_writes_through_to_the_object() {
    let _lock = vm_lock();
    let env = Env::new();

    // The game's `with(_logo) { .visible = true; }` shape, plus the
    // single-statement form, a read-through and an in-with method call
    // (all pinned by `reference/tests/test_files/tjs2/test_with.tjs`).
    env.run(
        "var logo = %[visible: false, name: 'logo']; \
         with(logo) { .visible = true; } \
         var readBack; \
         with(logo) { readBack = .visible; } \
         var o2 = %[]; \
         with(o2) .n = 5; \
         class WithHost { \
             var seen = 0; \
             function bump() { seen++; } \
         } \
         var wh = new WithHost(); \
         with(wh) { .bump(); }",
    );

    assert_eq!(
        env.eval_ok("logo.visible"),
        TjsValue::Integer(1),
        "with(obj) {{ .member = v }} must write through to obj"
    );
    assert_eq!(
        env.eval_ok("readBack"),
        TjsValue::Integer(1),
        "with(obj) {{ .member }} must read through from obj"
    );
    assert_eq!(
        env.eval_int("o2.n"),
        5,
        "the single-statement with form writes through"
    );
    assert_eq!(
        env.eval_int("wh.seen"),
        1,
        "with(obj) {{ .method() }} must call the member with the with-object as this"
    );
}

// ---------------------------------------------------------------------------
// 7. `instanceof` across a script -> native chain, execStorage globals
// ---------------------------------------------------------------------------

#[test]
fn instanceof_walks_the_script_to_native_chain() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "class MidTimer extends Timer { \
             function MidTimer(cb) { super.Timer(cb, ''); } \
         } \
         class SubTimer extends MidTimer { \
             function SubTimer(cb) { super.MidTimer(cb); } \
         } \
         var inst = new SubTimer(function() {});",
    );

    // `instanceof` takes a class-name string; every script class adds its
    // own name (VM_ADDCI) and the extends-call adds the native name
    // (ClassInstanceInfo ADD inside tTJSNativeClass::FuncCall).
    assert_eq!(
        env.eval_int("inst instanceof \"SubTimer\""),
        1,
        "own class name"
    );
    assert_eq!(
        env.eval_int("inst instanceof \"MidTimer\""),
        1,
        "script base class name"
    );
    assert_eq!(
        env.eval_int("inst instanceof \"Timer\""),
        1,
        "the native class name added by the extends-call"
    );
    assert_eq!(
        env.eval_int("inst instanceof \"Nope\""),
        0,
        "an unrelated name is false"
    );

    // An object operand stringifies to "(object %p:%p)"
    // (TJSObjectToString), which matches no class name.
    assert_eq!(
        env.eval_int("inst instanceof SubTimer"),
        0,
        "an object operand is not a class-name string"
    );
}

#[test]
fn exec_storage_class_is_reachable_through_global() {
    let _lock = vm_lock();
    let env = StorageEnv::new(
        "oop_stored_class.tjs",
        "class StoredTimer extends Timer { \
             function StoredTimer(cb) { super.Timer(cb, ''); } \
             function onTimer() { } \
         }",
    );

    env.run("Scripts.execStorage('oop_stored_class.tjs');");

    // `global.X` reaches a class defined at the top level of an
    // execStorage'd script (reference `test_class.tjs` uses
    // `global.ClassA.finalize()` the same way).
    assert_eq!(
        env.eval_int("typeof global.StoredTimer == 'Object' ? 1 : 0"),
        1,
        "the class must land on the global object"
    );
    env.run(
        "var SC = global.StoredTimer; \
         var stored = new SC(function() {}); \
         var stored2 = new StoredTimer(function() {});",
    );
    assert_eq!(
        env.eval_int("stored instanceof \"StoredTimer\""),
        1,
        "global.X from execStorage yields a constructible class"
    );
    assert_eq!(
        env.eval_int("stored2 instanceof \"Timer\""),
        1,
        "the storage-defined subclass extends the native Timer"
    );
}

// ---------------------------------------------------------------------------
// 8. Array/Dictionary `.count` + a method reference as a dictionary value
// ---------------------------------------------------------------------------

#[test]
fn array_count_and_method_valued_dictionary() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var arr = [10, 20, 30]; \
         var arrc = arr.count; \
         class Cb { function run() { return this.v; } } \
         var cbh = new Cb(); \
         cbh.v = 11; \
         var d = %[cb: cbh.run, n: 5]; \
         var dc = d.count; \
         var dn = d.n; \
         var dres = d.cb();",
    );

    assert_eq!(
        env.eval_ok("arrc"),
        TjsValue::Integer(3),
        "Array.count is a native property (tjsArray.cpp)"
    );
    assert_eq!(
        env.eval_ok("dc"),
        TjsValue::Void,
        "Dictionary has no count; tTJSDictionaryObject::PropGet returns \
         void for a missing member"
    );
    assert_eq!(env.eval_int("dn"), 5, "plain dictionary value round-trips");
    assert_eq!(
        env.eval_int("dres"),
        11,
        "a method reference stored in a dictionary must run with its bound this"
    );
    assert_eq!(
        env.eval_int("isvalid d.cb"),
        1,
        "the stored method reference stays a live object"
    );
}
