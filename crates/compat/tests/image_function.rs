//! Integration tests for the `ImageFunction` native class.
//!
//! `ImageFunction` is a stateless service class: `new ImageFunction()`
//! allocates an empty instance and every operation is a static method taking
//! the destination `Bitmap` as its first argument. The tests build a real VM
//! with a caller-owned L2 [`Scene`] attached — the same seam the app uses —
//! so the pixel assertions read back what the L2 operations actually wrote.
//!
//! A pixel getter hands the colour back as the packed word of the RGBA
//! planes, read little-endian: the red is in the low byte, the green in the
//! second and the blue in the third
//! (`reference/cpp/core/visual/BitmapIntf.cpp:51-58`,
//! `reference/cpp/core/visual/LayerBitmapIntf.cpp:165-171`,
//! `reference/cpp/core/visual/RenderManager.cpp:346-350`).

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached scene.
///
/// Fields drop in declaration order: the host first (releasing every retained
/// script object and removing the binding), then the engine, then the scene.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Kept alive for the host's raw pointer; never read directly.
    _scene: Box<Scene>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene outlives the host.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            _scene: scene,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "image-function-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "image-function-test")
            .map_err(|error| error.to_string())
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            Ok(TjsValue::Integer(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }
}

#[test]
fn constructor_is_empty_and_the_static_surface_is_present() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var f = new ImageFunction();");
    // Reference: `ImageFunction`'s native members are registered in
    // `reference/cpp/core/visual/ImageFunction.cpp` (`operateAffine`
    // `:188-308`, `operateRect` `:310-406`, `operateStretch` `:408-529`,
    // `flipLR` `:531-569`, `flipUD` `:571-609`, `adjustGamma` `:611-683`,
    // `doBoxBlur` `:685-745`, `doGrayScale` `:747-785`, `fillRect` `:787-859`,
    // `colorRect` `:861-965`, `drawText` `:967-1078`, `drawGlyph` `:1080-1179`).
    for member in [
        "operateAffine",
        "operateRect",
        "operateStretch",
        "flipLR",
        "flipUD",
        "adjustGamma",
        "doBoxBlur",
        "doGrayScale",
        "fillRect",
        "colorRect",
        "drawText",
        "drawGlyph",
    ] {
        let probe = format!("ImageFunction.{member}");
        assert!(env.eval(&probe).is_ok(), "{member} is not registered");
    }
}

#[test]
fn fill_rect_replaces_rgb_and_alpha_and_reports_the_region() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var b = new Bitmap(4, 4);");
    // `isalpha = true` is the reference `Fill` (a straight ARGB copy)
    // (`reference/cpp/core/visual/ImageFunction.cpp:787-859`).
    env.run("var r = ImageFunction.fillRect(b, 0xff112233, new Rect(1, 1, 3, 3), true, null);");
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0);
    assert_eq!(env.eval_int("b.getPixel(1, 1)"), 0x33_2211);
    assert_eq!(env.eval_int("b.getPixel(2, 2)"), 0x33_2211);
    assert_eq!(env.eval_int("b.getPixel(3, 3)"), 0);
    // The alpha plane the fill wrote is a separate member
    // (`reference/cpp/core/visual/BitmapIntf.cpp:67-75`).
    assert_eq!(env.eval_int("b.getMaskPixel(1, 1)"), 255);
    // The returned update region is a real `Rect`.
    assert_eq!(env.eval_int("r.left"), 1);
    assert_eq!(env.eval_int("r.top"), 1);
    assert_eq!(env.eval_int("r.right"), 3);
    assert_eq!(env.eval_int("r.bottom"), 3);
}

#[test]
fn fill_rect_without_alpha_holds_the_destination_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 1); \
         b.setPixel(0, 0, 0x00ff00); \
         b.setMaskPixel(0, 0, 0x80);",
    );
    // `isalpha = false` maps to `FillColor`, which holds alpha
    // (`reference/cpp/core/visual/ImageFunction.cpp:841-845`). The colour
    // goes through the system-colour table first, so `0xff0000ff` keeps its
    // low 24 bits with the red and blue bytes swapped.
    env.run("ImageFunction.fillRect(b, 0xff0000ff, null, false, null);");
    // RGB becomes the resolved red; the 0x80 alpha is preserved. The
    // packed word holds the red in its low byte.
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x00_00ff);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 0x80);
}

#[test]
fn color_rect_mask_face_writes_the_alpha_channel() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var b = new Bitmap(1, 1); b.setPixel(0, 0, 0x102030);");
    // `dfMask` replaces the alpha plane from the low byte (blue)
    // (`reference/cpp/core/visual/ImageFunction.cpp:861-965`).
    env.run("ImageFunction.colorRect(b, 0x0000007f, 255, null, dfMask, null);");
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x30_2010);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 0x7f);
}

#[test]
fn color_rect_opaque_face_sets_rgb_and_holds_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 1); \
         b.setPixel(0, 0, 0x112233); \
         b.setMaskPixel(0, 0, 0x80);",
    );
    // `dfOpaque` holds the destination alpha
    // (`reference/cpp/core/visual/ImageFunction.cpp:861-965`). The colour
    // goes through the system-colour table first
    // (`reference/cpp/core/visual/ImageFunction.cpp:939`), so `0xff445566`
    // keeps its low 24 bits with the red and blue bytes swapped.
    env.run("ImageFunction.colorRect(b, 0xff445566, 255, null, dfOpaque, null);");
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x44_5566);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 0x80);
}

#[test]
fn flip_lr_mirrors_a_region_in_place() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(4, 1); \
         b.setPixel(0, 0, 0x000001); b.setPixel(1, 0, 0x000002); \
         b.setPixel(2, 0, 0x000003); b.setPixel(3, 0, 0x000004);",
    );
    env.run("ImageFunction.flipLR(b, new Rect(1, 0, 3, 1));");
    // Reference: `flipLR` `reference/cpp/core/visual/ImageFunction.cpp:531-569`.
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x01_0000);
    assert_eq!(env.eval_int("b.getPixel(1, 0)"), 0x03_0000);
    assert_eq!(env.eval_int("b.getPixel(2, 0)"), 0x02_0000);
    assert_eq!(env.eval_int("b.getPixel(3, 0)"), 0x04_0000);
}

#[test]
fn flip_ud_mirrors_the_full_bitmap() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 3); \
         b.setPixel(0, 0, 0x000001); b.setPixel(0, 1, 0x000002); \
         b.setPixel(0, 2, 0x000003);",
    );
    env.run("ImageFunction.flipUD(b, null);");
    // Reference: `flipUD` `reference/cpp/core/visual/ImageFunction.cpp:571-609`.
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x03_0000);
    assert_eq!(env.eval_int("b.getPixel(0, 1)"), 0x02_0000);
    assert_eq!(env.eval_int("b.getPixel(0, 2)"), 0x01_0000);
}

#[test]
fn flip_rejects_an_out_of_bounds_rectangle() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var b = new Bitmap(2, 2);");
    assert!(
        env.eval("ImageFunction.flipLR(b, new Rect(0, 0, 5, 5))")
            .is_err()
    );
}

#[test]
fn do_gray_scale_uses_the_reference_grey() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 1); \
         b.setPixel(0, 0, 0xff0000); \
         b.setMaskPixel(0, 0, 255);",
    );
    env.run("ImageFunction.doGrayScale(b, null);");
    // The reference's grey is `(19*R + 183*G + 54*B) >> 8`: the packed
    // word's low byte is the red plane and carries the weight 19, its
    // third byte is the blue plane and carries 54
    // (`reference/cpp/core/visual/tvpgl.cpp:10359-10365`), so pure red is
    // `(19*255) >> 8 == 18 == 0x12`.
    // `doGrayScale` itself is `reference/cpp/core/visual/ImageFunction.cpp:747-785`.
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x12_1212);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 255);
}

#[test]
fn adjust_gamma_with_identity_data_is_a_no_op() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 1); \
         b.setPixel(0, 0, 0x808080); \
         b.setMaskPixel(0, 0, 0x80);",
    );
    // Identity gamma is a no-op (`reference/cpp/core/visual/ImageFunction.cpp:611-683`).
    env.run("ImageFunction.adjustGamma(b, 1.0, 0, 255, 1.0, 0, 255, 1.0, 0, 255, null, false);");
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x80_8080);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 0x80);
}

#[test]
fn do_box_blur_averages_over_the_kernel() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(3, 1); \
         b.setPixel(0, 0, 0x000000); b.setMaskPixel(0, 0, 255); \
         b.setPixel(1, 0, 0xffffff); b.setMaskPixel(1, 0, 255); \
         b.setPixel(2, 0, 0x000000); b.setMaskPixel(2, 0, 255);",
    );
    env.run("ImageFunction.doBoxBlur(b, 1, 0, null, true);");
    // A 1x3 horizontal box average of (0, 255, 0) is 85 == 0x55
    // (`reference/cpp/core/visual/ImageFunction.cpp:685-745`).
    assert_eq!(env.eval_int("b.getPixel(1, 0)"), 0x55_5555);
    assert_eq!(env.eval_int("b.getMaskPixel(1, 0)"), 255);
}

#[test]
fn operate_rect_copies_a_source_region() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var dst = new Bitmap(2, 2); var src = new Bitmap(1, 1); \
         src.setPixel(0, 0, 0x112233); src.setMaskPixel(0, 0, 255);",
    );
    // `omOpaque` + `dfOpaque` is the reference `bmCopy` (alpha included)
    // (`reference/cpp/core/visual/ImageFunction.cpp:310-406`).
    env.run(
        "ImageFunction.operateRect(dst, 1, 1, src, null, null, omOpaque, dfOpaque, 255, false);",
    );
    assert_eq!(env.eval_int("dst.getPixel(1, 1)"), 0x33_2211);
    assert_eq!(env.eval_int("dst.getMaskPixel(1, 1)"), 255);
    assert_eq!(env.eval_int("dst.getPixel(0, 0)"), 0);
}

#[test]
fn operate_stretch_scales_into_the_destination_rect() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var dst = new Bitmap(2, 2); var src = new Bitmap(1, 1); \
         src.setPixel(0, 0, 0x00ff00); src.setMaskPixel(0, 0, 255);",
    );
    env.run(
        "ImageFunction.operateStretch(dst, src, new Rect(0, 0, 2, 2), new Rect(0, 0, 1, 1), \
         null, omOpaque, dfOpaque, 255, 0, false);",
    );
    // Reference: `operateStretch` `reference/cpp/core/visual/ImageFunction.cpp:408-529`.
    for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        assert_eq!(env.eval_int(&format!("dst.getPixel({x}, {y})")), 0x00_ff00);
    }
}

#[test]
fn operate_affine_points_mode_maps_the_source_quad() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var dst = new Bitmap(2, 2); var src = new Bitmap(1, 1); \
         src.setPixel(0, 0, 0xff0000); src.setMaskPixel(0, 0, 255);",
    );
    // The source rect's LT/RT/LB corners map to (0,0)/(2,0)/(0,2)
    // (`reference/cpp/core/visual/ImageFunction.cpp:188-308`).
    env.run(
        "ImageFunction.operateAffine(dst, src, 0, 0, 2, 0, 0, 2, null, null, false, \
         omOpaque, dfOpaque, 255, 0, false);",
    );
    for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        assert_eq!(env.eval_int(&format!("dst.getPixel({x}, {y})")), 0x00_00ff);
    }
}

/// Reference: `operateAffine` hands the clip rectangle to `AffineBlt`
/// (`reference/cpp/core/visual/ImageFunction.cpp:279-290`), so the pass
/// writes the pixels of the clip alone and leaves the ones outside it.
#[test]
fn operate_affine_writes_inside_the_clip_rect_alone() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var dst = new Bitmap(2, 2); var src = new Bitmap(1, 1); \
         src.setPixel(0, 0, 0xff0000); src.setMaskPixel(0, 0, 255);",
    );
    // The source rect's LT/RT/LB corners map to (0,0)/(2,0)/(0,2), and the
    // clip rectangle keeps the pass inside its top-left pixel.
    env.run(
        "ImageFunction.operateAffine(dst, src, 0, 0, 2, 0, 0, 2, null, new Rect(0, 0, 1, 1), \
         false, omOpaque, dfOpaque, 255, 0, false);",
    );
    assert_eq!(env.eval_int("dst.getPixel(0, 0)"), 0x00_00ff);
    assert_eq!(env.eval_int("dst.getPixel(1, 0)"), 0);
    assert_eq!(env.eval_int("dst.getPixel(0, 1)"), 0);
    assert_eq!(env.eval_int("dst.getPixel(1, 1)"), 0);
}

#[test]
fn draw_text_and_draw_glyph_raise_explicit_errors() {
    let _lock = vm_lock();
    let env = Env::new();
    assert!(
        env.eval("ImageFunction.drawText(new Bitmap(1, 1), null, 0, 0, 'x', 0)")
            .is_err()
    );
    assert!(
        env.eval("ImageFunction.drawGlyph(new Bitmap(1, 1), 0, 0, null, 0)")
            .is_err()
    );
}

#[test]
fn missing_arguments_and_unknown_bitmaps_are_typed_errors() {
    let _lock = vm_lock();
    let env = Env::new();
    // Too few arguments.
    assert!(
        env.eval("ImageFunction.operateRect(new Bitmap(1, 1), 0, 0)")
            .is_err()
    );
    assert!(
        env.eval("ImageFunction.fillRect(new Bitmap(1, 1))")
            .is_err()
    );
    // A nonexistent bitmap id.
    assert!(env.eval("ImageFunction.doGrayScale(99)").is_err());
}
