//! Behaviour tests for the window's primary layer — the screen root a layer
//! created without a parent becomes.
//!
//! The reference gives that layer its own defaults at creation: the opaque
//! type, the opaque white neutral colour and the draw face the opaque type
//! resolves to. The tests drive the real script surface: they create the
//! layers through the VM and read the members and the pixels the script sees.

mod common;

use common::{Env, vm_lock};

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
        | (u32::from(rgba[0]) << 16)
        | (u32::from(rgba[1]) << 8)
        | u32::from(rgba[2])
}

/// The alpha byte of the pixel at `(x, y)` of the layer's main image.
fn layer_alpha(env: &Env, index: usize, x: usize, y: usize) -> u8 {
    (layer_pixel(env, index, x, y) >> 24) as u8
}

/// Reference: `tTJSNI_Layer::Construct` gives a layer that has no parent
/// the primary-layer defaults — `Type = DisplayType = ltOpaque` and
/// `NeutralColor = TransparentColor = TVP_RGBA2COLOR(255, 255, 255, 255)`
/// (`reference/cpp/core/visual/LayerIntf.cpp:463-472`). A layer with a parent
/// keeps the constructor's defaults — `DisplayType = Type = ltAlpha` and the
/// transparent white neutral colour
/// (`reference/cpp/core/visual/LayerIntf.cpp:332-334`).
#[test]
fn the_primary_layer_carries_the_opaque_type_and_neutral_colour() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var primary = new Layer(w, null); \
         var child = new Layer(w, primary);",
    );
    // The primary layer's type is the opaque one.
    assert_eq!(env.eval_int("primary.type"), 1);
    // Its neutral colour is opaque white.
    assert_eq!(env.eval_int("primary.neutralColor"), 0xffff_ffff);
    // A layer with a parent keeps the constructor's defaults.
    assert_eq!(env.eval_int("child.type"), 2);
    assert_eq!(env.eval_int("child.neutralColor"), 0x00ff_ffff);
}

/// Reference: `UpdateDrawFace` resolves the draw face from the layer's
/// display type when the outward face is `dfAuto`
/// (`reference/cpp/core/visual/LayerIntf.cpp:1525-1618`): the opaque type
/// resolves to `dfOpaque`, and an opaque face's fill holds the
/// destination's alpha where an alpha face writes it
/// (`reference/cpp/core/visual/LayerIntf.cpp:4280-4292`).
#[test]
fn the_primary_layer_resolves_the_opaque_draw_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var primary = new Layer(w, null); \
         primary.setImageSize(4, 4); \
         primary.holdAlpha = true; \
         primary.fillRect(0, 0, 4, 4, 0xff0000ff);",
    );
    // The default image is transparent white, so the destination's alpha is
    // zero and the opaque face holds it.
    assert_eq!(layer_alpha(&env, 0, 0, 0), 0);
}

/// Reference: `GetOperationModeFromType` maps the opaque type to the opaque
/// operation mode (`reference/cpp/core/visual/LayerIntf.cpp:1621-1666`), and
/// the operation mode and the draw face pick the blend method together
/// (`reference/cpp/core/visual/LayerIntf.cpp:4118-4271`): the opaque mode
/// copies the source onto an opaque face
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:1589-1622`). The mode of
/// an operation without an explicit one is the source layer's operation mode
/// (`reference/cpp/core/visual/LayerIntf.cpp:8981-8993`).
#[test]
fn an_operation_from_the_primary_layer_copies_the_source() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var primary = new Layer(w, null); \
         primary.setImageSize(2, 2); \
         primary.setMaskPixel(0, 0, 128); \
         primary.setMainPixel(0, 0, 0x00ff0000); \
         var dst = new Layer(w, primary); \
         dst.face = dfOpaque; \
         dst.setImageSize(2, 2); \
         dst.setMaskPixel(0, 0, 255); \
         dst.setMainPixel(0, 0, 0x0000ff00); \
         dst.operateRect(0, 0, primary, 0, 0, 2, 2);",
    );
    // The half-alpha red replaced the opaque green; a blend would have mixed
    // the green into it and kept the destination's alpha.
    assert_eq!(layer_pixel(&env, 1, 0, 0), 0x80ff_0000);
}
