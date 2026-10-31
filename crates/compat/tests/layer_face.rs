//! Reference-behaviour tests for the layer's draw face.
//!
//! The reference keeps two faces: the outward one the script reads and
//! writes, and the actual one the drawing and the pixel operations use. The
//! outward face defaults to `dfAuto` (128) and the actual face is resolved
//! from the layer's type whenever the outward face is `dfAuto`
//! (`reference/cpp/core/visual/LayerIntf.h:738-750`,
//! `reference/cpp/core/visual/LayerIntf.cpp:371-372`, `:1525-1618`).
//!
//! The tests drive the real script surface: they create layers through the
//! VM, read the face the script sees and read the pixels the operations
//! leave behind.

mod common;

use common::Env;
use common::vm_lock;

/// The ARGB pixel at `(x, y)` of the layer's main image.
///
/// `index` is the layer's position in the scene's layer list, which is the
/// order the script created the layers in.
fn layer_pixel(env: &Env, index: usize, x: usize, y: usize) -> u32 {
    let layer = env.scene.layers.get(index).expect("the layer");
    let bitmap_id = layer.bitmap.expect("the layer has a main image");
    let bitmap = env.scene.bitmap(bitmap_id).expect("the main image");
    let offset = (y * bitmap.width as usize + x) * 4;
    let rgba = bitmap
        .rgba
        .get(offset..offset + 4)
        .expect("the pixel is inside the image");
    (u32::from(rgba[3]) << 24)
        | (u32::from(rgba[2]) << 16)
        | (u32::from(rgba[1]) << 8)
        | u32::from(rgba[0])
}

/// The alpha byte of the pixel at `(x, y)` of the layer's main image.
fn layer_alpha(env: &Env, index: usize, x: usize, y: usize) -> u8 {
    (layer_pixel(env, index, x, y) >> 24) as u8
}

/// The four planes of the pixel at `(x, y)` of the layer's main image, in the
/// order the image stores them: red, green, blue, alpha.
fn layer_rgba(env: &Env, index: usize, x: usize, y: usize) -> [u8; 4] {
    let layer = env.scene.layers.get(index).expect("the layer");
    let bitmap_id = layer.bitmap.expect("the layer has a main image");
    let bitmap = env.scene.bitmap(bitmap_id).expect("the main image");
    let offset = (y * bitmap.width as usize + x) * 4;
    let rgba = bitmap
        .rgba
        .get(offset..offset + 4)
        .expect("the pixel is inside the image");
    [rgba[0], rgba[1], rgba[2], rgba[3]]
}

/// Reference: the layer constructor stores `Face = dfAuto` and resolves the
/// draw face from it (`reference/cpp/core/visual/LayerIntf.cpp:371-372`), and
/// the enum value of `dfAuto` is 128
/// (`reference/cpp/core/visual/LayerIntf.h:59-70`).
#[test]
fn a_fresh_layer_face_is_df_auto() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window(); var l = new Layer(w, null);");
    assert_eq!(env.eval_int("l.face"), 128);
}

/// Reference: `SetFace` stores the face the script set and `GetFace` returns
/// that stored value (`reference/cpp/core/visual/LayerIntf.h:745-749`), so
/// the script reads the outward face and not the resolved one.
#[test]
fn setting_the_face_stores_the_outward_value() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window(); var l = new Layer(w, null);");
    env.run("l.face = dfOpaque;");
    assert_eq!(env.eval_int("l.face"), 1);
    env.run("l.face = dfMask;");
    assert_eq!(env.eval_int("l.face"), 2);
    env.run("l.face = dfAddAlpha;");
    assert_eq!(env.eval_int("l.face"), 4);
    env.run("l.face = dfAuto;");
    assert_eq!(env.eval_int("l.face"), 128);
}

/// Reference: with an outward face of `dfAuto` the actual face is resolved
/// from the layer's display type (`reference/cpp/core/visual/LayerIntf.cpp:1525-1618`):
/// an opaque type resolves to `dfOpaque` and an alpha type to `dfAlpha`.
/// The two differ in what a fill does with the destination's alpha: the
/// opaque face fills the colour alone and keeps the alpha, the alpha face
/// fills the colour and the alpha
/// (`reference/cpp/core/visual/LayerIntf.cpp:4280-4292`).
#[test]
fn the_resolved_face_follows_the_layer_type() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var alpha = new Layer(w, null); \
         alpha.setImageSize(4, 4); \
         alpha.type = ltAlpha; \
         alpha.holdAlpha = true; \
         alpha.fillRect(0, 0, 4, 4, 0xff0000ff); \
         var opaque = new Layer(w, null); \
         opaque.setImageSize(4, 4); \
         opaque.type = ltOpaque; \
         opaque.holdAlpha = true; \
         opaque.fillRect(0, 0, 4, 4, 0xff0000ff);",
    );
    // The alpha face writes the alpha of the fill colour ...
    assert_eq!(layer_alpha(&env, 0, 0, 0), 0xff);
    // ... the opaque face keeps the alpha the layer holds.
    assert_eq!(layer_alpha(&env, 1, 0, 0), 0);
}

/// Reference: `SetType` calls `NotifyLayerTypeChange`, which resolves the
/// draw face again (`reference/cpp/core/visual/LayerIntf.cpp:1910`,
/// `:1517-1518`), so the face follows the new type.
#[test]
fn changing_the_type_resolves_the_face_again() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(4, 4); \
         l.type = ltOpaque; \
         l.holdAlpha = true; \
         l.fillRect(0, 0, 4, 4, 0xff0000ff);",
    );
    // The opaque face keeps the alpha the layer holds.
    assert_eq!(layer_alpha(&env, 0, 0, 0), 0);
    // The type change resolves the face again: the alpha face writes the
    // alpha of the fill colour.
    env.run("l.type = ltAlpha; l.fillRect(0, 0, 4, 4, 0xff0000ff);");
    assert_eq!(layer_alpha(&env, 0, 0, 0), 0xff);
}

/// Reference: the face-sensitive operation modes accept the `dfAlpha`,
/// `dfAddAlpha` and `dfOpaque` faces and reject every other face with
/// `TVPNotDrawableFaceType`
/// (`reference/cpp/core/visual/LayerIntf.cpp:4118-4271`, `:5239-5242`).
#[test]
fn the_operate_path_rejects_a_face_the_reference_rejects() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(4, 4); \
         var dst = new Layer(w, null); \
         dst.setImageSize(4, 4);",
    );
    // The default face resolves to `dfAlpha`, which the mode accepts.
    env.run("dst.operateRect(0, 0, src, 0, 0, 4, 4, omAlpha);");
    // The mask face is not one of the three the mode knows.
    env.run("dst.face = dfMask;");
    assert!(
        env.eval("dst.operateRect(0, 0, src, 0, 0, 4, 4, omAlpha);")
            .is_err()
    );
    // The province face is not one of them either.
    env.run("dst.face = dfProvince;");
    assert!(
        env.eval("dst.operateRect(0, 0, src, 0, 0, 4, 4, omAlpha);")
            .is_err()
    );
    // The alpha face is accepted again.
    env.run("dst.face = dfAlpha;");
    env.run("dst.operateRect(0, 0, src, 0, 0, 4, 4, omAlpha);");
}

/// Reference: the operation mode and the draw face select the blend method
/// together (`reference/cpp/core/visual/LayerIntf.cpp:4118-4271`). The
/// `omOpaque` mode copies onto an opaque face (`bmCopy`) and copies the
/// source as an opaque image onto an alpha face (`bmCopyOnAlpha`,
/// `reference/cpp/core/visual/LayerIntf.cpp:4247-4258`).
///
/// The layer's `holdAlpha` property is the argument the copy methods take
/// (`reference/cpp/core/visual/LayerIntf.cpp:5249-5251`), and the method
/// cache answers it with the `hda` spelling of each copy method
/// (`reference/cpp/core/visual/RenderManager.cpp:2398-2399`): both copies
/// hold the destination's alpha under `holdAlpha`, and the alpha face's
/// copy writes an opaque alpha without it
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3141-3147`).
#[test]
fn the_operate_path_holds_the_destination_alpha_on_an_opaque_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var bmp = new Bitmap(1, 1); \
         bmp.setPixel(0, 0, 0x0000ff); \
         bmp.setMaskPixel(0, 0, 255); \
         var opaque = new Layer(w, null); \
         opaque.setImageSize(4, 4); \
         opaque.type = ltOpaque; \
         opaque.holdAlpha = true; \
         opaque.operateRect(0, 0, bmp, 0, 0, 1, 1, omOpaque); \
         var alpha = new Layer(w, null); \
         alpha.setImageSize(4, 4); \
         alpha.type = ltAlpha; \
         alpha.holdAlpha = true; \
         alpha.operateRect(0, 0, bmp, 0, 0, 1, 1, omOpaque);",
    );
    // The opaque face keeps the destination's alpha ...
    assert_eq!(layer_alpha(&env, 0, 0, 0), 0);
    // ... and so does the alpha face, whose hold-alpha spelling is the
    // tree's "CopyColor".
    assert_eq!(layer_alpha(&env, 1, 0, 0), 0);
}

/// Reference: without `holdAlpha` the alpha face's `omOpaque` copy is the
/// tree's "CopyOpaqueImage": the source's colour replaces the destination's
/// and the destination's alpha becomes opaque
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3141-3147`; the CPU
/// twin writes `dest = src | 0xff000000`,
/// `reference/cpp/core/visual/tvpgl.cpp:3326-3370`). The opaque face's copy
/// is the tree's "Copy", which copies the source's alpha as well
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:2986`).
#[test]
fn the_operate_path_writes_an_opaque_alpha_on_an_alpha_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var bmp = new Bitmap(1, 1); \
         bmp.setPixel(0, 0, 0x0000ff); \
         bmp.setMaskPixel(0, 0, 128); \
         var opaque = new Layer(w, null); \
         opaque.setImageSize(4, 4); \
         opaque.type = ltOpaque; \
         opaque.operateRect(0, 0, bmp, 0, 0, 1, 1, omOpaque); \
         var alpha = new Layer(w, null); \
         alpha.setImageSize(4, 4); \
         alpha.type = ltAlpha; \
         alpha.operateRect(0, 0, bmp, 0, 0, 1, 1, omOpaque);",
    );
    // The opaque face copies the source's alpha.
    assert_eq!(layer_alpha(&env, 0, 0, 0), 128);
    // The alpha face writes an opaque alpha.
    assert_eq!(layer_alpha(&env, 1, 0, 0), 0xff);
}

/// Reference: the additive-alpha-on-alpha blend is not implemented: the
/// operation leaves the destination untouched
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:1705-1708`,
/// `reference/cpp/core/visual/RenderManager.cpp:2557-2561`).
#[test]
fn the_operate_path_does_nothing_for_the_unimplemented_blend() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(4, 4); \
         src.fillRect(0, 0, 4, 4, 0xff0000ff); \
         var dst = new Layer(w, null); \
         dst.setImageSize(4, 4); \
         dst.fillRect(0, 0, 4, 4, 0x40000000);",
    );
    // The additive-alpha mode on an alpha face selects the blend the
    // reference does not implement, so the destination keeps its pixels.
    env.run("dst.operateRect(0, 0, src, 0, 0, 4, 4, omAddAlpha);");
    assert_eq!(layer_alpha(&env, 1, 0, 0), 0x40);
    // The alpha mode on the same face blends.
    env.run("dst.operateRect(0, 0, src, 0, 0, 4, 4, omAlpha);");
    assert_eq!(layer_alpha(&env, 1, 0, 0), 0xff);
}

/// Reference: the additive-alpha-on-alpha blend is not implemented, so the
/// `ImageFunction` leaves the destination untouched as well
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:1705-1708`,
/// `reference/cpp/core/visual/ImageFunction.cpp:78-130`).
#[test]
fn the_image_function_does_nothing_for_the_unimplemented_blend() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var dst = new Bitmap(4, 4); \
         dst.setPixel(0, 0, 0x000000); \
         dst.setMaskPixel(0, 0, 0x40); \
         var src = new Bitmap(4, 4); \
         src.setPixel(0, 0, 0x0000ff); \
         src.setMaskPixel(0, 0, 0xff);",
    );
    env.run(
        "ImageFunction.operateRect(dst, 0, 0, src, null, null, omAddAlpha, dfAlpha, 255, false);",
    );
    assert_eq!(env.eval_int("dst.getMaskPixel(0, 0)"), 0x40);
    env.run("ImageFunction.operateRect(dst, 0, 0, src, null, null, omAlpha, dfAlpha, 255, false);");
    assert_eq!(env.eval_int("dst.getMaskPixel(0, 0)"), 0xff);
}

/// Reference: the operation mode and the draw face select the blend method
/// (`reference/cpp/core/visual/ImageFunction.cpp:78-130`), and the `hda`
/// argument is that method's hold-destination-alpha flag, which only the
/// plain methods take (`reference/cpp/core/visual/LayerBitmapIntf.cpp:1589-1622`):
/// an alpha face selects the plain-on-alpha method, which ignores the flag,
/// while an opaque face selects the plain method, which takes it.
#[test]
fn the_image_function_holds_the_alpha_only_on_an_opaque_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var alpha = new Bitmap(4, 4); \
         alpha.setPixel(0, 0, 0x000000); \
         alpha.setMaskPixel(0, 0, 0x40); \
         var opaque = new Bitmap(4, 4); \
         opaque.setPixel(0, 0, 0x000000); \
         opaque.setMaskPixel(0, 0, 0x40); \
         var src = new Bitmap(4, 4); \
         src.setPixel(0, 0, 0x0000ff); \
         src.setMaskPixel(0, 0, 0xff);",
    );
    // The alpha face ignores the hold argument: the destination's alpha is
    // written.
    env.run(
        "ImageFunction.operateRect(alpha, 0, 0, src, null, null, omOpaque, dfAlpha, 255, true);",
    );
    assert_eq!(env.eval_int("alpha.getMaskPixel(0, 0)"), 0xff);
    // The opaque face takes it: the destination's alpha is held.
    env.run(
        "ImageFunction.operateRect(opaque, 0, 0, src, null, null, omOpaque, dfOpaque, 255, true);",
    );
    assert_eq!(env.eval_int("opaque.getMaskPixel(0, 0)"), 0x40);
}

/// Reference: the add-alpha face cannot take a negative opacity
/// (`reference/cpp/core/visual/LayerIntf.cpp:4448-4452`,
/// `reference/cpp/core/visual/ImageFunction.cpp:1046-1049`).
#[test]
fn the_add_alpha_face_rejects_a_negative_opacity() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(8, 8); \
         l.type = ltAddAlpha; \
         var b = new Bitmap(8, 8); \
         var f = new Font('*missing', 12, 0);",
    );
    let error = env.error("l.drawText(0, 0, 'A', 0xff0000ff, -1);");
    assert!(error.contains("opacity"), "unexpected error: {error}");
    let error =
        env.error("ImageFunction.drawText(b, f, 0, 0, 'A', 0xff0000ff, -1, true, dfAddAlpha);");
    assert!(error.contains("opacity"), "unexpected error: {error}");
}

/// The operation mode and the draw face select the blend method together
/// (`reference/cpp/core/visual/LayerIntf.cpp:4217-4262`), and the render
/// method cache names each method's implementation
/// (`reference/cpp/core/visual/RenderManager.cpp:2416-2424`): `omAlpha` is
/// the plain alpha blend on `dfAlpha` (`AlphaBlend_d`,
/// `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3555-3558`), the
/// channel mix on `dfOpaque` (`AlphaBlend`, `:3401-3410`, which holds the
/// destination's alpha) and the premultiplied over on `dfAddAlpha`
/// (`AlphaBlend_a`, `:3499-3507`).
///
/// A destination that stores an additive alpha therefore cannot answer with
/// the plain alpha blend: its stored colour is premultiplied, so the pass
/// premultiplies the source and fades the destination by the source's alpha
/// (`reference/cpp/core/visual/gl/blend_util_func.h:70-83`, the CPU twin the
/// shader matches).
#[test]
fn the_operate_path_blends_onto_every_face_the_mode_accepts() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(1, 1); \
         src.fillRect(0, 0, 1, 1, 0xffff0000); \
         var alpha = new Layer(w, null); \
         alpha.setImageSize(1, 1); \
         alpha.type = ltAlpha; \
         alpha.fillRect(0, 0, 1, 1, 0x400000ff); \
         var add = new Layer(w, null); \
         add.setImageSize(1, 1); \
         add.type = ltAddAlpha; \
         add.fillRect(0, 0, 1, 1, 0x400000ff); \
         var opaque = new Layer(w, null); \
         opaque.setImageSize(1, 1); \
         opaque.type = ltOpaque; \
         opaque.fillRect(0, 0, 1, 1, 0x400000ff);",
    );
    // The alpha face runs the alpha-correct source-over: an opaque source
    // replaces the destination, alpha included.
    env.run("alpha.operateRect(0, 0, src, 0, 0, 1, 1, omAlpha);");
    assert_eq!(layer_rgba(&env, 1, 0, 0), [255, 0, 0, 255]);
    // The additive-alpha face premultiplies the source's colour and fades the
    // destination by its alpha: at the full source alpha that is the source's
    // colour and the source-over alpha, not the destination's colour.
    env.run("add.operateRect(0, 0, src, 0, 0, 1, 1, omAlpha);");
    assert_eq!(layer_rgba(&env, 2, 0, 0), [254, 0, 0, 255]);
    // The opaque face runs the channel mix: the colours are mixed by the
    // source's alpha and the destination's alpha is held.
    env.run("opaque.operateRect(0, 0, src, 0, 0, 1, 1, omAlpha);");
    assert_eq!(layer_rgba(&env, 3, 0, 0), [254, 0, 0, 0x40]);
}

/// `omAddAlpha` on an additive-alpha destination adds the opacity-scaled
/// source and fades the destination by the source's alpha
/// (`AdditiveAlphaBlend_a`, `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3640-3649`;
/// the CPU twin is the premultiplied blend,
/// `reference/cpp/core/visual/gl/blend_util_func.h:70-83`), the alpha
/// composing (`AdditiveAlphaBlend`, `:3629-3637`, holds the destination's
/// alpha instead).
#[test]
fn the_operate_path_adds_onto_an_additive_alpha_destination() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(1, 1); \
         src.fillRect(0, 0, 1, 1, 0xffff0000); \
         var add = new Layer(w, null); \
         add.setImageSize(1, 1); \
         add.type = ltAddAlpha; \
         add.fillRect(0, 0, 1, 1, 0x400000ff); \
         var opaque = new Layer(w, null); \
         opaque.setImageSize(1, 1); \
         opaque.type = ltOpaque; \
         opaque.fillRect(0, 0, 1, 1, 0x400000ff);",
    );
    // The additive-alpha destination adds the whole source and composes the
    // alphas.
    env.run("add.operateRect(0, 0, src, 0, 0, 1, 1, omAddAlpha);");
    assert_eq!(layer_rgba(&env, 1, 0, 0), [255, 0, 0, 255]);
    // The opaque destination adds the opacity-scaled source to the faded
    // destination and holds its alpha; at the full opacity that is the whole
    // source colour.
    env.run("opaque.operateRect(0, 0, src, 0, 0, 1, 1, omAddAlpha);");
    assert_eq!(layer_rgba(&env, 2, 0, 0), [255, 0, 0, 0x40]);
}

/// `omOpaque` on an additive-alpha destination copies the source as an
/// opaque image at the full opacity (`CopyOpaqueImage`,
/// `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3141-3147`), and
/// below it the constant-alpha blend adds the source's colour and leaves the
/// opacity as the alpha (`ConstAlphaBlend_a`, `:3171-3179`).
///
/// The hold spelling of that method does not exist: the cache registers no
/// hold variant (`reference/cpp/core/visual/RenderManager.cpp:2423-2424`), so
/// the full-opacity pass with the hold set draws nothing
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:1483-1484`).
#[test]
fn the_operate_path_draws_nothing_for_the_unholdable_copy() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(1, 1); \
         src.fillRect(0, 0, 1, 1, 0xffff0000); \
         var add = new Layer(w, null); \
         add.setImageSize(1, 1); \
         add.type = ltAddAlpha; \
         add.holdAlpha = true; \
         add.fillRect(0, 0, 1, 1, 0x400000ff);",
    );
    // The hold has no spelling at the full opacity: the destination keeps
    // every plane.
    env.run("add.operateRect(0, 0, src, 0, 0, 1, 1, omOpaque);");
    assert_eq!(layer_rgba(&env, 1, 0, 0), [0, 0, 255, 0x40]);
    // Without the hold the source is copied as an opaque image.
    env.run("add.holdAlpha = false; add.operateRect(0, 0, src, 0, 0, 1, 1, omOpaque);");
    assert_eq!(layer_rgba(&env, 1, 0, 0), [255, 0, 0, 255]);
    // Below the full opacity the method has one spelling, which draws.
    env.run(
        "var dst = new Layer(w, null); \
         dst.setImageSize(1, 1); \
         dst.type = ltAddAlpha; \
         dst.holdAlpha = true; \
         dst.fillRect(0, 0, 1, 1, 0x400000ff); \
         dst.operateRect(0, 0, src, 0, 0, 1, 1, omOpaque, 128);",
    );
    assert_eq!(layer_rgba(&env, 2, 0, 0), [255, 0, 126, 160]);
}
