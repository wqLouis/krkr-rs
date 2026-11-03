//! Reference-behaviour tests for the layer's operation-mode blends.
//!
//! The live per-pixel path of the reference is the C++ functor set that
//! `TVPGL_C_Init` installs
//! (`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:590-684`): the
//! kernels live in `gl/blend_functor_c.h`, the alpha-variation wrappers in
//! `gl/blend_variation.h` and the premultiplied helpers in
//! `gl/blend_util_func.h`. The legacy `_c` kernels in `tvpgl.cpp` are dead
//! in that tree — their registration blocks are `#if 0`
//! (`reference/krkr2/cpp/core/visual/tvpgl.cpp:13847-13882`) — so the
//! functors are the oracle.
//!
//! The operation mode and the draw face select the blend method
//! (`reference/krkr2/cpp/core/visual/LayerIntf.cpp:4118-4252`), and the
//! software renderer's method table picks the kernel of that method
//! (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2396-2459`).
//!
//! The tests drive the real script surface: they create layers through the
//! VM, run `operateRect` and read the pixels the operation leaves behind.

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

/// Reference: the Photoshop normal blend is the tree's "PsAlphaBlend": the
/// source's alpha mixes the colours, the destination's alpha is held and the
/// mix carries no alpha correction
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2425-2426`; the CPU
/// kernel is `ps_alpha_blend_func`,
/// `reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-383`).
#[test]
fn the_photoshop_normal_blend_mixes_without_an_alpha_correction() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(4, 4); \
         src.fillRect(0, 0, 4, 4, 0x8000ff00); \
         var dst = new Layer(w, null); \
         dst.setImageSize(4, 4); \
         dst.fillRect(0, 0, 4, 4, 0x80ff0000); \
         dst.operateRect(0, 0, src, 0, 0, 1, 1, omPsNormal);",
    );
    // The destination's red and the source's green mix by the source's alpha
    // of 128: `255 + ((0 - 255) * 128 >> 8) = 127` and
    // `0 + ((255 - 0) * 128 >> 8) = 127`. The destination's alpha is held.
    assert_eq!(layer_pixel(&env, 1, 0, 0), [127, 127, 0, 128]);
}

/// Reference: the opaque face's alpha blend is the tree's "AlphaBlend", whose
/// kernel is the one the Photoshop normal blend uses
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:67-76`); the
/// alpha face's is the tree's "AlphaBlend_d", whose kernel reads the
/// destination's alpha through the opacity-on-opacity table
/// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:109-149`,
/// `reference/krkr2/cpp/core/visual/tvpgl.cpp:224-301`).
#[test]
fn the_draw_face_selects_the_blend_of_the_alpha_mode() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(4, 4); \
         src.fillRect(0, 0, 4, 4, 0x8000ff00); \
         var opaque = new Layer(w, null); \
         opaque.setImageSize(4, 4); \
         opaque.fillRect(0, 0, 4, 4, 0x80ff0000); \
         opaque.face = dfOpaque; \
         opaque.operateRect(0, 0, src, 0, 0, 1, 1, omAlpha); \
         var alpha = new Layer(w, null); \
         alpha.setImageSize(4, 4); \
         alpha.fillRect(0, 0, 4, 4, 0x80ff0000); \
         alpha.operateRect(0, 0, src, 0, 0, 1, 1, omAlpha);",
    );
    // The opaque face mixes without an alpha correction and holds the
    // destination's alpha.
    assert_eq!(layer_pixel(&env, 1, 0, 0), [127, 127, 0, 128]);
    // The alpha face reads the destination's alpha: the weight of the two
    // alphas of 128 is `255 * 128 / 255 = 127`, so the red channel is
    // `0 + ((255 - 0) * 127 >> 8) = 126` and the green channel
    // `255 + ((0 - 255) * 127 >> 8) = 128`; the destination's alpha becomes
    // `255 - (255 - 128) * (255 - 128) / 255 = 192`.
    assert_eq!(layer_pixel(&env, 2, 0, 0), [85, 169, 0, 192]);
}

/// Reference: the alpha blend mixes with the *new* alpha, so a translucent
/// source over an identically translucent destination keeps the colour both
/// operands share (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:109-149`).
#[test]
fn the_alpha_blend_keeps_the_colour_of_a_translucent_destination() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(4, 4); \
         src.fillRect(0, 0, 4, 4, 0x1affcccc); \
         var dst = new Layer(w, null); \
         dst.setImageSize(4, 4); \
         dst.fillRect(0, 0, 4, 4, 0x1affcccc); \
         dst.operateRect(0, 0, src, 0, 0, 1, 1, omAlpha);",
    );
    // The weight of the two alphas of 26 is `255 * 26 / 52 = 127`, and the
    // destination's alpha becomes `255 - (255 - 26) * (255 - 26) / 255 = 50`,
    // so every channel keeps the value both operands share.
    assert_eq!(layer_pixel(&env, 1, 0, 0), [255, 204, 204, 50]);
}

/// Reference: the overlay is the tree's overlay table
/// (`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:539-542`): below the
/// midpoint `(s * d * 2) / 255` and above it
/// `(s + d) * 2 - (s * d * 2) / 255 - 255`.
#[test]
fn the_overlay_blend_applies_the_reference_table() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(4, 4); \
         src.fillRect(0, 0, 4, 4, 0xff32ff96); \
         var dst = new Layer(w, null); \
         dst.setImageSize(4, 4); \
         dst.fillRect(0, 0, 4, 4, 0xff646464); \
         dst.operateRect(0, 0, src, 0, 0, 1, 1, omPsOverlay);",
    );
    // Every destination channel of 100 is below the midpoint, so the overlay
    // is `(s * d * 2) / 255`: `(50 * 100 * 2) / 255 = 39`,
    // `(255 * 100 * 2) / 255 = 200` and `(150 * 100 * 2) / 255 = 117`.
    // The mix truncates by eight bits, so the channels land on 39, 199 and
    // 116.
    assert_eq!(layer_pixel(&env, 1, 0, 0), [39, 199, 116, 255]);
}

/// Reference: the tree rejects an operation mode it does not know
/// (`reference/krkr2/cpp/core/visual/LayerIntf.cpp:4236-4241`), so the operation
/// raises instead of blending with another mode.
#[test]
fn an_unknown_operation_mode_is_rejected() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setImageSize(4, 4); \
         var dst = new Layer(w, null); \
         dst.setImageSize(4, 4);",
    );
    assert!(
        env.eval("dst.operateRect(0, 0, src, 0, 0, 1, 1, 99);")
            .is_err()
    );
}
