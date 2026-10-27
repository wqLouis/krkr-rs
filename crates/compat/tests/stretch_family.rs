//! Reference-behaviour tests for the stretch blit family.
//!
//! `stretchCopy`, `stretchPile` and `stretchBlend` each name their blend
//! per draw face
//! (`reference/cpp/core/visual/LayerIntf.cpp:4672-4716` for `stretchCopy`,
//! `:5264-5317` for `stretchPile` and `:5319-5372` for `stretchBlend`).
//!
//! The tests drive the real script surface: they create the layers through
//! the VM, run the method and read the pixels it leaves behind.

mod common;

use common::Env;
use common::vm_lock;

/// The RGBA pixel at `(x, y)` of the layer's main image.
///
/// `index` is the layer's position in the scene's layer list, which is the
/// order the script created the layers in.
fn layer_pixel(env: &Env, index: usize, x: usize, y: usize) -> [u8; 4] {
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

/// The script that builds the source and the destination and then runs the
/// method under test.
///
/// The source is a half-transparent red, the destination a translucent blue.
fn script(method: &str) -> String {
    format!(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(1, 1); \
         src.type = ltAlpha; \
         src.fillRect(0, 0, 1, 1, 0x80ff0000); \
         var dst = new Layer(w, null); \
         dst.setImageSize(1, 1); \
         dst.type = ltAlpha; \
         dst.fillRect(0, 0, 1, 1, 0x400000ff); \
         {method}"
    )
}

/// Reference: `StretchCopy` copies with `bmCopy` on the alpha faces and
/// passes `false` as the hold argument
/// (`reference/cpp/core/visual/LayerIntf.cpp:4685-4695`): the copy writes
/// the source's own alpha.
#[test]
fn the_stretch_copy_on_an_alpha_face_copies_the_source_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script("dst.stretchCopy(0, 0, 1, 1, src, 0, 0, 1, 1);"));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x80]);
}

/// Reference: `StretchCopy` on an opaque face passes the layer's hold
/// argument to `bmCopy`
/// (`reference/cpp/core/visual/LayerIntf.cpp:4697-4706`), and the hold
/// spelling of the copy is the tree's "CopyColor"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3150-3152`): the
/// source's colour replaces the destination's and the destination's alpha is
/// held.
#[test]
fn the_stretch_copy_on_an_opaque_face_holds_the_destination_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfOpaque; \
         dst.holdAlpha = true; \
         dst.stretchCopy(0, 0, 1, 1, src, 0, 0, 1, 1);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x40]);
}

/// Reference: `StretchPile` piles with `bmAlphaOnAlpha` on an alpha face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5284-5294`), the tree's
/// "AlphaBlend_d" (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3555-3559`):
/// the destination's alpha takes part in the mix and the result's alpha is
/// the source-over alpha.
#[test]
fn the_stretch_pile_on_an_alpha_face_is_the_alpha_correct_source_over() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.stretchPile(0, 0, 1, 1, src, 0, 0, 1, 1, 255);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [204, 0, 51, 159]);
}

/// Reference: `StretchPile` piles with `bmAlpha` on an opaque face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5295-5304`), the tree's
/// "AlphaBlend" (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3402-3411`):
/// the colour is mixed by the source's alpha and the destination's alpha is
/// held (the CPU twin is `d + ((f - d) * a >> 8)`,
/// `reference/cpp/core/visual/gl/blend_functor_c.h:374-383`).
#[test]
fn the_stretch_pile_on_an_opaque_face_mixes_by_the_source_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfOpaque; \
         dst.stretchPile(0, 0, 1, 1, src, 0, 0, 1, 1, 255);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [127, 0, 127, 0x40]);
}

/// Reference: `StretchBlend` blends with `bmCopyOnAlpha` on an alpha face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5339-5349`); at the full
/// opacity that is the tree's "CopyOpaqueImage"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3141-3147`): the
/// source is read as an opaque image, so the source's colour replaces the
/// destination's and the destination's alpha becomes opaque.
#[test]
fn the_stretch_blend_on_an_alpha_face_copies_the_source_as_an_opaque_image() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.stretchBlend(0, 0, 1, 1, src, 0, 0, 1, 1, 255);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 255]);
}

/// Reference: `StretchBlend` on an alpha face passes the layer's hold
/// argument to `bmCopyOnAlpha`
/// (`reference/cpp/core/visual/LayerIntf.cpp:5339-5349`), and the hold
/// spelling of that copy is the tree's "CopyColor"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3150-3152`): the
/// source's colour replaces the destination's and the destination's alpha is
/// held.
#[test]
fn the_stretch_blend_on_an_alpha_face_holds_the_destination_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.holdAlpha = true; \
         dst.stretchBlend(0, 0, 1, 1, src, 0, 0, 1, 1, 255);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x40]);
}

/// Reference: `StretchBlend` blends with `bmCopy` on an opaque face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5350-5359`); at the full
/// opacity that is the tree's "Copy"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:2986`): the
/// source's colour and its alpha replace the destination's.
#[test]
fn the_stretch_blend_on_an_opaque_face_copies_the_source_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfOpaque; \
         dst.stretchBlend(0, 0, 1, 1, src, 0, 0, 1, 1, 255);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x80]);
}
