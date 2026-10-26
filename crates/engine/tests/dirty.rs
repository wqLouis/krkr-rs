//! Integration tests for the scene's change signal.
//!
//! The engine draws the whole scene when anything changed and draws nothing
//! when nothing did. The signal is conservative: a change that can alter the
//! picture sets it, and it is cleared only after a frame the display
//! accepted — a frame that was skipped leaves the previous frame's content on
//! screen, and the change must be drawn again.

use api::geometry::PixelRegion;
use engine::{Engine, EngineParts, GameFrame, Rect};
use render::{PipelineConfig, begin_render_state, create_render_pipeline};
use testkit::headless_window;

/// A hook that changes nothing, so a frame runs the real path.
struct NoopFrame;

impl GameFrame for NoopFrame {
    fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
        Ok(())
    }
}

/// Builds the bottom objects of a headless engine at `size`.
fn headless_parts(size: (u32, u32)) -> EngineParts {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    let handle = headless_window();
    let (surface, render_state) = begin_render_state(&pipeline, &handle).expect("surface");
    EngineParts {
        pipeline,
        surface,
        render_state,
        surface_size: size,
        headless: true,
        overlay: None,
    }
}

/// Builds a headless engine over the null backend.
fn headless_engine(size: (u32, u32)) -> Engine {
    Engine::new(headless_parts(size)).expect("engine")
}

/// Adds the primary layer at the headless surface size.
fn primary_layer(engine: &mut Engine, window: u32) -> u32 {
    let layer = engine.scene_mut().add_layer(window, None);
    engine
        .scene_mut()
        .update_layer(layer, |layer| {
            layer.rect.w = 64;
            layer.rect.h = 64;
            true
        })
        .expect("layer");
    layer
}

/// A frame with nothing to show does not draw, and leaves the scene clean.
#[test]
fn an_idle_frame_leaves_the_scene_clean() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;

    let first = engine.frame(0, &mut hook).expect("frame");
    assert!(first.presented, "the first frame draws");
    assert!(
        !engine.scene().dirty(),
        "a presented frame carries the change away"
    );

    let second = engine.frame(16, &mut hook).expect("frame");
    assert!(
        !second.presented,
        "a frame with nothing to show does not draw"
    );
    assert!(
        !engine.scene().dirty(),
        "an idle frame leaves the scene clean"
    );
}

/// A change is drawn again, and the frame presents.
#[test]
fn a_changed_frame_draws_again() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let layer = primary_layer(&mut engine, window);
    engine.frame(0, &mut hook).expect("frame");
    assert!(
        !engine.scene().dirty(),
        "the first frame carried the change"
    );

    engine
        .scene_mut()
        .update_layer(layer, |layer| {
            layer.rect = Rect::new(1, 2, 3, 4);
            true
        })
        .expect("layer");
    assert!(engine.scene().dirty(), "the change is recorded");

    let report = engine.frame(16, &mut hook).expect("frame");
    assert!(report.presented, "a changed frame draws");
    assert!(
        !engine.scene().dirty(),
        "the presented frame carried the change away"
    );
}

/// A pixel write changes the picture.
#[test]
fn a_bitmap_write_marks_the_scene_changed() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    engine.frame(0, &mut hook).expect("frame");
    assert!(!engine.scene().dirty());

    let id = engine.scene_mut().add_bitmap(4, 4, vec![0u8; 4 * 4 * 4]);
    assert!(engine.scene().dirty(), "a new bitmap changes the picture");
    engine.frame(16, &mut hook).expect("frame");
    assert!(!engine.scene().dirty());

    engine
        .scene_mut()
        .bitmap_mut(id)
        .expect("bitmap")
        .mark_dirty_rect(PixelRegion::new(0, 0, 2, 2));
    assert!(engine.scene().dirty(), "a pixel write changes the picture");
}

/// A change to the layer's geometry changes the picture.
#[test]
fn a_geometry_change_marks_the_scene_changed() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let layer = primary_layer(&mut engine, window);
    engine.frame(0, &mut hook).expect("frame");
    assert!(!engine.scene().dirty());

    engine
        .scene_mut()
        .update_layer(layer, |layer| {
            layer.rect = Rect::new(1, 2, 3, 4);
            true
        })
        .expect("layer");
    assert!(
        engine.scene().dirty(),
        "a geometry change changes the picture"
    );
}

/// A write that changes nothing does not mark the scene.
#[test]
fn a_write_that_changes_nothing_keeps_the_scene_clean() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let layer = primary_layer(&mut engine, window);
    engine.frame(0, &mut hook).expect("frame");
    assert!(!engine.scene().dirty());

    let changed = engine
        .scene_mut()
        .update_layer(layer, |_layer| false)
        .expect("layer");
    assert!(!changed, "the write reported no change");
    assert!(
        !engine.scene().dirty(),
        "a write that changes nothing keeps the scene clean"
    );
}

/// A change of the draw order changes the picture.
#[test]
fn an_order_change_marks_the_scene_changed() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let parent = primary_layer(&mut engine, window);
    let back = engine.scene_mut().add_layer(window, Some(parent));
    engine.scene_mut().add_layer(window, Some(parent));
    engine.frame(0, &mut hook).expect("frame");
    assert!(!engine.scene().dirty());

    engine
        .scene_mut()
        .set_order_index(back, 1)
        .expect("the order changed");
    assert!(
        engine.scene().dirty(),
        "an order change changes the picture"
    );
}

/// A removed layer changes the picture.
#[test]
fn a_removal_marks_the_scene_changed() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let layer = primary_layer(&mut engine, window);
    engine.frame(0, &mut hook).expect("frame");
    assert!(!engine.scene().dirty());

    engine.scene_mut().remove_layer(layer);
    assert!(
        engine.scene().dirty(),
        "a removed layer changes the picture"
    );
}

/// A change followed by a frame that presented nothing stays changed.
#[test]
fn a_change_followed_by_a_frame_that_presents_nothing_stays_dirty() {
    let mut engine = headless_engine((64, 64));
    // A window that cannot be fitted into the framebuffer leaves the frame
    // without a presentation.
    let window = engine.scene_mut().add_window("t", (0, 0));
    let layer = engine.scene_mut().add_layer(window, None);
    engine
        .scene_mut()
        .update_layer(layer, |layer| {
            layer.visible = false;
            true
        })
        .expect("layer");
    assert!(engine.scene().dirty(), "the change is recorded");

    let mut hook = NoopFrame;
    let report = engine.frame(0, &mut hook).expect("frame");
    assert!(!report.presented, "the frame had nothing to present");
    assert!(
        engine.scene().dirty(),
        "a frame that presented nothing keeps the change"
    );
}

/// A layer whose pixels change is presented again.
///
/// The frame draws the whole scene when the scene changed and draws nothing
/// when it did not, so a pixel write must mark the scene changed: a frame
/// that skipped the change would leave the old pixels on the screen, and the
/// change would never be presented.
#[test]
fn a_layer_whose_pixels_change_is_presented_again() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let layer = primary_layer(&mut engine, window);
    assert!(
        engine.scene_mut().ensure_layer_image(layer, 8, 8),
        "the layer has an image"
    );
    engine.frame(0, &mut hook).expect("frame");
    assert!(
        !engine.scene().dirty(),
        "the first frame carried the change"
    );

    engine
        .scene_mut()
        .with_main_image_pixels_mut(layer, |pixels| pixels[0] = 255)
        .expect("the layer has an image");

    assert!(
        engine.scene().dirty(),
        "the pixel write changes the picture"
    );
    let report = engine.frame(16, &mut hook).expect("frame");
    assert!(report.presented, "the changed frame draws");
    assert!(
        !engine.scene().dirty(),
        "the presented frame carried the change away"
    );
}

/// A child whose pixels change is presented again.
///
/// The child is drawn through its own node, so its new pixels reach the
/// screen when the frame draws again. A container that draws a child's pixels
/// must not hide the change behind an unchanged container image.
#[test]
fn a_child_that_changes_is_presented_again() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let parent = primary_layer(&mut engine, window);
    let child = engine.scene_mut().add_layer(window, Some(parent));
    engine
        .scene_mut()
        .update_layer(child, |layer| {
            layer.rect = Rect::new(0, 0, 8, 8);
            true
        })
        .expect("the child");
    assert!(
        engine.scene_mut().ensure_layer_image(child, 8, 8),
        "the child has an image"
    );
    engine.frame(0, &mut hook).expect("frame");
    assert!(
        !engine.scene().dirty(),
        "the first frame carried the change"
    );

    engine
        .scene_mut()
        .with_main_image_pixels_mut(child, |pixels| pixels[0] = 255)
        .expect("the child has an image");

    assert!(
        engine.scene().dirty(),
        "the child's pixel write changes the picture"
    );
    let report = engine.frame(16, &mut hook).expect("frame");
    assert!(report.presented, "the changed frame draws");
}

/// A pixel write arms the upload of the image that carries the pixels.
///
/// The scene flag makes the frame draw again; the bitmap's own flag makes
/// the backend receive the new pixels. A change that arms only the first is
/// drawn again and shows the old picture.
#[test]
fn a_pixel_write_arms_the_upload() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let layer = primary_layer(&mut engine, window);
    assert!(
        engine.scene_mut().ensure_layer_image(layer, 8, 8),
        "the layer has an image"
    );
    let image = engine
        .scene()
        .layer(layer)
        .and_then(|layer| layer.bitmap)
        .expect("the layer image");
    engine.frame(0, &mut hook).expect("frame");
    assert!(
        !engine.scene().bitmap(image).expect("the image").dirty,
        "the uploaded image is clean"
    );

    engine
        .scene_mut()
        .with_main_image_pixels_mut(layer, |pixels| pixels[0] = 255)
        .expect("the layer has an image");

    assert!(
        engine.scene().bitmap(image).expect("the image").dirty,
        "the pixel write arms the upload, or the change is never presented"
    );
}
