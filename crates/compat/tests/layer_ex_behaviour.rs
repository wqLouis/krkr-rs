//! Reference-behaviour parity tests for the `LayerEx*` plugin family:
//! `LayerExBase`, `LayerExPerspective`, `LayerExMovie` and the GDI+
//! `LayerExDraw` value types (`PointF`/`RectF`/`FontInfo`/`Appearance`/
//! `Path`).
//!
//! Every test states the **KiriKiri (reference C++) expectation** with a
//! `reference/cpp/**` citation above the assertion it pins, exactly like
//! `crates/compat/tests/layer_behaviour.rs`. The tests attach a
//! caller-owned L2 [`Scene`] to a [`CompatHost`] and drive the TJS surface —
//! the same seam the app uses — so write side effects are verified against
//! the L2 state the natives actually mutate.
//!
//! The movie *playback* path needs the crate-private decoded-frame source
//! and the per-frame poll, so it is covered by the unit tests in
//! `src/natives/plugins/layer_ex/movie.rs`; here the movie surface, its
//! arities and its callback members are pinned through the VM.
//!
//! Known reference divergences that **cannot** be fixed inside the audited
//! files (documented here so the exclusions are visible):
//! * `Layer.loadRecord` always returns `false` in the reference
//!   (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1890-1901`
//!   computes the redraw result and then falls through to `return false`),
//!   while `draw.rs` returns `true` on a successful load. The recorded
//!   content still round-trips (pinned below); only the boolean is not
//!   reference-exact.
//! * The reference `RectF.Union` is the *static* `static bool
//!   Union(RectF &ret, const RectF &a, const RectF &b)`
//!   (`reference/cpp/plugins/layerex_draw/windows/main.cpp:210`), which NCB
//!   registers with `TJS_STATICMEMBER`; a static class member is not copied
//!   onto instances (`reference/cpp/core/tjs2/tjsNative.cpp:347-365`). The
//!   port's earlier *instance* `Union` (`geom.rs`'s `rect_union`, asserted
//!   by `crates/compat/tests/layer_ex.rs:366`) is shadowed by that static
//!   member and is not reference surface; that stale assertion is outside
//!   this file's edit scope and must be changed to the class-level form
//!   pinned below.

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use tjs::{Tjs2Engine, TjsValue};

/// Script label for diagnostics.
const SCRIPT: &str = "layer-ex-behaviour";

/// A bound VM + host + attached scene.
///
/// Fields drop in declaration order: the host first (releasing every
/// retained script object), then the engine and the scene.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene outlives the host (declared after `_host`).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            scene,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    /// Run a script and keep a failure observable instead of panicking (for
    /// cases where the assertion is that the reference *raises*).
    fn try_run(&self, script: &str) -> Result<(), String> {
        self.engine
            .exec_script(script, SCRIPT)
            .map(|_| ())
            .map_err(|error| error.to_string())
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

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

    /// The id of the scene layer whose `name` member is `name`.
    fn layer_id(&self, name: &str) -> u32 {
        self.scene
            .layers
            .iter()
            .find(|layer| layer.name == name)
            .unwrap_or_else(|| panic!("no scene layer named {name:?}"))
            .id
    }

    /// The straight-alpha RGBA pixel at `(x, y)` of a layer's MainImage.
    fn pixel(&self, layer_id: u32, x: u32, y: u32) -> [u8; 4] {
        let bitmap_id = self
            .scene
            .layer(layer_id)
            .and_then(|layer| layer.bitmap)
            .expect("layer bitmap");
        let bitmap = self.scene.bitmap(bitmap_id).expect("bitmap");
        let offset = bitmap.pixel_offset(x, y).expect("pixel in range");
        [
            bitmap.rgba[offset],
            bitmap.rgba[offset + 1],
            bitmap.rgba[offset + 2],
            bitmap.rgba[offset + 3],
        ]
    }
}

/// Create a window with two 4x4 layers `src` (filled opaque red) and `dst`
/// (transparent).
fn two_layers(env: &Env) {
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         var dst = new Layer(w, null); \
         src.name = 'src'; \
         dst.name = 'dst'; \
         src.setImageSize(4, 4); \
         dst.setImageSize(4, 4); \
         src.fillRect(0, 0, 4, 4, 0xFFFF0000);",
    );
}

// ---------------------------------------------------------------------------
// PointF (geom.rs)
// ---------------------------------------------------------------------------

#[test]
fn pointf_constructor_requires_two_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/windows/main.cpp:133-137
    // registers `NCB_CONSTRUCTOR((REAL, REAL))`; NCB rejects a call below
    // the declared parameter count (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(
        env.try_run("new PointF(1);").is_err(),
        "`new PointF(1)` must raise"
    );
    env.run("var p = new PointF(1, 2);");
    assert_eq!(env.eval_real("p.x"), 1.0);
    assert_eq!(env.eval_real("p.y"), 2.0);
}

#[test]
fn pointf_components_are_settable_reals() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:341-342
    // (`PointF(tjs_real _x, tjs_real _y)`) and main.cpp:135-136 register
    // `x`/`y` as `REAL` member properties.
    env.run("var p = new PointF(1.5, 2.5); p.x = 3; p.y = 4.5;");
    assert_eq!(env.eval_real("p.x"), 3.0);
    assert_eq!(env.eval_real("p.y"), 4.5);
}

#[test]
fn pointf_equals_compares_components_exactly() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:344
    // (`bool Equals(PointF tgt) { return x == tgt.x && y == tgt.y; }`).
    assert_eq!(
        env.eval_int("(new PointF(1, 2)).Equals(new PointF(1, 2))"),
        1
    );
    assert_eq!(
        env.eval_int("(new PointF(1, 2)).Equals(new PointF(1, 3))"),
        0
    );
    // A non-object argument converts to the default `PointF()` (0, 0)
    // (`reference/cpp/plugins/layerex_draw/windows/main.cpp:98-100`).
    assert_eq!(env.eval_int("(new PointF(0, 0)).Equals(5)"), 1);
    // NCB requires the one parameter (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.try_run("(new PointF(1, 2)).Equals();").is_err());
}

// ---------------------------------------------------------------------------
// RectF (geom.rs)
// ---------------------------------------------------------------------------

#[test]
fn rectf_constructor_requires_four_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/windows/main.cpp:181-182
    // (`NCB_CONSTRUCTOR((REAL, REAL, REAL, REAL))`) with the NCB minimum
    // check (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.try_run("new RectF(1, 2, 3);").is_err());
    env.run("var r = new RectF(1, 2, 3, 4);");
    assert_eq!(env.eval_real("r.x"), 1.0);
    assert_eq!(env.eval_real("r.y"), 2.0);
    assert_eq!(env.eval_real("r.width"), 3.0);
    assert_eq!(env.eval_real("r.height"), 4.0);
}

#[test]
fn rectf_derived_edges_and_location() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:363-367:
    // `GetLeft/GetTop/GetRight/GetBottom` and `GetLocation` are read-only
    // members (`windows/main.cpp:189-192`).
    env.run("var r = new RectF(1, 2, 3, 4);");
    assert_eq!(env.eval_real("r.left"), 1.0);
    assert_eq!(env.eval_real("r.top"), 2.0);
    assert_eq!(env.eval_real("r.right"), 4.0);
    assert_eq!(env.eval_real("r.bottom"), 6.0);
    assert_eq!(env.eval_real("r.location.x"), 1.0);
    assert_eq!(env.eval_real("r.location.y"), 2.0);
}

#[test]
fn rectf_clone_and_equals_copy_the_components() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:369-373
    // (`Clone`/`Equals`), registered at `windows/main.cpp:193,199`.
    env.run("var r = new RectF(1, 2, 3, 4); var c = r.Clone();");
    assert_eq!(
        env.eval_int("c.x == 1 && c.y == 2 && c.width == 3 && c.height == 4"),
        1
    );
    assert_eq!(env.eval_int("r.Equals(c)"), 1);
    assert_eq!(env.eval_int("r.Equals(new RectF(0, 0, 0, 0))"), 0);
    // A non-object argument converts to the default `RectF()`
    // (`reference/cpp/plugins/layerex_draw/windows/main.cpp:171-173`).
    assert_eq!(env.eval_int("(new RectF(0, 0, 0, 0)).Equals(5)"), 1);
}

#[test]
fn rectf_inflate_and_offset_mutate_in_place() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:373-388:
    // `Inflate` grows by `(width, height)` on every side and `Offset` shifts
    // the origin.
    env.run("var r = new RectF(1, 2, 3, 4); r.Inflate(1, 1);");
    assert_eq!(env.eval_real("r.x"), 0.0);
    assert_eq!(env.eval_real("r.y"), 1.0);
    assert_eq!(env.eval_real("r.width"), 5.0);
    assert_eq!(env.eval_real("r.height"), 6.0);
    env.run("r.Offset(2, 3);");
    assert_eq!(env.eval_real("r.x"), 2.0);
    assert_eq!(env.eval_real("r.y"), 4.0);
    // `InflatePoint(point)` is `Inflate(point.x, point.y)`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:379`,
    // registered at `windows/main.cpp:201-202`).
    env.run("var q = new RectF(1, 1, 2, 2); q.InflatePoint(new PointF(1, 2));");
    assert_eq!(env.eval_real("q.x"), 0.0);
    assert_eq!(env.eval_real("q.y"), -1.0);
    assert_eq!(env.eval_real("q.width"), 4.0);
    assert_eq!(env.eval_real("q.height"), 6.0);
}

#[test]
fn rectf_intersection_and_empty_predicates() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:380-384
    // (`IntersectsWith` / `IsEmptyArea`), registered at `windows/main.cpp:203-204`.
    env.run("var r = new RectF(0, 0, 2, 2);");
    assert_eq!(env.eval_int("r.IntersectsWith(new RectF(1, 1, 2, 2))"), 1);
    assert_eq!(env.eval_int("r.IntersectsWith(new RectF(5, 5, 1, 1))"), 0);
    assert_eq!(env.eval_int("(new RectF(0, 0, 0, 1)).IsEmptyArea()"), 1);
    assert_eq!(env.eval_int("(new RectF(0, 0, 1, 1)).IsEmptyArea()"), 0);
}

#[test]
fn rectf_static_union_writes_the_target_and_returns_true() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:389-397
    // (`static bool Union(RectF &ret, const RectF &a, const RectF &b)`),
    // registered with `NCB_METHOD(Union)` at `windows/main.cpp:210`; the
    // first argument is overwritten with the bounding rectangle and the call
    // returns `true` (TJS integer 1).
    env.run(
        "var ret = new RectF(0, 0, 0, 0); \
         var a = new RectF(1, 2, 3, 4); \
         var b = new RectF(5, 6, 7, 8); \
         var ok = RectF.Union(ret, a, b);",
    );
    assert_eq!(env.eval_int("ok"), 1);
    assert_eq!(env.eval_real("ret.x"), 1.0);
    assert_eq!(env.eval_real("ret.y"), 2.0);
    assert_eq!(env.eval_real("ret.width"), 11.0);
    assert_eq!(env.eval_real("ret.height"), 12.0);
}

// ---------------------------------------------------------------------------
// FontInfo (font.rs)
// ---------------------------------------------------------------------------

#[test]
fn fontinfo_constructor_requires_three_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/windows/main.cpp:583-584 registers
    // `NCB_CONSTRUCTOR((const tjs_char *, REAL, INT))`; NCB rejects a
    // shorter call (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.try_run("new FontInfo('A', 12);").is_err());
    env.run("var f = new FontInfo('A', 12, 1);");
    assert_eq!(env.eval_real("f.emSize"), 12.0);
    assert_eq!(env.eval_int("f.style"), 1);
}

#[test]
fn fontinfo_property_types() {
    let _lock = vm_lock();
    let env = Env::new();
    // `getStyle` returns `INT` and `getEmSize` returns `REAL`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.hpp:77-84`,
    // registered at `windows/main.cpp:585-587`).
    env.run("var f = new FontInfo('A', 12.5, 3);");
    assert!(matches!(env.eval_ok("f.style"), TjsValue::Integer(3)));
    assert!(matches!(env.eval_ok("f.emSize"), TjsValue::Real(_)));
    // `forceSelfPathDraw` defaults to `false`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:222-224`).
    assert_eq!(env.eval_int("f.forceSelfPathDraw"), 0);
    env.run("f.forceSelfPathDraw = true;");
    assert_eq!(env.eval_int("f.forceSelfPathDraw"), 1);
    // `familyName` is the script-visible family
    // (`reference/cpp/plugins/layerex_draw/windows/main.cpp:585`).
    assert!(matches!(env.eval_ok("f.familyName"), TjsValue::String(_)));
}

#[test]
fn fontinfo_metrics_are_reals() {
    let _lock = vm_lock();
    let env = Env::new();
    // The five metric getters return `REAL`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:384-405`,
    // registered as read-only at `windows/main.cpp:588-592`). With no face
    // registered they read as 0 rather than fabricating a metric.
    env.run("var f = new FontInfo('GhostFamily', 16, 0);");
    for member in [
        "ascent",
        "descent",
        "ascentLeading",
        "descentLeading",
        "lineSpacing",
    ] {
        let expression = format!("f.{member}");
        assert!(
            matches!(env.eval_ok(&expression), TjsValue::Real(_)),
            "FontInfo.{member} must be a real"
        );
    }
}

// ---------------------------------------------------------------------------
// Appearance (appearance.rs)
// ---------------------------------------------------------------------------

#[test]
fn appearance_mutators_require_their_declared_arity() {
    let _lock = vm_lock();
    let env = Env::new();
    // `void addBrush(tTJSVariant, REAL ox = 0, REAL oy = 0)` and
    // `void addPen(tTJSVariant, tTJSVariant, REAL ox = 0, REAL oy = 0)` are
    // C++ default arguments only; NCB still requires the full parameter list
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:767,778`
    // and `reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(
        env.try_run("(new Appearance()).addBrush(0xFFFF0000);")
            .is_err()
    );
    assert!(
        env.try_run("(new Appearance()).addPen(0xFFFF0000, 1);")
            .is_err()
    );
    env.run("var app = new Appearance(); app.addBrush(0xFFFF0000, 0, 0); app.addPen(0xFF00FF00, 1, 0, 0);");
}

#[test]
fn appearance_clear_empties_the_draw_list() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // `Appearance::clear` drops every brush/pen
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:422-426`).
    env.run(
        "var app = new Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         app.clear(); \
         dst.clear(0); \
         dst.drawRectangle(app, 0, 0, 4, 4);",
    );
    assert_eq!(env.pixel(dst, 1, 1), [0, 0, 0, 0]);
}

#[test]
fn appearance_color_arguments_follow_create_dictionary_defaults() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");

    // A non-object colour is cast with `Color((tjs_int)colorOrBrush)`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:626-637`).
    env.run("dst.clear(0); var app = new Appearance(); app.addBrush(0xFFFF0000, 0, 0); dst.drawRectangle(app, 0, 0, 4, 4);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);

    // A string goes through `TJSStringToInteger`, so a hex spelling parses
    // (`reference/cpp/core/tjs2/tjsVariant.cpp:301-307`).
    env.run("dst.clear(0); var app2 = new Appearance(); app2.addBrush('0xFFFF0000', 0, 0); dst.drawRectangle(app2, 0, 0, 4, 4);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);

    // A dictionary/array without a `type` member reads the defaults: solid
    // white (`type` defaults to `BrushTypeSolidColor`, `color` to
    // `0xFFFFFFFF`, `reference/cpp/plugins/layerex_draw/windows/
    // LayerExDraw.cpp:633-638`).
    env.run("dst.clear(0); var app3 = new Appearance(); app3.addBrush([], 0, 0); dst.drawRectangle(app3, 0, 0, 4, 4);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 255, 255, 255]);
}

// ---------------------------------------------------------------------------
// Path (path.rs)
// ---------------------------------------------------------------------------

#[test]
fn path_primitives_require_their_declared_arity() {
    let _lock = vm_lock();
    let env = Env::new();
    // Each `Path` primitive is registered with `NCB_METHOD`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3438-3456`)
    // against its declared signature (`blend2d/LayerExDraw.hpp:944-977`),
    // so NCB requires the full parameter count
    // (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    env.run("var p = new Path();");
    for script in [
        "p.drawLine(1, 2, 3);",
        "p.drawRectangle(1, 2, 3);",
        "p.drawEllipse(1, 2, 3);",
        "p.drawArc(1, 2, 3, 4, 5);",
        "p.drawPie(1, 2, 3, 4, 5);",
        "p.drawBezier(1, 2, 3, 4, 5, 6, 7);",
        "p.drawCurve2([[0, 0], [1, 1]]);",
        "p.drawCurve3([[0, 0], [1, 1]], 0, 1);",
        "p.drawClosedCurve2([[0, 0], [1, 1]]);",
    ] {
        assert!(env.try_run(script).is_err(), "{script} must raise");
    }
}

#[test]
fn path_rectangle_and_polygon_fill_the_closed_area() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // `Path::drawRectangle` adds a closed figure
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1429-1437`)
    // and `Layer.drawPath` fills it with the appearance.
    env.run(
        "var p = new Path(); \
         p.drawRectangle(1, 1, 2, 2); \
         var app = new Appearance(); \
         app.addBrush(0xFF00FF00, 0, 0); \
         dst.drawPath(app, p);",
    );
    assert_eq!(env.pixel(dst, 2, 2), [0, 255, 0, 255]);
    assert_eq!(env.pixel(dst, 0, 0), [0, 0, 0, 0]);

    // `Path::drawPolygon` also closes its own figure
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1405-1425`).
    env.run(
        "dst.clear(0); \
         var q = new Path(); \
         q.drawPolygon([[0, 0], [4, 0], [2, 4]]); \
         var app2 = new Appearance(); \
         app2.addBrush(0xFFFF0000, 0, 0); \
         dst.drawPath(app2, q);",
    );
    assert_eq!(env.pixel(dst, 2, 1), [255, 0, 0, 255]);
}

#[test]
fn path_invalid_bezier_chain_adds_nothing() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // `Path::drawBeziers` silently ignores a point count below 4 or not of
    // the form `3n + 1`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1190-1191`);
    // the call still succeeds but draws nothing.
    env.run(
        "dst.clear(0); \
         var p = new Path(); \
         p.drawBeziers([[0, 0], [1, 1], [2, 0]]); \
         var app = new Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.drawPath(app, p);",
    );
    assert_eq!(env.pixel(dst, 1, 1), [0, 0, 0, 0]);
}

// ---------------------------------------------------------------------------
// LayerExBase (base.rs)
// ---------------------------------------------------------------------------

#[test]
fn layer_ex_base_reads_and_sets_clip() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    env.run("var b = new LayerExBase(src);");
    // `layerExBase` caches `imageLeft/imageTop/imageWidth/imageHeight` and the
    // clip members from the layer
    // (`reference/cpp/plugins/layerExBase.hpp:62-98`); no clip set reads 0.
    assert_eq!(env.eval_int("b.imageWidth"), 4);
    assert_eq!(env.eval_int("b.imageHeight"), 4);
    assert_eq!(env.eval_int("b.clipWidth"), 0);
    // The clip setters funnel through `SetClip`
    // (`reference/cpp/core/visual/LayerIntf.cpp:4042-4079`): left/top clamp
    // to 0 and right/bottom clamp to the MainImage bounds.
    env.run("b.clipLeft = 1; b.clipTop = 1; b.clipWidth = 2; b.clipHeight = 2;");
    assert_eq!(env.eval_int("b.clipLeft"), 1);
    assert_eq!(env.eval_int("b.clipTop"), 1);
    assert_eq!(env.eval_int("b.clipWidth"), 2);
    assert_eq!(env.eval_int("b.clipHeight"), 2);
    // A width that runs past the image clamps to the image bounds.
    env.run("b.clipLeft = 0; b.clipWidth = 100;");
    assert_eq!(env.eval_int("b.clipWidth"), 4);
}

#[test]
fn layer_ex_base_update_requires_zero_or_four_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    env.run("var b = new LayerExBase(src);");
    // Reference `Layer.update` (`reference/cpp/core/visual/LayerIntf.cpp:
    // 9663-9686`): no argument updates the whole image, four arguments the
    // rectangle, and 1-3 arguments raise `TJS_E_BADPARAMCOUNT` (`:9674-9676`).
    env.run("b.update(); b.update(0, 0, 1, 1);");
    assert!(env.try_run("b.update(0, 0, 1);").is_err());
}

// ---------------------------------------------------------------------------
// LayerExPerspective (perspective.rs)
// ---------------------------------------------------------------------------

#[test]
fn perspective_copy_requires_a_layer_source() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // The reference resolves `param[0]` with
    // `NativeInstanceSupport(TJS_NIS_GETINSTANCE, tTJSNC_Layer::ClassID, …)`
    // and throws `TVPSpecifyLayer` otherwise
    // (`reference/cpp/plugins/layerExPerspective.cpp:171-176`).
    assert!(
        env.try_run("dst.perspectiveCopy({}, 0, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);")
            .is_err()
    );
}

#[test]
fn perspective_copy_rejects_non_numeric_coordinates() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // Every coordinate is read with `param[i]->AsReal()`
    // (`reference/cpp/plugins/layerExPerspective.cpp:181-199`), which raises
    // a conversion error for an object argument rather than coercing to 0
    // (`reference/cpp/core/tjs2/tjsVariant.h:947-966`).
    assert!(
        env.try_run("dst.perspectiveCopy(src, {}, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);")
            .is_err()
    );
}

#[test]
fn perspective_copy_maps_the_source_quad() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // Argument order and quad mapping from `PerspectiveCopy_GL`
    // (`reference/cpp/plugins/layerExPerspective.cpp:166-199`): the source
    // rectangle is `(g_x1, g_y1, g_x1 + g_x3, g_y1 + g_y4)` and the four
    // destination corners are `(p5,p6) (p7,p8) (p9,p10) (p11,p12)`.
    env.run("dst.perspectiveCopy(src, 0, 0, 4, 4, 1, 1, 3, 1, 1, 3, 3, 3);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);
    assert_eq!(env.pixel(dst, 2, 2), [255, 0, 0, 255]);
    assert_eq!(env.pixel(dst, 0, 0), [0, 0, 0, 0]);
}

#[test]
fn perspective_class_constructor_rejects_a_plain_object() {
    let _lock = vm_lock();
    let env = Env::new();
    // The extension binds to a `Layer` through the native-instance lookup
    // (the reference `TJS_GET_NATIVE_INSTANCE`, used by
    // `reference/cpp/plugins/layerExPerspective.cpp:171-176` to reject a
    // non-Layer source); a plain object is not a Layer, so binding it would
    // be a silent fallback and must raise instead.
    assert!(
        env.try_run("new LayerExPerspective({});").is_err(),
        "a plain object is not a Layer"
    );
    // A real Layer still binds and exposes the base geometry members.
    two_layers(&env);
    env.run("var p = new LayerExPerspective(dst);");
    assert_eq!(env.eval_int("p.imageWidth"), 4);
    assert_eq!(env.eval_int("p.imageHeight"), 4);
}

// ---------------------------------------------------------------------------
// Movie surface (movie.rs)
// ---------------------------------------------------------------------------

#[test]
fn movie_surface_arities() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // `void openMovie(const tjs_char *filename, bool alpha)` and
    // `void startMovie(bool loop)` are registered with `NCB_METHOD`
    // (`reference/cpp/plugins/layerExMovie.cpp:72-74,338-339`), so NCB
    // requires 2 and 1 parameters respectively
    // (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.try_run("dst.openMovie('movie.mpg');").is_err());
    assert!(env.try_run("dst.startMovie();").is_err());
    // `stopMovie()` and `isPlayingMovie()` take no parameters.
    env.run("dst.stopMovie();");
    assert_eq!(env.eval_int("dst.isPlayingMovie()"), 0);
}

#[test]
fn movie_callbacks_are_layer_members() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // The reference reads `onStartMovie`/`onStopMovie`/`onUpdateMovie` from
    // the layer object (`reference/cpp/plugins/layerExMovie.cpp:205,222,274`),
    // so a script override is what the native dispatch reaches.
    env.run(
        "dst.onStartMovie = function() { return 7; }; \
         dst.onStopMovie = function() { return 8; }; \
         dst.onUpdateMovie = function() { return 9; };",
    );
    assert_eq!(env.eval_int("dst.onStartMovie()"), 7);
    assert_eq!(env.eval_int("dst.onStopMovie()"), 8);
    assert_eq!(env.eval_int("dst.onUpdateMovie()"), 9);
}

#[test]
fn movie_class_exposes_the_base_surface() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // `NCB_ATTACH_CLASS_WITH_HOOK(layerExMovie, Layer)`
    // (`reference/cpp/plugins/layerExMovie.cpp:337-342`) attaches
    // `openMovie`/`startMovie`/`stopMovie`/`isPlayingMovie`; the port also
    // exposes the class itself with the shared base geometry members.
    env.run("var m = new LayerExMovie(dst);");
    assert_eq!(env.eval_int("m.imageWidth"), 4);
    assert_eq!(env.eval_int("m.imageHeight"), 4);
    assert_eq!(env.eval_int("m.isPlayingMovie()"), 0);
}

#[test]
fn movie_class_constructor_rejects_a_plain_object() {
    let _lock = vm_lock();
    let env = Env::new();
    // `layerExMovie` derives from `layerExBase_GL`, whose constructor resolves
    // the receiver with the native-instance lookup and throws `Not Layer`
    // otherwise (`reference/cpp/plugins/layerExBase.hpp:152-158`, attached at
    // `reference/cpp/plugins/layerExMovie.cpp:337-342`); a plain object is not
    // a Layer, so binding it would be a silent fallback and must raise.
    assert!(
        env.try_run("new LayerExMovie({});").is_err(),
        "a plain object is not a Layer"
    );
    two_layers(&env);
    env.run("var m = new LayerExMovie(dst);");
    assert_eq!(env.eval_int("m.imageWidth"), 4);
}

// ---------------------------------------------------------------------------
// Record lifecycle (record.rs, driven through the Layer members)
// ---------------------------------------------------------------------------

#[test]
fn record_lifecycle_round_trips_through_a_file() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    let record_path = std::env::temp_dir().join("krkr_layer_ex_behaviour_record.kexr");
    let record_path_str = record_path.to_string_lossy().replace('\\', "\\\\");

    // `setRecord(true)` creates the recording; `getRecord()` reports it
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1770-1781`).
    env.run(
        "var app = new Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.record = true; \
         dst.drawRectangle(app, 0, 0, 2, 2);",
    );
    assert_eq!(env.eval_int("dst.record"), 1);

    // `getRecordImage()` replays the recording into a fresh image
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1808-1839`).
    env.run("var rec = dst.getRecordImage();");
    let rec_id = env.scene.bitmaps.last().expect("record bitmap").id;
    let bitmap = env.scene.bitmap(rec_id).expect("bitmap");
    let offset = bitmap.pixel_offset(1, 1).expect("pixel");
    assert_eq!(
        [
            bitmap.rgba[offset],
            bitmap.rgba[offset + 1],
            bitmap.rgba[offset + 2],
            bitmap.rgba[offset + 3],
        ],
        [255, 0, 0, 255]
    );

    // `saveRecord(filename)` returns `true` on success
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1856-1888`).
    assert_eq!(
        env.eval_int(&format!("dst.saveRecord('{record_path_str}')")),
        1
    );
    assert!(record_path.exists(), "record file should be written");

    // `loadRecord(filename)` restores the recording and redraws it
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1890-1901`;
    // the reference's `return false` is the documented divergence above).
    env.run("dst.clear(0); dst.record = false;");
    assert_eq!(env.pixel(dst, 1, 1), [0, 0, 0, 0]);
    env.run(&format!("dst.loadRecord('{record_path_str}');"));
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);

    // `redrawRecord()` re-renders the current recording
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1841-1854`).
    env.run("dst.clear(0);");
    assert_eq!(env.eval_int("dst.redrawRecord()"), 1);

    let _ = std::fs::remove_file(&record_path);
}

// ---------------------------------------------------------------------------
// Hand-off: the removed `nativeId`/`id` fallback
// ---------------------------------------------------------------------------

#[test]
fn copy_to_bitmap_reports_a_member_specific_error() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // `Layer.copyToBitmapFromMainImage` resolves its argument with the
    // native-instance lookup for `Bitmap` and rejects anything else with
    // `TJS_E_INVALIDPARAM` (`reference/cpp/core/visual/LayerIntf.cpp:9893-9913`);
    // a plain object must raise the member's own error, not the generic
    // object-resolution message.
    // `%[]` is the TJS empty dictionary literal; `{}` is a block and is a
    // syntax error as an expression, so it never reached the member.
    let message = env
        .try_run("dst.copyToBitmapFromMainImage(%[]);")
        .expect_err("a plain object is not a Bitmap");
    assert!(
        message.contains("copyToBitmapFromMainImage"),
        "error must name the member: {message}"
    );
    assert!(
        message.contains("not a Bitmap"),
        "error must say it is not a Bitmap: {message}"
    );
}
