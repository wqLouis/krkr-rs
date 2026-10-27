//! Reference-behaviour tests for the affine blit family.
//!
//! The four affine methods — `affineCopy`, `affinePile`, `affineBlend`
//! and `operateAffine` — do **not** take their blend method from the
//! operation-mode table the layer's draw path uses
//! (`reference/cpp/core/visual/LayerIntf.cpp:4118-4271`): each of them
//! names its own blend method for each draw face
//! (`reference/cpp/core/visual/LayerIntf.cpp:4722-4802`, `:5419-5466`,
//! `:5517-5566`, `:5617-5680`).
//!
//! The tests drive the real script surface: they create the layers through
//! the VM, run the affine method and read the pixels it leaves behind.

mod common;

use common::Env;
use common::vm_lock;

/// The straight RGBA pixel at `(x, y)` of the layer's main image.
///
/// `index` is the layer's position in the scene's layer list, which is the
/// order the script created the layers in.
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

/// The alpha byte of the pixel at `(x, y)` of the layer's main image.
fn layer_alpha(env: &Env, index: usize, x: usize, y: usize) -> u8 {
    layer_rgba(env, index, x, y)[3]
}

/// Reference: `AffineCopy` copies with `bmCopy` on the `dfAlpha` and
/// `dfAddAlpha` faces and passes `false` as the hold argument
/// (`reference/cpp/core/visual/LayerIntf.cpp:4735-4741`); the copy writes
/// the source's own alpha — the tree's "Copy"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:2986`).
///
/// A sprite whose transparent pixels carry a colour of their own therefore
/// stays transparent on an alpha face: the hidden colour is written, but the
/// alpha that goes with it is the source's. The copy is scaled here, the way
/// the game scales a character sprite, so the pass resamples the source
/// instead of copying it texel by texel.
#[test]
fn the_affine_copy_carries_the_source_alpha_on_an_alpha_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(2, 2); \
         src.type = ltAlpha; \
         src.fillRect(0, 0, 2, 2, 0x00808080); \
         src.fillRect(0, 0, 1, 1, 0xffff0000); \
         var dst = new Layer(w, null); \
         dst.setImageSize(4, 4); \
         dst.type = ltAlpha; \
         dst.affineCopy(src, 0, 0, 2, 2, true, 2, 0, 0, 2, 0, 0);",
    );
    let mut transparent = 0;
    let mut opaque = 0;
    for y in 0..4 {
        for x in 0..4 {
            let pixel = layer_rgba(&env, 1, x, y);
            match pixel {
                // The grey is the transparent source pixel's own colour: the
                // copy writes it, but the alpha that goes with it is the
                // source's.
                [128, 128, 128, 0] => transparent += 1,
                // The white is the layer's neutral colour, which the copy
                // cleared the rest of the box with.
                [255, 255, 255, 0] => {}
                [255, 0, 0, 255] => opaque += 1,
                other => panic!("unexpected pixel {other:?} at ({x}, {y})"),
            }
        }
    }
    assert!(
        transparent > 0,
        "the transparent source pixel was not copied"
    );
    assert!(opaque > 0, "the opaque source pixel was not copied");
}

/// Reference: the alpha faces pass `false` as the hold argument
/// (`reference/cpp/core/visual/LayerIntf.cpp:4735-4741`), so the layer's
/// `holdAlpha` does not hold the destination's alpha there: the copy writes
/// the source's alpha whatever `holdAlpha` says.
#[test]
fn the_affine_copy_on_an_alpha_face_ignores_the_hold_argument() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(1, 1); \
         src.type = ltAlpha; \
         src.fillRect(0, 0, 1, 1, 0xffff0000); \
         var dst = new Layer(w, null); \
         dst.setImageSize(1, 1); \
         dst.type = ltAlpha; \
         dst.fillRect(0, 0, 1, 1, 0x40000000); \
         dst.holdAlpha = true; \
         dst.affineCopy(src, 0, 0, 1, 1, true, 1, 0, 0, 1, 0, 0);",
    );
    // The destination's alpha (0x40) is not held: the source's alpha (0xff)
    // is written.
    assert_eq!(layer_alpha(&env, 1, 0, 0), 0xff);
}

/// Reference: `AffineCopy` on the `dfOpaque` face passes the layer's
/// `HoldAlpha` as the hold argument
/// (`reference/cpp/core/visual/LayerIntf.cpp:4743-4750`). Without
/// `holdAlpha` the copy is the tree's "Copy"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:2986`): the
/// source's alpha is written.
#[test]
fn the_affine_copy_on_an_opaque_face_copies_the_source_alpha() {
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
         dst.type = ltOpaque; \
         dst.affineCopy(src, 0, 0, 1, 1, true, 1, 0, 0, 1, 0, 0);",
    );
    // The source's alpha is copied.
    assert_eq!(layer_rgba(&env, 1, 0, 0), [255, 0, 0, 0x80]);
}

/// Reference: `AffineCopy` on the `dfOpaque` face passes the layer's
/// `HoldAlpha` as the hold argument
/// (`reference/cpp/core/visual/LayerIntf.cpp:4743-4750`), and the hold
/// spelling of the copy is the tree's "CopyColor"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3150-3152`): the
/// source's colour replaces the destination's and the destination's alpha is
/// held.
#[test]
fn the_affine_copy_on_an_opaque_face_holds_the_destination_alpha() {
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
         dst.fillRect(0, 0, 1, 1, 0x40000000); \
         dst.type = ltOpaque; \
         dst.holdAlpha = true; \
         dst.affineCopy(src, 0, 0, 1, 1, true, 1, 0, 0, 1, 0, 0);",
    );
    // The source's colour replaces the destination's ...
    assert_eq!(&layer_rgba(&env, 1, 0, 0)[..3], &[255, 0, 0]);
    // ... and the destination's alpha (0x40) is held.
    assert_eq!(layer_alpha(&env, 1, 0, 0), 0x40);
}

/// Reference: `AffinePile` piles with `bmAlphaOnAlpha` on an alpha face
/// and with `bmAlpha` on an opaque face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5437-5456`). The alpha method
/// is the tree's "AlphaBlend_d"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3549-3556`): the
/// destination's alpha takes part in the mix and the result's alpha is the
/// source-over alpha.
#[test]
fn the_affine_pile_on_an_alpha_face_is_the_alpha_correct_source_over() {
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
         dst.fillRect(0, 0, 1, 1, 0x400000ff); \
         dst.affinePile(src, 0, 0, 1, 1, true, 1, 0, 0, 1, 0, 0, 255, 0);",
    );
    // The destination's alpha (0x40) and the source's (0x80) give the
    // source-over alpha 0x80 + 0x40 * (1 - 0x80) = 0x9f, and the colour is
    // the mix by the source's share of the new alpha.
    assert_eq!(layer_rgba(&env, 1, 0, 0), [204, 0, 51, 159]);
}

/// Reference: `AffinePile` piles with `bmAlpha` on an opaque face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5445-5455`), the tree's
/// "AlphaBlend"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3402-3411`): the
/// colour is mixed by the source's alpha and the destination's alpha is
/// held.
#[test]
fn the_affine_pile_on_an_opaque_face_holds_the_destination_alpha() {
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
         dst.fillRect(0, 0, 1, 1, 0x400000ff); \
         dst.type = ltOpaque; \
         dst.affinePile(src, 0, 0, 1, 1, true, 1, 0, 0, 1, 0, 0, 255, 0);",
    );
    // The colour is mixed by the source's alpha (the tree's CPU twin,
    // `d + ((f - d) * a >> 8)`,
    // `reference/cpp/core/visual/gl/blend_functor_c.h:374-383`) and the
    // destination's alpha (0x40) is held.
    assert_eq!(layer_rgba(&env, 1, 0, 0), [127, 0, 127, 0x40]);
}

/// Reference: `AffineBlend` copies the source as an opaque image with
/// `bmCopyOnAlpha` on an alpha face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5535-5543`), the tree's
/// "CopyOpaqueImage"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3141-3147`): the
/// source's colour replaces the destination's and the destination's alpha
/// becomes opaque.
#[test]
fn the_affine_blend_on_an_alpha_face_copies_the_source_as_an_opaque_image() {
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
         dst.fillRect(0, 0, 1, 1, 0x400000ff); \
         dst.affineBlend(src, 0, 0, 1, 1, true, 1, 0, 0, 1, 0, 0, 255, 0);",
    );
    // The source's colour replaces the destination's and the destination's
    // alpha becomes opaque.
    assert_eq!(layer_rgba(&env, 1, 0, 0), [255, 0, 0, 255]);
}

/// Reference: `AffineBlend` copies with `bmCopy` on an opaque face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5545-5556`), the tree's
/// "Copy" (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:2986`):
/// the source's colour and its alpha replace the destination's.
#[test]
fn the_affine_blend_on_an_opaque_face_copies_the_source_alpha() {
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
         dst.fillRect(0, 0, 1, 1, 0x400000ff); \
         dst.type = ltOpaque; \
         dst.affineBlend(src, 0, 0, 1, 1, true, 1, 0, 0, 1, 0, 0, 255, 0);",
    );
    // The source's colour and its alpha replace the destination's.
    assert_eq!(layer_rgba(&env, 1, 0, 0), [255, 0, 0, 0x80]);
}

/// Reference: `OperateAffine` takes its blend method from the operation
/// mode and the draw face
/// (`reference/cpp/core/visual/LayerIntf.cpp:5626-5638`, the table at
/// `:4118-4271`): `omOpaque` on an alpha face is the tree's
/// "CopyOpaqueImage" (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3141-3147`)
/// and `omAlpha` on an alpha face is the tree's "AlphaBlend_d"
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3549-3556`).
#[test]
fn the_operate_affine_keeps_the_mode_and_face_table() {
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
         dst.fillRect(0, 0, 1, 1, 0x400000ff); \
         dst.operateAffine(src, 0, 0, 1, 1, true, 1, 0, 0, 1, 0, 0, omOpaque, 255, 0);",
    );
    // The alpha face's `omOpaque` copy writes an opaque alpha.
    assert_eq!(layer_rgba(&env, 1, 0, 0), [255, 0, 0, 255]);
    // The alpha face's `omAlpha` is the alpha-correct source-over.
    env.run("dst.fillRect(0, 0, 1, 1, 0x400000ff);");
    env.run("dst.operateAffine(src, 0, 0, 1, 1, true, 1, 0, 0, 1, 0, 0, omAlpha, 255, 0);");
    assert_eq!(layer_rgba(&env, 1, 0, 0), [204, 0, 51, 159]);
}
