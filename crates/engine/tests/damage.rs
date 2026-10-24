//! Integration tests for the damage model: what the scene records, and what
//! the engine resolves it into.
//!
//! The model is conservative: the region it reports must cover every pixel
//! whose rendered result may differ from the previous frame. These tests pin
//! that containment property for the changes the scene can see, and the
//! fallback to a full repaint for the ones it cannot bound.

use engine::Scene;
use engine::damage::DamageRegion;
use engine::{Engine, EngineParts, GameFrame, Rect};
use render::{PipelineConfig, begin_render_state, create_render_pipeline};
use testkit::headless_window;

/// A hook that does nothing, so a frame runs the real path.
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

/// Adds a window with one layer at `rect` and returns the layer's id.
fn scene_with_layer(scene: &mut Scene, rect: Rect) -> (u32, u32) {
    let window = scene.add_window("t", (320, 240));
    let layer = scene.add_layer(window, None);
    scene.layer_mut(layer).expect("layer").rect = rect;
    (window, layer)
}

// --- the scene's capture points ---------------------------------------------

#[test]
fn a_property_write_that_changes_nothing_records_no_damage() {
    let mut scene = Scene::default();
    let (_window, layer) = scene_with_layer(&mut scene, Rect::new(1, 2, 3, 4));

    let changed = scene
        .update_layer(layer, |_layer| false)
        .expect("layer exists");
    assert!(!changed, "the write reported no change");

    assert!(
        scene.damage().is_empty(),
        "an unchanged write records nothing"
    );
}

#[test]
fn a_property_write_records_the_previous_rectangle() {
    let mut scene = Scene::default();
    let (_window, layer) = scene_with_layer(&mut scene, Rect::new(1, 2, 3, 4));

    let changed = scene
        .update_layer(layer, |layer| {
            layer.rect = Rect::new(5, 6, 7, 8);
            true
        })
        .expect("layer exists");
    assert!(changed, "the write reported a change");

    let log = scene.damage();
    assert_eq!(log.changed.len(), 1);
    assert_eq!(log.changed[0].layer, layer);
    assert_eq!(
        log.changed[0].previous,
        Some(Rect::new(1, 2, 3, 4)),
        "the rectangle before the change is recorded"
    );
}

#[test]
fn an_order_change_records_the_layer() {
    let mut scene = Scene::default();
    let window = scene.add_window("t", (320, 240));
    let parent = scene.add_layer(window, None);
    let back = scene.add_layer(window, Some(parent));
    let front = scene.add_layer(window, Some(parent));
    scene.layer_mut(back).expect("layer").rect = Rect::new(0, 0, 4, 4);
    scene.layer_mut(front).expect("layer").rect = Rect::new(8, 8, 4, 4);
    scene.take_damage();

    scene.set_order_index(back, 1).expect("the order changed");

    let log = scene.damage();
    assert_eq!(log.changed.len(), 1);
    assert_eq!(log.changed[0].layer, back);
}

#[test]
fn a_removal_asks_for_a_full_repaint() {
    let mut scene = Scene::default();
    let (_window, layer) = scene_with_layer(&mut scene, Rect::new(1, 2, 3, 4));
    scene.take_damage();

    scene.remove_layer(layer);

    let log = scene.damage();
    assert!(
        log.full,
        "a removed layer leaves an area that cannot be composed"
    );
}

#[test]
fn taking_the_damage_leaves_the_scene_clean() {
    let mut scene = Scene::default();
    let (_window, layer) = scene_with_layer(&mut scene, Rect::new(1, 2, 3, 4));
    scene
        .update_layer(layer, |layer| {
            layer.rect = Rect::new(5, 6, 7, 8);
            true
        })
        .expect("layer exists");

    let log = scene.take_damage();
    assert_eq!(log.changed.len(), 1);
    assert!(scene.damage().is_empty(), "the log was taken");
}

// --- the engine's resolution ------------------------------------------------

#[test]
fn the_first_frame_repaints_the_whole_surface() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;

    engine.frame(0, &mut hook).expect("frame");

    assert!(
        engine.last_damage().covers_everything(),
        "the first frame has no previous content to keep"
    );
}

#[test]
fn a_frame_that_changed_nothing_leaves_no_damage() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    engine.frame(0, &mut hook).expect("frame");

    engine.frame(16, &mut hook).expect("frame");

    assert!(
        engine.last_damage().is_empty(),
        "an unchanged frame damages nothing"
    );
}

#[test]
fn a_changed_layer_damages_its_own_rectangle() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let layer = engine.scene_mut().add_layer(window, None);
    engine.scene_mut().layer_mut(layer).expect("layer").rect = Rect::new(10, 20, 30, 40);
    engine.frame(0, &mut hook).expect("frame");

    engine
        .scene_mut()
        .update_layer(layer, |layer| {
            layer.rect = Rect::new(10, 20, 30, 40);
            true
        })
        .expect("layer exists");
    engine.frame(16, &mut hook).expect("frame");

    let damage = engine.last_damage();
    assert!(!damage.covers_everything(), "the change is bounded");
    assert_eq!(damage.rects(), &[Rect::new(10, 20, 30, 40)]);
}

#[test]
fn a_layer_under_an_offset_parent_damages_the_composed_rectangle() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let parent = engine.scene_mut().add_layer(window, None);
    let child = engine.scene_mut().add_layer(window, Some(parent));
    engine.scene_mut().layer_mut(parent).expect("layer").rect = Rect::new(100, 50, 64, 64);
    engine.scene_mut().layer_mut(child).expect("layer").rect = Rect::new(5, 7, 8, 9);
    engine.frame(0, &mut hook).expect("frame");

    engine
        .scene_mut()
        .update_layer(child, |layer| {
            layer.opacity = 0.5;
            true
        })
        .expect("layer exists");
    engine.frame(16, &mut hook).expect("frame");

    let damage = engine.last_damage();
    assert_eq!(
        damage.rects(),
        &[Rect::new(105, 57, 8, 9)],
        "the damage is the composed rectangle, not the layer's own"
    );
}

#[test]
fn a_layer_that_moved_damages_the_old_and_the_new_rectangle() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let layer = engine.scene_mut().add_layer(window, None);
    engine.scene_mut().layer_mut(layer).expect("layer").rect = Rect::new(0, 0, 10, 10);
    engine.frame(0, &mut hook).expect("frame");

    engine
        .scene_mut()
        .update_layer(layer, |layer| {
            layer.rect = Rect::new(50, 50, 10, 10);
            true
        })
        .expect("layer exists");
    engine.frame(16, &mut hook).expect("frame");

    let damage = engine.last_damage();
    assert_eq!(damage.rects().len(), 2, "the old and the new area");
    assert!(damage.rects().contains(&Rect::new(0, 0, 10, 10)));
    assert!(damage.rects().contains(&Rect::new(50, 50, 10, 10)));
}

#[test]
fn a_full_repaint_is_reported_as_unbounded() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;
    let window = engine.scene_mut().add_window("t", (64, 64));
    let layer = engine.scene_mut().add_layer(window, None);
    engine.scene_mut().layer_mut(layer).expect("layer").rect = Rect::new(0, 0, 10, 10);
    engine.frame(0, &mut hook).expect("frame");

    engine.scene_mut().remove_layer(layer);
    engine.frame(16, &mut hook).expect("frame");

    assert!(
        engine.last_damage().covers_everything(),
        "a removed layer asks for the whole surface"
    );
}

#[test]
fn the_pending_damage_is_empty_after_a_presented_frame() {
    let mut engine = headless_engine((64, 64));
    let mut hook = NoopFrame;

    engine.frame(0, &mut hook).expect("frame");

    assert!(
        engine.pending_damage().is_empty(),
        "a presented frame carries the damage away"
    );
}

#[test]
fn the_damage_model_reports_its_own_shape() {
    let mut region = DamageRegion::empty();
    assert!(region.is_empty());
    region.add(Rect::new(1, 2, 3, 4));
    assert_eq!(region.rects().len(), 1);
    assert_eq!(region.bounds(), Some(Rect::new(1, 2, 3, 4)));
}
