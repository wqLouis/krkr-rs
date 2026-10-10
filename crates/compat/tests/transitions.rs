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
//! Time is driven by calling [`compat::advance_transitions`] directly — the
//! frame-body step — with short millisecond deltas; no frame loop is
//! needed. Every scenario gets a fresh set of layers inside one [`Env`],
//! serialized through [`common::vm_lock`].

mod common;

use std::path::Path;

use api::fs::{FsPath, OpenMode};
use api::media::{DecodedImage, ImageInfo};
use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
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
        // fails anything else.
        host.set_image_codec(
            |bytes, _hint| {
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
        // SAFETY: the boxed scene and storage outlive the host.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            host,
            engine,
            _scene: scene,
            _storage: storage,
        }
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

    fn eval_str(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }

    /// The frame-body step: advance every active transition by `dt_ms`.
    fn advance(&self, dt_ms: f64) {
        let ctx = self.host.ctx().expect("bound host");
        compat::advance_transitions(&ctx, dt_ms).expect("advance transitions");
    }
}

/// A window with two `size` x `size` layers: `a` — the `transwith` source,
/// filled blue (`0x0000ff`) — and `b` — the destination, filled red
/// (`0xff0000`) and made visible (a non-primary layer defaults to hidden,
/// and an invisible destination ends the transition at the first frame —
/// reference/cpp/core/visual/LayerIntf.cpp:8056-8060).
fn two_layers(env: &Env, size: i64) {
    env.run(&format!(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setImageSize({size}, {size}); \
         var b = new Layer(w, null); b.setImageSize({size}, {size}); b.visible = true; \
         for (var i = 0; i < {size} * {size}; i++) {{ \
             b.setMainPixel(i % {size}, i / {size}, {RED}); \
             a.setMainPixel(i % {size}, i / {size}, {BLUE}); \
         }}"
    ));
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

/// Every `getMainPixel` of `layer` as one comma-joined string.
fn grid(env: &Env, layer: &str, size: i64) -> String {
    env.eval_str(&format!(
        "(function() {{ var s = ''; for (var y = 0; y < {size}; y++) \
         for (var x = 0; x < {size}; x++) s += {layer}.getMainPixel(x, y) + ','; return s; }})()"
    ))
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
         var a = new Layer(w, null); a.setImageSize(4, 4); \
         var b = new Layer(w, null); b.setImageSize(2, 2); b.visible = true;",
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

// ---------------------------------------------------------------------------
// Endpoint short-circuits
// ---------------------------------------------------------------------------

/// `tTVPCrossFadeTransHandler::Process`: at `Phase == 0` the destination is
/// source 1 verbatim — the advance at time 0 changes nothing.
///
/// reference/cpp/core/visual/TransIntf.cpp:578-594
#[test]
fn phase_zero_copies_source_one_exactly() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]);");
    env.advance(0.0);
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);
    assert_eq!(env.eval_int("b.getMainPixel(1, 1)"), RED);
}

/// `Process` at `Phase == PhaseMax` copies source 2 **verbatim** (the
/// transition ends with the new image exactly, not a 255-ratio blend: a
/// blend at `opa = 255` would leave the blue channel at 254 —
/// `blend_rgb(RED, BLUE, 255)`).
///
/// reference/cpp/core/visual/TransIntf.cpp:596-601
#[test]
fn phase_max_copies_source_two_exactly() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    env.run("b.beginTransition('crossfade', true, a, %[time: 100]);");
    env.advance(100.0);
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), BLUE);
    assert_eq!(env.eval_int("b.getMainPixel(1, 1)"), BLUE);
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
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), BLUE);
    assert_eq!(env.eval_int("b.getMainPixel(3, 0)"), RED);

    // `from: 2` (sttRight): mirrored — red on the left, blue on the right.
    two_layers(&env, 4);
    env.run("b.beginTransition('scroll', true, a, %[time: 100, from: 2]);");
    env.advance(50.0);
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);
    assert_eq!(env.eval_int("b.getMainPixel(3, 0)"), BLUE);

    // `stay: 2` (ststStaySrc): source 1 (the destination's own red image)
    // slides right and keeps priority, so the left half shows the blue
    // source and the right half stays red.
    // reference/cpp/core/visual/TransIntf.cpp:1151-1159,1202-1211
    two_layers(&env, 4);
    env.run("b.beginTransition('scroll', true, a, %[time: 100, stay: 2]);");
    env.advance(50.0);
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), BLUE);
    assert_eq!(env.eval_int("b.getMainPixel(3, 0)"), RED);

    // The port's string aliases are a compat extension; they must agree
    // with the numeric form they stand for.
    two_layers(&env, 4);
    env.run("b.beginTransition('scroll', true, a, %[time: 100, from: 'right']);");
    env.advance(50.0);
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), RED);
    assert_eq!(env.eval_int("b.getMainPixel(3, 0)"), BLUE);

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
/// (`time: 100`, 50 ms in, vague 64 → phase 160, `phase0 = 96`,
/// `tvpgl.cpp:4576-4594`): rule grey 0 → opacity 255 (source 2), grey 128
/// → `255 - (128-96)*255/64 = 128`, and the 2x2 rule repeats over the 4x4
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
    assert_eq!(
        env.eval_int("b.getMainPixel(0, 0)"),
        blend_rgb(RED, BLUE, 255)
    );
    // Rule (1,0) = grey 128 → opacity 128.
    assert_eq!(
        env.eval_int("b.getMainPixel(1, 0)"),
        blend_rgb(RED, BLUE, 128)
    );
    // Tiling: the 2x2 rule repeats over the 4x4 source, in both axes.
    assert_eq!(
        env.eval_int("b.getMainPixel(2, 0)"),
        env.eval_int("b.getMainPixel(0, 0)")
    );
    assert_eq!(
        env.eval_int("b.getMainPixel(1, 2)"),
        env.eval_int("b.getMainPixel(1, 0)")
    );
    assert_eq!(
        env.eval_int("b.getMainPixel(3, 3)"),
        env.eval_int("b.getMainPixel(1, 1)")
    );
    // A mid-grey rule value is not an endpoint of either source.
    assert_ne!(env.eval_int("b.getMainPixel(1, 0)"), RED);
    assert_ne!(env.eval_int("b.getMainPixel(1, 0)"), BLUE);

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

/// `vague` defaults to 64 (`TransIntf.cpp:806-810`) and drives the soft
/// edge of `TVPInitUnivTransBlendTable_c` (`tvpgl.cpp:4576-4594`). With the
/// 0/128 checkerboard rule mid-flight: the default equals an explicit
/// `vague: 64`; `vague: 0` is the hard threshold (grey 128 ≥ phase 128 →
/// source 1 verbatim); `vague: 255` yields opacity
/// `255 - 128*255/255 = 127`.
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
        grid(&env, "b", 2)
    };

    let default_grid = universal("");
    assert_eq!(
        default_grid,
        universal(", vague: 64"),
        "vague defaults to 64"
    );

    let hard_grid = universal(", vague: 0");
    assert_ne!(hard_grid, default_grid, "vague: 0 is a hard threshold");
    // In the hard-threshold run the grey-128 pixel is source 1 verbatim
    // (128 >= phase 128 → opacity 0).
    two_layers(&env, 2);
    env.run("b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img', vague: 0]);");
    env.advance(50.0);
    assert_eq!(env.eval_int("b.getMainPixel(1, 0)"), RED);

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
    let mask_grid = grid(&env, "b", 4);

    two_layers(&env, 4);
    env.run("b.beginTransition('universal', true, a, %[time: 100, rule: 'rule.img', vague: 0]);");
    env.advance(50.0);
    assert_eq!(mask_grid, grid(&env, "b", 4));
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
         var a = new SubLayer(w, null); a.setImageSize(2, 2); \
         var b = new SubLayer(w, null); b.setImageSize(2, 2); b.visible = true; \
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

/// Stopping midway keeps the last rendered frame (the teardown renders
/// nothing) and runs the `ttExchange` exchange: the two layers trade
/// position (`SetPosition`) and visibility.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7969-7991
#[test]
fn stopping_midway_keeps_the_frame_and_exchanges_layers() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    // `a` is the window's first layer, so the layer manager attaches it as
    // the primary layer and forces it visible
    // (`reference/cpp/core/visual/LayerManager.cpp:161-172`); hide it
    // explicitly so the `ttExchange` visibility swap below is observable
    // (`reference/cpp/core/visual/LayerIntf.cpp:7974-7986`).
    env.run(
        "a.left = 10; a.top = 20; a.visible = false; \
         b.beginTransition('crossfade', true, a, %[time: 1000]);",
    );
    env.advance(50.0);
    let before = env.eval_int("b.getMainPixel(0, 0)");
    assert_ne!(before, RED, "mid-flight is not source 1");
    assert_ne!(before, BLUE, "mid-flight is not source 2");

    env.run("b.stopTransition();");
    assert_eq!(
        env.eval_int("b.getMainPixel(0, 0)"),
        before,
        "the teardown keeps the last rendered frame"
    );
    assert_eq!(env.eval_int("b.left"), 10, "b took a's position");
    assert_eq!(env.eval_int("b.top"), 20);
    assert_eq!(env.eval_int("a.left"), 0, "a took b's position");
    assert_eq!(env.eval_int("a.top"), 0);
    assert_eq!(env.eval_int("b.visible"), 0, "b took a's visibility");
    assert_eq!(env.eval_int("a.visible"), 1, "a took b's visibility");
    assert_eq!(env.eval_int("hits"), 1, "and still fired the event");
}

/// A naturally completed transition ends through the same teardown: the
/// final frame is source 2 verbatim (`TransIntf.cpp:596-601`) and the
/// layers exchange position/visibility (`LayerIntf.cpp:7969-7991`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:7969-7991
/// reference/cpp/core/visual/TransIntf.cpp:596-601
#[test]
fn completion_exchanges_layers_and_copies_source_two() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env, 2);
    counter(&env);
    // `a` is the window's first layer, so `AttachPrimary` forces it visible
    // (`reference/cpp/core/visual/LayerManager.cpp:161-172`); hide it so the
    // completed transition's visibility swap is observable
    // (`reference/cpp/core/visual/LayerIntf.cpp:7974-7986`).
    env.run(
        "a.left = 10; a.top = 20; a.visible = false; \
         b.beginTransition('crossfade', true, a, %[time: 100]);",
    );
    env.advance(100.0);
    assert_eq!(env.eval_int("b.getMainPixel(0, 0)"), BLUE);
    assert_eq!(env.eval_int("b.getMainPixel(1, 1)"), BLUE);
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

/// Begin `name` with `{time: 100}` plus `options`, run 50 ms in (mid
/// flight) and return the destination's pixel grid.
fn extrans_grid(env: &Env, name: &str, options: &str, size: i64) -> String {
    two_layers(env, size);
    env.run(&format!(
        "b.beginTransition('{name}', true, a, %[time: 100{options}]);"
    ));
    env.advance(50.0);
    grid(env, "b", size)
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
         var a = new Layer(w, null); a.setImageSize({size}, {size}); \
         var b = new Layer(w, null); b.setImageSize({size}, {size}); b.visible = true; \
         for (var y = 0; y < {size}; y++) for (var x = 0; x < {size}; x++) {{ \
             b.setMainPixel(x, y, ((x * 16) << 16) | ((y * 16) << 8) | ((x + y) * 8)); \
             a.setMainPixel(x, y, ((({size} - 1 - x) * 16) << 16) | \
                                ((({size} - 1 - y) * 16) << 8) | ((x + y) * 8)); \
         }}"
    ));
    env.run(&format!(
        "b.beginTransition('{name}', true, a, %[time: 100{options}]);"
    ));
    env.advance(50.0);
    grid(env, "b", size)
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
    assert_ne!(
        extrans_patterned_grid(&env, "mosaic", ", maxsize: 4", 16),
        extrans_patterned_grid(&env, "mosaic", ", maxsize: 12", 16),
        "maxsize must reach the mosaic handler"
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
