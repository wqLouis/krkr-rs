//! Reference-behaviour tests for the rectangle blit family.
//!
//! `copyRect`, `piledCopy`, `pileRect` and `blendRect` each name their
//! blend per draw face: the copy methods copy the planes, the pile mixes the
//! source into the destination and the blend copies the source
//! (`reference/cpp/core/visual/LayerIntf.cpp:4574-4657` for `copyRect`,
//! `:4529-4560` for `piledCopy`, `:5112-5170` for `pileRect` and
//! `:5171-5223` for `blendRect`).
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

/// Reference: `CopyRect` on an alpha face copies the main and the mask
/// plane (`reference/cpp/core/visual/LayerIntf.cpp:4587-4596`): the
/// source's colour and the source's alpha replace the destination's.
#[test]
fn the_copy_rect_on_an_alpha_face_copies_the_source_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script("dst.copyRect(0, 0, src, 0, 0, 1, 1);"));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x80]);
}

/// Reference: `CopyRect` on an additive-alpha face copies the main and the
/// mask plane as well
/// (`reference/cpp/core/visual/LayerIntf.cpp:4587-4596`).
#[test]
fn the_copy_rect_on_an_add_alpha_face_copies_the_source_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfAddAlpha; \
         dst.copyRect(0, 0, src, 0, 0, 1, 1);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x80]);
}

/// Reference: `CopyRect` on an opaque face copies the main and the mask
/// plane (`reference/cpp/core/visual/LayerIntf.cpp:4599-4609`): the
/// source's alpha replaces the destination's.
#[test]
fn the_copy_rect_on_an_opaque_face_copies_the_source_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfOpaque; \
         dst.copyRect(0, 0, src, 0, 0, 1, 1);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x80]);
}

/// Reference: `CopyRect` on an opaque face copies the main plane alone when
/// the layer holds its alpha
/// (`reference/cpp/core/visual/LayerIntf.cpp:4599-4609`): the source's
/// colour replaces the destination's and the destination's alpha is held.
#[test]
fn the_copy_rect_on_an_opaque_face_holds_the_destination_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfOpaque; \
         dst.holdAlpha = true; \
         dst.copyRect(0, 0, src, 0, 0, 1, 1);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x40]);
}

/// Reference: `CopyRect` on a mask face copies the mask plane alone
/// (`reference/cpp/core/visual/LayerIntf.cpp:4612-4621`): the source's
/// alpha replaces the destination's and the destination's colour is held.
#[test]
fn the_copy_rect_on_a_mask_face_copies_the_alpha_plane_alone() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfMask; \
         dst.copyRect(0, 0, src, 0, 0, 1, 1);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [0, 0, 255, 0x80]);
}

/// Reference: `PiledCopy` copies the source layer's composited image and
/// ignores the draw face of either layer
/// (`reference/cpp/core/visual/LayerIntf.cpp:4529-4560`): the main and the
/// mask plane are copied even on an opaque face that holds its alpha.
#[test]
fn the_piled_copy_ignores_the_draw_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfOpaque; \
         dst.holdAlpha = true; \
         dst.piledCopy(0, 0, src, 0, 0, 1, 1);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x80]);
}

/// Reference: `PileRect` piles with `bmAlphaOnAlpha` on an alpha face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5135-5144`), the tree's
/// "AlphaBlend_d" (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3555-3559`):
/// the destination's alpha takes part in the mix and the result's alpha is
/// the source-over alpha.
#[test]
fn the_pile_rect_on_an_alpha_face_is_the_alpha_correct_source_over() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script("dst.pileRect(0, 0, src, 0, 0, 1, 1, 255);"));
    // The destination's alpha (0x40) and the source's (0x80) give the
    // source-over alpha 0x80 + 0x40 * (1 - 0x80) = 0x9f, and the colour is
    // the mix by the source's share of the new alpha.
    assert_eq!(layer_pixel(&env, 1, 0, 0), [204, 0, 51, 159]);
}

/// Reference: the hold argument is the copy methods' alone: the method
/// cache gives the alpha methods one spelling with and without it
/// (`reference/cpp/core/visual/RenderManager.cpp:2400-2402`), and
/// `GetRenderMethod` consults it for the copy methods only
/// (`reference/cpp/core/visual/RenderManager.cpp:2470-2494`). The pile's
/// `bmAlphaOnAlpha` therefore ignores the layer's `holdAlpha`
/// (`reference/cpp/core/visual/LayerIntf.cpp:5135-5144`).
#[test]
fn the_pile_rect_on_an_alpha_face_ignores_the_hold_argument() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.holdAlpha = true; \
         dst.pileRect(0, 0, src, 0, 0, 1, 1, 255);",
    ));
    // The destination's alpha is not held: the mix writes the source-over
    // alpha.
    assert_eq!(layer_pixel(&env, 1, 0, 0), [204, 0, 51, 159]);
}

/// Reference: `PileRect` piles with `bmAlpha` on an opaque face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5146-5155`), the tree's
/// "AlphaBlend" (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3402-3411`):
/// the colour is mixed by the source's alpha and the destination's alpha is
/// held (the CPU twin is `d + ((f - d) * a >> 8)`,
/// `reference/cpp/core/visual/gl/blend_functor_c.h:374-383`).
#[test]
fn the_pile_rect_on_an_opaque_face_mixes_by_the_source_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfOpaque; \
         dst.pileRect(0, 0, src, 0, 0, 1, 1, 255);",
    ));
    // The colour is mixed by the source's alpha of 0x80 and the
    // destination's alpha (0x40) is held.
    assert_eq!(layer_pixel(&env, 1, 0, 0), [127, 0, 127, 0x40]);
}

/// Reference: `BlendRect` blends with `bmCopyOnAlpha` on an alpha face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5189-5198`); at the full
/// opacity that is the tree's "CopyOpaqueImage"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3141-3147`): the
/// source's colour replaces the destination's and the destination's alpha
/// becomes opaque.
#[test]
fn the_blend_rect_on_an_alpha_face_copies_the_source_as_an_opaque_image() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script("dst.blendRect(0, 0, src, 0, 0, 1, 1, 255);"));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 255]);
}

/// Reference: below the full opacity the copy methods answer with the
/// constant-alpha blend, which the method cache names the same with and
/// without the hold argument
/// (`reference/cpp/core/visual/RenderManager.cpp:2398-2399`): the hold
/// argument is ignored there. The destination's alpha is therefore mixed
/// with the opacity instead of being held
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3185-3189`, the
/// tree's "ConstAlphaBlend_d").
#[test]
fn the_blend_rect_on_an_alpha_face_ignores_the_hold_argument_below_the_full_opacity() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(1, 1); \
         src.type = ltAlpha; \
         src.fillRect(0, 0, 1, 1, 0x80ff0000); \
         var dst = new Layer(w, null); \
         dst.setImageSize(1, 1); \
         dst.type = ltAlpha; \
         dst.fillRect(0, 0, 1, 1, 0x00000000); \
         dst.holdAlpha = true; \
         dst.blendRect(0, 0, src, 0, 0, 1, 1, 128);",
    );
    // The source's colour replaces the destination's and the destination's
    // alpha becomes the opacity: the hold argument does not keep it at zero.
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 128]);
}

/// Reference: `BlendRect` blends with `bmCopy` on an opaque face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5199-5208`); at the full
/// opacity that is the tree's "Copy"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:2986`): the
/// source's colour and its alpha replace the destination's.
#[test]
fn the_blend_rect_on_an_opaque_face_copies_the_source_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfOpaque; \
         dst.blendRect(0, 0, src, 0, 0, 1, 1, 255);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x80]);
}

/// Reference: `BlendRect` on an opaque face passes the layer's hold
/// argument to `bmCopy`
/// (`reference/cpp/core/visual/LayerIntf.cpp:5199-5208`), and the hold
/// spelling of the copy is the tree's "CopyColor"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3150-3152`): the
/// source's colour replaces the destination's and the destination's alpha is
/// held.
#[test]
fn the_blend_rect_on_an_opaque_face_holds_the_destination_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&script(
        "dst.face = dfOpaque; \
         dst.holdAlpha = true; \
         dst.blendRect(0, 0, src, 0, 0, 1, 1, 255);",
    ));
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 0, 0, 0x40]);
}
