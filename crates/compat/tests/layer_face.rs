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
/// `omOpaque` mode copies onto an opaque face and copies the source as an
/// opaque image onto an alpha face
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:1589-1622`). The layer's
/// `holdAlpha` property is the argument the plain method takes
/// (`reference/cpp/core/visual/LayerIntf.cpp:5249-5251`): the opaque face
/// keeps the destination's alpha, the alpha face writes it.
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
    // ... the alpha face writes it.
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
