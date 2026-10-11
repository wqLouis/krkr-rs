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

use common::vm_lock;
use compat::CompatHost;
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
            .exec_script(script, "layer-ex-test")
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
            .exec_script(script, "layer-ex-test")
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
