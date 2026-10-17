//! The frame-owning engine object: layer tree, projection, textures, timers.
//!
//! [`Engine`] owns every evolving value the middle layer needs — the layer
//! [`Scene`], the projected [`RenderScene`], the [`Surface`]/[`RenderState`]
//! pair, the bitmap-id → [`Texture`] table, the [`TimerState`] and the raw
//! input snapshot — and advances them one frame at a time.
//!
//! One frame is: get the clock, fire due timers, run the upward hook, refresh
//! the render-only overlay, upload dirty bitmaps, project the layer tree when
//! it changed, and draw it with a single pipeline call.
//!
//! Nothing here reads a wall clock. [`Engine::frame`] takes the time and
//! [`Engine::step`] adds a delta to the clock the engine already holds, so a
//! test drives exactly the same code path as the running game.
//!
//! The two traits are the only places where behaviour from higher layers
//! enters: [`GameFrame`] is the per-frame upward edge implemented above this
//! layer, and [`EngineOverlay`] supplies the render-only debug overlay. This
//! module never names those layers.

use std::time::Instant;

use api::error::RenderError;
use api::geometry::PixelRegion;
use api::host::{InputState, SurfaceEvent};
use render::{
    BackendKind, FrameStats, FrameTimings, RenderNode, RenderPipeline, RenderScene, RenderState,
    Surface, Texture, TextureDesc, TextureUpload, create_texture, drop_texture, last_frame_timings,
    render, resize as resize_render, upload_textures,
};

use crate::hasher::{Map, map};
use crate::projection::project_scene;
use crate::scene::{Rect, Scene};
use crate::timer::TimerState;

/// The bottom objects and initial state [`Engine::new`] assembles into an
/// [`Engine`].
///
/// The caller creates these itself: the render pipeline comes from the
/// backend selection, the surface and render state from the window handle, and
/// the overlay from whatever higher layer draws one.
pub struct EngineParts {
    /// Stateless render service.
    pub pipeline: RenderPipeline,
    /// Presented surface for `render_state`.
    pub surface: Surface,
    /// GPU/staging/texture bookkeeping for `surface`.
    pub render_state: RenderState,
    /// Initial surface size in pixels; clamped to at least `1x1`.
    pub surface_size: (u32, u32),
    /// Whether the engine runs offscreen with no window.
    pub headless: bool,
    /// Render-only overlay drawn above every layer, or `None` for none.
    pub overlay: Option<Box<dyn EngineOverlay>>,
}

/// Behaviour the engine calls once per frame before the frame is projected.
///
/// This is the single upward edge of the frame: the layer above runs its VM
/// tick and its per-frame event dispatch here and returns the failure as a
/// message. The engine projects and draws whatever the hook left in the scene.
pub trait GameFrame {
    /// Runs one frame body worth of work.
    ///
    /// `now_ms` is the frame clock and `dt_ms` the delta since the previous
    /// frame (zero on the first [`Engine::frame`] call).
    ///
    /// # Errors
    ///
    /// The message is returned to the caller as [`EngineError::Frame`]; the
    /// frame is abandoned without projecting or drawing.
    fn before_render(&mut self, now_ms: u64, dt_ms: f64) -> Result<(), String>;
}

/// A render-only overlay appended above every projected layer.
///
/// The overlay is not part of the engine [`Scene`] and owns no bitmap a script
/// could reach, so it cannot change the layer tree. The engine refreshes it
/// once per frame and attaches [`EngineOverlay::node`] to the render tree on
/// every projection.
pub trait EngineOverlay {
    /// Refreshes the overlay's contents before the frame is projected.
    ///
    /// `now_ms` is the frame clock; the overlay decides its own refresh
    /// cadence from it.
    ///
    /// # Errors
    ///
    /// The message is returned to the caller as [`EngineError::Overlay`]; the
    /// frame is abandoned without projecting or drawing.
    fn refresh(
        &mut self,
        pipeline: &RenderPipeline,
        state: &mut RenderState,
        now_ms: u64,
    ) -> Result<(), String>;

    /// Returns the overlay node, in device/framebuffer coordinates.
    ///
    /// The engine sets its parent to the render scene root.
    fn node(&self) -> RenderNode;
}

/// Errors surfaced by [`Engine`] frames and surface actions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// A bottom-layer render action failed.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// The engine was handed a state value it cannot run with.
    #[error("engine config: {0}")]
    Config(String),
    /// The overlay could not be refreshed.
    #[error("engine overlay: {0}")]
    Overlay(String),
    /// The frame hook reported a failure.
    #[error("engine frame: {0}")]
    Frame(String),
}

/// Outcome of one [`Engine::step`] or [`Engine::frame`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameReport {
    /// Number of frames drawn so far, counting this one.
    pub frame_index: u64,
    /// The frame clock this frame ran at, in milliseconds.
    pub elapsed_ms: f64,
    /// Per-frame counters from the null backend, or `None` on a GPU backend.
    pub stats: Option<FrameStats>,
    /// Milliseconds spent syncing dirty bitmaps to textures.
    pub sync_ms: f64,
    /// Milliseconds of `sync_ms` spent uploading dirty bitmaps.
    pub upload_ms: f64,
    /// Milliseconds of `sync_ms` spent dropping textures whose bitmap is gone.
    pub prune_ms: f64,
    /// Milliseconds spent projecting the scene tree.
    pub project_ms: f64,
    /// Milliseconds spent in the pipeline's render call.
    pub render_ms: f64,
}

/// Resizes a window's client area to `size`.
///
/// The window's client size and, when it has one, its primary layer are
/// resized; the layer stays at the origin because the primary layer is
/// anchored at the window client area's top-left.
fn resize_window_client(scene: &mut Scene, window: u32, size: (u32, u32)) {
    let primary = scene.window_mut(window).map(|window| {
        window.inner_size = size;
        window.primary_layer
    });
    if let Some(Some(primary)) = primary
        && let Some(layer) = scene.layer_mut(primary)
    {
        layer.rect = Rect::new(0, 0, size.0, size.1);
    }
}

/// The middle-layer engine: scene, projection, textures, timers and the frame.
///
/// Every value is a field of this object; there are no globals, statics or
/// hidden caches, so two engines can run in one process. Callers reach the
/// layer system through [`Engine::scene`] / [`Engine::scene_mut`] and drive
/// frames through [`Engine::step`] / [`Engine::frame`].
pub struct Engine {
    /// Stateless render service.
    pipeline: RenderPipeline,
    /// Layer tree owned by the engine.
    scene: Scene,
    /// Projected render tree, rebuilt in place when [`Scene::revision`]
    /// changes.
    render_scene: RenderScene,
    /// [`Scene::revision`] value `render_scene` was projected from.
    ///
    /// `None` until the first projection, so the first frame always projects.
    render_scene_revision: Option<u64>,
    /// Presented surface for `render_state`.
    surface: Surface,
    /// GPU/staging/texture bookkeeping.
    render_state: RenderState,
    /// Bitmap id → uploaded texture handle.
    textures: Map<u32, Texture>,
    /// Timer registry advanced by the frame clock.
    timer: TimerState,
    /// Latest raw input snapshot.
    input: InputState,
    /// Current surface size in pixels.
    surface_size: (u32, u32),
    /// The frame clock of the last frame, in milliseconds.
    now_ms: u64,
    /// The frame clock of the previous frame, or `None` before the first one.
    last_now_ms: Option<u64>,
    /// Frames drawn so far.
    frame_index: u64,
    /// The frame clock, equal to the most recent injected `now_ms`.
    elapsed_ms: f64,
    /// Whether the engine runs offscreen with no window.
    headless: bool,
    /// Render-only overlay, or `None` when none is drawn.
    overlay: Option<Box<dyn EngineOverlay>>,
}

impl Engine {
    /// Creates an engine from ready-made bottom objects.
    ///
    /// The surface is sized to [`EngineParts::surface_size`] (clamped to at
    /// least `1x1`), which is the one surface action the engine performs
    /// before its first frame.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Render`] when the backend rejects the initial
    /// surface size.
    pub fn new(parts: EngineParts) -> Result<Self, EngineError> {
        let EngineParts {
            pipeline,
            surface,
            render_state,
            surface_size,
            headless,
            overlay,
        } = parts;
        let surface_size = (surface_size.0.max(1), surface_size.1.max(1));
        let mut engine = Self {
            pipeline,
            scene: Scene::default(),
            render_scene: RenderScene::with_root_node(),
            render_scene_revision: None,
            surface,
            render_state,
            textures: map(),
            timer: TimerState::new(),
            input: InputState::default(),
            surface_size,
            now_ms: 0,
            last_now_ms: None,
            frame_index: 0,
            elapsed_ms: 0.0,
            headless,
            overlay,
        };
        resize_render(
            &engine.pipeline,
            &engine.surface,
            &mut engine.render_state,
            surface_size,
        )?;
        Ok(engine)
    }

    /// Advances the frame clock by `dt_ms` and draws one frame.
    ///
    /// A negative delta counts as zero. The delta becomes the frame clock's
    /// increment, so three `step(10.0)` calls leave the engine at 30 ms.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the frame hook, the overlay, or a render
    /// action fails.
    pub fn step(
        &mut self,
        dt_ms: f32,
        hooks: &mut dyn GameFrame,
    ) -> Result<FrameReport, EngineError> {
        let dt = f64::from(dt_ms.max(0.0));
        let now_ms = self.now_ms.saturating_add(dt as u64);
        self.advance(now_ms, dt, hooks)
    }

    /// Draws one frame at the injected clock value `now_ms`.
    ///
    /// The clock is set to `now_ms` rather than accumulated, so a test can
    /// jump between frames deterministically. The delta handed to the hook is
    /// the distance from the previous frame's clock, and zero on the first
    /// frame.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the frame hook, the overlay, or a render
    /// action fails.
    pub fn frame(
        &mut self,
        now_ms: u64,
        hooks: &mut dyn GameFrame,
    ) -> Result<FrameReport, EngineError> {
        let dt_ms = now_ms.saturating_sub(self.last_now_ms.unwrap_or(now_ms)) as f64;
        self.advance(now_ms, dt_ms, hooks)
    }

    /// Runs one frame at `now_ms` with `dt_ms` as the hook's delta.
    ///
    /// The order is the frame contract: set the clock, fire due timers, run
    /// the upward hook, refresh the overlay, then upload, project and draw.
    fn advance(
        &mut self,
        now_ms: u64,
        dt_ms: f64,
        hooks: &mut dyn GameFrame,
    ) -> Result<FrameReport, EngineError> {
        self.now_ms = now_ms;
        self.last_now_ms = Some(now_ms);
        self.elapsed_ms = now_ms as f64;
        self.timer.advance(now_ms);
        hooks
            .before_render(now_ms, dt_ms)
            .map_err(EngineError::Frame)?;
        self.refresh_overlay(now_ms)?;

        let sync_started = Instant::now();
        let upload_started = Instant::now();
        self.sync_dirty_bitmaps();
        let upload_ms = upload_started.elapsed().as_secs_f64() * 1000.0;
        let prune_started = Instant::now();
        self.prune_stale_textures();
        let prune_ms = prune_started.elapsed().as_secs_f64() * 1000.0;
        let sync_ms = sync_started.elapsed().as_secs_f64() * 1000.0;

        let project_started = Instant::now();
        self.reproject_render_scene();
        let project_ms = project_started.elapsed().as_secs_f64() * 1000.0;

        let render_started = Instant::now();
        render(
            &self.pipeline,
            &self.surface,
            &mut self.render_state,
            &self.render_scene,
        )?;
        let render_ms = render_started.elapsed().as_secs_f64() * 1000.0;

        self.frame_index = self.frame_index.saturating_add(1);

        Ok(FrameReport {
            frame_index: self.frame_index,
            elapsed_ms: self.elapsed_ms,
            stats: self.last_frame(),
            sync_ms,
            upload_ms,
            prune_ms,
            project_ms,
            render_ms,
        })
    }

    /// Rebinds the surface to a new size in physical pixels.
    ///
    /// The size is clamped to at least `1x1`. The main window's client size
    /// and its primary layer follow the new size; the layer stays at the
    /// origin. An unchanged size does not touch the backend.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Render`] when the backend rejects the resize.
    pub fn resize(&mut self, size: (u32, u32)) -> Result<SurfaceEvent, EngineError> {
        let size = (size.0.max(1), size.1.max(1));
        let event = if size == self.surface_size {
            SurfaceEvent::Resized(size.0, size.1)
        } else {
            resize_render(&self.pipeline, &self.surface, &mut self.render_state, size)?
        };
        self.surface_size = size;

        if let Some(window) = self.scene.windows.first().map(|window| window.id) {
            resize_window_client(&mut self.scene, window, size);
        }
        Ok(event)
    }

    /// Rebuilds [`Engine::render_scene`] when the layer tree is stale.
    ///
    /// The projection is skipped while [`Scene::revision`] is unchanged, so a
    /// static tree does not reallocate the render tree every frame. A rebuild
    /// reuses the existing arena: the node list is cleared (retaining its
    /// capacity) and refilled, so only the projected nodes' own allocations
    /// are touched. The revision is `None` until the first projection, so the
    /// first frame always projects.
    fn reproject_render_scene(&mut self) {
        let revision = self.scene.revision();
        if self.render_scene_revision == Some(revision) {
            return;
        }

        let textures = &self.textures;
        let projected = project_scene(&self.scene, |bitmap| textures.get(&bitmap).cloned());

        self.render_scene.nodes.clear();
        self.render_scene.root = projected.root;
        self.render_scene.background = projected.background;
        self.render_scene.nodes.extend(projected.nodes);

        self.attach_overlay();
        self.render_scene_revision = Some(revision);
    }

    /// Appends the overlay as the last child of the render scene root.
    ///
    /// The projection clears and refills the arena, so the overlay node is
    /// re-added after every rebuild and never on a skipped one. The overlay's
    /// texture is stable across refreshes, so the node needs no refresh here.
    fn attach_overlay(&mut self) {
        let Some(overlay) = self.overlay.as_ref() else {
            return;
        };
        let root = self.render_scene.root;
        let mut node = overlay.node();
        node.parent = Some(root);
        let id = self.render_scene.push(node);
        if let Some(root_node) = self.render_scene.node_mut(root) {
            root_node.children.push(id);
        }
    }

    /// Advances the overlay by one frame, letting it refresh its own texture.
    fn refresh_overlay(&mut self, now_ms: u64) -> Result<(), EngineError> {
        let Some(overlay) = self.overlay.as_mut() else {
            return Ok(());
        };
        overlay
            .refresh(&self.pipeline, &mut self.render_state, now_ms)
            .map_err(EngineError::Overlay)
    }

    /// Replaces the stored raw input snapshot.
    pub fn handle_input(&mut self, input: InputState) {
        self.input = input;
    }

    /// Returns the stateless render pipeline.
    pub fn pipeline(&self) -> &RenderPipeline {
        &self.pipeline
    }

    /// Returns the backend the pipeline was created with.
    pub fn backend(&self) -> BackendKind {
        self.pipeline.backend()
    }

    /// Returns the layer tree.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Returns the layer tree for mutation.
    pub fn scene_mut(&mut self) -> &mut Scene {
        &mut self.scene
    }

    /// Returns the most recently projected render tree.
    pub fn render_scene(&self) -> &RenderScene {
        &self.render_scene
    }

    /// Returns the bitmap-id → texture table.
    pub fn textures(&self) -> &Map<u32, Texture> {
        &self.textures
    }

    /// Returns the latest raw input snapshot.
    pub fn input(&self) -> &InputState {
        &self.input
    }

    /// Returns the latest raw input snapshot for mutation.
    pub fn input_mut(&mut self) -> &mut InputState {
        &mut self.input
    }

    /// Returns the timer registry for setup and inspection.
    pub fn timer(&self) -> &TimerState {
        &self.timer
    }

    /// Returns the timer registry for arming, rescheduling and removal.
    pub fn timer_mut(&mut self) -> &mut TimerState {
        &mut self.timer
    }

    /// Whether the engine runs offscreen with no window.
    pub fn is_headless(&self) -> bool {
        self.headless
    }

    /// Returns the current surface size in pixels.
    pub fn surface_size(&self) -> (u32, u32) {
        self.surface_size
    }

    /// Returns the number of frames drawn so far.
    pub fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Returns the frame clock of the most recent frame, in milliseconds.
    pub fn elapsed_ms(&self) -> f64 {
        self.elapsed_ms
    }

    /// Returns the null backend's counters for the last frame, if available.
    pub fn last_frame(&self) -> Option<FrameStats> {
        render::NullRenderPipeline::last_frame(&self.render_state)
    }

    /// Returns the presentation calls' costs for the last presented frame.
    ///
    /// The Vulkan backend reports the acquire/present/fence/recreate costs it
    /// measured; the null backend reports zeros. `None` means no frame has
    /// been presented through this state yet.
    pub fn last_frame_timings(&self) -> Option<FrameTimings> {
        last_frame_timings(&self.render_state)
    }

    /// Whether the engine draws a render-only overlay.
    pub fn debug_overlay_enabled(&self) -> bool {
        self.overlay.is_some()
    }

    /// Uploads every bitmap whose dirty flag is set and clears the flag.
    ///
    /// A bitmap whose dimensions changed recreates its texture; one with an
    /// inconsistent pixel length is skipped with a warning so one bad bitmap
    /// cannot abort the frame. All dirty bitmaps are uploaded in one batch, so
    /// the frame submits them once and the present path waits for that batch
    /// instead of waiting once per bitmap.
    fn sync_dirty_bitmaps(&mut self) {
        let dirty: Vec<u32> = self
            .scene
            .bitmaps
            .iter()
            .filter(|bitmap| bitmap.dirty)
            .map(|bitmap| bitmap.id)
            .collect();

        let mut ready: Vec<u32> = Vec::with_capacity(dirty.len());
        for id in dirty {
            let Some((width, height, len)) = self
                .scene
                .bitmap(id)
                .map(|bitmap| (bitmap.width, bitmap.height, bitmap.rgba.len()))
            else {
                continue;
            };
            if width == 0 || height == 0 {
                log::warn!("bitmap #{id} has a zero dimension; skipping upload");
                self.scene.set_bitmap_clean(id);
                continue;
            }
            let expected = width as usize * height as usize * 4;
            if len != expected {
                log::warn!(
                    "bitmap #{id} has {len} pixel bytes, expected {expected}; skipping upload"
                );
                self.scene.set_bitmap_clean(id);
                continue;
            }

            let recreate = self
                .textures
                .get(&id)
                .is_none_or(|texture| texture.width() != width || texture.height() != height);
            if recreate {
                if let Some(old) = self.textures.remove(&id) {
                    drop_texture(&self.pipeline, &mut self.render_state, old);
                }
                match create_texture(&self.pipeline, TextureDesc::new(width, height)) {
                    Ok(texture) => {
                        self.textures.insert(id, texture);
                    }
                    Err(error) => {
                        log::warn!("failed to create a texture for bitmap #{id}: {error}");
                        continue;
                    }
                }
            }
            ready.push(id);
        }

        if !ready.is_empty() {
            let uploads: Vec<TextureUpload<'_>> = ready
                .iter()
                .filter_map(|id| {
                    let bitmap = self.scene.bitmap(*id)?;
                    let texture = self.textures.get(id)?;
                    Some(TextureUpload {
                        texture,
                        region: PixelRegion::new(0, 0, bitmap.width, bitmap.height),
                        pixels: &bitmap.rgba,
                    })
                })
                .collect();
            if let Err(error) = upload_textures(&self.pipeline, &mut self.render_state, &uploads) {
                log::warn!("failed to upload {} bitmap(s): {error}", uploads.len());
            }
            drop(uploads);
            for id in ready {
                self.scene.set_bitmap_clean(id);
            }
        }
    }

    /// Drops textures whose bitmap no longer exists in the scene.
    fn prune_stale_textures(&mut self) {
        let stale: Vec<u32> = self
            .textures
            .keys()
            .copied()
            .filter(|id| self.scene.bitmap(*id).is_none())
            .collect();
        for id in stale {
            if let Some(texture) = self.textures.remove(&id) {
                drop_texture(&self.pipeline, &mut self.render_state, texture);
            }
        }
    }
}
