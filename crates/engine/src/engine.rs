//! The frame-owning engine object: layer tree, projection, textures, timers.
//!
//! [`Engine`] owns every evolving value the middle layer needs — the layer
//! [`Scene`], the projected [`RenderScene`], the [`Surface`]/[`RenderState`]
//! pair, the bitmap-id → [`Texture`] table, the [`TimerState`] and the raw
//! input snapshot — and advances them one frame at a time.
//!
//! One frame is: get the clock, fit the VM→device presentation, fire due
//! timers, run the upward hook, fit the presentation again (the hook may have
//! resized a window's primary layer), refresh the render-only overlay, upload
//! dirty bitmaps, project the layer tree when it changed, run the
//! after-projection hook with the emitted layer ids, reproject if that hook
//! changed the scene, translate it into device pixels, and draw it with a
//! single pipeline call. A size pair that cannot be fitted skips everything
//! that draws.
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
use api::host::{Presentation, SurfaceEvent};
use render::{
    BackendKind, FrameStats, FrameTimings, RenderNode, RenderPipeline, RenderScene, RenderState,
    Surface, Texture, TextureDesc, TextureUpload, create_texture, drop_texture, last_frame_timings,
    recover_surface, render, resize as resize_render, upload_textures,
};

use crate::hasher::{Map, map};
use crate::projection::{ProjectionReport, project_scene_report};
use crate::scene::Scene;
use crate::timer::TimerState;
use crate::translate::{PixelTranslation, TranslateError, translate_render_scene};

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

/// Live object counts the engine exposes to a render-only overlay.
///
/// These are the engine's own tables, not script handles: the counts follow
/// the scene and texture lifetime and are refreshed with the overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjectCounts {
    /// Windows in the scene.
    pub windows: usize,
    /// Layers in the scene.
    pub layers: usize,
    /// Bitmaps in the scene.
    pub bitmaps: usize,
    /// Fonts in the scene.
    pub fonts: usize,
    /// Uploaded bitmap textures the engine currently holds.
    pub textures: usize,
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

    /// Runs after the projection emitted this frame's layers, before the
    /// final upload, projection and draw.
    ///
    /// `drawn_layers` is the projection's report for the current scene. A hook
    /// that changes the scene here is followed by a second upload and
    /// projection, so its changes are in the frame being drawn.
    ///
    /// # Errors
    ///
    /// The message is returned to the caller as [`EngineError::Frame`]; the
    /// frame is abandoned without drawing.
    fn after_project(&mut self, _drawn_layers: &[u32]) -> Result<(), String> {
        Ok(())
    }
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
        objects: ObjectCounts,
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
    /// The overlay could not be refreshed.
    #[error("engine overlay: {0}")]
    Overlay(String),
    /// The frame hook reported a failure.
    #[error("engine frame: {0}")]
    Frame(String),
}

/// A frame the backend presented, or the reason it did not.
///
/// A backend that presents nothing but reports no error leaves the previous
/// surface contents on screen, so the frame is counted as skipped and retried
/// instead of being treated as an ordinary frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PresentOutcome {
    /// Whether the backend presented this frame.
    presented: bool,
    /// The surface event the backend reported, if any.
    surface_event: Option<SurfaceEvent>,
    /// The fault to report when the frame was not presented.
    fault: Option<PresentFault>,
    /// Whether the engine should retry the presentation this frame.
    retry: bool,
}

/// The reason a frame was not presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentFault {
    /// The backend presented nothing and named no surface event.
    Unreported,
    /// The backend named the surface event that stopped presentation.
    Surface(SurfaceEvent),
}

/// Reduces a backend presentation to the outcome the frame reports.
fn present_outcome(presentation: Presentation) -> PresentOutcome {
    PresentOutcome {
        presented: presentation.presented,
        surface_event: presentation.surface_event,
        fault: if presentation.presented {
            None
        } else {
            Some(match presentation.surface_event {
                Some(event) => PresentFault::Surface(event),
                None => PresentFault::Unreported,
            })
        },
        retry: !presentation.presented
            && matches!(
                presentation.surface_event,
                None | Some(SurfaceEvent::OutOfDate)
            ),
    }
}

/// Outcome of one [`Engine::step`] or [`Engine::frame`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameReport {
    /// Number of frames drawn so far, counting this one.
    pub frame_index: u64,
    /// The frame clock this frame ran at, in milliseconds.
    pub elapsed_ms: f64,
    /// Whether the frame was presented to the surface.
    ///
    /// A frame with `presented == false` updated the scene but the backend
    /// kept the previous surface contents; the engine reports and retries it
    /// instead of treating it as an ordinary frame.
    pub presented: bool,
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

/// The middle-layer engine: scene, projection, textures, timers, presentation
/// and the frame.
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
    /// Layer ids the latest projection emitted, including descendants covered
    /// by a cached ancestor.
    drawn_layers: Vec<u32>,
    /// Presented surface for `render_state`.
    surface: Surface,
    /// GPU/staging/texture bookkeeping.
    render_state: RenderState,
    /// Bitmap id → uploaded texture handle.
    textures: Map<u32, Texture>,
    /// Timer registry advanced by the frame clock.
    timer: TimerState,
    /// Latest script-visible input state.
    input: crate::input::InputState,
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
    /// The VM-pixel → device-pixel mapping for the current sizes, or `None` when
    /// the two sizes cannot be fitted.
    translation: Option<PixelTranslation>,
    /// The first window's client size the most recent presentation sync
    /// observed, or `None` when the scene had no window.
    last_window_client_size: Option<(u32, u32)>,
    /// A script-side window size the composition root has not applied yet.
    pending_window_resize: Option<(u32, u32)>,
    /// Whether the unfittable-size warning has been logged since the sizes
    /// last fitted.
    presentation_skipped_logged: bool,
    /// Presentation fault the engine has already reported, or `None` while
    /// frames present normally.
    reported_present_fault: Option<PresentFault>,
    /// Consecutive frames the backend did not present.
    present_skips: u32,
    /// Whether the most recent presentation retry failed, so a repeating
    /// retry failure is logged once.
    present_retry_failed: bool,
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
        let scene = Scene::default();
        let mut engine = Self {
            pipeline,
            scene,
            render_scene: RenderScene::with_root_node(),
            render_scene_revision: None,
            drawn_layers: Vec::new(),
            surface,
            render_state,
            textures: map(),
            timer: TimerState::new(),
            input: crate::input::InputState::default(),
            surface_size,
            now_ms: 0,
            last_now_ms: None,
            frame_index: 0,
            elapsed_ms: 0.0,
            headless,
            overlay,
            translation: None,
            last_window_client_size: None,
            pending_window_resize: None,
            presentation_skipped_logged: false,
            reported_present_fault: None,
            present_skips: 0,
            present_retry_failed: false,
        };
        let event = resize_render(
            &engine.pipeline,
            &engine.surface,
            &mut engine.render_state,
            surface_size,
        )?;
        // The surface can adopt an extent different from the requested one (a
        // DPI or compositor clamp), so the presentation fits the extent the
        // backend actually took rather than the size we asked for.
        engine.surface_size = reported_surface_size(event, surface_size);
        engine.sync_presentation();
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
    /// The order is the frame contract: set the clock, fit the presentation,
    /// fire due timers, run the upward hook, fit the presentation again (the
    /// hook may have resized a window's primary layer), then refresh the
    /// overlay, upload, project, translate and draw. A size pair that cannot be
    /// fitted skips everything that draws and reports no frame statistics.
    fn advance(
        &mut self,
        now_ms: u64,
        dt_ms: f64,
        hooks: &mut dyn GameFrame,
    ) -> Result<FrameReport, EngineError> {
        self.now_ms = now_ms;
        self.last_now_ms = Some(now_ms);
        self.elapsed_ms = now_ms as f64;
        self.sync_presentation();
        self.timer.advance(now_ms);
        hooks
            .before_render(now_ms, dt_ms)
            .map_err(EngineError::Frame)?;
        self.sync_presentation();

        if self.translation.is_none() {
            self.frame_index = self.frame_index.saturating_add(1);
            self.present_skips = self.present_skips.saturating_add(1);
            return Ok(FrameReport {
                frame_index: self.frame_index,
                elapsed_ms: self.elapsed_ms,
                presented: false,
                stats: None,
                sync_ms: 0.0,
                upload_ms: 0.0,
                prune_ms: 0.0,
                project_ms: 0.0,
                render_ms: 0.0,
            });
        }

        self.refresh_overlay(now_ms)?;

        let sync_started = Instant::now();
        let upload_started = Instant::now();
        self.sync_dirty_bitmaps();
        let mut upload_ms = upload_started.elapsed().as_secs_f64() * 1000.0;
        let prune_started = Instant::now();
        self.prune_stale_textures();
        let mut prune_ms = prune_started.elapsed().as_secs_f64() * 1000.0;
        let mut sync_ms = sync_started.elapsed().as_secs_f64() * 1000.0;

        let project_started = Instant::now();
        self.reproject_render_scene();
        let mut project_ms = project_started.elapsed().as_secs_f64() * 1000.0;

        // The upward hook sees the projection's actual draw report. A hook
        // that changes the scene (a self-updating transition step) is
        // followed by a second upload and projection so this same frame draws
        // the new state, not the previous one.
        hooks
            .after_project(&self.drawn_layers)
            .map_err(EngineError::Frame)?;
        self.sync_presentation();

        // The hook may have rewritten a transition frame bitmap without
        // changing the scene graph; upload those dirty bitmaps either way.
        let sync2_started = Instant::now();
        let upload2_started = Instant::now();
        self.sync_dirty_bitmaps();
        upload_ms += upload2_started.elapsed().as_secs_f64() * 1000.0;
        let prune2_started = Instant::now();
        self.prune_stale_textures();
        prune_ms += prune2_started.elapsed().as_secs_f64() * 1000.0;
        sync_ms += sync2_started.elapsed().as_secs_f64() * 1000.0;
        if self.translation.is_some() && self.render_scene_revision != Some(self.scene.revision()) {
            let project2_started = Instant::now();
            self.reproject_render_scene();
            project_ms += project2_started.elapsed().as_secs_f64() * 1000.0;
        }

        let render_started = Instant::now();
        let outcome = match render(
            &self.pipeline,
            &self.surface,
            &mut self.render_state,
            &self.render_scene,
        ) {
            Ok(presentation) => present_outcome(presentation),
            // A swapchain that could not be rebuilt is retried below and on
            // the next frame; the backend logged the precise cause when it
            // happened.
            Err(RenderError::SurfaceOutOfDate) => present_outcome(Presentation {
                presented: false,
                surface_event: Some(SurfaceEvent::OutOfDate),
            }),
            Err(error) => return Err(error.into()),
        };
        // A lost surface is recoverable: rebuild the surface and swapchain in
        // the bottom layer and keep the scene, textures and VM in place. The
        // recovered frame is skipped and the next frame presents normally.
        if outcome.surface_event == Some(SurfaceEvent::Lost) {
            recover_surface(&self.pipeline, &mut self.surface, &mut self.render_state)?;
            log::info!("engine: presentation surface recreated after it was lost");
        }
        if outcome.presented {
            if self.reported_present_fault.take().is_some() {
                log::info!(
                    "engine: presentation resumed after {} skipped frame(s)",
                    self.present_skips
                );
            }
            self.present_skips = 0;
            self.present_retry_failed = false;
        } else {
            self.present_skips = self.present_skips.saturating_add(1);
            self.report_present_fault(outcome.fault);
            if outcome.retry {
                match resize_render(
                    &self.pipeline,
                    &self.surface,
                    &mut self.render_state,
                    self.surface_size,
                ) {
                    Ok(_) => self.present_retry_failed = false,
                    Err(error) => {
                        if !self.present_retry_failed {
                            log::error!("engine: retrying the presentation failed: {error}");
                            self.present_retry_failed = true;
                        }
                    }
                }
            }
        }
        let render_ms = render_started.elapsed().as_secs_f64() * 1000.0;

        self.frame_index = self.frame_index.saturating_add(1);

        Ok(FrameReport {
            frame_index: self.frame_index,
            elapsed_ms: self.elapsed_ms,
            presented: outcome.presented,
            stats: self.last_frame(),
            sync_ms,
            upload_ms,
            prune_ms,
            project_ms,
            render_ms,
        })
    }

    /// Reports a presentation fault once for as long as it is the current
    /// fault, so a persistent stall does not log on every frame.
    fn report_present_fault(&mut self, fault: Option<PresentFault>) {
        let Some(fault) = fault else {
            return;
        };
        if self.reported_present_fault == Some(fault) {
            return;
        }
        match fault {
            PresentFault::Unreported => {
                log::warn!("engine: the backend presented no frame and reported no surface event")
            }
            PresentFault::Surface(SurfaceEvent::Lost) => log::warn!(
                "engine: the presentation surface was lost; this frame was not presented"
            ),
            PresentFault::Surface(SurfaceEvent::OutOfDate) => log::warn!(
                "engine: the presentation surface is out of date; rebuilding the swapchain"
            ),
            PresentFault::Surface(SurfaceEvent::Restored) => {
                log::warn!("engine: the surface was restored but this frame was not presented")
            }
            PresentFault::Surface(SurfaceEvent::Resized(width, height)) => log::warn!(
                "engine: this frame was not presented after a resize to {width}x{height}"
            ),
        }
        self.reported_present_fault = Some(fault);
    }

    /// Rebinds the surface to a new size in physical pixels.
    ///
    /// The size is clamped to at least `1x1`, and an unchanged size does not
    /// touch the backend. The framebuffer is not the logical screen: the scene
    /// keeps its window client sizes and primary-layer rects, and the
    /// presentation refits the screen into the new framebuffer, so an OS resize
    /// moves no value a script observes.
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
        self.surface_size = reported_surface_size(event, size);
        self.sync_presentation();
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
    ///
    /// The tree is projected in VM pixels and then translated into device
    /// pixels, which is why a changed presentation also invalidates it. The
    /// overlay is attached after the translation because it is already in
    /// device coordinates. Nothing is rebuilt while the two sizes cannot be
    /// fitted; the frame path draws nothing in that case.
    fn reproject_render_scene(&mut self) {
        let revision = self.scene.revision();
        if self.render_scene_revision == Some(revision) {
            return;
        }
        let Some(translation) = self.translation else {
            return;
        };

        let textures = &self.textures;
        let ProjectionReport {
            scene: projected,
            drawn_layers,
        } = project_scene_report(&self.scene, |bitmap| textures.get(&bitmap).cloned());
        self.drawn_layers = drawn_layers;

        self.render_scene.nodes.clear();
        self.render_scene.root = projected.root;
        self.render_scene.background = projected.background;
        self.render_scene.nodes.extend(projected.nodes);

        translate_render_scene(&mut self.render_scene, &translation);
        self.attach_overlay();
        self.render_scene_revision = Some(revision);
    }

    /// Refits the VM→device presentation for the current sizes and reports a
    /// script-side window resize to the composition root.
    ///
    /// The VM size is the first window's primary-layer rect: the layer whose
    /// MainImage is the window's screen buffer. Before the window has a primary
    /// layer the window's client size stands in, and with no window the
    /// framebuffer size does. An OS resize moves only the framebuffer and a
    /// script window-client write moves only the window size; only the latter
    /// is surfaced to the composition root. A changed mapping invalidates the
    /// projected tree, which is built in VM pixels. A size pair that cannot be
    /// fitted leaves the frame without a mapping and warns once rather than
    /// substituting a scale.
    fn sync_presentation(&mut self) {
        let vm_size = self.vm_size();
        let translation = match PixelTranslation::fit(vm_size, self.surface_size) {
            Ok(translation) => {
                self.presentation_skipped_logged = false;
                Some(translation)
            }
            Err(TranslateError::EmptyDimension { vm, device }) => {
                if !self.presentation_skipped_logged {
                    log::warn!(
                        "presentation skipped: a {vm:?} game surface cannot be fitted into a {device:?} framebuffer"
                    );
                    self.presentation_skipped_logged = true;
                }
                None
            }
        };
        if translation != self.translation {
            self.translation = translation;
            self.render_scene_revision = None;
            if let Some(translation) = translation {
                log::info!(
                    "presentation: VM {}x{} -> framebuffer {}x{} (scale {:.3})",
                    translation.vm_size.0,
                    translation.vm_size.1,
                    translation.device_size.0,
                    translation.device_size.1,
                    translation.scale
                );
            }
        }

        let window_client_size = self.scene.windows.first().map(|window| window.inner_size);
        if window_client_size != self.last_window_client_size {
            // The first observation asks for a window resize only when the
            // client size already differs from the framebuffer; every later
            // change is a script resize and is surfaced to the composition
            // root. An OS resize leaves the client size alone, so it never
            // asks.
            if let Some(size) = window_client_size
                && (self.last_window_client_size.is_some() || size != self.surface_size)
            {
                self.pending_window_resize = Some(size);
            }
            self.last_window_client_size = window_client_size;
        }
    }

    /// Appends the overlay as the last child of the render scene root.
    ///
    /// The projection clears and refills the arena, so the overlay node is
    /// re-added after every rebuild and never on a skipped one. The overlay is
    /// attached after the tree has been translated, so its node stays in
    /// device coordinates; its texture is stable across refreshes, so the node
    /// needs no refresh here.
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
        let objects = self.object_counts();
        let Some(overlay) = self.overlay.as_mut() else {
            return Ok(());
        };
        overlay
            .refresh(&self.pipeline, &mut self.render_state, now_ms, objects)
            .map_err(EngineError::Overlay)
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

    /// Returns the layer ids the latest projection actually emitted, including
    /// visible descendants covered by a cached ancestor.
    pub fn drawn_layers(&self) -> &[u32] {
        &self.drawn_layers
    }

    /// Returns the bitmap-id → texture table.
    pub fn textures(&self) -> &Map<u32, Texture> {
        &self.textures
    }

    /// Returns the script-visible input state.
    pub fn input(&self) -> &crate::input::InputState {
        &self.input
    }

    /// Returns the script-visible input state for mutation.
    pub fn input_mut(&mut self) -> &mut crate::input::InputState {
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

    /// Returns the script-visible screen size: the first window's primary-layer
    /// rect, the window's client size while it has no primary layer, or the
    /// framebuffer size when the scene has no window.
    pub fn vm_size(&self) -> (u32, u32) {
        let Some(window) = self.scene.windows.first() else {
            return self.surface_size;
        };
        match window
            .primary_layer
            .and_then(|primary| self.scene.layer(primary))
        {
            Some(layer) => (layer.rect.w, layer.rect.h),
            None => window.inner_size,
        }
    }

    /// Returns the live scene and texture object counts.
    pub fn object_counts(&self) -> ObjectCounts {
        ObjectCounts {
            windows: self.scene.windows.len(),
            layers: self.scene.layers.len(),
            bitmaps: self.scene.bitmaps.len(),
            fonts: self.scene.fonts.len(),
            textures: self.textures.len(),
        }
    }

    /// Returns the current VM-pixel → device-pixel mapping, or `None` when the
    /// VM and framebuffer sizes cannot be fitted.
    pub fn translation(&self) -> Option<PixelTranslation> {
        self.translation
    }

    /// Takes the script-side window size the composition root still has to
    /// apply to the OS window, if any.
    ///
    /// A request is produced when the script creates a window whose client
    /// size differs from the framebuffer and on every later change of that
    /// size. Taking it clears it, so a size that does not change again is not
    /// requested twice.
    pub fn take_window_resize(&mut self) -> Option<(u32, u32)> {
        self.pending_window_resize.take()
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
            let uploaded = match upload_textures(&self.pipeline, &mut self.render_state, &uploads) {
                Ok(()) => true,
                Err(error) => {
                    log::warn!(
                        "failed to upload {} bitmap(s): {error}; the bitmaps stay dirty and the \
                         next frame retries them",
                        uploads.len()
                    );
                    false
                }
            };
            drop(uploads);
            clear_uploaded_dirty(&mut self.scene, &ready, uploaded);
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

/// Returns the framebuffer size a backend resize event actually took.
///
/// A backend reports the extent the surface accepted; a driver or compositor
/// clamp can make it differ from the requested size, and the presentation must
/// fit the real framebuffer. A non-resize event leaves `requested` in place.
fn reported_surface_size(event: SurfaceEvent, requested: (u32, u32)) -> (u32, u32) {
    match event {
        SurfaceEvent::Resized(width, height) => (width.max(1), height.max(1)),
        SurfaceEvent::Lost | SurfaceEvent::Restored | SurfaceEvent::OutOfDate => requested,
    }
}

/// Clears the dirty flag of every bitmap in `uploaded` after a batch upload.
///
/// A failed batch did not reach the backend, so the bitmaps stay dirty and the
/// next frame retries them; clearing the flag anyway would present a frame
/// that silently omits their textures.
fn clear_uploaded_dirty(scene: &mut Scene, uploaded: &[u32], succeeded: bool) {
    if !succeeded {
        return;
    }
    for id in uploaded {
        scene.set_bitmap_clean(*id);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PresentFault, Scene, clear_uploaded_dirty, present_outcome, reported_surface_size,
    };
    use api::host::{Presentation, SurfaceEvent};

    #[test]
    fn a_failed_upload_leaves_every_bitmap_dirty_for_a_retry() {
        let mut scene = Scene::default();
        let id = scene.add_bitmap(1, 1, vec![0u8; 4]);
        assert!(scene.bitmap(id).expect("bitmap").dirty);

        clear_uploaded_dirty(&mut scene, &[id], false);
        assert!(
            scene.bitmap(id).expect("bitmap").dirty,
            "a batch that did not reach the backend must be retried"
        );

        clear_uploaded_dirty(&mut scene, &[id], true);
        assert!(!scene.bitmap(id).expect("bitmap").dirty);
    }

    #[test]
    fn a_resize_event_reports_the_extent_the_surface_accepted() {
        assert_eq!(
            reported_surface_size(SurfaceEvent::Resized(640, 480), (1280, 720)),
            (640, 480)
        );
        // A non-resize event leaves the requested size in place.
        assert_eq!(
            reported_surface_size(SurfaceEvent::OutOfDate, (1280, 720)),
            (1280, 720)
        );
    }

    #[test]
    fn a_frame_the_backend_did_not_present_is_reported_and_retried() {
        // reference/cpp/core/visual/impl/VSyncTimingThread.cpp:114,141-142
        // The reference presents on every vsync wake; a wake that produced no
        // frame is a skipped, reported frame rather than a normal one.
        let outcome = present_outcome(Presentation {
            presented: false,
            surface_event: Some(SurfaceEvent::OutOfDate),
        });
        assert!(!outcome.presented);
        assert!(outcome.retry);
        assert_eq!(
            outcome.fault,
            Some(PresentFault::Surface(SurfaceEvent::OutOfDate))
        );

        let normal = present_outcome(Presentation {
            presented: true,
            surface_event: None,
        });
        assert!(normal.presented);
        assert!(!normal.retry);
        assert_eq!(normal.fault, None);
    }

    #[test]
    fn a_lost_surface_is_recovered_without_a_second_swapchain_rebuild() {
        let lost = present_outcome(Presentation {
            presented: false,
            surface_event: Some(SurfaceEvent::Lost),
        });
        assert!(!lost.presented);
        assert!(
            !lost.retry,
            "recovering the surface already rebuilt the swapchain"
        );
        assert_eq!(lost.fault, Some(PresentFault::Surface(SurfaceEvent::Lost)));

        let unreported = present_outcome(Presentation {
            presented: false,
            surface_event: None,
        });
        assert_eq!(unreported.fault, Some(PresentFault::Unreported));
        assert!(unreported.retry);
    }
}
