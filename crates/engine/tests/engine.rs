//! Frame tests for the middle-layer [`Engine`]: clock, hook, timers, surface
//! resize, projection reuse and the render-only overlay.
//!
//! Everything runs on the null backend with a headless window handle, so the
//! tests drive the real frame path without a display server or a GPU.

use std::cell::Cell;
use std::rc::Rc;

use api::geometry::RectF;
use api::host::SurfaceEvent;
use engine::{Engine, EngineError, EngineOverlay, EngineParts, GameFrame, Rect};
use render::{
    PipelineConfig, RenderContent, RenderNode, RenderPipeline, RenderState, Texture, TextureDesc,
    begin_render_state, create_render_pipeline, create_texture,
};
use testkit::headless_window;

/// Sibling sort key the test overlay uses, so its node is identifiable in the
/// projected arena.
const OVERLAY_Z: i32 = i32::MAX;

/// Records every hook call's `(now_ms, dt_ms)` pair.
#[derive(Default)]
struct Recorder {
    calls: Vec<(u64, f64)>,
}

impl GameFrame for Recorder {
    fn before_render(&mut self, now_ms: u64, dt_ms: f64) -> Result<(), String> {
        self.calls.push((now_ms, dt_ms));
        Ok(())
    }
}

/// A render-only overlay that counts its own refreshes.
struct CountingOverlay {
    texture: Texture,
    refreshes: Rc<Cell<u32>>,
}

impl EngineOverlay for CountingOverlay {
    fn refresh(
        &mut self,
        _pipeline: &RenderPipeline,
        _state: &mut RenderState,
        _now_ms: u64,
    ) -> Result<(), String> {
        self.refreshes.set(self.refreshes.get().saturating_add(1));
        Ok(())
    }

    fn node(&self) -> RenderNode {
        RenderNode {
            content: RenderContent::Texture(self.texture.clone()),
            dst: RectF::new(0.0, 0.0, 8.0, 8.0),
            z: OVERLAY_Z,
            ..RenderNode::default()
        }
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

/// Arena indices of the overlay's nodes in the projected scene.
fn overlay_nodes(engine: &Engine) -> Vec<usize> {
    engine
        .render_scene()
        .nodes
        .iter()
        .enumerate()
        .filter(|(_index, node)| node.z == OVERLAY_Z)
        .map(|(index, _node)| index)
        .collect()
}

/// The index of the render scene root's last child.
fn last_child_index(engine: &Engine) -> Option<usize> {
    let root = engine.render_scene().root;
    engine
        .render_scene()
        .node(root)
        .expect("render scene root")
        .children
        .last()
        .map(|id| id.index() as usize)
}

/// Adds a layer with an `8x8` image at the window origin.
///
/// A layer draws only once it has an image, so this is what makes the
/// projection produce a node for it.
fn drawable_layer(engine: &mut Engine, window: u32) -> u32 {
    let layer = engine.scene_mut().add_layer(window, None);
    let bitmap = engine.scene_mut().add_bitmap(8, 8, vec![0u8; 8 * 8 * 4]);
    let state = engine.scene_mut().layer_mut(layer).expect("layer");
    state.rect = Rect::new(0, 0, 8, 8);
    state.visible = true;
    state.bitmap = Some(bitmap);
    state.image_width = 8;
    state.image_height = 8;
    layer
}

#[test]
fn step_advances_the_frame_clock_and_frame_index() {
    let mut engine = headless_engine((64, 64));
    let mut hooks = Recorder::default();

    let first = engine.step(10.0, &mut hooks).expect("first frame");
    assert_eq!(first.frame_index, 1);
    assert_eq!(first.elapsed_ms, 10.0);

    engine.step(10.0, &mut hooks).expect("second frame");
    let third = engine.step(10.0, &mut hooks).expect("third frame");
    assert_eq!(third.frame_index, 3);
    assert_eq!(third.elapsed_ms, 30.0);
    assert_eq!(engine.frame_index(), 3);
    assert_eq!(engine.elapsed_ms(), 30.0);

    // A negative delta counts as zero and does not rewind the clock.
    let fourth = engine.step(-5.0, &mut hooks).expect("fourth frame");
    assert_eq!(fourth.frame_index, 4);
    assert_eq!(fourth.elapsed_ms, 30.0);
}

#[test]
fn frame_report_carries_the_null_backend_stats() {
    let mut engine = headless_engine((32, 32));
    let mut hooks = Recorder::default();

    let report = engine.step(1.0, &mut hooks).expect("frame");
    let stats = report.stats.expect("the null backend reports stats");
    assert_eq!(stats, engine.last_frame().expect("last frame"));
    assert!(engine.last_frame_timings().is_some());
    assert_eq!(engine.backend(), render::BackendKind::Null);
}

#[test]
fn hook_receives_the_frame_clock_and_delta() {
    let mut engine = headless_engine((16, 16));
    let mut hooks = Recorder::default();

    engine.step(10.0, &mut hooks).expect("first frame");
    engine.step(5.0, &mut hooks).expect("second frame");
    engine.frame(40, &mut hooks).expect("third frame");

    assert_eq!(hooks.calls, vec![(10, 10.0), (15, 5.0), (40, 25.0)]);

    // The first `frame` call of an engine has no previous clock, so the hook
    // sees a zero delta.
    let mut fresh = headless_engine((16, 16));
    let mut fresh_hooks = Recorder::default();
    fresh.frame(7, &mut fresh_hooks).expect("first frame");
    assert_eq!(fresh_hooks.calls, vec![(7, 0.0)]);
}

#[test]
fn timer_callback_fires_inside_step() {
    let mut engine = headless_engine((16, 16));
    let fired = Rc::new(Cell::new(0u32));
    let counter = Rc::clone(&fired);
    engine
        .timer_mut()
        .create_interval(
            10,
            Box::new(move |_id| counter.set(counter.get().saturating_add(1))),
        )
        .expect("timer");

    let mut hooks = Recorder::default();
    engine.step(9.0, &mut hooks).expect("frame at 9ms");
    assert_eq!(fired.get(), 0, "the timer is due at 10ms");

    engine.step(1.0, &mut hooks).expect("frame at 10ms");
    assert_eq!(fired.get(), 1);

    engine.step(10.0, &mut hooks).expect("frame at 20ms");
    assert_eq!(fired.get(), 2, "the interval keeps re-arming");
}

#[test]
fn resize_updates_the_surface_window_and_primary_layer() {
    // reference/cpp/core/visual/LayerManager.cpp:342 — a window resize sets
    // the primary layer's size; reference/cpp/core/visual/LayerManager.cpp:187
    // derives the client size back from that primary layer.
    let mut engine = headless_engine((64, 48));
    let window = engine.scene_mut().add_window("game", (64, 48));
    let primary = engine.scene_mut().add_layer(window, None);

    let event = engine.resize((128, 96)).expect("resize");
    assert_eq!(event, SurfaceEvent::Resized(128, 96));
    assert_eq!(engine.surface_size(), (128, 96));
    assert_eq!(
        engine.scene().window(window).expect("window").inner_size,
        (128, 96)
    );
    assert_eq!(
        engine.scene().window(window).expect("window").primary_layer,
        Some(primary)
    );
    assert_eq!(
        engine.scene().layer(primary).expect("layer").rect,
        Rect::new(0, 0, 128, 96)
    );

    // A zero size clamps to one pixel per axis.
    assert_eq!(
        engine.resize((0, 0)).expect("resize"),
        SurfaceEvent::Resized(1, 1)
    );
    assert_eq!(engine.surface_size(), (1, 1));

    // An unchanged size is reported without touching the backend.
    assert_eq!(
        engine.resize((1, 1)).expect("resize"),
        SurfaceEvent::Resized(1, 1)
    );
    assert_eq!(
        engine.scene().window(window).expect("window").inner_size,
        (1, 1)
    );
}

#[test]
fn a_skipped_frame_reuses_the_render_arena_and_a_mutation_reprojects() {
    let mut engine = headless_engine((32, 32));
    let window = engine.scene_mut().add_window("game", (32, 32));
    let primary = drawable_layer(&mut engine, window);
    let mut hooks = Recorder::default();

    engine.step(1.0, &mut hooks).expect("first frame");
    let nodes = engine.render_scene().nodes.len();
    let arena = engine.render_scene().nodes.as_ptr();
    assert!(nodes >= 2, "the root plus the projected layer, got {nodes}");

    // An unchanged revision is not rebuilt, so the arena is untouched.
    engine.step(1.0, &mut hooks).expect("second frame");
    assert_eq!(engine.render_scene().nodes.len(), nodes);
    assert_eq!(engine.render_scene().nodes.as_ptr(), arena);

    // A property write bumps the revision; the rebuild clears and refills the
    // same arena, so neither the node count nor the allocation changes.
    engine
        .scene_mut()
        .layer_mut(primary)
        .expect("layer")
        .opacity = 0.5;
    engine.step(1.0, &mut hooks).expect("third frame");
    assert_eq!(engine.render_scene().nodes.len(), nodes);
    assert_eq!(engine.render_scene().nodes.as_ptr(), arena);

    // A structural change adds a projected node.
    drawable_layer(&mut engine, window);
    engine.step(1.0, &mut hooks).expect("fourth frame");
    assert!(engine.render_scene().nodes.len() > nodes);
}

#[test]
fn overlay_node_is_attached_once_and_refreshed_every_frame() {
    let mut parts = headless_parts((32, 32));
    let refreshes = Rc::new(Cell::new(0u32));
    let texture = create_texture(&parts.pipeline, TextureDesc::new(8, 8)).expect("texture");
    parts.overlay = Some(Box::new(CountingOverlay {
        texture,
        refreshes: Rc::clone(&refreshes),
    }));
    let mut engine = Engine::new(parts).expect("engine");
    assert!(engine.debug_overlay_enabled());

    let window = engine.scene_mut().add_window("game", (32, 32));
    drawable_layer(&mut engine, window);
    let mut hooks = Recorder::default();

    engine.step(1.0, &mut hooks).expect("first frame");
    assert_eq!(refreshes.get(), 1);
    assert_eq!(overlay_nodes(&engine).len(), 1);

    // The overlay node is the last child of the render scene root.
    assert_eq!(
        last_child_index(&engine),
        overlay_nodes(&engine).first().copied()
    );

    // A frame that skips the rebuild neither refreshes the node list nor drops
    // the overlay, and the overlay itself is still refreshed once per frame.
    engine.step(1.0, &mut hooks).expect("second frame");
    assert_eq!(refreshes.get(), 2);
    assert_eq!(overlay_nodes(&engine).len(), 1);

    // A rebuild re-attaches the overlay exactly once.
    drawable_layer(&mut engine, window);
    engine.step(1.0, &mut hooks).expect("third frame");
    assert_eq!(refreshes.get(), 3);
    assert_eq!(overlay_nodes(&engine).len(), 1);
    assert_eq!(
        last_child_index(&engine),
        overlay_nodes(&engine).first().copied()
    );
}

#[test]
fn a_hook_error_abandons_the_frame() {
    struct Failing;

    impl GameFrame for Failing {
        fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
            Err("hook failed".to_owned())
        }
    }

    let mut engine = headless_engine((16, 16));
    let error = engine.step(1.0, &mut Failing).expect_err("frame must fail");
    match error {
        EngineError::Frame(message) => assert_eq!(message, "hook failed"),
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(engine.frame_index(), 0);
}
