//! Integration tests for the `layerExDraw` plugin family: the `GdiPlus`
//! namespace and the `Layer.perspectiveCopy` / movie members.
//!
//! `LayerExBase` / `LayerExMovie` / `LayerExPerspective` are not TJS classes
//! in the reference (`reference/cpp/plugins/layerExPerspective.cpp:255-296`),
//! so they are absent from the script surface.
//!
//! The tests attach a caller-owned L2 [`Scene`] to a [`CompatHost`] and drive
//! the TJS surface — the same seam the app uses — so the pixel work is
//! verified against the L2 state the methods mutate. The movie playback
//! internals (which need a decoded-frame source) are unit-tested in the
//! plugin module; here the movie surface and its error paths are covered.

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use api::error::MediaError;
use api::media::{AudioSpec, DecodedAudio, RgbaFrame, VideoSpec};
use common::vm_lock;
use compat::{CompatHost, VideoSource, movie_poll, set_movie_source};
use engine::Scene;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached scene.
///
/// Fields drop in declaration order: the host first (releasing every retained
/// script object), then the engine and the scene.
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
            .exec_script_discard(script, "layer-ex-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "layer-ex-test")
            .map_err(|error| error.to_string())
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            Ok(TjsValue::Integer(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn layer_id(&self, variable: &str) -> u32 {
        // The reference surface has no `Layer.id` member, so the tests name
        // the layers they create and the id comes from the Rust-side scene.
        self.scene
            .layers
            .iter()
            .find(|layer| layer.name == variable)
            .unwrap_or_else(|| panic!("no scene layer named {variable:?}"))
            .id
    }

    /// The straight-alpha RGBA pixel at `(x, y)` of a layer's MainImage.
    fn pixel(&self, layer_id: u32, x: u32, y: u32) -> [u8; 4] {
        let bitmap_id = self
            .scene
            .layer(layer_id)
            .and_then(|layer| layer.bitmap)
            .expect("layer bitmap");
        self.pixel_bitmap(bitmap_id, x, y)
    }

    /// The straight-alpha RGBA pixel at `(x, y)` of a scene bitmap.
    fn pixel_bitmap(&self, bitmap_id: u32, x: u32, y: u32) -> [u8; 4] {
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

#[test]
fn perspective_copy_identity_copies_the_source() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // The untouched pixel is the reference's default image: transparent white
    // `TVP_RGBA2COLOR(255, 255, 255, 0)`, filled by `setImageSize`
    // (`reference/cpp/core/visual/LayerIntf.cpp:104-110,2313`) — not a
    // cleared `[0, 0, 0, 0]`.
    assert_eq!(env.pixel(dst, 1, 1), [255, 255, 255, 0]);
    env.run("dst.perspectiveCopy(src, 0, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);
    assert_eq!(env.pixel(dst, 3, 3), [255, 0, 0, 255]);
    assert_eq!(env.pixel(dst, 0, 0), [255, 0, 0, 255]);
}

#[test]
fn perspective_copy_maps_to_a_smaller_quad() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // The 2x2 destination quad (1,1)-(3,3) maps onto the whole 4x4 source.
    env.run("dst.perspectiveCopy(src, 0, 0, 4, 4, 1, 1, 3, 1, 1, 3, 3, 3);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);
    assert_eq!(env.pixel(dst, 2, 2), [255, 0, 0, 255]);
    // Outside the quad stays at the reference's default image (transparent
    // white, `reference/cpp/core/visual/LayerIntf.cpp:104-110,400`), not a
    // cleared colour.
    assert_eq!(env.pixel(dst, 0, 0), [255, 255, 255, 0]);
    assert_eq!(env.pixel(dst, 3, 3), [255, 255, 255, 0]);
}

#[test]
fn perspective_copy_requires_thirteen_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    env.run(
        "var caught = 0; \
         try { dst.perspectiveCopy(src, 0, 0, 4, 4); } catch (e) { caught = 1; }",
    );
    assert_eq!(env.eval_int("caught"), 1);
}

#[test]
fn movie_surface_is_attached_to_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // The four methods are callable; `isPlayingMovie` reports false and the
    // playback commands raise when no movie is open (D11).
    assert_eq!(env.eval_int("dst.isPlayingMovie()"), 0);
    env.run(
        "var caught = 0; \
         try { dst.startMovie(false); } catch (e) { caught = 1; }",
    );
    assert_eq!(env.eval_int("caught"), 1);
    // Without a host-installed decoded source, `openMovie` raises instead of
    // fabricating metadata.
    env.run(
        "var caught = 0; \
         try { dst.openMovie('movie.mpg', false); } catch (e) { caught = 1; }",
    );
    assert_eq!(env.eval_int("caught"), 1);
}

#[test]
fn movie_callbacks_are_layer_members() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    env.run(
        "dst.onStartMovie = function() { return 7; }; \
         dst.onStopMovie = function() { return 8; }; \
         dst.onUpdateMovie = function() { return 9; };",
    );
    assert_eq!(env.eval_int("dst.onStartMovie()"), 7);
    assert_eq!(env.eval_int("dst.onStopMovie()"), 8);
    assert_eq!(env.eval_int("dst.onUpdateMovie()"), 9);
}

/// A host video source for the movie tests: the same seam the app installs
/// (`compat::set_movie_source`).
struct MovieSource {
    spec: VideoSpec,
}

impl VideoSource for MovieSource {
    fn spec(&self, _path: &str) -> Option<VideoSpec> {
        Some(self.spec)
    }

    fn frame_at(&mut self, _path: &str, _at_ms: i64) -> Option<Arc<RgbaFrame>> {
        Some(Arc::new(RgbaFrame::new(
            4,
            4,
            0,
            [0u8, 255, 0, 255].repeat(16),
        )))
    }
}

/// A layer that opened and started a movie is released when the script drops
/// it.
///
/// The reference keeps a layer's movie state on the layer's own native
/// instance (`reference/cpp/plugins/layerExMovie.cpp:318-327`, `new
/// ClassT(objthis)` registered with `NCB_INSTANCE_GETTER` and destroyed with
/// the object), so nothing outside the object roots it; dropping the last
/// script reference runs `Layer.Invalidate`
/// (`reference/cpp/core/visual/LayerIntf.cpp:515-566`), which deallocates the
/// MainImage. A slot that retains the layer object prevents exactly that.
#[test]
fn a_movie_layer_is_released_when_the_script_drops_it() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var mov = new Layer(w, null); \
         mov.name = 'movie'; \
         mov.setImageSize(4, 4); \
         var started = 0; \
         mov.onStartMovie = function() { started = 1; };",
    );
    let ctx = env._host.ctx().expect("bound context");
    set_movie_source(
        &ctx,
        Box::new(MovieSource {
            spec: VideoSpec {
                width: 4,
                height: 4,
                fps: 10.0,
                total_frames: Some(10),
                sample_rate: 0,
                channels: 0,
                ..VideoSpec::default()
            },
        }),
    );
    env.run("mov.openMovie('movie.mpg', false); mov.startMovie(false);");
    // The callback is dispatched through the layer object, so the non-owning
    // handle must still resolve a live receiver.
    assert_eq!(env.eval_int("started"), 1);
    movie_poll(&ctx, 16.0).expect("movie step");
    assert_eq!(env.eval_int("mov.isPlayingMovie()"), 1);
    assert_eq!(env.scene.layers.len(), 1);
    assert_eq!(env.scene.bitmaps.len(), 1);

    // The movie layer is also the window's primary layer, so drop both script
    // references while the movie is still playing, then let the idle collector
    // run outside the creating frame (the frame body's automatic idle pass,
    // `reference/cpp/core/environ/impl/SystemControl.cpp:174-179`). The slot
    // must not keep the layer alive after the script references are gone.
    env.run("mov = null; w = null;");
    env.run("System.doCompact();");
    env.engine.do_gc().expect("idle compact");

    assert!(
        env.scene.layers.is_empty(),
        "the movie layer must not stay in the scene: {:?}",
        env.scene.layers.iter().map(|l| l.id).collect::<Vec<_>>()
    );
    assert!(
        env.scene.bitmaps.is_empty(),
        "the layer's main image must be freed with it"
    );
    // The frame body keeps running after the layer is gone.
    movie_poll(&ctx, 16.0).expect("movie step after teardown");
}

// ---------------------------------------------------------------------------
// layerExDraw: GDI+ value types and the Layer.draw* extension
// ---------------------------------------------------------------------------

use std::path::{Path, PathBuf};

use engine::Storage;
use fs::FileSystem;

/// System fonts tried by the text tests; a missing/unusable font skips the
/// font-dependent cases rather than failing (mirrors `tests/font.rs`).
const TEST_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

fn test_font() -> Option<(PathBuf, String)> {
    for candidate in TEST_FONT_PATHS {
        let path = Path::new(candidate);
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if engine::text::TextEngine::new().add_font(&bytes, 0).is_err() {
            continue;
        }
        let dir = path.parent()?.to_path_buf();
        let name = path.file_name()?.to_string_lossy().into_owned();
        return Some((dir, name));
    }
    None
}

/// A bound VM + host + scene + optional font storage for the text cases.
struct FontEnv {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    _storage: Option<Box<Storage>>,
}

impl FontEnv {
    fn new(storage: Option<Storage>) -> FontEnv {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = storage.map(Box::new);
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene/storage outlive the host (declared after it).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            if let Some(storage) = storage.as_mut() {
                host.services_mut()
                    .attach_storage(storage.as_mut() as *mut Storage);
            }
        }
        host.install(&engine).expect("install compat surface");
        FontEnv {
            _host: host,
            engine,
            scene,
            _storage: storage,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script_discard(script, "layer-ex-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_real(&self, expression: &str) -> f64 {
        match self.engine.eval(expression, "layer-ex-test") {
            Ok(TjsValue::Real(value)) => value,
            Ok(TjsValue::Integer(value)) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

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

#[test]
fn pointf_and_rectf_value_types_work() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    env.run(
        "var p = new GdiPlus.PointF(1.5, 2.5); \
         var q = new GdiPlus.PointF(1.5, 2.5); \
         var r = new GdiPlus.RectF(1, 2, 3, 4);",
    );
    assert_eq!(env.eval_int("p.x == 1.5 && p.y == 2.5"), 1);
    assert_eq!(env.eval_int("p.Equals(q)"), 1);
    assert_eq!(env.eval_int("p.Equals(new GdiPlus.PointF(0, 0))"), 0);

    assert_eq!(env.eval_int("r.left == 1 && r.top == 2"), 1);
    assert_eq!(env.eval_int("r.right == 4 && r.bottom == 6"), 1);
    assert_eq!(env.eval_int("r.width == 3 && r.height == 4"), 1);
    assert_eq!(env.eval_int("r.IsEmptyArea()"), 0);
    assert_eq!(env.eval_int("r.location.x == 1 && r.location.y == 2"), 1);
    assert_eq!(
        env.eval_int("r.bounds.width == 3 && r.bounds.height == 4"),
        1
    );
    assert_eq!(env.eval_int("r.Clone().x == 1"), 1);
    assert_eq!(
        env.eval_int("r.IntersectsWith(new GdiPlus.RectF(0, 0, 10, 10))"),
        1
    );
    assert_eq!(
        env.eval_int("r.IntersectsWith(new GdiPlus.RectF(9, 9, 1, 1))"),
        0
    );
    // `Union` is the class-level static
    // `static bool Union(RectF &ret, const RectF &a, const RectF &b)`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:389-397`,
    // registered at `windows/main.cpp:210`), so the bounding rectangle is
    // written through the first argument rather than returned by an instance
    // call.
    env.run(
        "var ret = new GdiPlus.RectF(0, 0, 0, 0); \
         var ua = new GdiPlus.RectF(0, 0, 1, 1); \
         var ub = new GdiPlus.RectF(2, 2, 1, 1); \
         GdiPlus.RectF.Union(ret, ua, ub);",
    );
    assert_eq!(env.eval_int("ret.width == 3 && ret.height == 3"), 1);

    env.run("r.Inflate(1, 1);");
    assert_eq!(env.eval_int("r.x == 0 && r.width == 5"), 1);
    env.run("r.Offset(2, 3);");
    assert_eq!(env.eval_int("r.x == 2 && r.y == 4"), 1);
}

#[test]
fn appearance_supports_solid_and_hatch_brushes() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    env.run(
        "var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.drawRectangle(app, 0, 0, 4, 4);",
    );
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);

    env.run("dst.clear(0); app.clear();");
    assert_eq!(env.pixel(dst, 1, 1), [0, 0, 0, 0]);

    // A hatch brush paints both fore and back colours.
    env.run(
        "app.addBrush(%[type: 1, hatchStyle: 2, foreColor: 0xFFFF0000, backColor: 0xFF0000FF], 0, 0); \
         dst.drawRectangle(app, 0, 0, 4, 4);",
    );
    let mut red = 0;
    let mut blue = 0;
    for y in 0..4 {
        for x in 0..4 {
            let pixel = env.pixel(dst, x, y);
            if pixel[0] > 200 && pixel[2] < 50 {
                red += 1;
            }
            if pixel[2] > 200 && pixel[0] < 50 {
                blue += 1;
            }
        }
    }
    assert!(red > 0 && blue > 0, "hatch should paint both colors");
}

#[test]
fn path_value_type_builds_and_draws() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    env.run(
        "var p = new GdiPlus.Path(); \
         p.drawRectangle(1, 1, 2, 2); \
         var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFF00FF00, 0, 0); \
         dst.drawPath(app, p);",
    );
    assert_eq!(env.pixel(dst, 2, 2), [0, 255, 0, 255]);
    // The corner was never drawn on: it holds the reference's default image
    // (transparent white, `reference/cpp/core/visual/LayerIntf.cpp:104-110,400`).
    assert_eq!(env.pixel(dst, 0, 0), [255, 255, 255, 0]);

    // The remaining Path members are callable and mutate the path.
    env.run(
        "var q = new GdiPlus.Path(); \
         q.startFigure(); q.drawLine(0, 0, 3, 3); q.closeFigure(); \
         q.drawArc(0, 0, 4, 4, 0, 90); \
         q.drawPie(0, 0, 4, 4, 0, 90); \
         q.drawBezier(0, 0, 1, 2, 3, 2, 4, 0); \
         q.drawBeziers([[0,0],[1,2],[3,2],[4,0]]); \
         q.drawCurve([[0,0],[1,2],[3,2]]); \
         q.drawCurve2([[0,0],[1,2],[3,2]], 0.5); \
         q.drawCurve3([[0,0],[1,2],[3,2],[4,0]], 0, 2, 0.5); \
         q.drawClosedCurve([[0,0],[1,2],[3,2]]); \
         q.drawClosedCurve2([[0,0],[1,2],[3,2]], 0.5); \
         q.drawEllipse(0, 0, 4, 4); \
         q.drawLines([[0,0],[1,1],[2,0]]); \
         q.drawPolygon([[0,0],[2,0],[1,2]]); \
         q.drawRectangles([new GdiPlus.RectF(0,0,1,1), [1,1,2,2]]);",
    );
    env.run("dst.drawPath(app, q);");
}

#[test]
fn layer_clear_and_strokes_work() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    env.run("dst.clear(0xFF0000FF);");
    assert_eq!(env.pixel(dst, 3, 3), [0, 0, 255, 255]);

    env.run(
        "dst.clear(0); \
         var app = new GdiPlus.Appearance(); \
         app.addPen(0xFF00FF00, 1, 0, 0); \
         dst.drawLine(app, 0, 2, 4, 2);",
    );
    let on_line = env.pixel(dst, 2, 2);
    assert!(on_line[1] > 200, "green stroke expected, got {on_line:?}");
    assert!(on_line[3] > 0);
    assert_eq!(env.pixel(dst, 0, 0), [0, 0, 0, 0]);
}

#[test]
fn layer_draws_other_shapes() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    env.run(
        "var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.drawEllipse(app, 0, 0, 4, 4); \
         dst.drawPolygon(app, [[0,0],[4,0],[2,4]]); \
         dst.drawPie(app, 0, 0, 4, 4, 0, 90); \
         dst.drawBezier(app, 0, 0, 1, 4, 3, 4, 4, 0); \
         dst.drawBeziers(app, [[0,0],[1,2],[3,2],[4,0]]); \
         dst.drawClosedCurve(app, [[0,0],[1,2],[3,2]]); \
         dst.drawCurve(app, [[0,0],[1,2],[3,2]]); \
         dst.drawLines(app, [[0,0],[1,1],[2,0]]); \
         dst.drawRectangle(app, 1, 1, 2, 2);",
    );
    assert_eq!(env.pixel(dst, 2, 2), [255, 0, 0, 255]);
}

/// The reference `drawPolygon` resolves its paint source through the
/// native-instance lookup (`ncbInstanceAdaptor<Appearance>::GetNativeInstance`,
/// the `NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)` behind
/// `TJS_GET_NATIVE_INSTANCE`; `reference/cpp/plugins/layerex_draw/windows/
/// main.cpp:596,872-902`), never through a script-visible member. The port
/// therefore exposes no `__handle` member, and every `draw*` method keeps
/// working without one.
#[test]
fn layer_shape_draws_use_the_native_instance_not_a_hidden_property() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // No hidden handle member: the native instance is looked up on the
    // argument itself.
    assert_eq!(
        env.eval_int("typeof ((new GdiPlus.Appearance()).__handle) == 'undefined'"),
        1
    );
    assert_eq!(
        env.eval_int("typeof ((new GdiPlus.Path()).__handle) == 'undefined'"),
        1
    );
    env.run(
        "var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.drawPolygon(app, [[0,0],[4,0],[2,4]]);",
    );
    // (2, 1) is the triangle's centroid pixel, fully inside the fill. (2, 2)
    // sits on the anti-aliased right edge (the (4,0)-(2,4) edge cuts it to 75%
    // coverage), so it reads a partial alpha.
    assert_eq!(env.pixel(dst, 2, 1), [255, 0, 0, 255]);

    // Every `draw*` method shares the same Appearance lookup.
    env.run(
        "dst.clear(0); \
         dst.drawArc(app, 0, 0, 4, 4, 0, 360); \
         dst.drawEllipse(app, 0, 0, 4, 4); \
         dst.drawRectangle(app, 1, 1, 2, 2); \
         dst.drawLines(app, [[0,0],[1,1],[2,0]]);",
    );
    assert_eq!(env.pixel(dst, 2, 2), [255, 0, 0, 255]);

    // `drawPath` reads the Appearance and the Path the same way.
    env.run(
        "dst.clear(0); \
         var p = new GdiPlus.Path(); \
         p.drawRectangle(1, 1, 2, 2); \
         dst.drawPath(app, p);",
    );
    assert_eq!(env.pixel(dst, 2, 2), [255, 0, 0, 255]);
}

/// The real game's `MessageFrame.tjs` `ResizeButton` constructor
/// (system/MessageFrame.tjs:1920-1931): `class ResizeButton extends Layer`
/// with a legacy-style bare `Layer(win, par)` base-constructor call, an
/// `Appearance` built from the layer's own `width`/`height`, and a bare
/// `drawPolygon(app, points)`. The reference accepts it: `drawPolygon` is
/// registered on the `LayerExDraw` attach
/// (`reference/cpp/plugins/layerex_draw/windows/main.cpp:902`) and its
/// implementation reads the `Appearance`
/// (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1392`); the
/// port must too.
#[test]
fn layer_draw_polygon_accepts_the_game_resize_button_shape() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    env.run(
        "class ResizeButton extends Layer { \
             function ResizeButton(win, par) { \
                 Layer(win, par); \
                 setSize(16, 16); \
                 var app = new GdiPlus.Appearance(); \
                 app.addBrush(0xFFFF0000, 0, 0); \
                 drawPolygon(app, [[width\\2, 0], [width, 0], [width, height], [0, height], [0, height\\2]]); \
             } \
         } \
         var rb = new ResizeButton(w, dst); \
         rb.name = 'resize'; \
         rb.visible = true;",
    );
    let resize = env.layer_id("resize");
    // The pentagon covers its center; the cut corner (0, 0) stays clear.
    assert_eq!(env.pixel(resize, 8, 8), [255, 0, 0, 255]);
    assert_eq!(env.pixel(resize, 0, 0), [255, 255, 255, 0]);
}

/// A genuinely wrong argument still raises a typed error. The reference
/// converts the argument through the `GetNativeInstance` adaptor
/// (`reference/cpp/plugins/layerex_draw/windows/main.cpp:105-113`, registered
/// at `:902`), which rejects an object of the wrong class; it never panics and
/// never accepts an arbitrary value.
#[test]
fn layer_draw_rejects_non_appearance_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let error = env
        .eval("dst.drawPolygon(42, [[0,0],[4,0],[2,4]])")
        .expect_err("a number is not an Appearance");
    assert!(error.contains("Appearance"), "unexpected message: {error}");
    let error = env
        .eval("dst.drawPolygon(%[x:1], [[0,0],[4,0],[2,4]])")
        .expect_err("a plain object is not an Appearance");
    assert!(error.contains("Appearance"), "unexpected message: {error}");
    env.run("var app = new GdiPlus.Appearance();");
    let error = env
        .eval("dst.drawPath(app, 42)")
        .expect_err("a number is not a Path");
    assert!(error.contains("Path"), "unexpected message: {error}");
}

#[test]
fn layer_transforms_apply_to_drawing() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    env.run(
        "var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.translateTransform(2, 2); \
         dst.drawRectangle(app, 0, 0, 2, 2);",
    );
    assert_eq!(env.pixel(dst, 3, 3), [255, 0, 0, 255]);
    // The translated draw leaves the origin at the reference's default image
    // (transparent white, `reference/cpp/core/visual/LayerIntf.cpp:104-110,400`).
    assert_eq!(env.pixel(dst, 0, 0), [255, 255, 255, 0]);
    env.run("dst.resetTransform(); dst.translateViewTransform(1, 1); dst.smoothingMode = 4;");
    assert_eq!(env.eval_int("dst.smoothingMode == 4"), 1);
    assert_eq!(env.eval_int("dst.updateWhenDraw"), 1);
}

#[test]
fn layer_draws_images() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // `src` is opaque red 4x4.
    env.run("dst.drawImage(0, 0, src);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);
    env.run("dst.clear(0); dst.drawImageRect(0, 0, src, 1, 1, 2, 2);");
    assert_eq!(env.pixel(dst, 0, 0), [255, 0, 0, 255]);
    env.run("dst.clear(0); dst.drawImageStretch(0, 0, 4, 4, src, 0, 0, 2, 2);");
    assert_eq!(env.pixel(dst, 3, 3), [255, 0, 0, 255]);
    env.run("dst.clear(0); dst.drawImageAffine(src, 0, 0, 4, 4, true, 1, 0, 0, 1, 0, 0);");
    assert_eq!(env.pixel(dst, 2, 2), [255, 0, 0, 255]);
}

#[test]
fn layer_records_and_replays() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    let record_path = std::env::temp_dir().join("krkr_layer_ex_record.kexr");
    let record_path_str = record_path.to_string_lossy().replace('\\', "\\\\");
    env.run(
        "var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.record = true; \
         dst.drawRectangle(app, 0, 0, 2, 2);",
    );
    assert_eq!(env.eval_int("dst.record"), 1);
    // The recorded image renders the rectangle.
    env.run("var rec = dst.getRecordImage();");
    // The record wrapper has no `id` member (the reference `Bitmap` surface);
    // the record image is the newest scene bitmap `new Bitmap` created.
    let rec_id = env.scene.bitmaps.last().expect("record bitmap").id;
    let red = env.pixel_bitmap(rec_id, 1, 1);
    assert_eq!(red, [255, 0, 0, 255]);

    // Saving and loading round-trips the recording.
    env.run(&format!("dst.saveRecord('{record_path_str}');"));
    assert!(record_path.exists(), "record file should be written");
    env.run("dst.clear(0); dst.record = false;");
    assert_eq!(env.pixel(dst, 1, 1), [0, 0, 0, 0]);
    env.run(&format!("dst.loadRecord('{record_path_str}');"));
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);
    let _ = std::fs::remove_file(&record_path);
}

#[test]
fn font_info_properties_work() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    env.run("var f = new GdiPlus.Font('Ghost', 20, 3);");
    assert_eq!(env.eval_int("f.emSize == 20"), 1);
    assert_eq!(env.eval_int("f.style == 3"), 1);
    assert_eq!(env.eval_int("f.forceSelfPathDraw == 0"), 1);
    env.run("f.familyName = 'Other'; f.emSize = 12; f.style = 1; f.forceSelfPathDraw = true;");
    assert_eq!(env.eval_int("f.forceSelfPathDraw == 1"), 1);
    assert_eq!(env.eval_int("f.style == 1"), 1);
}

/// The five `FontInfo` metrics follow `FontInfo::updateSizeParams`
/// (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:373-379`):
/// `ascent`/`descent` are the Windows cell metrics, `lineSpacing` is their
/// sum (`tmHeight`), and the leading values are the cell metrics minus the
/// font's maximum glyph-box extents. The host font decides the absolute
/// numbers, so the test compares against the same L1 metrics the plugin reads
/// instead of hard-coding values.
#[test]
fn fontinfo_metrics_follow_the_reference_formulas() {
    let Some((dir, name)) = test_font() else {
        return;
    };
    let bytes = std::fs::read(dir.join(&name)).expect("read the test font");
    let mut engine = engine::text::TextEngine::new();
    let id = engine.add_font(&bytes, 0).expect("register the test font");
    let l1 = engine.font_metrics(id, 16.0).expect("font metrics");

    let _lock = vm_lock();
    let storage = Storage::mount_archives(
        FileSystem::host(&dir),
        dir.clone(),
        std::iter::empty::<&str>(),
    )
    .expect("mount font directory");
    let env = FontEnv::new(Some(storage));
    env.run(&format!("var f = new GdiPlus.Font('{name}', 16, 0);"));

    let ascent = env.eval_real("f.ascent");
    let descent = env.eval_real("f.descent");
    let ascent_leading = env.eval_real("f.ascentLeading");
    let descent_leading = env.eval_real("f.descentLeading");
    let line_spacing = env.eval_real("f.lineSpacing");

    assert!(ascent > 0.0, "the face should expose cell metrics");
    // tmHeight == tmAscent + tmDescent.
    assert_eq!(line_spacing, ascent + descent);
    // tmAscent/tmDescent are the OS/2 Windows cell metrics.
    assert_eq!(ascent, f64::from(l1.win_ascent));
    assert_eq!(descent, f64::from(l1.win_descent));
    // ascentLeading = tmAscent - otmAscent; descentLeading = tmDescent +
    // otmDescent (`otmDescent` is negative).
    assert_eq!(
        ascent_leading,
        f64::from(l1.win_ascent) - f64::from(l1.otm_ascent)
    );
    assert_eq!(
        descent_leading,
        f64::from(l1.win_descent) + f64::from(l1.otm_descent)
    );
}

#[test]
fn layer_measures_and_draws_text_with_a_font() {
    let Some((dir, name)) = test_font() else {
        return;
    };
    let _lock = vm_lock();
    let storage = Storage::mount_archives(
        FileSystem::host(&dir),
        dir.clone(),
        std::iter::empty::<&str>(),
    )
    .expect("mount font directory");
    let env = FontEnv::new(Some(storage));
    // Build the script with the font file name embedded safely.
    let script = format!(
        "var w = new Window(); var dst = new Layer(w, null); \
         dst.setImageSize(64, 32); var f = new GdiPlus.Font('{name}', 16, 0);"
    );
    env.run(&script);
    env.run("var measured = dst.measureString(f, 'Hello');");
    assert!(env.eval_real("measured.width") > 0.0);
    assert!(env.eval_real("measured.height") > 0.0);
    env.run("var inner = dst.measureStringInternal(f, 'Hello');");
    assert!(env.eval_real("inner.width") > 0.0);

    env.run(
        "var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.drawString(f, app, 0, 12, 'Hello');",
    );
    // The reference surface has no `Layer.id` member; the id comes from the
    // Rust-side scene, whose only layer is `dst`.
    let dst_id = env.scene.layers[0].id;
    let mut any_ink = false;
    for y in 0..32 {
        for x in 0..64 {
            if env.pixel(dst_id, x, y)[3] > 0 {
                any_ink = true;
            }
        }
    }
    assert!(any_ink, "drawString should composite glyphs");
}

/// A host-decoded movie source with an audio track, so the `Layer.openMovie`
/// audio path can be driven without a real movie file.
struct MovieAudioSource {
    spec: VideoSpec,
    frames: Vec<RgbaFrame>,
    audio: DecodedAudio,
}

impl compat::VideoSource for MovieAudioSource {
    fn spec(&self, _path: &str) -> Option<VideoSpec> {
        Some(self.spec)
    }

    fn frame_at(&mut self, _path: &str, at_ms: i64) -> Option<Arc<RgbaFrame>> {
        let mut chosen = self.frames.first()?.clone();
        for frame in &self.frames {
            if frame.pts_ms as i64 <= at_ms {
                chosen = frame.clone();
            } else {
                break;
            }
        }
        Some(Arc::new(chosen))
    }

    fn audio(&mut self, _path: &str) -> Result<Option<Arc<DecodedAudio>>, MediaError> {
        Ok(Some(Arc::new(self.audio.clone())))
    }
}

/// A 4x4 @ 10 fps, 10-frame movie with one 48 kHz stereo audio stream.
fn movie_audio_spec() -> VideoSpec {
    VideoSpec {
        width: 4,
        height: 4,
        fps: 10.0,
        total_frames: Some(10),
        sample_rate: 48_000,
        channels: 2,
        audio_streams: 1,
        video_streams: 1,
        audio_stream: 0,
        video_stream: 0,
    }
}

/// A 4x4 opaque red frame at timestamp 0.
fn movie_audio_frame() -> RgbaFrame {
    let mut rgba = Vec::with_capacity(4 * 4 * 4);
    for _ in 0..(4 * 4) {
        rgba.extend_from_slice(&[255, 0, 0, 255]);
    }
    RgbaFrame::new(4, 4, 0, rgba)
}

/// A movie opened through `Layer.openMovie` plays its decoded audio on the one
/// shared mixer at the device defaults, and `stopMovie` releases it.
///
/// Reference: the reference `layerExMovie` drives the movie player's own sound
/// device, whose buffer starts at `_volume = 1` and `_pan = 0`
/// (`reference/cpp/core/sound/win32/WaveMixer.cpp:126-127`) and which
/// `layerExMovie` never adjusts (it exposes no volume/pan members,
/// `reference/cpp/plugins/layerExMovie.cpp:154-227`). The player is parked at
/// `DVD_PLAYSPEED_PAUSE` on open
/// (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:309`) and `Play` raises it
/// to speed 1 (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:201-205`), so
/// `startMovie` is where the audio starts; `stopMovie` releases the player
/// through `clearMovie()` (`reference/cpp/plugins/layerExMovie.cpp:214-227`).
#[test]
fn movie_audio_reaches_the_mixer_at_the_default_gain() {
    let _lock = vm_lock();
    let env = Env::new();
    let ctx = env._host.ctx().expect("bound context");
    compat::set_movie_source(
        &ctx,
        Box::new(MovieAudioSource {
            spec: movie_audio_spec(),
            frames: vec![movie_audio_frame()],
            audio: DecodedAudio::new(
                AudioSpec {
                    sample_rate: 48_000,
                    channels: 2,
                    total_frames: Some(1000),
                },
                vec![0.5; 2000],
            ),
        }),
    );
    env.run(
        "var w = new Window(); var l = new Layer(w, null); l.name = 'movie'; \
         l.setImageSize(4, 4); l.openMovie('movie.mpg', false);",
    );
    // Opening installs the track but the player parks at
    // `DVD_PLAYSPEED_PAUSE` (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:309`),
    // so nothing is audible yet.
    let out_spec = AudioSpec {
        sample_rate: 48_000,
        channels: 2,
        total_frames: None,
    };
    let mut parked = vec![0.0f32; 200];
    compat::sound_render(&ctx, &mut parked, out_spec).expect("render");
    assert!(
        parked.iter().all(|sample| sample.abs() < 1e-6),
        "the movie's audio started before startMovie"
    );

    // `Play` raises the speed to 1
    // (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:201-205`), so
    // `startMovie` is where the audio starts.
    env.run("l.startMovie(false);");
    compat::movie_poll(&ctx, 10.0).expect("poll");

    let mut out = vec![0.0f32; 200];
    compat::sound_render(&ctx, &mut out, out_spec).expect("render");
    assert!(
        (out[0] - 0.5).abs() < 1e-3,
        "the left channel is not at the unity gain: {}",
        out[0]
    );
    assert!(
        (out[1] - 0.5).abs() < 1e-3,
        "the right channel is not at the centre pan: {}",
        out[1]
    );

    env.run("l.stopMovie();");
    let mut stopped = vec![0.0f32; 200];
    compat::sound_render(&ctx, &mut stopped, out_spec).expect("render");
    assert!(
        stopped.iter().all(|sample| sample.abs() < 1e-6),
        "stopMovie left the movie's voice on the mixer"
    );
}

/// A decode failure while opening a movie's audio is a typed error, not
/// silence: the reference `openMovie` returns on a failed stream open
/// (`reference/cpp/plugins/layerExMovie.cpp:154-166`) and never plays a
/// substitute.
#[test]
fn movie_audio_decode_failure_raises() {
    struct FailingAudio {
        spec: VideoSpec,
        frames: Vec<RgbaFrame>,
    }

    impl compat::VideoSource for FailingAudio {
        fn spec(&self, _path: &str) -> Option<VideoSpec> {
            Some(self.spec)
        }

        fn frame_at(&mut self, _path: &str, at_ms: i64) -> Option<Arc<RgbaFrame>> {
            let _ = at_ms;
            self.frames.first().cloned().map(Arc::new)
        }

        fn audio(&mut self, _path: &str) -> Result<Option<Arc<DecodedAudio>>, MediaError> {
            Err(MediaError::Decode {
                format: "mp3".to_string(),
                message: "corrupt audio frame".to_string(),
            })
        }
    }

    let _lock = vm_lock();
    let env = Env::new();
    let ctx = env._host.ctx().expect("bound context");
    compat::set_movie_source(
        &ctx,
        Box::new(FailingAudio {
            spec: movie_audio_spec(),
            frames: vec![movie_audio_frame()],
        }),
    );
    env.run(
        "var w = new Window(); var l = new Layer(w, null); var caught = 0; \
         try { l.openMovie('movie.mpg', false); } catch (e) { caught = 1; }",
    );
    assert_eq!(env.eval_int("caught"), 1);
}

/// Records the storage name each `openMovie` asks its host source for.
struct PathRecordingSource {
    spec: VideoSpec,
    frames: Vec<RgbaFrame>,
    seen: Rc<RefCell<Vec<String>>>,
}

impl compat::VideoSource for PathRecordingSource {
    fn spec(&self, path: &str) -> Option<VideoSpec> {
        self.seen.borrow_mut().push(path.to_string());
        Some(self.spec)
    }

    fn frame_at(&mut self, _path: &str, at_ms: i64) -> Option<Arc<RgbaFrame>> {
        let _ = at_ms;
        self.frames.first().cloned().map(Arc::new)
    }
}

/// `Layer.openMovie` strips the `file:` media prefix before it asks the host
/// source, and the resolved name is what the frame body reuses.
///
/// Reference: `layerExMovie::openMovie` opens the stream through
/// `TVPCreateStream(filename, TJS_BS_READ)`
/// (`reference/cpp/plugins/layerExMovie.cpp:159-161`), which normalizes the
/// media name (`reference/cpp/core/base/StorageIntf.cpp:240-430`), so the
/// decoded source sees the bare name; the overlay keeps that same name for
/// `GetVideoBuffer`/`SetVideoBuffer` and the frame body
/// (`reference/cpp/plugins/layerExMovie.cpp:154-196`).
#[test]
fn open_movie_strips_the_file_media_prefix() {
    let _lock = vm_lock();
    let env = Env::new();
    let ctx = env._host.ctx().expect("bound context");
    let seen = Rc::new(RefCell::new(Vec::new()));
    compat::set_movie_source(
        &ctx,
        Box::new(PathRecordingSource {
            spec: movie_audio_spec(),
            frames: vec![movie_audio_frame()],
            seen: Rc::clone(&seen),
        }),
    );
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         l.setImageSize(4, 4); l.openMovie('file:movie.mpg', false);",
    );
    let seen = seen.borrow();
    assert_eq!(seen.len(), 1, "the host source was asked more than once");
    assert_eq!(
        seen[0], "movie.mpg",
        "the host source did not see the stripped storage name"
    );
}
