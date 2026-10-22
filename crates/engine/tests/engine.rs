//! Frame tests for the middle-layer [`Engine`]: clock, hook, timers, surface
//! resize, projection reuse and the render-only overlay.
//!
//! Everything runs on the null backend with a headless window handle, so the
//! tests drive the real frame path without a display server or a GPU.

use std::cell::Cell;
use std::rc::Rc;

use api::geometry::{RectF, Transform2D};
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
        _objects: engine::ObjectCounts,
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

/// The projected `dst` rectangles of the drawn (textured) nodes, in arena
/// order.
fn drawn_dsts(engine: &Engine) -> Vec<RectF> {
    engine
        .render_scene()
        .nodes
        .iter()
        .filter(|node| matches!(&node.content, RenderContent::Texture(_)))
        .map(|node| node.dst)
        .collect()
}

/// The transform the render scene root carries.
fn scene_root_transform(engine: &Engine) -> Transform2D {
    let root = engine.render_scene().root;
    engine
        .render_scene()
        .node(root)
        .expect("render scene root")
        .transform
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
fn step_keeps_the_fractional_millisecond_across_frames() {
    // reference/cpp/core/utils/impl/TimerImpl.cpp:113,128-146
    // Each timer check re-arms from the current tick, so a frame delta must
    // not discard its sub-millisecond remainder.
    let mut engine = headless_engine((16, 16));
    let fired = Rc::new(Cell::new(0u32));
    let counter = Rc::clone(&fired);
    engine
        .timer_mut()
        .create_interval(
            16,
            Box::new(move |_id| counter.set(counter.get().saturating_add(1))),
        )
        .expect("timer");
    let mut hooks = Recorder::default();

    let mut last = None;
    for _ in 0..60 {
        last = Some(engine.step(16.6, &mut hooks).expect("frame"));
    }
    let last = last.expect("60 frames");

    assert!(
        (last.elapsed_ms - 996.0).abs() < 0.01,
        "60 x 16.6 ms must reach 996 ms, got {}",
        last.elapsed_ms
    );
    assert_eq!(last.frame_index, 60);
    assert_eq!(
        fired.get(),
        60,
        "a 16 ms timer fires on every 16.6 ms frame"
    );
    assert_eq!(engine.elapsed_ms(), last.elapsed_ms);
}

#[test]
fn a_saturating_delta_keeps_the_clock_monotonic() {
    let mut engine = headless_engine((16, 16));
    let mut hooks = Recorder::default();

    engine.step(f32::MAX, &mut hooks).expect("saturating frame");
    let saturated = engine.elapsed_ms();
    assert!(saturated.is_finite(), "the clock stays representable");
    assert_eq!(hooks.calls.last().expect("call").0, u64::MAX);

    let repeated = engine
        .step(f32::MAX, &mut hooks)
        .expect("second saturating frame");
    assert!(repeated.elapsed_ms >= saturated);
    assert_eq!(repeated.elapsed_ms, saturated, "the clock stays capped");
    assert_eq!(hooks.calls.last().expect("call").0, u64::MAX);

    let paused = engine.step(0.0, &mut hooks).expect("zero-delta frame");
    assert_eq!(
        paused.elapsed_ms, saturated,
        "a pause neither wraps nor rewinds"
    );
    assert_eq!(hooks.calls.last().expect("call").0, u64::MAX);
}

#[test]
fn frame_discards_the_fractional_step_remainder() {
    let mut engine = headless_engine((16, 16));
    let mut hooks = Recorder::default();

    engine.step(10.6, &mut hooks).expect("step");
    assert!((engine.elapsed_ms() - 10.6).abs() < 1e-3);

    let framed = engine.frame(40, &mut hooks).expect("frame");
    assert_eq!(framed.elapsed_ms, 40.0, "frame sets the clock exactly");

    let stepped = engine.step(0.5, &mut hooks).expect("step");
    assert!((stepped.elapsed_ms - 40.5).abs() < f64::EPSILON);
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
fn resize_changes_the_framebuffer_without_touching_the_scene_window() {
    // reference/cpp/core/visual/LayerManager.cpp:334-343 — a window resize
    // notification would size the primary layer, and the reference leaves that
    // path unused. The logical screen is the primary layer's rect, so an OS
    // resize moves only the framebuffer; presenting an unchanged screen at a
    // different framebuffer size is the documented extension (D34/D39/D40),
    // not reference behaviour.
    let mut engine = headless_engine((64, 48));
    let window = engine.scene_mut().add_window("game", (64, 48));
    let primary = engine.scene_mut().add_layer(window, None);
    engine.scene_mut().layer_mut(primary).expect("layer").rect = Rect::new(0, 0, 64, 48);
    let layer_rect = engine.scene().layer(primary).expect("layer").rect;

    // Observe the window before the OS resize: the initial window matches the
    // framebuffer, so it queues no request. The later framebuffer changes are
    // OS resizes and must not ask for a window resize.
    let mut hooks = Recorder::default();
    engine.step(1.0, &mut hooks).expect("frame");

    let event = engine.resize((128, 96)).expect("resize");
    assert_eq!(event, SurfaceEvent::Resized(128, 96));
    assert_eq!(engine.surface_size(), (128, 96));
    assert_eq!(engine.vm_size(), (64, 48), "the OS size is not the VM size");
    assert_eq!(
        engine.scene().window(window).expect("window").inner_size,
        (64, 48)
    );
    assert_eq!(
        engine.scene().window(window).expect("window").primary_layer,
        Some(primary)
    );
    assert_eq!(
        engine.scene().layer(primary).expect("layer").rect,
        layer_rect
    );

    // 64x48 into 128x96 doubles both axes with no margin.
    let translation = engine.translation().expect("mapping");
    assert_eq!(translation.scale, 2.0);
    assert_eq!(translation.offset, (0.0, 0.0));

    // An OS resize never asks the composition root for a window resize.
    assert_eq!(engine.take_window_resize(), None);

    // A zero size clamps to one pixel per axis.
    assert_eq!(
        engine.resize((0, 0)).expect("resize"),
        SurfaceEvent::Resized(1, 1)
    );
    assert_eq!(engine.surface_size(), (1, 1));
    assert_eq!(engine.vm_size(), (64, 48));

    // An unchanged size is reported without touching the backend.
    assert_eq!(
        engine.resize((1, 1)).expect("resize"),
        SurfaceEvent::Resized(1, 1)
    );
    assert_eq!(engine.vm_size(), (64, 48));
}

#[test]
fn a_scaled_presentation_sets_the_root_transform_and_keeps_the_vm_tree() {
    // The layer's VM placement (10,10,50,50) is reference-compatible; fitting
    // it into a differently sized framebuffer is the documented extension
    // (D34/D39/D40).
    let mut engine = headless_engine((200, 200));
    let window = engine.scene_mut().add_window("game", (100, 100));
    // The logical screen is the primary layer's rect; the layer under test is
    // a later, freely placed layer.
    let primary = engine.scene_mut().add_layer(window, None);
    engine.scene_mut().layer_mut(primary).expect("primary").rect = Rect::new(0, 0, 100, 100);
    let layer = engine.scene_mut().add_layer(window, None);
    let bitmap = engine
        .scene_mut()
        .add_bitmap(50, 50, vec![0u8; 50 * 50 * 4]);
    {
        let state = engine.scene_mut().layer_mut(layer).expect("layer");
        state.rect = Rect::new(10, 10, 50, 50);
        state.visible = true;
        state.bitmap = Some(bitmap);
        state.image_width = 50;
        state.image_height = 50;
    }
    let mut hooks = Recorder::default();

    engine.step(1.0, &mut hooks).expect("frame");
    let translation = engine.translation().expect("mapping");
    assert_eq!(translation.scale, 2.0);
    assert_eq!(translation.offset, (0.0, 0.0));
    assert_eq!(translation.vm_size, (100, 100));
    assert_eq!(translation.device_size, (200, 200));
    assert_eq!(
        scene_root_transform(&engine),
        translation.root_transform(),
        "the root carries the fit mapping"
    );
    assert_eq!(
        drawn_dsts(&engine),
        vec![RectF::new(10.0, 10.0, 50.0, 50.0)],
        "the projected nodes stay in VM pixels"
    );

    // A wider framebuffer letterboxes the VM surface: the scale stays 2 and
    // the 200-device-pixel margin is split into 100 on each side.
    engine.resize((400, 200)).expect("resize");
    engine.step(1.0, &mut hooks).expect("frame");
    let translation = engine.translation().expect("mapping");
    assert_eq!(translation.scale, 2.0);
    assert_eq!(translation.offset, (100.0, 0.0));
    assert_eq!(
        scene_root_transform(&engine),
        translation.root_transform(),
        "the root follows the letterboxed presentation"
    );
    assert_eq!(
        drawn_dsts(&engine),
        vec![RectF::new(10.0, 10.0, 50.0, 50.0)],
        "an OS resize does not rewrite the VM nodes"
    );
    assert_eq!(
        engine.scene().window(window).expect("window").inner_size,
        (100, 100),
        "an OS resize leaves the VM window alone"
    );
}

#[test]
fn the_overlay_stays_in_device_pixels_under_a_scaled_root() {
    // The overlay authors its panel in framebuffer pixels; its node transform
    // cancels the root so the Fit mapping moves no overlay pixel (D34/D39/D40).
    let mut parts = headless_parts((200, 200));
    let texture = create_texture(&parts.pipeline, TextureDesc::new(8, 8)).expect("texture");
    parts.overlay = Some(Box::new(CountingOverlay {
        texture,
        refreshes: Rc::new(Cell::new(0)),
    }));
    let mut engine = Engine::new(parts).expect("engine");
    let window = engine.scene_mut().add_window("game", (100, 100));
    let primary = engine.scene_mut().add_layer(window, None);
    engine.scene_mut().layer_mut(primary).expect("primary").rect = Rect::new(0, 0, 100, 100);
    let mut hooks = Recorder::default();

    engine.step(1.0, &mut hooks).expect("frame");
    let translation = engine.translation().expect("mapping");
    assert_eq!(translation.scale, 2.0);
    let root = scene_root_transform(&engine);
    assert_eq!(root, translation.root_transform());

    let index = overlay_nodes(&engine).first().copied().expect("overlay");
    let overlay = &engine.render_scene().nodes[index];
    assert_eq!(
        overlay.dst,
        RectF::new(0.0, 0.0, 8.0, 8.0),
        "the overlay authors device pixels"
    );
    assert_eq!(overlay.transform, translation.inverse_root_transform());
    let composed = overlay.transform.to_affine().then(&root.to_affine());
    assert_eq!(composed.transform_point((0.0, 0.0)), (0.0, 0.0));
    assert_eq!(composed.transform_point((8.0, 8.0)), (8.0, 8.0));

    // A letterboxed framebuffer moves the root by 100 px; the overlay stays at
    // its device origin because its inverse transform moves with the root.
    engine.resize((400, 200)).expect("resize");
    engine.step(1.0, &mut hooks).expect("letterboxed frame");
    let translation = engine.translation().expect("mapping");
    assert_eq!(translation.offset, (100.0, 0.0));
    let root = scene_root_transform(&engine);
    assert_eq!(root, translation.root_transform());
    let index = overlay_nodes(&engine).first().copied().expect("overlay");
    let overlay = &engine.render_scene().nodes[index];
    assert_eq!(overlay.dst, RectF::new(0.0, 0.0, 8.0, 8.0));
    assert_eq!(overlay.transform, translation.inverse_root_transform());
    let composed = overlay.transform.to_affine().then(&root.to_affine());
    assert_eq!(composed.transform_point((0.0, 0.0)), (0.0, 0.0));
}

#[test]
fn a_script_window_resize_is_surfaced_once() {
    // reference/cpp/core/visual/WindowIntf.cpp:885-893 — a script window resize
    // writes the client size; it is reported to the composition root, which
    // owns the OS window, while the logical screen stays the primary layer's
    // rect.
    let mut engine = headless_engine((200, 200));
    let window = engine.scene_mut().add_window("game", (100, 100));
    let primary = engine.scene_mut().add_layer(window, None);
    engine.scene_mut().layer_mut(primary).expect("primary").rect = Rect::new(0, 0, 100, 100);
    let mut hooks = Recorder::default();

    engine.step(1.0, &mut hooks).expect("frame");
    assert_eq!(
        engine.take_window_resize(),
        Some((100, 100)),
        "the declared size differs from the framebuffer, so it asks for a window resize"
    );
    assert_eq!(engine.take_window_resize(), None, "taking it clears it");

    engine.step(1.0, &mut hooks).expect("frame");
    assert_eq!(
        engine.take_window_resize(),
        None,
        "a size that does not change asks for nothing"
    );

    engine
        .scene_mut()
        .window_mut(window)
        .expect("window")
        .inner_size = (150, 150);
    engine.step(1.0, &mut hooks).expect("frame");
    assert_eq!(engine.take_window_resize(), Some((150, 150)));

    engine.step(1.0, &mut hooks).expect("frame");
    assert_eq!(
        engine.take_window_resize(),
        None,
        "later frames do not repeat the request"
    );
    assert_eq!(
        engine.vm_size(),
        (100, 100),
        "the logical screen is the primary layer, not the window client size"
    );
}

#[test]
fn a_primary_layer_resize_changes_the_vm_size_without_touching_the_client_size() {
    // reference/cpp/core/visual/LayerManager.cpp:334-343 — the primary layer is
    // the window's screen buffer, so its rect is the logical screen while the
    // window client size stays what the script wrote.
    let mut engine = headless_engine((1280, 720));
    let window = engine.scene_mut().add_window("game", (800, 600));
    let primary = engine.scene_mut().add_layer(window, None);
    engine.scene_mut().layer_mut(primary).expect("primary").rect = Rect::new(0, 0, 800, 600);
    let mut hooks = Recorder::default();

    engine.step(1.0, &mut hooks).expect("frame");
    assert_eq!(
        engine.scene().layer(primary).expect("layer").rect,
        Rect::new(0, 0, 800, 600),
        "the test starts the primary layer at 800x600"
    );
    assert_eq!(engine.vm_size(), (800, 600));
    assert_eq!(engine.take_window_resize(), Some((800, 600)));

    // A window client-size write is surfaced to the composition root but does
    // not move the logical screen.
    engine
        .scene_mut()
        .window_mut(window)
        .expect("window")
        .inner_size = (1024, 768);
    engine.step(1.0, &mut hooks).expect("frame");
    assert_eq!(engine.take_window_resize(), Some((1024, 768)));
    assert_eq!(engine.vm_size(), (800, 600));
    assert_eq!(
        engine.scene().window(window).expect("window").inner_size,
        (1024, 768)
    );
    assert_eq!(
        engine.scene().layer(primary).expect("layer").rect,
        Rect::new(0, 0, 800, 600)
    );

    // Resizing the primary layer is what changes the screen and therefore the
    // presentation mapping; the window client size is untouched and no window
    // resize is requested.
    engine.scene_mut().layer_mut(primary).expect("layer").rect = Rect::new(0, 0, 1920, 1080);
    engine.step(1.0, &mut hooks).expect("frame");
    assert_eq!(engine.vm_size(), (1920, 1080));
    assert_eq!(
        engine.scene().window(window).expect("window").inner_size,
        (1024, 768)
    );
    assert_eq!(engine.take_window_resize(), None);
    let translation = engine.translation().expect("mapping");
    assert_eq!(translation.vm_size, (1920, 1080));
    assert_eq!(translation.device_size, (1280, 720));
    assert!((translation.scale - 2.0 / 3.0).abs() < 1e-9);
    assert_eq!(translation.offset, (0.0, 0.0));
}

#[test]
fn an_empty_vm_window_skips_the_draw_and_recovers() {
    let mut engine = headless_engine((100, 100));
    let window = engine.scene_mut().add_window("game", (0, 0));
    let primary = engine.scene_mut().add_layer(window, None);
    engine.scene_mut().layer_mut(primary).expect("primary").rect = Rect::new(0, 0, 0, 0);
    let mut hooks = Recorder::default();

    let skipped = engine.step(1.0, &mut hooks).expect("frame");
    assert!(
        engine.translation().is_none(),
        "a zero VM dimension has no fit"
    );
    assert!(skipped.stats.is_none(), "nothing was drawn");
    assert_eq!(skipped.render_ms, 0.0);
    assert_eq!(skipped.project_ms, 0.0);
    assert_eq!(skipped.sync_ms, 0.0);
    assert_eq!(skipped.frame_index, 1, "the frame is still counted");

    // The script's size is reported even when it cannot be presented; the
    // composition root owns making it a valid OS window size.
    assert_eq!(engine.take_window_resize(), Some((0, 0)));

    // The unfittable size keeps skipping on later frames.
    let second = engine.step(1.0, &mut hooks).expect("frame");
    assert!(second.stats.is_none());
    assert_eq!(engine.take_window_resize(), None);

    engine
        .scene_mut()
        .window_mut(window)
        .expect("window")
        .inner_size = (50, 50);
    engine.scene_mut().layer_mut(primary).expect("layer").rect = Rect::new(0, 0, 50, 50);
    let recovered = engine.step(1.0, &mut hooks).expect("frame");
    assert!(
        recovered.stats.is_some(),
        "the draw resumes once a size fits"
    );
    assert!(engine.translation().is_some());
    assert_eq!(engine.frame_index(), 3);
}

#[test]
fn a_skipped_frame_still_runs_the_hook_and_its_due_timers() {
    // reference/cpp/core/visual/impl/VSyncTimingThread.cpp:114,141-146
    // A wake that presents nothing still delivers the VM/timer beat: the
    // reference posts the continuous-handler event right after the vsync wait,
    // whether or not that wake drew anything. The acquire-timeout skip takes
    // the same path — it happens inside the draw, after the timers and the
    // frame hook.
    let mut engine = headless_engine((100, 100));
    let window = engine.scene_mut().add_window("game", (0, 0));
    let primary = engine.scene_mut().add_layer(window, None);
    // A zero VM dimension has no presentation fit, so the frame draws nothing.
    engine.scene_mut().layer_mut(primary).expect("primary").rect = Rect::new(0, 0, 0, 0);
    let fired = Rc::new(Cell::new(0u32));
    let hits = Rc::clone(&fired);
    let callback: engine::timer::TimerCallback =
        Box::new(move |_| hits.set(hits.get().saturating_add(1)));
    engine
        .timer_mut()
        .create_interval(10, callback)
        .expect("timer");
    let mut hooks = Recorder::default();

    let skipped = engine.step(16.0, &mut hooks).expect("frame");
    assert!(!skipped.presented, "nothing was presented");
    assert_eq!(fired.get(), 1, "the due timer still fired");
    assert_eq!(hooks.calls.len(), 1, "the frame hook still ran");
    assert_eq!(skipped.frame_index, 1, "the frame is still counted");

    // A presentable size resumes the draw and the timer keeps its cadence.
    engine
        .scene_mut()
        .window_mut(window)
        .expect("window")
        .inner_size = (100, 100);
    engine.scene_mut().layer_mut(primary).expect("primary").rect = Rect::new(0, 0, 100, 100);
    let presented = engine.step(16.0, &mut hooks).expect("frame");
    assert!(presented.presented, "the draw resumes once a size fits");
    assert_eq!(
        fired.get(),
        2,
        "the timer fired again on the presented frame"
    );
    assert_eq!(presented.frame_index, 2);
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

#[test]
fn object_counts_follow_the_scene_and_uploaded_textures() {
    // reference/cpp/core/visual/LayerIntf.cpp:304-410 — layers, bitmaps and
    // fonts are the scene's own object tables; the texture table is the
    // engine's upload cache and grows with the first dirty bitmap.
    let mut engine = headless_engine((64, 64));
    assert_eq!(engine.object_counts(), engine::ObjectCounts::default());

    let window = engine.scene_mut().add_window("game", (64, 64));
    let layer = engine.scene_mut().add_layer(window, None);
    let bitmap = engine.scene_mut().add_bitmap(2, 2, vec![255; 2 * 2 * 4]);
    if let Some(state) = engine.scene_mut().layer_mut(layer) {
        state.rect = Rect::new(0, 0, 2, 2);
        state.bitmap = Some(bitmap);
        state.image_width = 2;
        state.image_height = 2;
    }
    let font = engine
        .scene_mut()
        .add_font(String::new(), 12, [255, 255, 255, 255]);
    let counts = engine.object_counts();
    assert_eq!(counts.windows, 1);
    assert_eq!(counts.layers, 1);
    assert_eq!(counts.bitmaps, 1);
    assert_eq!(counts.fonts, 1);
    assert_eq!(counts.textures, 0, "the texture is uploaded by the frame");
    assert_eq!(
        engine.scene().fonts.len(),
        1,
        "font {font} stays in the scene"
    );

    let mut hooks = Recorder::default();
    engine.step(1.0, &mut hooks).expect("frame");
    assert_eq!(engine.object_counts().textures, 1);
}
