//! Receiver-dispatch tests for the "remaining / format" native families:
//! `Rect`, `text_render`, `psd_file`, `fstat`, `get_sample`, `save_struct`,
//! `image_function` and the objthis-family plugins.
//!
//! Mechanism under test (`docs/audits/object-write-dispatch.md`):
//! native dispatch has exactly one receiver channel, the `objthis` the VM
//! passes to `PropGet`/`PropSet`/`FuncCall`, derived in
//! `crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1440` as
//! `clo.ObjThis ? clo.ObjThis : ra[-1].AsObjectNoAddRef()`
//! (`ra[-1]` is the *executing function's* `this`,
//! `tjsInterCodeExec.cpp:710`). `tTJSVariantClosure::PropSet` forwards
//! `ObjThis ? ObjThis : (objthis ? objthis : Object)`
//! (`crates/tjs/cpp/tjs2/tjsVariant.h:261-269`). So a `with(obj){ .prop = v }`
//! whose with-register closure carries no `ObjThis` falls back to the
//! enclosing function's `this`.
//!
//! The ABI then makes a wrong receiver unrecoverable:
//!
//! * **instance** families derive the Rust payload *only* from `objthis`
//!   (`NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)`,
//!   `crates/tjs/cpp/tjs2_abi.cpp:1136` method, `:1327` property set,
//!   `:1263` property get);
//! * **objthis** families hand the raw `objthis` to Rust
//!   (`tjs2_abi.cpp:825` method, `:946` property set, `:912` property get);
//! * **static** families discard it (`tjs2_abi.cpp:569`/`:671`/`:718`).
//!
//! Each test builds a **script subclass** of the native class (the reference
//! `tTJSNativeClass::FuncCall` copies native members onto direct `new`
//! instances with `ChangeClosureObjThis`, `reference/cpp/core/tjs2/tjsNative.cpp:353`,
//! but the compat constructor member used by `super.ClassName()` does not —
//! `tjs2_abi.cpp:1392-1470`), writes a member in the constructor, and then
//! performs a `with(target){ .member }` access from a **different `this`**
//! (a method of another instance, and for `Rect` a `Timer` callback). The
//! assertion is that the access reached *that* object and not the enclosing
//! one.
//!
//! Correct-behaviour assertions here are expected to **fail until the
//! central receiver fix lands** in `crates/tjs/**`; `crates/tjs` is not
//! touched by this file. Every test serializes on the shared VM lock because
//! the engine → context binding is process-wide.

mod common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use engine::Storage;
use engine::input::InputState;
use engine::timer::TimerState;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-dispatch-fmt-{}-{}",
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

/// A bound VM + host + attached scene, timer registry, input and storage.
///
/// Fields drop in declaration order, so `_host` (which owns the process-wide
/// binding) drops before the engine/scene/timer/input/storage its raw
/// pointers reference.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
    /// The attached timer registry; tests move its injected time directly so a
    /// poll never depends on the wall clock.
    timer: RefCell<Box<TimerState>>,
    _input: Box<InputState>,
    _storage: Box<Storage>,
    _dir: GameDir,
}

impl Env {
    fn new() -> Env {
        let dir = GameDir::new();
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
        let mut input = Box::new(InputState::new());
        let mut host = CompatHost::new();
        // SAFETY: every boxed service is declared after `_host`, so it
        // outlives the host's raw pointer (the other tests' `attach_*`
        // contract).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
            host.services_mut().attach_storage(&mut *storage);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: scene,
            timer: RefCell::new(timer),
            _input: input,
            _storage: storage,
            _dir: dir,
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self._dir.path().join(name), bytes).expect("write fixture");
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "dispatch-fmt-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "dispatch-fmt-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            TjsValue::Integer(value) => value,
            TjsValue::Real(value) => value as i64,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_ok(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "dispatch-fmt-test")
            .map_err(|error| error.to_string())
    }

    /// Evaluate an expression that must raise, returning the message.
    fn error(&self, expression: &str) -> String {
        self.eval_ok(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }

    /// Move the attached timer registry's injected time forward by `ms`.
    fn advance_time(&self, ms: u64) {
        let mut timer = self.timer.borrow_mut();
        timer.now_ms = timer.now_ms.saturating_add(ms);
    }

    /// One frame-body `timer_poll` step at a virtual 16 ms frame step.
    fn poll_timer(&self) -> bool {
        self.advance_time(16);
        let mut ctx = self._host.ctx().expect("bound host");
        compat::timer_poll(&mut ctx).expect("timer poll")
    }
}

// ---------------------------------------------------------------------------
// PSD fixture (same grammar as tests/psd_behaviour.rs)
// ---------------------------------------------------------------------------

fn be16(value: u16) -> [u8; 2] {
    value.to_be_bytes()
}

fn be32(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

/// One image-resource block: `8BIM`, id word, Pascal name padded to even,
/// size dword, data padded to even
/// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:251-260`).
fn image_resource(id: u16, data: &[u8]) -> Vec<u8> {
    let mut out = b"8BIM".to_vec();
    out.extend_from_slice(&be16(id));
    out.push(0);
    out.push(0);
    out.extend_from_slice(&be32(data.len() as u32));
    out.extend_from_slice(data);
    if !data.len().is_multiple_of(2) {
        out.push(0);
    }
    out
}

/// A minimal, flattened PSD (`width == 3`, `height == 2`), accepted by the
/// reference top-level grammar (`psdparse/psdparse.h:668-669`).
fn minimal_psd() -> Vec<u8> {
    let mut out = b"8BPS".to_vec();
    out.extend_from_slice(&be16(1)); // version 1
    out.extend_from_slice(&[0u8; 6]); // reserved
    out.extend_from_slice(&be16(3)); // channels
    out.extend_from_slice(&be32(2)); // height
    out.extend_from_slice(&be32(3)); // width
    out.extend_from_slice(&be16(8)); // depth
    out.extend_from_slice(&be16(3)); // RGB
    out.extend_from_slice(&be32(0)); // colour-mode data
    let resource = image_resource(1000, b"PSDTEST!"); // 8 bytes, even
    out.extend_from_slice(&be32(resource.len() as u32));
    out.extend_from_slice(&resource);
    out.extend_from_slice(&be32(0)); // layer & mask
    out.extend_from_slice(&be16(0)); // raw merged image
    out.extend_from_slice(&[0u8; 18]); // 3 channels × 3 × 2 bytes
    out
}

// ---------------------------------------------------------------------------
// Rect — instance-payload family
// ---------------------------------------------------------------------------

/// `rect.rs` registers `left`/`top`/`right`/`bottom`/`width`/`height` as
/// instance properties and `setSize`/`set`/… as instance methods, all
/// resolving the payload from `objthis`
/// (`crates/compat/src/natives/rect.rs:707-807`,
/// `crates/tjs/cpp/tjs2_abi.cpp:1327`). Reference macro use:
/// `reference/cpp/core/visual/RectItf.cpp`.
///
/// A write made in the subclass constructor reaches the target's native
/// payload (the constructor's `this` is the target and `VM_SPD` does not set
/// `TJS_MEMBERENSURE`); the later `with`-write from a different `this` must
/// reach the same native payload. `includedPos` is used as the native-state
/// probe so a script-level shadow member cannot mask a misroute.
#[test]
fn rect_ctor_write_and_with_write_reach_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubRect extends Rect { \
             function SubRect() { super.Rect(0, 0, 10, 10); left = 1; } \
         } \
         var target = new SubRect(); \
         class Host extends Rect { \
             function Host() { super.Rect(0, 0, 10, 10); } \
             function writeRight(t) { with(t) { .right = 42; } } \
         } \
         var host = new Host(); \
         var ctorNative = target.includedPos(0, 0); \
         host.writeRight(target); \
         var targetNative = target.includedPos(41, 5); \
         var hostNative = host.includedPos(41, 5); \
         var targetRead = target.right;",
    );

    // The constructor write reached the target's native payload: `left = 1`
    // makes `(0, 0)` fall outside the rect.
    assert_eq!(
        env.eval_int("ctorNative"),
        0,
        "the constructor write must reach the target's native payload"
    );

    // A `with`-write from the *enclosing* object's method must reach the
    // target's native payload (`right = 42` makes `(41, 5)` fall inside).
    assert_eq!(
        env.eval_int("targetNative"),
        1,
        "with(target){{ .right = 42 }} from a different `this` must write the target's native payload"
    );
    assert_eq!(
        env.eval_int("hostNative"),
        0,
        "the enclosing object's native payload must be untouched"
    );
    assert_eq!(env.eval_int("targetRead"), 42);
}

/// Contrast cases for the same family: a **read** and a **method call**
/// through `with(target)` from a different `this` must resolve `target`
/// (`PropGet` uses the same receiver derivation, `tjsInterCodeExec.cpp:1432`;
/// method dispatch at `tjs2_abi.cpp:1136`).
#[test]
fn rect_read_and_method_call_through_with_reach_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubRect extends Rect { \
             function SubRect() { super.Rect(0, 0, 10, 10); left = 7; } \
         } \
         var target = new SubRect(); \
         class Host extends Rect { \
             function Host() { super.Rect(100, 100, 200, 200); } \
             function readThrough(t) { var v; with(t) { v = .left; } return v; } \
             function sizeThrough(t) { with(t) { .setSize(4, 5); } } \
         } \
         var host = new Host(); \
         var readBack = host.readThrough(target);",
    );

    assert_eq!(
        env.eval_int("readBack"),
        7,
        "with(target){{ .left }} must read the target"
    );

    env.run("host.sizeThrough(target);");
    assert_eq!(
        env.eval_int("target.width"),
        4,
        "with(target){{ .setSize(4, 5) }} must call the target's native method"
    );
    assert_eq!(
        env.eval_int("host.width"),
        100,
        "the enclosing object's native width must be untouched"
    );
}

/// The measured game shape: a `Timer` callback's `this` is the timer owner
/// (the enclosing object), and a `with(target){ .left }` **read** inside it
/// must observe the target. `tjs2_call_value` invokes the retained callback
/// with `FuncCall(..., nullptr)` and relies on the closure's `ObjThis`
/// (`tjs2_abi.cpp:2676-2712`).
#[test]
fn rect_timer_callback_with_read_reaches_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubRect extends Rect { \
             function SubRect() { super.Rect(0, 0, 10, 10); left = 1; } \
         } \
         var target = new SubRect(); \
         var seen = -1; \
         class Owner extends Rect { \
             var _target = null; \
             function Owner(t) { super.Rect(0, 0, 0, 0); _target = t; } \
             function onTimer() { with(_target) { seen = .left; } } \
         } \
         var owner = new Owner(target); \
         var timer = new Timer(owner.onTimer, ''); \
         timer.interval = 1; \
         timer.enabled = 1;",
    );
    env.poll_timer();

    assert_eq!(
        env.eval_int("seen"),
        1,
        "the timer callback's with-read must observe the target"
    );
    assert_eq!(
        env.eval_int("owner.left"),
        0,
        "the timer owner's native `left` must be untouched"
    );
}

// ---------------------------------------------------------------------------
// TextRenderBase — instance-payload family
// ---------------------------------------------------------------------------

/// `TextRenderBase` registers `fontSize`/`bold`/… as instance properties and
/// `setRenderSize`/`render`/… as instance methods
/// (`crates/compat/src/natives/plugins/text_render/mod.rs:939-1120`).
/// Reference: `reference/cpp/plugins/TextRender.cpp`.
#[test]
fn text_render_write_read_and_method_through_with_reach_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubTR extends TextRenderBase { \
             function SubTR() { super.TextRenderBase(); fontSize = 18; } \
         } \
         var target = new SubTR(); \
         class Host extends TextRenderBase { \
             function Host() { super.TextRenderBase(); } \
             function writeThrough(t) { with(t) { .fontSize = 33; } } \
             function readThrough(t) { var v; with(t) { v = .fontSize; } return v; } \
             function sizeThrough(t) { with(t) { .setRenderSize(320, 240); } } \
         } \
         var host = new Host();",
    );

    // The constructor write reached the target's payload.
    assert_eq!(env.eval_int("target.fontSize"), 18);

    env.run("host.writeThrough(target);");
    assert_eq!(
        env.eval_int("target.fontSize"),
        33,
        "with(target){{ .fontSize = 33 }} must write the target"
    );
    assert_eq!(
        env.eval_int("host.fontSize"),
        24,
        "the enclosing object's native fontSize must stay at the default"
    );

    env.run("var readBack = host.readThrough(target);");
    assert_eq!(
        env.eval_int("readBack"),
        33,
        "with(target){{ .fontSize }} must read the target"
    );

    env.run("host.sizeThrough(target);");
    assert_eq!(
        env.eval_int("target.renderRight"),
        320,
        "with(target){{ .setRenderSize(...) }} must call the target's native method"
    );
    assert_eq!(
        env.eval_int("host.renderRight"),
        0,
        "the enclosing object's render box must be untouched"
    );
}

// ---------------------------------------------------------------------------
// PSD — instance-payload, read-only property
// ---------------------------------------------------------------------------

/// `PSD` registers `width`/`height`/`channels`/… as getter-only instance
/// properties (`crates/compat/src/natives/plugins/psd_file/mod.rs:1172-1258`).
/// Reference `get_width` reads the header
/// (`reference/cpp/plugins/psdfile/psdclass.h:56-63`); a failed/missing load
/// answers `-1` (`psdparse/psdfile.cpp:17-24`).
#[test]
fn psd_width_read_through_with_reaches_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("ok.psd", &minimal_psd());
    env.run(
        "class SubPSD extends PSD { function SubPSD() { super.PSD(); } } \
         var target = new SubPSD(); \
         target.load('ok.psd'); \
         class Host extends PSD { \
             function Host() { super.PSD(); } \
             function readThrough(t) { var v; with(t) { v = .width; } return v; } \
         } \
         var host = new Host(); \
         var readBack = host.readThrough(target);",
    );

    assert_eq!(env.eval_int("target.width"), 3);
    assert_eq!(
        env.eval_int("host.width"),
        -1,
        "the host never loaded a PSD"
    );
    assert_eq!(
        env.eval_int("readBack"),
        3,
        "with(target){{ .width }} must read the target, not the enclosing object"
    );

    // The property is read-only on a direct instance as on the reference
    // (a script-subclass write would only shadow the inherited member).
    let _ = env.error("(new PSD()).width = 5");
}

// ---------------------------------------------------------------------------
// fstat — TemporaryFiles instance class (receiver-independent body)
// ---------------------------------------------------------------------------

/// `fstat` registers `TemporaryFiles` through `register_instance`
/// (`crates/compat/src/natives/plugins/fstat/mod.rs:1816-1828`) but its
/// `entry`/`entryFolder` callbacks ignore the `instance` parameter and work
/// from their explicit argument + attached storage
/// (`fstat/mod.rs:1660-1688`). The ABI still resolves the receiver before the
/// callback (`tjs2_abi.cpp:1136`), so a foreign receiver is a typed error
/// unless it is a `TemporaryFiles`; with a plain enclosing object the result
/// is receiver-independent. Reference `fstat/main.cpp`.
///
/// **Registration finding.** The reference registers a class-name
/// `Constructor()` (`reference/cpp/plugins/fstat/main.cpp:987-991`,
/// `NCB_REGISTER_CLASS(TemporaryFiles) { Constructor(); … }`), but the compat
/// builder lists only `entry`/`entryFolder` and no method named
/// `TemporaryFiles`. `tjs2_register_native_class_instance` only installs the
/// constructor dispatch when a method named after the class is present
/// (`crates/tjs/cpp/tjs2_abi.cpp:2273-2282`), so `new TemporaryFiles()` works
/// (`tTJSNativeClass::CreateNew` tolerates a missing constructor) while
/// `super.TemporaryFiles()` raises `Member "TemporaryFiles" does not exist`
/// and a script subclass is impossible. The test therefore uses a direct
/// instance; see the report for the recommended local fix.
#[test]
fn temporary_files_members_are_receiver_independent() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("present.txt", b"x");
    env.run(
        "var target = new TemporaryFiles(); \
         class Host { \
             function Host() {} \
             function callThrough(t, name) { var v; with(t) { v = .entry(name); } return v; } \
         } \
         var host = new Host();",
    );

    assert_eq!(env.eval_int("target.entry('present.txt')"), 1);
    assert_eq!(env.eval_int("target.entry('absent.txt')"), 0);
    assert_eq!(
        env.eval_int("host.callThrough(target, 'present.txt')"),
        1,
        "the argument, not the receiver, selects the path"
    );
}

// ---------------------------------------------------------------------------
// get_sample — WaveSoundBuffer objthis members
// ---------------------------------------------------------------------------

/// `get_sample` attaches `sampleCount`/`sampleAhead`/`sampleValue` and
/// `getSample`/`setDefaultCounts`/… to `WaveSoundBuffer` through
/// `register_native_instance_members`
/// (`crates/compat/src/natives/plugins/get_sample/mod.rs:518-556`); every
/// callback keys its per-object state on the raw `objthis`
/// (`get_sample/mod.rs:127-134`, `:305-341`). Reference
/// `reference/cpp/core/sound/WaveIntf.cpp`.
///
/// The per-object entry cannot be seeded with a plain `target.sampleCount =`
/// assignment on a script subclass (that write carries `TJS_MEMBERENSURE` and
/// only shadows the inherited member), so the entries are seeded through
/// `setDefaultCounts` plus a first *read* of each object, which is exactly the
/// receiver-sensitive path under test.
#[test]
fn wave_sound_buffer_sample_count_read_through_with_reaches_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubWSB extends WaveSoundBuffer { \
             function SubWSB() { super.WaveSoundBuffer(); } \
         } \
         var target = new SubWSB(); \
         class Host extends WaveSoundBuffer { \
             function Host() { super.WaveSoundBuffer(); } \
             function readThrough(t) { var v; with(t) { v = .sampleCount; } return v; } \
         } \
         var host = new Host(); \
         target.setDefaultCounts(128); \
         var directTarget = target.sampleCount; \
         target.setDefaultCounts(64); \
         var directHost = host.sampleCount; \
         var viaWith = host.readThrough(target);",
    );

    assert_eq!(env.eval_int("directTarget"), 128);
    assert_eq!(env.eval_int("directHost"), 64);
    assert_eq!(
        env.eval_int("viaWith"),
        env.eval_int("directTarget"),
        "with(target){{ .sampleCount }} must read the target's per-object state"
    );
}

// ---------------------------------------------------------------------------
// save_struct — Array/Dictionary objthis members
// ---------------------------------------------------------------------------

/// `save_struct` attaches `toStructString`/`saveStruct2`/`save2` to the TJS2
/// builtin `Array` and `Dictionary` through `register_native_instance_members`
/// (`crates/compat/src/natives/plugins/save_struct/mod.rs:501-533`); the body
/// retains the raw `objthis` and serializes it (`save_struct/mod.rs:200-227`).
/// Reference `reference/cpp/plugins/saveStruct`.
#[test]
fn array_to_struct_string_through_with_reaches_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var target = [1, 2, 3]; \
         var host = ['a', 'b']; \
         class Host { \
             function Host() {} \
             function callThrough(t) { var s; with(t) { s = .toStructString(); } return s; } \
         } \
         var h = new Host(); \
         var directTarget = target.toStructString(); \
         var viaWith = h.callThrough(target); \
         var directHost = host.toStructString();",
    );

    assert_ne!(
        env.eval("directTarget"),
        env.eval("directHost"),
        "the fixture arrays must serialize differently"
    );
    assert_eq!(
        env.eval("viaWith"),
        env.eval("directTarget"),
        "with(target){{ .toStructString() }} must serialize the target"
    );
}

// ---------------------------------------------------------------------------
// csv_parser — instance-payload family (with an objthis fallback)
// ---------------------------------------------------------------------------

/// `CSVParser` registers `currentLineNumber` (getter-only) plus
/// `init`/`parse`/… as instance properties/methods
/// (`crates/compat/src/natives/plugins/csv_parser/mod.rs:680-712`); the
/// getter reads the `instance` payload (`csv_parser/mod.rs:661-676`). The
/// script `doLine` target falls back to `objthis` inside `parse`
/// (`csv_parser/mod.rs:286-330`). Reference `CSVParserIntf.cpp`.
#[test]
fn csv_current_line_number_through_with_reaches_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubCSV extends CSVParser { function SubCSV() { super.CSVParser(); } } \
         var target = new SubCSV(); \
         target.init('a\\nb\\nc'); \
         target.getNextLine(); \
         class Host extends CSVParser { \
             function Host() { super.CSVParser(); } \
             function readThrough(t) { var v; with(t) { v = .currentLineNumber; } return v; } \
         } \
         var host = new Host(); \
         host.init('x\\ny\\nz'); \
         host.getNextLine(); \
         host.getNextLine(); \
         var directTarget = target.currentLineNumber; \
         var directHost = host.currentLineNumber; \
         var viaWith = host.readThrough(target);",
    );

    assert_ne!(
        env.eval_int("directTarget"),
        env.eval_int("directHost"),
        "the two parsers must be at different lines"
    );
    assert_eq!(
        env.eval_int("viaWith"),
        env.eval_int("directTarget"),
        "with(target){{ .currentLineNumber }} must read the target"
    );
}

// ---------------------------------------------------------------------------
// psb_file — instance-payload family
// ---------------------------------------------------------------------------

/// `PSBFile` registers `root` (getter-only) and `load`
/// (`crates/compat/src/natives/plugins/psb_file/mod.rs:491-509`); `root`
/// reads the `instance` payload (`psb_file/mod.rs:454-487`). Reference
/// `reference/cpp/plugins/psbfile/main.cpp:29-42`.
#[test]
fn psb_root_read_through_with_is_an_object_and_read_only() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubPSB extends PSBFile { function SubPSB() { super.PSBFile(); } } \
         var target = new SubPSB(); \
         class Host extends PSBFile { \
             function Host() { super.PSBFile(); } \
             function readThrough(t) { var v; with(t) { v = .root; } return v; } \
         } \
         var host = new Host(); \
         var root = host.readThrough(target); \
         var kind = typeof root;",
    );

    assert_eq!(env.eval("kind"), TjsValue::String("Object".into()));
    // `root` is getter-only on a direct instance (a script-subclass write
    // would only shadow the inherited member).
    let _ = env.error("(new PSBFile()).root = %[]");
}

// ---------------------------------------------------------------------------
// steam — DrawDeviceForSteam instance property
// ---------------------------------------------------------------------------

/// `steam` registers `DrawDeviceForSteam.interface` as a getter-only
/// instance property (`crates/compat/src/natives/plugins/steam/mod.rs:660-661`);
/// the getter reports the stable native payload address as a non-zero
/// synthetic pointer (`steam/mod.rs:501-509`). Reference
/// `reference/cpp/plugins/steam`.
#[test]
fn steam_draw_device_interface_is_an_instance_property() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubSteam extends DrawDeviceForSteam { \
             function SubSteam() { super.DrawDeviceForSteam(); } \
         } \
         var target = new SubSteam(); \
         class Host extends DrawDeviceForSteam { \
             function Host() { super.DrawDeviceForSteam(); } \
             function readThrough(t) { var v; with(t) { v = .interface; } return v; } \
         } \
         var host = new Host();",
    );

    // Both the direct read and the `with`-read reach the same instance
    // getter and return the same non-zero synthetic pointer.
    let direct = env.eval_int("target.interface");
    let via = env.eval_int("host.readThrough(target)");
    assert_ne!(direct, 0, "interface must be a non-zero synthetic pointer");
    assert_eq!(direct, via, "the with-read must reach the instance getter");

    // `interface` is getter-only on a direct instance.
    let _ = env.error("(new DrawDeviceForSteam()).interface = null");
}

// ---------------------------------------------------------------------------
// image_function and the static-only plugins — receiver is dropped
// ---------------------------------------------------------------------------

/// `ImageFunction` is registered as an instance class with only its
/// constructor (`crates/compat/src/natives/image_function.rs:1212-1221`); the
/// drawing entry points are **static** members (`image_function.rs:1223-1274`)
/// dispatched through `tjs2_dispatch_native_method`, which discards `objthis`
/// (`crates/tjs/cpp/tjs2_abi.cpp:569`). Static members live on the class
/// object, so they are not reachable as instance members at all.
#[test]
fn image_function_static_members_are_not_instance_routed() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var f = new ImageFunction();");
    assert_eq!(
        env.eval("typeof ImageFunction.operateRect"),
        TjsValue::String("Object".into()),
        "the reference entry point is a class-level static member"
    );
    assert!(
        env.eval_ok("f.operateRect(%[], %[] , 0)").is_err(),
        "a static member is not an instance member"
    );
    assert!(
        env.eval_ok("with(f) { .operateRect(%[], %[], 0); }")
            .is_err(),
        "a with-access cannot invent the static member on the instance"
    );
}

/// `add_font`, `scripts_ex` and `json` register only static members
/// (`crates/compat/src/natives/plugins/add_font/mod.rs:101-108`,
/// `scripts_ex/mod.rs:1437`, `json/mod.rs:1109`), whose dispatch drops
/// `objthis` (`tjs2_abi.cpp:671`/`:718`). `get_about`, `var_file`,
/// `emote_player` and `extrans` register no native members at all (their
/// `register` bodies are no-ops).
#[test]
fn static_only_and_stub_plugins_have_no_instance_receiver() {
    let _lock = vm_lock();
    let env = Env::new();

    // Static members are reachable on their class object (a method
    // reference is an object, `tjsInterCodeExec.cpp:2629`)…
    assert_eq!(
        env.eval("typeof System.addFont"),
        TjsValue::String("Object".into())
    );
    assert_eq!(
        env.eval("typeof Scripts.getObjectKeys"),
        TjsValue::String("Object".into())
    );

    // …and the stub plugins contribute nothing script-visible that could be
    // misrouted (the globals simply do not exist; a missing global reports
    // neither `Object` nor a class instance).
    //
    // `EmotePlayer` is deliberately **not** in this list: the reference
    // registers it into the `Motion` namespace only
    // (`reference/cpp/plugins/motionplayer/main.cpp:299`
    // `NCB_REGISTER_SUBCLASS_DELAY(EmotePlayer)` + `:431`
    // `NCB_SUBCLASS(EmotePlayer, EmotePlayer)`; the macro maps to
    // `ncbRegistSubClass`, `reference/cpp/core/plugin/ncbind.hpp:2171-2175`,
    // and `SubClass` attaches it with `TJS_STATICMEMBER`,
    // `ncbind.hpp:1792-1798`), so a top-level global `EmotePlayer` is a
    // `motion_player` port deviation — see the report. The `emote_player`
    // stub itself (`crates/.../emote_player/mod.rs`) registers nothing.
    for name in ["GetAbout", "VarFile", "Extrans"] {
        let kind = env.eval(&format!("typeof global['{name}']"));
        assert_ne!(
            kind,
            TjsValue::String("Object".into()),
            "{name} must stay unregistered, got {kind:?}"
        );
    }
}
