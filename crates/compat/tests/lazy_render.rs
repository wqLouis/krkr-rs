//! Behaviour tests for the change signal on the layer draw path.
//!
//! The engine draws the whole scene when anything changed and draws nothing
//! when nothing did. The signal has two halves: the scene is marked changed,
//! and the bitmap that carries the changed pixels is armed for the upload. A
//! layer whose pixels change must arm both — the scene flag makes the frame
//! draw again, and the bitmap flag makes the texture hold the new pixels. A
//! change that arms only one of them is computed and never presented.
//!
//! Reference `tTJSNI_BaseLayer::BeforeCompletion`
//! (`reference/cpp/core/visual/LayerIntf.cpp:6394-6407`): the reference
//! redraws the layer tree on every frame and posts `onPaint` on every draw
//! completion, so every change a draw would have shown is shown. The
//! engine's signal replaces the redraw, so it must mark every such change.

mod common;

use common::{Env, vm_lock};

/// The id of the bitmap that carries the layer's main image.
fn main_image(env: &Env, layer: usize) -> u32 {
    env.scene
        .layers
        .get(layer)
        .and_then(|layer| layer.bitmap)
        .expect("the layer has a main image")
}

/// A pixel write arms the upload of the image that carries the pixels.
///
/// The upload flag is the half of the signal the scene flag cannot replace:
/// the frame draws again when the scene changed, but it shows the new pixels
/// only when the bitmap's texture was sent to the backend.
#[test]
fn a_pixel_write_arms_the_upload() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(32, 32);\
         var l = new Layer(w, null);\
         l.setImageSize(32, 32);",
    );
    let image = main_image(&env, 0);
    assert!(
        env.scene.set_bitmap_clean(image),
        "the layer image is present"
    );
    assert!(
        !env.scene.bitmap(image).expect("the image").dirty,
        "a clean image is not uploaded"
    );

    env.run("l.fillRect(0, 0, 32, 32, 0xff00ff00);");

    assert!(
        env.scene.bitmap(image).expect("the image").dirty,
        "the pixel write arms the upload, or the change is never presented"
    );
}

/// A child's pixel write arms the upload of the child's own image.
///
/// The child is drawn through its own node, so the change is presented when
/// the child's image is uploaded; the parent's image keeps its own pixels and
/// is not uploaded.
#[test]
fn a_childs_pixel_write_arms_the_upload() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(32, 32);\
         var parent = new Layer(w, null);\
         parent.setImageSize(32, 32);\
         var child = new Layer(w, parent);\
         child.setImageSize(16, 16);",
    );
    let parent_image = main_image(&env, 0);
    let child_image = main_image(&env, 1);
    assert!(
        env.scene.set_bitmap_clean(parent_image),
        "the parent image is present"
    );
    assert!(
        env.scene.set_bitmap_clean(child_image),
        "the child image is present"
    );

    env.run("child.fillRect(0, 0, 16, 16, 0xffff0000);");

    assert!(
        env.scene.bitmap(child_image).expect("the image").dirty,
        "the child's pixel write arms the child's upload"
    );
    assert!(
        !env.scene.bitmap(parent_image).expect("the image").dirty,
        "the parent draws the child through its own node, so its own image is untouched"
    );
}

/// A write that changes no pixel leaves the image clean.
///
/// The signal must not weaken into "always dirty": a frame with nothing to
/// show is not drawn again, and its image is not uploaded.
#[test]
fn a_write_that_changes_nothing_leaves_the_image_clean() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(32, 32);\
         var l = new Layer(w, null);\
         l.setImageSize(32, 32);",
    );
    let image = main_image(&env, 0);
    assert!(
        env.scene.set_bitmap_clean(image),
        "the layer image is present"
    );

    // An empty rectangle intersects the clip in nothing, so no pixel changes.
    env.run("l.fillRect(0, 0, 0, 0, 0xff00ff00);");

    assert!(
        !env.scene.bitmap(image).expect("the image").dirty,
        "a write that changes nothing leaves the image clean"
    );
}

/// A layer that redraws itself in `onPaint` changes its pixels, and the
/// change is presented.
///
/// The reference posts `onPaint` on every draw completion
/// (`reference/cpp/core/visual/LayerIntf.cpp:6394-6407`), and a scripted
/// layer that draws itself there is animated by the dispatch. The dispatch
/// runs in the frame body, before the engine draws, so the new pixels belong
/// to the frame the engine presents.
#[test]
fn a_layer_that_redraws_itself_in_on_paint_arms_the_upload() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(32, 32);\
         var l = new Layer(w, null);\
         l.setImageSize(32, 32);\
         l.onPaint = function() { this.fillRect(0, 0, 32, 32, 0xff00ff00); };\
         l.update();",
    );
    let image = main_image(&env, 0);
    assert!(
        env.scene.set_bitmap_clean(image),
        "the layer image is present"
    );

    // The frame body's transition step dispatches the `onPaint` the
    // `update()` armed.
    env.advance_transitions();

    assert!(
        env.scene.bitmap(image).expect("the image").dirty,
        "the redraw in onPaint arms the upload, or the change is never presented"
    );
}
