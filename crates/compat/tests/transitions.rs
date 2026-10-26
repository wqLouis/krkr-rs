//! Reference-behaviour tests for `Layer` transitions (`beginTransition` /
//! `stopTransition`) and the `extrans.dll` providers behind them.
//!
//! Every test states the **KiriKiri (reference C++) expectation** with a
//! `reference/cpp/**` citation above it and asserts that expectation against
//! the observed script-visible behaviour:
//!
//! * option parsing and the reference defaults of every `extrans` provider
//!   (`wave.cpp`, `mosaic.cpp`, `turn.cpp`, `rotatetrans.cpp`,
//!   `ripple.cpp`) plus the core providers (`TransIntf.cpp`);
//! * invalid options surface as typed errors during the `beginTransition`
//!   call — where the reference's providers validate — never silently;
//! * `time` is required, `callback` supplies the transition tick, and
//!   `selfupdate` waits for a frame whose projection drew its subtree;
//! * the phase-0 / phase-max endpoint short-circuits of
//!   `tTVPCrossFadeTransHandler::Process`;
//! * `universal` rule-image loading (`LoadImage` → grayscale, tiled) and
//!   the `vague` soft edge;
//! * `onTransitionCompleted` is fired **through the layer object** (so a
//!   script override wins) for both natural completion and a manual
//!   `stopTransition`, and is suppressed the way the reference suppresses
//!   it (dead source, invalidated endpoints);
//! * the `ttExchange` teardown: the two layers exchange position and
//!   visibility, and a stopped transition keeps the last rendered frame.
//!
//! Time is driven by the engine's frame clock: the test moves the clock
//! forward and calls [`compat::advance_transitions`] — the frame-body step —
//! so the transition's phase follows the clock; no frame loop is needed.
//! Every scenario gets a fresh set of layers inside one [`Env`],
//! serialized through [`common::vm_lock`].

mod common;

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use api::fs::{FsPath, OpenMode};
use api::media::{DecodedImage, ImageInfo};
use common::vm_lock;
use compat::CompatHost;
use engine::{FrameClock, Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// Script label for diagnostics.
const SCRIPT: &str = "transitions-test";

/// `0xRRGGBB` of the destination layer's fill (`Layer.setMainPixel`).
const RED: i64 = 0xFF0000;
/// `0xRRGGBB` of the transition source layer's fill.
const BLUE: i64 = 0x0000FF;

/// The bytes stored as `rule.img`; the synthetic decoder below recognises
/// them and returns [`RULE_RGBA`].
const RULE_BYTES: &[u8] = b"rule-image";

/// The 2x2 rule graphic the synthetic decoder returns: greys 0 and 128 in a
/// checkerboard (grey `g` encoded as `(g, g, g)`, whose Rec.601 luma
/// `(77r + 150g + 29b) >> 8` is exactly `g`).
const RULE_RGBA: [u8; 16] = [
    0, 0, 0, 255, 128, 128, 128, 255, //
    128, 128, 128, 255, 0, 0, 0, 255,
];

/// Writes `data` to `path` in an in-memory file system, creating parents.
fn write_memory_file(fs: &FileSystem, path: &str, data: &[u8]) {
    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(fs, &FsPath::host(parent)).expect("create parent dirs");
    }
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
    fs::write_at(fs, &file, 0, data).expect("write memory file");
    fs::flush(fs, &file).expect("flush memory file");
}

/// A bound VM + host + scene, plus an in-memory storage holding the rule
/// graphics `universal` loads through the (synthetic) image codec.
///
/// The boxes are declared after `host` so they outlive the host's raw
/// pointers (fields drop in declaration order).
struct Env {
    host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Kept alive for the host's raw pointer; never read directly.
    _scene: Box<Scene>,
    /// Only the rule-image tests use the storage; declared after `host` so
    /// it outlives the binding's raw pointer.
    _storage: Box<Storage>,
    /// The engine's frame clock: the transition's phase follows it.
    clock: RefCell<Box<FrameClock>>,
    /// How many times the synthetic codec decoded an image.
    decode_count: Arc<AtomicU32>,
}

impl Env {
    fn new() -> Env {
        let fs = FileSystem::memory();
        write_memory_file(&fs, "/rule.img", RULE_BYTES);
        let storage =
            Storage::mount_archives(fs, "/", std::iter::empty::<&str>()).expect("mount storage");

        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = Box::new(storage);
        let mut host = CompatHost::new();
        // The `rule` option is loaded through the same storage + decoder
        // pair `Layer.loadImages` uses (reference: `LoadImage` inside
        // `tTVPUniversalTransHandlerProvider::StartTransition`,
        // reference/cpp/core/visual/TransIntf.cpp:812-821); the codec is
        // synthetic — it decodes `RULE_BYTES` to the 2x2 checkerboard and
        // fails anything else. The counter tells a cached load from a fresh
        // decode.
        let decode_count = Arc::new(AtomicU32::new(0));
        let decoded = Arc::clone(&decode_count);
        host.set_image_codec(
            move |bytes, _hint| {
                decoded.fetch_add(1, Ordering::Relaxed);
                if bytes == RULE_BYTES {
                    Ok(DecodedImage::new(2, 2, RULE_RGBA.to_vec()))
                } else {
                    Err("synthetic rule decode failure".to_string())
                }
            },
            |_bytes, _hint| {
                Ok(ImageInfo {
                    width: 2,
                    height: 2,
                    has_alpha: true,
                    format: Some("img".to_string()),
                })
            },
            |_name, _type, _width, _height, _rgba| Ok(()),
        );
        let mut clock = Box::new(FrameClock::new());
        // SAFETY: the boxed scene, storage and clock outlive the host.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
            host.services_mut()
                .attach_clock(clock.as_mut() as *mut FrameClock);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            host,
            engine,
            _scene: scene,
            _storage: storage,
            clock: RefCell::new(clock),
            decode_count,
        }
    }

    /// How many times the synthetic codec decoded an image.
    fn decodes(&self) -> u32 {
        self.decode_count.load(Ordering::Relaxed)
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
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

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }

    /// The frame-body step: move the clock forward by `dt_ms` and advance
    /// every active transition to it.
    fn advance(&self, dt_ms: f64) {
        self.advance_clock(dt_ms);
        let ctx = self.host.ctx().expect("bound host");
        compat::advance_transitions(&ctx).expect("advance transitions");
    }

    /// One frame with the projection's drawn-layer report: the normal frame
    /// step, then the self-update step for the layers the projection emitted.
    fn advance_drawn(&self, dt_ms: f64, drawn: &[u32]) {
        self.advance_clock(dt_ms);
        let ctx = self.host.ctx().expect("bound host");
        compat::advance_transitions(&ctx).expect("advance transitions");
        compat::advance_self_updating_transitions(&ctx, drawn)
            .expect("advance self-updating transitions");
    }

    /// Move the engine's frame clock forward by `dt_ms`.
    fn advance_clock(&self, dt_ms: f64) {
        let mut clock = self.clock.borrow_mut();
        clock.advance(dt_ms);
    }
}

/// A window with two `size` x `size` layers: `a` — the `transwith` source,
/// filled blue (`0x0000ff`) — and `b` — the destination, filled red
/// (`0xff0000`) and made visible (a non-primary layer defaults to hidden,
/// and an invisible destination ends the transition at the first frame —
/// reference/cpp/core/visual/LayerIntf.cpp:8056-8060).
fn two_layers(env: &Env, size: i64) {
    two_layers_with(env, size, "");
}

/// `two_layers` with `preamble` inserted right after the window is created:
/// the tests that position `a` pass a throwaway first layer there, because
/// a window's first layer is its primary layer and the reference refuses to
/// move the primary layer (`reference/cpp/core/visual/LayerIntf.cpp:2163-2179`).
fn two_layers_with(env: &Env, size: i64, preamble: &str) {
    env.run(&format!(
        "var w = new Window(); {preamble} \
         var a = new Layer(w, null); a.setSize({size}, {size}); a.setImageSize({size}, {size}); a.name = 'src'; \
         var b = new Layer(w, null); b.setSize({size}, {size}); b.setImageSize({size}, {size}); b.visible = true; b.name = 'dest'; \
         for (var i = 0; i < {size} * {size}; i++) {{ \
             b.setMainPixel(i % {size}, i / {size}, {RED}); \
             a.setMainPixel(i % {size}, i / {size}, {BLUE}); \
         }}"
    ));
}

/// A window whose `a` (the transition source) and `b` (the destination) hold
/// their content in child layers — the KAG `Sprite` shape, with no main image
/// of their own — and a whole-subtree transition from `a` to `b` running.
fn two_layers_with_children(env: &Env) {
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setSize(2, 2); a.name = 'src'; a.hasImage = false; \
         var ac = new Layer(w, a); ac.setSize(2, 2); ac.setImageSize(2, 2); \
         ac.fillRect(0, 0, 2, 2, 0xff0000ff); ac.visible = true; \
         var b = new Layer(w, null); b.setSize(2, 2); b.visible = true; b.name = 'dest'; b.hasImage = false; \
         var bc = new Layer(w, b); bc.setSize(2, 2); bc.setImageSize(2, 2); \
         bc.fillRect(0, 0, 2, 2, 0xffff0000); bc.visible = true; \
         b.beginTransition('crossfade', true, a, %[time: 100]);",
    );
}

/// Install an `onTransitionCompleted` counter on `b` (an instance member
/// shadows the native default; the dispatch resolves the member on the
/// object, exactly like the reference's synchronous event at the owner —
/// reference/cpp/core/visual/LayerIntf.cpp:8000-8015).
fn counter(env: &Env) {
    env.run(
        "var hits = 0; var sawDest = 0; var sawSrc = 0; \
         b.onTransitionCompleted = function(dest, src) { \
             hits++; \
             sawDest = (dest === b) ? 1 : 0; \
             sawSrc = (src === a) ? 1 : 0; \
         };",
    );
}

/// The reference const-alpha blend per channel,
/// `s1 + ((s2 - s1) * opa >> 8)` (`reference/cpp/plugins/common.h:17-29`,
/// the `TVPConstAlphaBlend_SD` arithmetic of
/// `reference/cpp/core/visual/tvpgl.cpp:4379-4404`), on `0xRRGGBB`.
fn blend_rgb(s1: i64, s2: i64, opa: i32) -> i64 {
    let mut out = 0i64;
    for shift in [16, 8, 0] {
        let a = (s1 >> shift) & 0xff;
        let b = (s2 >> shift) & 0xff;
        out |= (a + (((b - a) * i64::from(opa)) >> 8)) << shift;
    }
    out
}

/// The most recently created scene layer carrying the script `name`: a test
/// that builds several windows of `a`/`b` layers reuses those names, and the
/// newest window's layers are appended after the earlier ones.
fn named_layer<'a>(env: &'a Env, name: &str) -> &'a engine::LayerState {
    env._scene
        .layers
        .iter()
        .rev()
        .find(|layer| layer.name == name)
        .unwrap_or_else(|| panic!("layer {name:?} not found"))
}

/// The layer's rendered subtree cache (the reference `CacheBitmap`), or
/// `None` before the first frame and after the teardown.
fn cache_bitmap_of(env: &Env, name: &str) -> Option<u32> {
    named_layer(env, name).cache_bitmap
}

/// The bitmap a whole-subtree transition blends into: the destination's own
/// frame, which the projection draws in place of the layer's subtree while the
/// transition runs (`reference/cpp/core/visual/LayerIntf.cpp:7221-7226`).
fn transition_frame(env: &Env, name: &str) -> u32 {
    cache_bitmap_of(env, name).unwrap_or_else(|| panic!("{name} has no transition frame"))
}

/// The `0xRRGGBB` colour of `name`'s transition frame at `(x, y)`.
fn frame_pixel(env: &Env, name: &str, x: u32, y: u32) -> i64 {
    scene_pixel(env, transition_frame(env, name), x, y)
}

/// The layer's self image: the per-frame blend a transition without children
/// draws in place of the layer's own image (`DrawSelf` blends into a temporary,
/// `reference/cpp/core/visual/LayerIntf.cpp:6721-6768`), or `None` before the
/// first frame and after the teardown.
fn self_image_of(env: &Env, name: &str) -> Option<u32> {
    named_layer(env, name).transition_self
}

/// Every pixel of `name`'s transition frame as one comma-joined string.
fn frame_grid(env: &Env, name: &str, size: i64) -> String {
    let frame = transition_frame(env, name);
    let size = u32::try_from(size).expect("grid size");
    let mut out = String::new();
    for y in 0..size {
        for x in 0..size {
            out.push_str(&format!("{},", scene_pixel(env, frame, x, y)));
        }
    }
    out
}

/// The `0xRRGGBB` colour of a scene bitmap at `(x, y)`.
fn scene_pixel(env: &Env, bitmap: u32, x: u32, y: u32) -> i64 {
    let bitmap = env._scene.bitmap(bitmap).expect("scene bitmap");
    let offset = bitmap.pixel_offset(x, y).expect("pixel offset");
    let px = &bitmap.rgba[offset..offset + 4];
    (i64::from(px[0]) << 16) | (i64::from(px[1]) << 8) | i64::from(px[2])
}

/// Assert that every pixel of the 2x2 bitmap `bitmap` is `expected`.
fn assert_2x2(env: &Env, bitmap: u32, expected: i64, what: &str) {
    for (x, y) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
        assert_eq!(
            scene_pixel(env, bitmap, x, y),
            expected,
            "{what} at ({x},{y})"
        );
    }
}

// ---------------------------------------------------------------------------
// beginTransition: parameter and option validation
// ---------------------------------------------------------------------------

/// `TJS_BEGIN_NATIVE_METHOD_DECL(beginTransition)`: `numparams < 1` is
/// `TJS_E_BADPARAMCOUNT` — calling without a name raises.
///
/// reference/cpp/core/visual/LayerIntf.cpp:9824-9826
#[test]
fn begin_transition_requires_a_name() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    let message = env.error("b.beginTransition()");
    assert!(
        message.contains("requires a name"),
        "unexpected error: {message}"
    );
}

/// The native member resolves param 2 through
/// `NativeInstanceSupport(TJS_NIS_GETINSTANCE, tTJSNC_Layer::ClassID)` and
/// throws `TVPSpecifyLayer` when it is absent, null, or not a Layer.
///
/// reference/cpp/core/visual/LayerIntf.cpp:9828-9841
#[test]
fn begin_transition_requires_a_layer_source() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    for expression in [
        "b.beginTransition('crossfade')",
        "b.beginTransition('crossfade', true, null)",
        "b.beginTransition('crossfade', true, w)",
    ] {
        let message = env.error(expression);
        assert!(
            message.contains("transwith"),
            "{expression:?} should complain about transwith, got: {message}"
        );
    }
}

/// `TVPFindTransHandlerProvider` looks the name up in a hash table keyed by
/// the exact `ttstr` (`TransIntf.cpp:296-299`); the reference falls back to
/// `crossfade` with a warning box on a miss (`:331-341`), this port reports
/// a typed error instead — but matching stays exact and
/// case-sensitive either way.
///
/// reference/cpp/core/visual/TransIntf.cpp:296-341
#[test]
fn transition_names_are_matched_exactly() {
    let _lock = vm_lock();
    let env = Env::new();
    for name in ["nope", "CrossFade"] {
        two_layers(&env, 2);
        let message = env.error(&format!(
            "b.beginTransition({name:?}, true, a, %[time: 100])"
        ));
        assert!(
            message.contains("unsupported transition"),
            "{name:?} should be unsupported, got: {message}"
        );
    }
    two_layers(&env, 2);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]); b.stopTransition();");
}

/// An empty method name is the KAG default (`method=""`), used by the game's
/// `normal`/`500sec`/`300secNosync` transition entries. The reference's
/// `TVPFindTransHandlerProvider` misses the empty key and falls back to
/// `crossfade` (`TransIntf.cpp:331-341`), so `beginTransition("")` must
/// start a crossfade rather than raise.
///
/// reference/cpp/core/visual/TransIntf.cpp:331-341
#[test]
fn an_empty_method_name_is_the_reference_crossfade_default() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run("b.beginTransition('', false, a, %[time: 100]);");
    env.advance(100.0);
    assert_eq!(
        env.eval_int("hits"),
        1,
        "the empty-method transition completed"
    );
    // Without children the frame blends the two main images into a temporary
    // that is never stored on either layer (`LayerIntf.cpp:6721-6744`), so the
    // destination's own image is untouched when the transition ends.
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);
}

/// `StartTransition` refuses to begin while the layer is already in a
/// transition (`TVPCurrentTransitionMustBeStopping`); the running
/// transition is unaffected by the failed second call.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7776-7778
#[test]
fn a_second_begin_transition_is_rejected_while_running() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]);");
    let message = env.error("b.beginTransition('crossfade', true, a, %[time: 100]);");
    assert!(
        message.contains("already running"),
        "unexpected error: {message}"
    );
    // The first transition still completes normally.
    env.advance(100.0);
    assert_eq!(env.eval_int("hits"), 1, "the first transition completed");
}

/// `StartTransition` rejects a source that is itself transitioning *from
/// this* layer (`TVPTransitionMutualSource`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:7779-7781
#[test]
fn mutual_transition_source_is_rejected() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]);");
    let message = env.error("a.beginTransition('crossfade', true, b, %[time: 100]);");
    assert!(
        message.contains("already transitioning from this layer"),
        "unexpected error: {message}"
    );
}

/// The providers reject two sources of different sizes in
/// `StartTransition` (`wave.cpp:319-320`; the core providers throw
/// `TVPTransitionLayerSizeMismatch`, `TransIntf.cpp:493-497`) — i.e. the
/// `beginTransition` call itself raises, not the first rendered frame.
///
/// reference/cpp/plugins/wave.cpp:317-320
#[test]
fn source_size_mismatch_is_rejected_at_begin() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setSize(4, 4); a.setImageSize(4, 4); \
         var b = new Layer(w, null); b.setSize(2, 2); b.setImageSize(2, 2); b.visible = true;",
    );
    let message = env.error("b.beginTransition('crossfade', true, a, %[time: 100]);");
    assert!(
        message.contains("size mismatch"),
        "unexpected error: {message}"
    );
}

/// Every provider raises sub-2 millisecond times to 2
/// (`if(time < 2) time = 2`) instead of running a shorter transition or
/// falling back to the default: `time: 1` completes on the second
/// millisecond, never the first; `time: 0` behaves the same way.
///
/// reference/cpp/plugins/wave.cpp:332-336
/// reference/cpp/core/visual/TransIntf.cpp:512-516
#[test]
fn time_below_two_milliseconds_is_raised_to_two() {
    let _lock = vm_lock();
    let env = Env::new();
    for time in [1, 0] {
        two_layers(&env, 2);
        counter(&env);
        env.run(&format!(
            "b.beginTransition('crossfade', true, a, %[time: {time}]);"
        ));
        env.advance(1.0);
        assert_eq!(
            env.eval_int("hits"),
            0,
            "time: {time} must last 2 ms, not {time} ms"
        );
        env.advance(1.0);
        assert_eq!(env.eval_int("hits"), 1, "time: {time} completes at 2 ms");
    }
}

/// Every core provider requires the `time` option: `GetAsNumber` raises
/// `TVPSpecifyOption` for an absent, `void` or non-numeric member, and the
/// call itself raises — there is no default duration.
///
/// reference/cpp/core/visual/TransIntf.cpp:512-519
#[test]
fn time_option_is_required() {
    let _lock = vm_lock();
    let env = Env::new();
    for options in ["null", "%[]", "%[time: void]", "%[time: %[]]"] {
        two_layers(&env, 2);
        let message = env.error(&format!(
            "b.beginTransition('crossfade', true, a, {options});"
        ));
        assert!(
            message.contains("Specify option time"),
            "{options} must raise TVPSpecifyOption time, got: {message}"
        );
    }

    // A valid `time` still starts the transition (`time: 0` is clamped to 2).
    two_layers(&env, 2);
    counter(&env);
    env.run("b.beginTransition('crossfade', true, a, %[time: 0]);");
    env.advance(1.0);
    assert_eq!(env.eval_int("hits"), 0);
    env.advance(1.0);
    assert_eq!(env.eval_int("hits"), 1);
}

/// The `callback` option replaces the frame clock: `GetTransTick` calls it and
/// uses the returned tick as the transition time, so advancing the frame by
/// any amount does not move a callback-driven transition until the callback's
/// value does. The initial tick is 0 (`StartTransition`'s dummy
/// `StartProcess(0)`), so the callback's result is the elapsed time.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7807-7818
/// reference/cpp/core/visual/LayerIntf.cpp:7909-7922
/// reference/cpp/core/visual/LayerIntf.cpp:8053-8056
/// reference/cpp/core/visual/LayerIntf.cpp:8261-8270
#[test]
fn callback_supplies_the_transition_tick() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run(
        "var tick = 0; var calls = 0; \
         var ticker = function() { calls++; return tick; }; \
         b.beginTransition('crossfade', true, a, %[time: 100, callback: ticker]);",
    );

    // A large frame delta is ignored while the callback still returns 0.
    env.advance(500.0);
    assert_eq!(env.eval_int("calls"), 1, "the callback ran once");
    assert_eq!(
        env.eval_int("hits"),
        0,
        "the frame clock did not advance it"
    );

    env.run("tick = 50;");
    env.advance(0.0);
    assert_eq!(env.eval_int("calls"), 2);
    assert_eq!(env.eval_int("hits"), 0, "50 ms of a 100 ms transition");

    env.run("tick = 99;");
    env.advance(0.0);
    assert_eq!(env.eval_int("hits"), 0, "99 ms is still short");

    env.run("tick = 100;");
    env.advance(0.0);
    assert_eq!(env.eval_int("hits"), 1, "the callback reached the duration");
}

/// `StartTransition` reads `callback` with `AsObjectClosure`, which throws for
/// a non-object while the script is still inside `beginTransition`; it is not
/// deferred to the first tick. A TJS `null` is an object closure, so it is the
/// separate null-access failure at tick time.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7807-7818
/// reference/cpp/core/tjs2/tjsVariant.h:719-726
#[test]
fn callback_must_be_an_object_at_begin() {
    let _lock = vm_lock();
    let env = Env::new();
    for value in ["1", "1.5", "'tick'"] {
        two_layers(&env, 2);
        let message = env.error(&format!(
            "b.beginTransition('crossfade', true, a, %[time: 100, callback: {value}]);"
        ));
        assert!(
            message.contains("callback must be a function"),
            "callback: {value} must raise at begin, got: {message}"
        );
    }
}

/// `selfupdate: true` keeps the transition off the automatic frame pump
/// (`StartTransition` skips `TVPAddContinuousEventHook`). It advances only on a
/// frame whose projection drew the destination subtree; wall-clock time that
/// passed while it waited is applied on that draw, not discarded.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7797-7803
/// reference/cpp/core/visual/LayerIntf.cpp:7906-7908
/// reference/cpp/core/visual/LayerIntf.cpp:6410-6422
/// reference/cpp/core/visual/LayerIntf.cpp:8053-8056
#[test]
fn selfupdate_waits_for_a_drawn_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100, selfupdate: true]);");
    let drawn = [named_layer(&env, "dest").id];

    env.advance_drawn(50.0, &[]);
    env.advance_drawn(50.0, &[]);
    assert_eq!(
        env.eval_int("hits"),
        0,
        "without a drawn layer the transition did not run"
    );

    env.advance_drawn(0.0, &drawn);
    assert_eq!(
        env.eval_int("hits"),
        1,
        "the drawn layer applied the elapsed 100 ms"
    );
}

/// A self-updating transition with a `callback` does not call the callback
/// while it waits, and uses its tick on the draw that emits the layer.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7807-7818
/// reference/cpp/core/visual/LayerIntf.cpp:7906-7908
/// reference/cpp/core/visual/LayerIntf.cpp:6414-6422
/// reference/cpp/core/visual/LayerIntf.cpp:8261-8270
#[test]
fn selfupdate_and_callback_advance_only_on_draw() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run(
        "var tick = 0; var calls = 0; \
         var ticker = function() { calls++; return tick; }; \
         b.beginTransition('crossfade', true, a, \
             %[time: 100, selfupdate: true, callback: ticker]);",
    );
    let drawn = [named_layer(&env, "dest").id];

    env.advance_drawn(100.0, &[]);
    assert_eq!(env.eval_int("calls"), 0, "no callback without a draw");
    assert_eq!(env.eval_int("hits"), 0);

    env.advance_drawn(0.0, &drawn);
    assert_eq!(env.eval_int("calls"), 1);
    assert_eq!(env.eval_int("hits"), 0, "the callback still reports 0");

    env.run("tick = 100;");
    env.advance_drawn(0.0, &drawn);
    assert_eq!(env.eval_int("calls"), 2);
    assert_eq!(env.eval_int("hits"), 1, "the draw's tick completes it");
}

// ---------------------------------------------------------------------------
// withchildren: the whole-subtree path
// ---------------------------------------------------------------------------

/// The native member reads `withchildren` from argument index 1: a `void`
/// (or omitted) argument keeps the default `true`, any other value is its
/// boolean coercion (`LayerIntf.cpp:9819-9821`). Without children
/// `StartTransition` requires both endpoints' main images
/// (`TVPTransitionSourceAndDestinationMustHaveImage`,
/// `LayerIntf.cpp:7860-7868`); with the default it transitions the layer
/// trees and does not.
///
/// reference/cpp/core/visual/LayerIntf.cpp:9815-9848
/// reference/cpp/core/visual/LayerIntf.cpp:7860-7868
#[test]
fn with_children_is_read_at_argument_one() {
    let _lock = vm_lock();
    let env = Env::new();
    // Both layers have a size but no main image.
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setSize(2, 2); a.name = 'src'; a.hasImage = false; \
         var b = new Layer(w, null); b.setSize(2, 2); b.visible = true; b.name = 'dest'; b.hasImage = false;",
    );
    for value in ["false", "0"] {
        let message = env.error(&format!(
            "b.beginTransition('crossfade', {value}, a, %[time: 100]);"
        ));
        assert!(
            message.contains("must have image"),
            "withchildren={value} must require the main images, got: {message}"
        );
    }
    // `void` keeps the default `true`; the tree transition is accepted and
    // the teardown releases the cache consumers.
    env.run("b.beginTransition('crossfade', void, a, %[time: 100]); b.stopTransition();");
    assert_eq!(named_layer(&env, "dest").cache_enabled_count, 0);
}

/// Without children both endpoints must have a main image
/// (`LayerIntf.cpp:7860-7868`). `DrawSelf` blends them into a temporary it
/// hands to the draw target (`:6721-6768`): source 1 is the layer's own main
/// image and source 2 the transition source's (`DoDivisibleTransition`,
/// `:8078-8133`), and the layer draws that temporary in place of its own image
/// while its children stay live (`:7278-7309`). The endpoint short-circuits
/// make phase 0 exactly source 1 and the final phase exactly source 2
/// (`TransIntf.cpp:578-601`). The `ttExchange` teardown uses `Swap`
/// (`Exchange(target, true)`, `LayerIntf.h:260-263`), which also moves the two
/// layers' children to the other layer.
///
/// reference/cpp/core/visual/LayerIntf.cpp:6721-6768
/// reference/cpp/core/visual/LayerIntf.cpp:6781-6783
/// reference/cpp/core/visual/LayerIntf.cpp:7278-7309
/// reference/cpp/core/visual/LayerIntf.cpp:7673-7737
/// reference/cpp/core/visual/LayerIntf.cpp:8154-8249
/// reference/cpp/core/visual/TransIntf.cpp:578-601
#[test]
fn a_transition_without_children_blends_into_the_self_image() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setSize(2, 2); a.setImageSize(2, 2); a.name = 'src'; a.visible = false; \
         var b = new Layer(w, null); b.setSize(2, 2); b.setImageSize(2, 2); b.visible = true; b.name = 'dest'; \
         for (var i = 0; i < 4; i++) { b.setMainPixel(i % 2, i / 2, 0xff0000); a.setMainPixel(i % 2, i / 2, 0x0000ff); } \
         var bc = new Layer(w, b); bc.setSize(2, 2); bc.setImageSize(2, 2); bc.name = 'ctl'; \
         bc.fillRect(0, 0, 2, 2, 0xff00ff00); bc.visible = true; \
         b.beginTransition('crossfade', false, a, %[time: 100]);",
    );
    counter(&env);
    // Nothing has been drawn yet: the slot is still empty.
    assert!(self_image_of(&env, "dest").is_none());

    // Phase 0: the self image holds source 1 verbatim.
    env.advance(0.0);
    let self_image = self_image_of(&env, "dest").expect("self image");
    assert_2x2(&env, self_image, RED, "phase 0 self image");

    // 50 ms into a 100 ms cross-fade truncates to phase 127, drawn into the
    // same self image.
    env.advance(50.0);
    assert_eq!(
        self_image_of(&env, "dest"),
        Some(self_image),
        "the self image is reused between frames"
    );
    assert_2x2(
        &env,
        self_image,
        blend_rgb(RED, BLUE, 127),
        "phase 127 self image",
    );

    // 99 ms into the transition truncates to phase 252, still short of the
    // PhaseMax guarantee that makes the last frame source 2 verbatim.
    env.advance(49.0);
    assert_2x2(
        &env,
        self_image,
        blend_rgb(RED, BLUE, 252),
        "phase 252 self image",
    );
    assert_eq!(env.eval_int("hits"), 0, "still running at phase 252");

    // Both main images and the live child are untouched at every phase.
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);
    assert_eq!(env.eval_int("b.getMainPixel(1, 1)"), RED);
    assert_eq!(env.eval_int("a.getMainPixel(0, 0)"), BLUE);
    assert_eq!(env.eval_int("a.getMainPixel(1, 1)"), BLUE);
    assert_eq!(env.eval_int("bc.getMainPixel(0, 0)"), 0x00ff00);
    assert_eq!(
        named_layer(&env, "ctl").parent,
        Some(named_layer(&env, "dest").id)
    );

    // Completion clears the slot and runs the `Swap` teardown.
    env.advance(1.0);
    assert_eq!(env.eval_int("hits"), 1, "the transition completed");
    assert!(
        self_image_of(&env, "dest").is_none(),
        "completion clears the self image slot"
    );
    assert!(
        env._scene.bitmap(self_image).is_none(),
        "the self image bitmap is released"
    );
    assert!(
        cache_bitmap_of(&env, "dest").is_none(),
        "a childless transition allocates no subtree frame"
    );
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);
    assert_eq!(env.eval_int("a.getMainPixel(0, 0)"), BLUE);
    assert_eq!(env.eval_int("bc.getMainPixel(0, 0)"), 0x00ff00);
    assert_eq!(
        named_layer(&env, "ctl").parent,
        Some(named_layer(&env, "src").id),
        "Swap exchanges the layers' direct children"
    );
    assert_eq!(env.eval_int("b.visible"), 0, "b takes a's visibility");
    assert_eq!(env.eval_int("a.visible"), 1, "a takes b's visibility");
}

/// The self image belongs to one transition: the teardown drops it, so the
/// next transition on the layer starts from an empty slot and allocates a fresh
/// blend target (`DrawSelf` allocates its temporary per draw,
/// `LayerIntf.cpp:6721-6768`; `InternalStopTransition` releases the transition
/// state, `:7950-8031`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:6721-6768
/// reference/cpp/core/visual/LayerIntf.cpp:6781-6783
/// reference/cpp/core/visual/LayerIntf.cpp:7156-7226
#[test]
fn a_stopped_transition_releases_its_self_image() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    env.run("b.beginTransition('crossfade', false, a, %[time: 100]);");
    env.advance(0.0);
    let first = self_image_of(&env, "dest").expect("self image");
    env.run("b.stopTransition();");
    assert!(
        self_image_of(&env, "dest").is_none(),
        "stopping clears the slot"
    );
    assert!(
        env._scene.bitmap(first).is_none(),
        "and releases its bitmap"
    );
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);

    // The teardown hid the destination; make it visible again before the second
    // transition, which ends at the first frame while its destination is
    // hidden (`LayerIntf.cpp:8056-8060`).
    env.run("b.visible = true; b.beginTransition('crossfade', false, a, %[time: 100]);");
    assert!(
        self_image_of(&env, "dest").is_none(),
        "the second transition starts from a clean slot"
    );
    env.advance(0.0);
    let second = self_image_of(&env, "dest").expect("second self image");
    assert!(
        second > first,
        "a fresh bitmap, not the released one ({second} vs {first})"
    );
    assert_2x2(&env, second, RED, "the second transition's self image");
}

/// A layer with a transition self image that sits inside another layer's
/// cached subtree is cached as the blended self image, not as its main image:
/// the caching parent composites its subtree into `CacheBitmap`
/// (`reference/cpp/core/visual/LayerIntf.cpp:7172-7198`), and a child in a
/// transition without children draws its self image there through
/// `CopySelfForRect` (`:6781-6783`, `:7278-7309`). Such a child draws no
/// cache of its own (`:7156-7157`), and the parent's cache is recomposited
/// while a drawn image changes (`:6305-6335`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:7172-7198
/// reference/cpp/core/visual/LayerIntf.cpp:6781-6783
/// reference/cpp/core/visual/LayerIntf.cpp:7278-7309
/// reference/cpp/core/visual/LayerIntf.cpp:7156-7157
/// reference/cpp/core/visual/LayerIntf.cpp:6305-6335
#[test]
fn a_nested_childless_transition_is_cached_as_its_self_image() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var p = new Layer(w, null); p.setSize(2, 2); p.name = 'parent'; p.hasImage = false; p.visible = true; \
         var c = new Layer(w, p); c.setSize(2, 2); c.setImageSize(2, 2); c.name = 'inner'; \
         c.fillRect(0, 0, 2, 2, 0xffff0000); c.visible = true; \
         var cs = new Layer(w, null); cs.setSize(2, 2); cs.setImageSize(2, 2); cs.name = 'innerSrc'; \
         cs.fillRect(0, 0, 2, 2, 0xff0000ff); \
         var ps = new Layer(w, null); ps.setSize(2, 2); ps.setImageSize(2, 2); ps.name = 'outerSrc'; \
         ps.fillRect(0, 0, 2, 2, 0xff00ff00); \
         c.beginTransition('crossfade', false, cs, %[time: 100]);",
    );
    // 50 ms into a 100 ms cross-fade truncates to phase 127.
    env.advance(50.0);
    let blended = blend_rgb(RED, BLUE, 127);
    let inner_self = self_image_of(&env, "inner").expect("the child's self image");
    assert_eq!(scene_pixel(&env, inner_self, 0, 0), blended);

    // The parent caches its subtree: `Complete` composites the child through
    // `CopySelfForRect`, so the child's transition lands in the cache.
    env.run("p.beginTransition('crossfade', true, ps, %[time: 100]);");
    // Phase 0 repaints from the cached content verbatim at either tick order,
    // so the nested self image is the value the parent composites.
    env.advance(0.0);
    assert_eq!(named_layer(&env, "parent").cache_enabled_count, 1);
    let content = named_layer(&env, "parent")
        .cache_content
        .expect("the parent's cached subtree");
    assert_eq!(
        scene_pixel(&env, content, 0, 0),
        blended,
        "the cache holds the child's blended self image, not its main image"
    );
    assert_eq!(
        env.eval_int("c.getMainPixel(0, 0)"),
        RED,
        "the child's main image is untouched"
    );
    // The parent's frame is its phase-0 content verbatim.
    let frame = cache_bitmap_of(&env, "parent").expect("the parent's frame");
    assert_eq!(scene_pixel(&env, frame, 0, 0), blended);

    // The nested self image is rewritten on every tick, so the next composite
    // rebuilds the cache in place instead of leaving the stale main image.
    env._scene.set_bitmap_clean(content);
    env.advance(0.0);
    assert_eq!(
        named_layer(&env, "parent").cache_content,
        Some(content),
        "the cache is recomposited into the same bitmap"
    );
    assert!(
        env._scene.bitmap(content).unwrap().dirty,
        "the changed nested self image invalidates the cache"
    );
    assert_eq!(scene_pixel(&env, content, 0, 0), blended);
}

/// With `withchildren = true` (the default) `StartTransition` transitions the
/// whole layer tree: caching is enabled on both layers
/// (`IncCacheEnabledCount`, `LayerIntf.cpp:7869-7876`) and each layer's
/// content is rendered through `Complete`, which composites the layer's own
/// image and its children (`LayerIntf.cpp:7673-7755`). A KAG `Sprite` keeps
/// its content in child layers and has no main image of its own, so the
/// blend must read the children. The frame is the same linear cross-fade as
/// the main-image path (`TransIntf.cpp:578-601`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:7673-7755
/// reference/cpp/core/visual/LayerIntf.cpp:7860-7876
/// reference/cpp/core/visual/TransIntf.cpp:578-601
#[test]
fn with_children_blends_the_layer_subtrees() {
    let _lock = vm_lock();
    let env = Env::new();
    // `a` and `b` have no main image; their children hold the red/blue
    // pixels. A fresh layer already has the reference's 32x32
    // transparent-white image, and `setImageSize` fills only the region it
    // expands, so shrinking to 2x2 keeps those default pixels; the children
    // are painted opaque with `fillRect`
    // (`LayerIntf.cpp:108-110`, `LayerBitmapIntf.cpp:122-160`,
    // `LayerIntf.cpp:4272-4285`).
    two_layers_with_children(&env);
    // Caching is enabled on both layers for the transition.
    assert_eq!(named_layer(&env, "dest").cache_enabled_count, 1);
    assert_eq!(named_layer(&env, "src").cache_enabled_count, 1);

    env.advance(50.0);
    let cache = cache_bitmap_of(&env, "dest").expect("destination cache");
    assert!(cache_bitmap_of(&env, "src").is_some(), "source cache");
    // 50 ms into a 100 ms cross-fade truncates to phase 127.
    assert_2x2(&env, cache, blend_rgb(RED, BLUE, 127), "destination cache");
    // The children keep their own pixels: only the cache holds the blend.
    assert_eq!(env.eval_int("bc.getMainPixel(0, 0)"), RED);
    assert_eq!(env.eval_int("ac.getMainPixel(0, 0)"), BLUE);

    // The teardown drops both caches (`DecCacheEnabledCount`,
    // LayerIntf.cpp:7961-7964).
    env.run("b.stopTransition();");
    assert!(
        cache_bitmap_of(&env, "dest").is_none(),
        "destination cache freed"
    );
    assert!(cache_bitmap_of(&env, "src").is_none(), "source cache freed");
    assert_eq!(named_layer(&env, "dest").cache_enabled_count, 0);
    assert_eq!(named_layer(&env, "src").cache_enabled_count, 0);
}

/// A whole-subtree transition composites each endpoint once and reuses the
/// content while it is unchanged: `Complete` returns the existing
/// `CacheBitmap` while `CacheRecalcRegion` is empty, and the transition's
/// `tempupdate` update never appends to it.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7734-7737
/// reference/cpp/core/visual/LayerIntf.cpp:6305-6335
#[test]
fn with_children_reuses_the_subtree_content_between_frames() {
    let _lock = vm_lock();
    let mut env = Env::new();
    two_layers_with_children(&env);
    env.advance(25.0);
    let dest_content = named_layer(&env, "dest")
        .cache_content
        .expect("destination content");
    let src_content = named_layer(&env, "src")
        .cache_content
        .expect("source content");
    // Clear the render-dirty flags: a reused cache is not re-marked dirty.
    env._scene.set_bitmap_clean(dest_content);
    env._scene.set_bitmap_clean(src_content);
    env.advance(25.0);
    assert!(
        !env._scene.bitmap(dest_content).unwrap().dirty,
        "the destination subtree content is reused, not recomposited"
    );
    assert!(
        !env._scene.bitmap(src_content).unwrap().dirty,
        "the source subtree content is reused, not recomposited"
    );
    // The blend still advances: 50 ms into a 100 ms cross-fade truncates to
    // phase 127.
    let display = named_layer(&env, "dest")
        .cache_bitmap
        .expect("destination frame");
    assert_eq!(scene_pixel(&env, display, 0, 0), blend_rgb(RED, BLUE, 127));
}

/// The composited-frame host records a new frame every tick, but the frame's
/// sources are the endpoint subtrees: their content is unchanged, so it is
/// reused rather than recomposited. `Complete` returns the existing
/// `CacheBitmap` while `CacheRecalcRegion` is empty
/// (`LayerIntf.cpp:7734-7737`), and a frame change is not a source change
/// (`LayerIntf.cpp:6305-6335`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:7734-7737
/// reference/cpp/core/visual/LayerIntf.cpp:6305-6335
#[test]
fn a_composited_whole_subtree_transition_reuses_its_content() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env._scene.composite_transition_frames = true;
    two_layers_with_children(&env);
    env.advance(25.0);
    let dest_content = named_layer(&env, "dest")
        .cache_content
        .expect("destination content");
    let src_content = named_layer(&env, "src")
        .cache_content
        .expect("source content");
    env._scene.set_bitmap_clean(dest_content);
    env._scene.set_bitmap_clean(src_content);
    env.advance(25.0);
    assert!(
        named_layer(&env, "dest").transition_frame.is_some(),
        "the composited frame advances"
    );
    assert!(
        !env._scene.bitmap(dest_content).unwrap().dirty,
        "the destination subtree content is reused, not recomposited"
    );
    assert!(
        !env._scene.bitmap(src_content).unwrap().dirty,
        "the source subtree content is reused, not recomposited"
    );

    // The teardown returns the layer to its own subtree; a cache kept from
    // before or during the transition is stale and must be recomposited
    // (LayerIntf.cpp:7961-7991).
    env.run("b.stopTransition();");
    assert!(
        named_layer(&env, "dest").cache_recalc,
        "the ended transition resumes from a fresh composite"
    );
}

/// A mid-transition change to a drawn child invalidates the cached content:
/// the next frame recomposites it before blending.
///
/// reference/cpp/core/visual/LayerIntf.cpp:6305-6335
#[test]
fn with_children_rebuilds_when_a_drawn_child_changes() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers_with_children(&env);
    env.advance(25.0);
    let dest_content = named_layer(&env, "dest")
        .cache_content
        .expect("destination content");
    assert_eq!(scene_pixel(&env, dest_content, 0, 0), RED);
    env.run("bc.setMainPixel(0, 0, 0x0000ff00);");
    env.advance(25.0);
    assert_eq!(
        scene_pixel(&env, dest_content, 0, 0),
        0x00ff00,
        "the changed child is recomposited"
    );
    let display = named_layer(&env, "dest")
        .cache_bitmap
        .expect("destination frame");
    assert_eq!(
        scene_pixel(&env, display, 0, 0),
        blend_rgb(0x00ff00, BLUE, 127)
    );
}

/// A destination that is trivial while its control layer is hidden and
/// becomes a real subtree when the control appears: `Complete` returns the
/// MainImage while no child is visible and the rebuilt `CacheBitmap` once one
/// is (`LayerIntf.cpp:7725-7737`), and the blend always goes into the
/// destination's own frame (`tTransDrawable::DrawCompleted`, `:8206-8249`).
/// Showing the control mid-transition must neither drop it nor leave the
/// half-blended frame behind: the frame is recomposited with the control, and
/// the teardown hands the projection the live subtree with every control
/// present and the original main image intact.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7673-7737
/// reference/cpp/core/visual/LayerIntf.cpp:8154-8249
#[test]
fn a_destination_that_shows_a_hidden_control_mid_transition_ends_clean() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setSize(4, 4); a.setImageSize(4, 4); a.name = 'src'; \
         a.fillRect(0, 0, 4, 4, 0xff0000ff); \
         var b = new Layer(w, null); b.setSize(4, 4); b.setImageSize(4, 4); b.visible = true; b.name = 'dest'; \
         b.fillRect(0, 0, 4, 4, 0xffff0000); \
         var bc = new Layer(w, b); bc.setSize(2, 2); bc.setImageSize(2, 2); bc.name = 'ctl'; \
         bc.fillRect(0, 0, 2, 2, 0xff00ff00); bc.visible = false; \
         b.beginTransition('crossfade', true, a, %[time: 100]);",
    );
    // 25 ms into a 100 ms cross-fade truncates to phase 63. No child is
    // visible, so the destination content is its MainImage and the frame is
    // the pure blend.
    env.advance(25.0);
    assert_eq!(frame_pixel(&env, "dest", 1, 1), blend_rgb(RED, BLUE, 63));
    assert_eq!(
        env.eval_int("b.getMainPixel(1, 1)"),
        RED,
        "the main image is never the blend target"
    );

    // The control appears: the next frame recomposites the subtree, so the
    // control is part of the blend rather than missing from it.
    env.run("bc.visible = true;");
    env.advance(25.0);
    assert_eq!(
        frame_pixel(&env, "dest", 0, 0),
        blend_rgb(0x00ff00, BLUE, 127),
        "the control recomposited under the blend"
    );
    assert_eq!(
        frame_pixel(&env, "dest", 3, 3),
        blend_rgb(RED, BLUE, 127),
        "the rest of the destination content"
    );
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);

    // Completion releases the frame: the projection draws the live subtree,
    // which still carries the control and the original image — no ghost of
    // the last blended frame.
    env.advance(50.0);
    assert!(
        cache_bitmap_of(&env, "dest").is_none(),
        "no frame left behind"
    );
    assert_eq!(named_layer(&env, "dest").cache_enabled_count, 0);
    assert_eq!(
        env.eval_int("bc.getMainPixel(0, 0)"),
        0x00ff00,
        "the control is still there"
    );
    assert_eq!(env.eval_int("bc.visible"), 1, "and still visible");
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);
}

/// The exact game path (`system/advscreen.tjs:216-217,3353,3719`: the source
/// is the whole-screen container, whose content lives in child layers and
/// which has no main image of its own). `withchildren = true` must accept it
/// — the image requirement applies only without children
/// (`LayerIntf.cpp:7860-7868`) — and blend the composited child subtrees
/// through the cache (`LayerIntf.cpp:7869-7876`). Before the fix this call
/// raised `the transition source has no main image`.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7860-7876
#[test]
fn container_source_without_image_transitions_its_children() {
    let _lock = vm_lock();
    let env = Env::new();
    // `container` is the transition source: sized, no `setImageSize`, one
    // visible child (the ADV object). `dest` mirrors it with the other colour.
    env.run(
        "var w = new Window(); \
         var container = new Layer(w, null); container.setSize(2, 2); container.name = 'src'; container.hasImage = false; \
         var sc = new Layer(w, container); sc.setSize(2, 2); sc.setImageSize(2, 2); \
         sc.fillRect(0, 0, 2, 2, 0xffff0000); sc.visible = true; \
         var dest = new Layer(w, null); dest.setSize(2, 2); dest.visible = true; dest.name = 'dest'; dest.hasImage = false; \
         var dc = new Layer(w, dest); dc.setSize(2, 2); dc.setImageSize(2, 2); \
         dc.fillRect(0, 0, 2, 2, 0xff0000ff); dc.visible = true; \
         dest.beginTransition('crossfade', true, container, %[time: 100]);",
    );
    env.advance(50.0);
    let cache = cache_bitmap_of(&env, "dest").expect("destination cache");
    assert_2x2(&env, cache, blend_rgb(BLUE, RED, 127), "destination cache");
}

/// The `ttExchange` teardown runs `Exchange` (`LayerIntf.cpp:754-979`),
/// which re-parents the two layers: when they are not siblings each moves to
/// the other's parent (`this->Join(target_parent)`, `target->Join(this_parent)`,
/// `:880-883`), so the destination ends up under the source's old parent.
///
/// reference/cpp/core/visual/LayerIntf.cpp:754-979
#[test]
fn exchange_reparents_non_sibling_layers() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var p1 = new Layer(w, null); p1.setSize(2, 2); \
         var p2 = new Layer(w, null); p2.setSize(2, 2); p2.visible = true; \
         var a = new Layer(w, p1); a.setSize(2, 2); a.name = 'src'; \
         var ac = new Layer(w, a); ac.setSize(2, 2); ac.setImageSize(2, 2); ac.visible = true; \
         var b = new Layer(w, p2); b.setSize(2, 2); b.visible = true; b.name = 'dest'; \
         var bc = new Layer(w, b); bc.setSize(2, 2); bc.setImageSize(2, 2); bc.visible = true; \
         b.beginTransition('crossfade', true, a, %[time: 100]); \
         b.stopTransition();",
    );
    assert_eq!(env.eval_int("b.parent === p1"), 1, "b moved to a's parent");
    assert_eq!(env.eval_int("a.parent === p2"), 1, "a moved to b's parent");
}

// ---------------------------------------------------------------------------
// Endpoint short-circuits
// ---------------------------------------------------------------------------

/// `tTVPCrossFadeTransHandler::Process` copies source 1 into the destination
/// **frame** at `Phase == 0` (`TransIntf.cpp:578-594`) and source 2 verbatim
/// at `Phase == PhaseMax` (`:596-601`). `Complete` returns a trivial layer's
/// MainImage as an endpoint of the blend, never as its target
/// (`LayerIntf.cpp:7725-7732`), and the blend is drawn into the parent drawable
/// or a temporary (`tTransDrawable::DrawCompleted`, `:8206-8249`) — so the
/// MainImage keeps its original pixels through the whole transition.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7673-7732
/// reference/cpp/core/visual/LayerIntf.cpp:8154-8249
/// reference/cpp/core/visual/TransIntf.cpp:578-601
#[test]
fn a_trivial_destination_keeps_its_main_image() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]);");

    // Phase 0: the frame holds source 1 verbatim; the MainImage is untouched.
    env.advance(0.0);
    assert_eq!(frame_pixel(&env, "dest", 0, 0), RED);
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);

    // 50 ms into a 100 ms cross-fade truncates to phase 127: the frame holds
    // the blend, every pixel read of the MainImage still sees the original
    // image.
    env.advance(50.0);
    let expected = blend_rgb(RED, BLUE, 127);
    assert_2x2(&env, transition_frame(&env, "dest"), expected, "frame");
    for (x, y) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
        assert_eq!(
            env.eval_int(&format!("b.getMainPixel({x}, {y})")),
            RED,
            "the main image at ({x},{y}) is not the blend"
        );
    }

    // The completed transition releases its frame with the caches
    // (`DecCacheEnabledCount`, `LayerIntf.cpp:7961-7964`), so the projection
    // draws the live subtree — whose image is still the destination's own.
    env.advance(50.0);
    assert_eq!(env.eval_int("hits"), 1, "the transition completed");
    assert!(cache_bitmap_of(&env, "dest").is_none(), "frame released");
    assert_eq!(named_layer(&env, "dest").cache_enabled_count, 0);
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);
    assert_eq!(env.eval_int("b.getMainPixel(1, 1)"), RED);
}

// ---------------------------------------------------------------------------
// scroll: the numeric from/stay enums
// ---------------------------------------------------------------------------

/// `tTVPScrollTransHandlerProvider::GetTransitionObject` reads `from` and
/// `stay` as **numbers** cast straight to `tTVPScrollTransFrom` /
/// `tTVPScrollTransStay` (`TransIntf.cpp:1094-1108`), defaulting to
/// `sttLeft` / `ststNoStay` (`TransIntf.h:118-119`). Mid-flight on a 4x4
/// image (`time: 100`, 50 ms in → phase 2): the default slides the blue
/// source in from the left (`from: 0`), `from: 2` (`sttRight`) enters from
/// the right, and `stay: 2` (`ststStaySrc`) slides the destination's own
/// image right while it keeps priority, so the new source shows on the
/// left. An out-of-range number has no reference meaning and is a typed
/// error here.
///
/// reference/cpp/core/visual/TransIntf.cpp:1090-1124,1151-1218
/// reference/cpp/core/visual/TransIntf.h:118-119
#[test]
fn scroll_reads_the_reference_numeric_enums() {
    let _lock = vm_lock();
    let env = Env::new();
    // Default (`sttLeft`, `ststNoStay`): blue occupies the left half.
    two_layers(&env, 4);
    env.run("b.beginTransition('scroll', true, a, %[time: 100]);");
    env.advance(50.0);
    assert_eq!(frame_pixel(&env, "dest", 0, 0), BLUE);
    assert_eq!(frame_pixel(&env, "dest", 3, 0), RED);

    // `from: 2` (sttRight): mirrored — red on the left, blue on the right.
    two_layers(&env, 4);
    env.run("b.beginTransition('scroll', true, a, %[time: 100, from: 2]);");
    env.advance(50.0);
    assert_eq!(frame_pixel(&env, "dest", 0, 0), RED);
    assert_eq!(frame_pixel(&env, "dest", 3, 0), BLUE);

    // `stay: 2` (ststStaySrc): source 1 (the destination's own red image)
    // slides right and keeps priority, so the left half shows the blue
    // source and the right half stays red.
    // reference/cpp/core/visual/TransIntf.cpp:1151-1159,1202-1211
    two_layers(&env, 4);
    env.run("b.beginTransition('scroll', true, a, %[time: 100, stay: 2]);");
    env.advance(50.0);
    assert_eq!(frame_pixel(&env, "dest", 0, 0), BLUE);
    assert_eq!(frame_pixel(&env, "dest", 3, 0), RED);

    // The port's string aliases are a compat extension; they must agree
    // with the numeric form they stand for.
    two_layers(&env, 4);
    env.run("b.beginTransition('scroll', true, a, %[time: 100, from: 'right']);");
    env.advance(50.0);
    assert_eq!(frame_pixel(&env, "dest", 0, 0), RED);
    assert_eq!(frame_pixel(&env, "dest", 3, 0), BLUE);

    for (option, value) in [("from", 9), ("stay", 9)] {
        two_layers(&env, 4);
        let message = env.error(&format!(
            "b.beginTransition('scroll', true, a, %[time: 100, {option}: {value}]);"
        ));
        assert!(
            message.contains("invalid transition option"),
            "{option}: {value} should be a typed error, got: {message}"
        );
    }
}

// ---------------------------------------------------------------------------
// ripple: StartTransition's size-dependent validation
// ---------------------------------------------------------------------------

/// `tTVPRippleTransHandlerProvider::StartTransition` validates its options
/// while the script is inside `beginTransition`: the centre defaults to
/// `src1w >> 1` / `src1h >> 1` and must stay inside the image
/// (`ripple.cpp:1574,1593-1601`), `rwidth` must be 16/32/64/128
/// (`:1604-1610`), `roundness` positive (`:1612-1616`), and `maxdrift`
/// within `0..=127` (`:1621-1624`) **and** below both image dimensions
/// (`:1625-1627`) — so even the defaults do not run on a 4x4 image, whose
/// default `maxdrift` of 24 is not smaller than 4.
///
/// reference/cpp/plugins/ripple.cpp:1574-1627
#[test]
fn ripple_options_are_validated_during_begin() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 4);

    // Defaults on a 4x4 image: maxdrift 24 is not < 4.
    let message = env.error("b.beginTransition('ripple', true, a, %[time: 100]);");
    assert!(
        message.contains("invalid transition option maxdrift"),
        "unexpected error: {message}"
    );

    let cases = [
        ("centerx: 4", "centerx"),
        ("centerx: -1", "centerx"),
        ("rwidth: 17", "rwidth"),
        ("roundness: 0", "roundness"),
        ("maxdrift: 200", "maxdrift"),
    ];
    for (options, expected) in cases {
        two_layers(&env, 4);
        let message = env.error(&format!(
            "b.beginTransition('ripple', true, a, %[time: 100, maxdrift: 3, {options}]);"
        ));
        assert!(
            message.contains(&format!("invalid transition option {expected}")),
            "{options} should raise {expected}, got: {message}"
        );
    }

    // A valid size bound runs, and a one-sided centre defaults the other
    // side from the image (centery = 4 / 2), like the reference.
    two_layers(&env, 4);
    env.run("b.beginTransition('ripple', true, a, %[time: 100, maxdrift: 3]); b.stopTransition();");
    two_layers(&env, 4);
    env.run(
        "b.beginTransition('ripple', true, a, %[time: 100, maxdrift: 3, centery: 1]); \
         b.stopTransition();",
    );
}

// ---------------------------------------------------------------------------
// universal: rule image and vague
// ---------------------------------------------------------------------------

/// `tTVPUniversalTransHandlerProvider::StartTransition` requires the `rule`
/// option (`TVPSpecifyOption`, `TransIntf.cpp:812-817`) and loads it
/// through the image provider (`:819-821`), which decodes it as 8-bit
/// grayscale (`GraphicsLoaderIntf.cpp:1697`) and **tiles** it when smaller
/// than the source (`GraphicsLoaderIntf.cpp:1693-1696`). Mid-flight
/// (`time: 100`, 50 ms in, vague 64 → phase 159, `phase0 = 95`,
/// `tvpgl.cpp:4576-4594`): rule grey 0 → opacity 255 (source 2), grey 128
/// → `255 - (128-95)*255/64 = 124`, and the 2x2 rule repeats over the 4x4
/// source.
///
/// reference/cpp/core/visual/TransIntf.cpp:806-821
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1689-1697
/// reference/cpp/core/visual/tvpgl.cpp:4576-4594
#[test]
fn universal_rule_image_is_loaded_grayscale_and_tiled() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 4);
    env.run("b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img']);");
    env.advance(50.0);
    // Rule (0,0) = grey 0 → opacity 255: the blue source at full blend.
    assert_eq!(frame_pixel(&env, "dest", 0, 0), blend_rgb(RED, BLUE, 255));
    // Rule (1,0) = grey 128 → opacity 124.
    assert_eq!(frame_pixel(&env, "dest", 1, 0), blend_rgb(RED, BLUE, 124));
    // Tiling: the 2x2 rule repeats over the 4x4 source, in both axes.
    assert_eq!(
        frame_pixel(&env, "dest", 2, 0),
        frame_pixel(&env, "dest", 0, 0)
    );
    assert_eq!(
        frame_pixel(&env, "dest", 1, 2),
        frame_pixel(&env, "dest", 1, 0)
    );
    assert_eq!(
        frame_pixel(&env, "dest", 3, 3),
        frame_pixel(&env, "dest", 1, 1)
    );
    // A mid-grey rule value is not an endpoint of either source.
    assert_ne!(frame_pixel(&env, "dest", 1, 0), RED);
    assert_ne!(frame_pixel(&env, "dest", 1, 0), BLUE);

    // A missing `rule` option is `TVPSpecifyOption` (a typed error).
    two_layers(&env, 4);
    let message = env.error("b.beginTransition('universal', true, a, %[time: 100]);");
    assert!(
        message.contains("requires a `rule` image"),
        "unexpected error: {message}"
    );

    // An unloadable rule graphic is `TVPCannotLoadRuleGraphic` (a typed
    // error).
    two_layers(&env, 4);
    let message =
        env.error("b.beginTransition('universal', true, a, %[time: 100, rule: 'missing.img']);");
    assert!(
        message.contains("storage not found"),
        "unexpected error: {message}"
    );
}

/// The rule graphic is loaded through the scene's decoded-image cache — the
/// reference's `TVPLoadGraphic` cache path
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1878-1960`), which the
/// universal provider reaches through `LoadImage`
/// (`reference/cpp/core/visual/TransIntf.cpp:819-821`). A repeated
/// `beginTransition` with the same rule decodes it once while the cache is
/// enabled, and the frame it produces is the same either way.
///
/// The reference's cache is off by default (`TVPGraphicCacheEnabled = false`,
/// `GraphicsLoaderIntf.cpp:1433`; `TVPSetGraphicCacheLimit(0)`,
/// `:2426-2428`), so the default path decodes the rule every time; a script
/// enables it with `System.graphicCacheLimit`
/// (`reference/cpp/core/base/SystemIntf.cpp:359-372`).
///
/// reference/cpp/core/visual/TransIntf.cpp:819-821
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1433,1878-1960
#[test]
fn the_rule_graphic_goes_through_the_decoded_image_cache() {
    let _lock = vm_lock();
    let env = Env::new();

    // The cache is off, so the rule is decoded for every transition.
    two_layers(&env, 2);
    env.run("b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img']);");
    env.advance(50.0);
    assert_eq!(env.decodes(), 1, "the first transition decodes the rule");
    let uncached = frame_pixel(&env, "dest", 0, 0);
    // Rule (0,0) is grey 0, so the source-2 opacity is 255.
    assert_eq!(uncached, blend_rgb(RED, BLUE, 255));

    // The cache is on, so the first transition fills it and the next one hits
    // it.
    env.run("System.graphicCacheLimit = 1048576;");
    two_layers(&env, 2);
    env.run("b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img']);");
    env.advance(50.0);
    assert_eq!(
        env.decodes(),
        2,
        "the first cached transition decodes the rule"
    );
    let cached = frame_pixel(&env, "dest", 0, 0);
    assert_eq!(
        cached, uncached,
        "the cached rule gives the same frame as the uncached one"
    );

    two_layers(&env, 2);
    env.run("b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img']);");
    env.advance(50.0);
    assert_eq!(env.decodes(), 2, "the cached rule is not decoded again");
    assert_eq!(
        frame_pixel(&env, "dest", 0, 0),
        uncached,
        "the cached rule gives the same frame as the uncached one"
    );
}

/// `vague` defaults to 64 (`TransIntf.cpp:806-810`) and drives the soft
/// edge of `TVPInitUnivTransBlendTable_c` (`tvpgl.cpp:4576-4594`). With the
/// 0/128 checkerboard rule mid-flight: the default equals an explicit
/// `vague: 64`; `vague: 0` is the hard threshold (grey 128 ≥ phase 127 →
/// source 1 verbatim); `vague: 255` yields opacity
/// `255 - (128 - (-96))*255/255 = 31`.
///
/// reference/cpp/core/visual/TransIntf.cpp:806-810
/// reference/cpp/core/visual/tvpgl.cpp:4576-4594
#[test]
fn universal_vague_defaults_to_64_and_is_read() {
    let _lock = vm_lock();
    let env = Env::new();
    let universal = |options: &str| -> String {
        two_layers(&env, 2);
        env.run(&format!(
            "b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img'{options}]);"
        ));
        env.advance(50.0);
        frame_grid(&env, "dest", 2)
    };

    let default_grid = universal("");
    assert_eq!(
        default_grid,
        universal(", vague: 64"),
        "vague defaults to 64"
    );
    // `AsInteger` accepts numeric strings (reference/cpp/core/tjs2/tjsVariant.h:862-877).
    assert_eq!(
        default_grid,
        universal(", vague: '64'"),
        "a numeric string uses the same integer coercion"
    );

    // A non-numeric value cannot be converted, so it is a typed error rather
    // than silently falling back to the default.
    two_layers(&env, 2);
    let message = env.error(
        "b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img', vague: %[]]);",
    );
    assert!(
        message.contains("invalid transition option vague"),
        "unexpected error: {message}"
    );

    let hard_grid = universal(", vague: 0");
    assert_ne!(hard_grid, default_grid, "vague: 0 is a hard threshold");
    // In the hard-threshold run the grey-128 pixel is source 1 verbatim
    // (128 >= phase 127 → opacity 0).
    two_layers(&env, 2);
    env.run("b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img', vague: 0]);");
    env.advance(50.0);
    assert_eq!(frame_pixel(&env, "dest", 1, 0), RED);

    assert_ne!(
        universal(", vague: 255"),
        default_grid,
        "vague: 255 differs"
    );
}

/// The port's `mask` name is the hard-threshold form of `universal`
/// (`vague: 0` — the reference has no separate `mask` provider;
/// `TransIntf.cpp:1237-1255` registers only crossfade/universal/scroll), so
/// both spellings must render identical frames.
///
/// reference/cpp/core/visual/TransIntf.cpp:1237-1255
/// reference/cpp/core/visual/tvpgl.cpp:4576-4594
#[test]
fn mask_alias_matches_universal_vague_zero() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 4);
    env.run("b.beginTransition('mask', true, a, %[time: 100, rule: 'rule.img']);");
    env.advance(50.0);
    let mask_grid = frame_grid(&env, "dest", 4);

    two_layers(&env, 4);
    env.run("b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img', vague: 0]);");
    env.advance(50.0);
    assert_eq!(mask_grid, frame_grid(&env, "dest", 4));
}

// ---------------------------------------------------------------------------
// onTransitionCompleted dispatch and the teardown
// ---------------------------------------------------------------------------

/// `InternalStopTransition` fires `onTransitionCompleted` as a
/// **synchronous event at the destination's owner** (`TVPPostEvent(Owner,
/// …, TVP_EPT_IMMEDIATE, 2, param)`), so the member is resolved on the
/// object and a script subclass override wins; the arguments are the
/// destination and source layer objects.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7997-8015
#[test]
fn on_transition_completed_is_dispatched_through_the_object() {
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
         var a = new SubLayer(w, null); a.setSize(2, 2); a.setImageSize(2, 2); \
         var b = new SubLayer(w, null); b.setSize(2, 2); b.setImageSize(2, 2); b.visible = true; \
         b.beginTransition('crossfade', true, a, %[time: 100]);",
    );
    env.advance(100.0);
    assert_eq!(env.eval_int("hits"), 1, "the subclass override ran once");
    assert_eq!(env.eval_int("sawDest"), 1, "argument 0 is the destination");
    assert_eq!(env.eval_int("sawSrc"), 1, "argument 1 is the source");
}

/// A manual `stopTransition()` runs the same `InternalStopTransition` as a
/// natural completion — including the synchronous `onTransitionCompleted`
/// event with `(dest, src)`.
///
/// reference/cpp/core/visual/LayerIntf.cpp:8033-8036, 7950-8015
#[test]
fn manual_stop_fires_on_transition_completed() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run("b.beginTransition('crossfade', true, a, %[time: 1000]);");
    env.advance(50.0);
    env.run("b.stopTransition();");
    assert_eq!(env.eval_int("hits"), 1, "the manual stop fired the event");
    assert_eq!(env.eval_int("sawDest"), 1, "argument 0 is the destination");
    assert_eq!(env.eval_int("sawSrc"), 1, "argument 1 is the source");
}

/// `InternalStopTransition` acts only `if(InTransition)`: stopping a layer
/// without a transition is a silent no-op (no event, no error).
///
/// reference/cpp/core/visual/LayerIntf.cpp:7950-7951
#[test]
fn stop_without_an_active_transition_is_a_noop() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run("b.stopTransition(); b.stopTransition();");
    assert_eq!(env.eval_int("hits"), 0, "no event without a transition");
}

/// Stopping midway releases the transition frame with the caches
/// (`DecCacheEnabledCount`, `LayerIntf.cpp:7961-7964`) and runs the
/// `ttExchange` exchange: the two layers trade position (`SetPosition`) and
/// visibility. Neither MainImage was ever the blend target (`:7673-7726`,
/// `:8206-8249`), so the projection draws the live subtrees of the exchanged
/// layers instead of a frozen frame.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7961-7991
/// reference/cpp/core/visual/LayerIntf.cpp:7673-7732
#[test]
fn stopping_midway_releases_the_frame_and_exchanges_layers() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers_with(&env, 2, "var base = new Layer(w, null); ");
    counter(&env);
    // The window's primary layer is the throwaway layer — the reference
    // refuses to move the primary layer
    // (`reference/cpp/core/visual/LayerIntf.cpp:2163-2179`) — and `a` is
    // hidden so the `ttExchange` visibility swap below is observable
    // (`reference/cpp/core/visual/LayerIntf.cpp:7974-7986`).
    env.run(
        "a.left = 10; a.top = 20; a.visible = false; \
         b.beginTransition('crossfade', true, a, %[time: 1000]);",
    );
    env.advance(50.0);
    let frame = frame_pixel(&env, "dest", 0, 0);
    assert_ne!(frame, RED, "mid-flight is not source 1");
    assert_ne!(frame, BLUE, "mid-flight is not source 2");
    assert_eq!(
        env.eval_int("b.getMainPixel(0, 0)"),
        RED,
        "the main image is never the blend target"
    );

    env.run("b.stopTransition();");
    assert_eq!(
        env.eval_int("b.getMainPixel(0, 0)"),
        RED,
        "the stopped transition leaves the main image intact"
    );
    assert!(
        cache_bitmap_of(&env, "dest").is_none(),
        "the frame is released with the caches"
    );
    assert_eq!(named_layer(&env, "dest").cache_enabled_count, 0);
    assert_eq!(env.eval_int("b.left"), 10, "b took a's position");
    assert_eq!(env.eval_int("b.top"), 20);
    assert_eq!(env.eval_int("a.left"), 0, "a took b's position");
    assert_eq!(env.eval_int("a.top"), 0);
    assert_eq!(env.eval_int("b.visible"), 0, "b took a's visibility");
    assert_eq!(env.eval_int("a.visible"), 1, "a took b's visibility");
    assert_eq!(env.eval_int("hits"), 1, "and still fired the event");
}

/// A naturally completed transition ends through the same teardown:
/// the destination's MainImage keeps the destination's own image (`Complete`
/// returns it as a blend source only, `LayerIntf.cpp:7673-7726`), the frame is
/// released with the caches (`:7961-7964`) and the layers exchange
/// position/visibility (`:7969-7991`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:7673-7732
/// reference/cpp/core/visual/LayerIntf.cpp:7961-7991
#[test]
fn completion_exchanges_layers_and_leaves_the_main_image() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers_with(&env, 2, "var base = new Layer(w, null); ");
    counter(&env);
    // The window's primary layer is the throwaway layer — the reference
    // refuses to move the primary layer
    // (`reference/cpp/core/visual/LayerIntf.cpp:2163-2179`) — and `a` is
    // hidden so the completed transition's visibility swap is observable
    // (`reference/cpp/core/visual/LayerIntf.cpp:7974-7986`).
    env.run(
        "a.left = 10; a.top = 20; a.visible = false; \
         b.beginTransition('crossfade', true, a, %[time: 100]);",
    );
    env.advance(100.0);
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);
    assert_eq!(env.eval_int("b.getMainPixel(1, 1)"), RED);
    assert!(cache_bitmap_of(&env, "dest").is_none(), "frame released");
    assert_eq!(env.eval_int("b.left"), 10, "b took a's position");
    assert_eq!(env.eval_int("a.left"), 0, "a took b's position");
    assert_eq!(env.eval_int("b.visible"), 0, "b took a's visibility");
    assert_eq!(env.eval_int("a.visible"), 1, "a took b's visibility");
    assert_eq!(env.eval_int("hits"), 1);
    assert_eq!(env.eval_int("sawDest"), 1);
    assert_eq!(env.eval_int("sawSrc"), 1);
}

/// `InvokeTransition` ends a transition whose destination node is no longer
/// visible — `if(!GetNodeVisible()) StopTransitionByHandler();` — at that
/// frame, event included; it does not keep running until the time is up.
///
/// reference/cpp/core/visual/LayerIntf.cpp:8047-8061
#[test]
fn an_invisible_destination_stops_at_the_first_frame() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run(
        "b.visible = false; \
         b.beginTransition('crossfade', true, a, %[time: 100]);",
    );
    env.advance(10.0);
    assert_eq!(
        env.eval_int("hits"),
        1,
        "an invisible destination stops immediately"
    );
    env.advance(90.0);
    assert_eq!(env.eval_int("hits"), 1, "and does not fire again");
}

/// Invalidating the transition source stops the destination's transition
/// (`Invalidate` calls `TransDest->StopTransition()`), but with
/// `Shutdown` already set — so `transsrcalive` is false and
/// `onTransitionCompleted` is **not** fired.
///
/// reference/cpp/core/visual/LayerIntf.cpp:517-521, 7997-8004
#[test]
fn an_invalidated_source_suppresses_the_completion_event() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]);");
    env.run("invalidate a;");
    env.advance(100.0);
    assert_eq!(
        env.eval_int("hits"),
        0,
        "a dead source suppresses the event entirely"
    );
    env.advance(100.0);
    assert_eq!(env.eval_int("hits"), 0, "the transition is gone for good");
}

// ---------------------------------------------------------------------------
// extrans option defaults and forwarding
// ---------------------------------------------------------------------------

/// Begin `name` with `{time: 100}` plus `options` on the current `a`/`b`
/// layers, run 50 ms in (mid flight) and return the destination's pixel grid.
fn extrans_frame_grid(env: &Env, name: &str, options: &str, size: i64) -> String {
    env.run(&format!(
        "b.beginTransition('{name}', true, a, %[time: 100{options}]);"
    ));
    env.advance(50.0);
    frame_grid(env, "dest", size)
}

/// Begin `name` with `{time: 100}` plus `options`, run 50 ms in (mid
/// flight) and return the destination's pixel grid.
fn extrans_grid(env: &Env, name: &str, options: &str, size: i64) -> String {
    two_layers(env, size);
    extrans_frame_grid(env, name, options, size)
}

/// Like [`extrans_grid`], but with two **spatially varying** layers, so a
/// handler that samples one colour per region is sensitive to its block
/// size.
///
/// `mosaic` paints every block with a single colour sampled at the block
/// centre (`FILL_ONE`, `reference/cpp/plugins/mosaic.cpp:288-305`), so the
/// flat fills [`two_layers`] installs render the same colour for every
/// `maxsize`; the gradient below makes a different block size sample
/// different pixels.
fn extrans_patterned_grid(env: &Env, name: &str, options: &str, size: i64) -> String {
    env.run(&format!(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setSize({size}, {size}); a.setImageSize({size}, {size}); a.name = 'src'; \
         var b = new Layer(w, null); b.setSize({size}, {size}); b.setImageSize({size}, {size}); b.visible = true; b.name = 'dest'; \
         for (var y = 0; y < {size}; y++) for (var x = 0; x < {size}; x++) {{ \
             b.setMainPixel(x, y, ((x * 16) << 16) | ((y * 16) << 8) | ((x + y) * 8)); \
             a.setMainPixel(x, y, ((({size} - 1 - x) * 16) << 16) | \
                                ((({size} - 1 - y) * 16) << 8) | ((x + y) * 8)); \
         }}"
    ));
    extrans_frame_grid(env, name, options, size)
}

/// Each provider's implicit options must equal the reference's declared
/// defaults, and an unknown option member must be ignored (the providers
/// read only their named members):
/// `wave`: maxh 50, maxomega 0.2, bgcolor1/2 0, wavetype 0
/// (`wave.cpp:327-331,337-350`); `mosaic`: maxsize 30 (`mosaic.cpp:436-446`);
/// `turn`: bgcolor 0 (`turn.cpp:555-564`); `rotatezoom`: factor 1, accel 0,
/// twist 2, twistaccel -2, centre = image centre
/// (`rotatetrans.cpp:183-210`); `rotatevanish`: accel 2, twist 2,
/// twistaccel 2 (`rotatetrans.cpp:284-305`); `rotateswap`: bgcolor 0,
/// twist 1 (`rotatetrans.cpp:453-466`); `ripple`: centre = `src1 >> 1`,
/// rwidth 128, roundness 1, speed 6, maxdrift 24 (`ripple.cpp:1574-1581`).
///
/// reference/cpp/plugins/wave.cpp:327-350
/// reference/cpp/plugins/mosaic.cpp:436-446
/// reference/cpp/plugins/turn.cpp:555-564
/// reference/cpp/plugins/rotatetrans.cpp:183-210,284-305,453-466
/// reference/cpp/plugins/ripple.cpp:1574-1581
#[test]
fn extrans_default_options_match_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    let cases: &[(&str, &str, i64)] = &[
        (
            "wave",
            ", maxh: 50, maxomega: 0.2, bgcolor1: 0, bgcolor2: 0, wavetype: 0",
            16,
        ),
        ("mosaic", ", maxsize: 30", 16),
        ("turn", ", bgcolor: 0", 16),
        (
            "rotatezoom",
            ", factor: 1, accel: 0, twist: 2, twistaccel: -2",
            16,
        ),
        ("rotatevanish", ", accel: 2, twist: 2, twistaccel: 2", 16),
        ("rotateswap", ", bgcolor: 0, twist: 1", 16),
        // The ripple defaults must fit under the image size
        // (`ripple.cpp:1625-1627`), so this one runs on a 32x32 layer.
        (
            "ripple",
            ", rwidth: 128, roundness: 1, speed: 6, maxdrift: 24",
            32,
        ),
    ];
    for (name, explicit, size) in cases {
        let implicit_grid = extrans_grid(&env, name, "", *size);
        // The explicit run also carries an unknown member: providers ignore
        // options they do not know (`wave.cpp:337-350` reads only the
        // named ones), so it must not change — or break — the result.
        let explicit_grid =
            extrans_grid(&env, name, &format!("{explicit}, nosuchoption: 5"), *size);
        assert_eq!(implicit_grid, explicit_grid, "{name} defaults");
    }
}

/// A supplied option must reach the handler: `maxh` (the wave amplitude,
/// `wave.cpp:337-339`) and `maxsize` (the mosaic block size,
/// `mosaic.cpp:444-446`) change the mid-flight pixels.
///
/// reference/cpp/plugins/wave.cpp:337-339
/// reference/cpp/plugins/mosaic.cpp:444-446
#[test]
fn extrans_options_reach_the_handlers() {
    let _lock = vm_lock();
    let env = Env::new();
    let wave_default = extrans_grid(&env, "wave", "", 16);
    assert_ne!(
        wave_default,
        extrans_grid(&env, "wave", ", maxh: 5", 16),
        "maxh must reach the wave handler"
    );
    // `AsInteger` accepts numeric strings (reference/cpp/core/tjs2/tjsVariant.h:862-877).
    assert_eq!(
        extrans_grid(&env, "wave", ", maxh: 5", 16),
        extrans_grid(&env, "wave", ", maxh: '5'", 16),
        "a numeric string maxh uses the same integer coercion"
    );
    assert_ne!(
        extrans_patterned_grid(&env, "mosaic", ", maxsize: 4", 16),
        extrans_patterned_grid(&env, "mosaic", ", maxsize: 12", 16),
        "maxsize must reach the mosaic handler"
    );

    // A non-numeric value is rejected instead of silently using the default.
    two_layers(&env, 16);
    let message = env.error("b.beginTransition('wave', true, a, %[time: 100, maxh: %[]]);");
    assert!(
        message.contains("invalid transition option maxh"),
        "unexpected error: {message}"
    );
}

/// The reference's `rotateswap` provider reads only `time`, `bgcolor` and
/// `twist` — it has no `centerx`/`centery` option, so supplying a centre
/// must not move the pivot: both runs render identical frames.
///
/// reference/cpp/plugins/rotatetrans.cpp:453-466
#[test]
fn rotateswap_ignores_centre_options() {
    let _lock = vm_lock();
    let env = Env::new();
    let plain = extrans_grid(&env, "rotateswap", "", 16);
    let centred = extrans_grid(&env, "rotateswap", ", centerx: 0, centery: 0", 16);
    assert_eq!(plain, centred, "rotateswap has no centre option");
}

/// The bitmap ids with a pending texture upload.
fn dirty_bitmaps(env: &Env) -> Vec<u32> {
    let mut ids: Vec<u32> = env
        ._scene
        .bitmaps
        .iter()
        .filter(|bitmap| bitmap.dirty)
        .map(|bitmap| bitmap.id)
        .collect();
    ids.sort_unstable();
    ids
}

/// A host that composites transition frames records the frame the projection
/// draws — the two sources placed in the layer's frame
/// (`reference/cpp/core/visual/TransIntf.cpp:592-600`) — instead of blending a
/// display bitmap on the CPU every tick. Nothing is written into a bitmap, so a
/// running transition adds no upload.
#[test]
fn a_composited_transition_records_its_frame_and_writes_no_bitmap() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env._scene.composite_transition_frames = true;
    two_layers(&env, 2);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]);");
    // The engine uploads the main images the script wrote before the first
    // frame; nothing else is pending.
    for id in dirty_bitmaps(&env) {
        env._scene.set_bitmap_clean(id);
    }
    assert!(dirty_bitmaps(&env).is_empty());

    env.advance(25.0);
    let first = named_layer(&env, "dest")
        .transition_frame
        .expect("a composited frame");
    assert_eq!(first.size, (2, 2));
    assert!(first.covers_layer, "a whole-subtree frame covers the layer");
    assert_eq!(first.src1.alpha, 1.0);
    assert!(
        first.src2.alpha > 0.0 && first.src2.alpha < 1.0,
        "the fade source carries phase/256, got {}",
        first.src2.alpha
    );
    assert!(
        cache_bitmap_of(&env, "dest").is_none(),
        "the composited frame needs no display bitmap"
    );
    assert!(
        self_image_of(&env, "dest").is_none(),
        "the composited frame needs no self image"
    );
    let after_first = dirty_bitmaps(&env);

    env.advance(25.0);
    let second = named_layer(&env, "dest")
        .transition_frame
        .expect("a composited frame");
    assert!(
        second.src2.alpha > first.src2.alpha,
        "the frame advances: {} then {}",
        first.src2.alpha,
        second.src2.alpha
    );
    assert_eq!(
        after_first,
        dirty_bitmaps(&env),
        "a composited transition frame is not re-uploaded per tick"
    );
    assert!(
        after_first.is_empty(),
        "no bitmap is written while the frame runs: {after_first:?}"
    );

    env.run("b.stopTransition();");
    assert!(
        named_layer(&env, "dest").transition_frame.is_none(),
        "the teardown clears the frame"
    );
}

/// The headless CPU path blends the same frame into the destination's display
/// bitmap every tick, which is exactly the per-tick full-frame write the
/// composited path avoids.
#[test]
fn the_cpu_path_blends_the_display_bitmap_every_tick() {
    let _lock = vm_lock();
    let mut env = Env::new();
    two_layers(&env, 2);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]);");
    for id in dirty_bitmaps(&env) {
        env._scene.set_bitmap_clean(id);
    }

    env.advance(25.0);
    let frame = transition_frame(&env, "dest");
    assert_eq!(frame_pixel(&env, "dest", 0, 0), blend_rgb(RED, BLUE, 63));
    assert!(
        dirty_bitmaps(&env).contains(&frame),
        "the CPU path writes (and so uploads) the display bitmap"
    );
    assert!(
        named_layer(&env, "dest").transition_frame.is_none(),
        "the CPU path records no frame"
    );

    env._scene.set_bitmap_clean(frame);
    assert!(!dirty_bitmaps(&env).contains(&frame));
    env.advance(25.0);
    assert!(
        dirty_bitmaps(&env).contains(&frame),
        "every tick writes the display bitmap again"
    );
}

/// A host whose backend cannot run effect pipelines has no pipeline for the
/// transition's program, so the transition blends its display bitmap on the
/// CPU and records no custom-shader draw. The draw is what a GPU host
/// records instead.
#[test]
fn a_host_without_effect_pipelines_records_no_effect_draw() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]);");
    env.advance(25.0);

    assert!(
        named_layer(&env, "dest").effect.is_none(),
        "a host without effect pipelines records no effect draw"
    );
    assert!(
        env._scene.effect_pipelines.is_empty(),
        "the headless host has no effect pipeline"
    );
    assert!(
        dirty_bitmaps(&env).contains(&transition_frame(&env, "dest")),
        "the CPU path writes the display bitmap instead"
    );
}
