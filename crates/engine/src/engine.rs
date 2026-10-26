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
//! changed the scene, carry the VM→device presentation on the render scene
//! root, and draw it with a single pipeline call. A size pair that cannot be
//! fitted skips everything that draws.
//!
//! Nothing here reads a wall clock. [`Engine::run`] takes the elapsed
//! time from the caller's clock source, [`Engine::frame`] takes the absolute
//! time and [`Engine::step`] adds a delta to the clock the engine already
//! holds, so a test drives exactly the same code path as the running game.
//!
//! The two traits are the only places where behaviour from higher layers
//! enters: [`GameFrame`] is the per-frame upward edge implemented above this
//! layer, and [`EngineOverlay`] supplies the render-only debug overlay. This
//! module never names those layers.

use std::sync::Arc;
use std::time::Instant;

use api::blend::BlendMode;
use api::error::RenderError;
use api::geometry::PixelRegion;
use api::host::{PresentSkip, Presentation, SurfaceEvent};
use render::{
    BackendKind, FrameStats, FrameTimings, GpuCapabilities, RenderNode, RenderPipeline,
    RenderScene, RenderState, Surface, Texture, TextureDesc, TextureUpload, capabilities,
    create_texture, drop_texture, last_frame_timings, recover_surface, render,
    resize as resize_render, upload_textures,
};

use crate::clock::{FrameClock, FrameTimeSource, InjectedFrameTime};
use crate::hasher::{Map, map};
use crate::imagedecode::{
    DecodePool, DecodePoolConfig, DecodePoolError, DecodeStats, ImageDecoder,
};
use crate::projection::{ProjectionReport, project_scene_report};
use crate::scene::{BitmapState, Scene};
use crate::shader::{PipelineId, Program, ProgramDesc, ShaderError, ShaderId, ShaderLibrary};
use crate::storage::{StoragePoolConfig, StoragePoolError, StorageReadPool, StorageReader};
use crate::timer::{TimerState, deliver_due};
use crate::translate::{PixelTranslation, TranslateError};

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
    /// The engine sets its parent to the render scene root and replaces its
    /// transform with the inverse of the presentation root transform, so the
    /// node's `dst` and rectangle clip stay in device pixels.
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
/// surface contents on screen, so the frame is counted as skipped and reported
/// instead of being treated as an ordinary frame. A skip is reported without a
/// retry: nothing was submitted, so there is nothing to retry until the next
/// frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PresentOutcome {
    /// Whether the backend presented this frame.
    presented: bool,
    /// The surface event the backend reported, if any.
    surface_event: Option<SurfaceEvent>,
    /// The fault to report when the frame was not presented.
    fault: Option<PresentFault>,
}

/// The reason a frame was not presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentFault {
    /// The backend presented nothing and named no surface event.
    Unreported,
    /// The backend named the surface event that stopped presentation.
    Surface(SurfaceEvent),
    /// The backend abandoned the frame because the display was not ready.
    Skipped(PresentSkip),
}

/// Reduces a backend presentation to the outcome the frame reports.
fn present_outcome(presentation: Presentation) -> PresentOutcome {
    PresentOutcome {
        presented: presentation.presented,
        surface_event: presentation.surface_event,
        fault: if presentation.presented {
            None
        } else {
            Some(match (presentation.skip, presentation.surface_event) {
                // A skip means the backend abandoned the frame before any GPU
                // work, so it outranks any surface event it may also carry.
                (Some(skip), _) => PresentFault::Skipped(skip),
                (None, Some(event)) => PresentFault::Surface(event),
                (None, None) => PresentFault::Unreported,
            })
        },
    }
}

/// How long a presentation stall may last before its age is reported.
const STALL_REPORT_MS: u64 = 1_000;

/// How often a continuing presentation stall reports its age after the first
/// report.
const STALL_REPORT_INTERVAL_MS: u64 = 10_000;

/// A reason a frame's image did not reach the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StallCause {
    /// The backend did not present the frame.
    Backend(PresentFault),
    /// The game surface and the framebuffer have no presentation mapping, so
    /// the frame drew nothing.
    NoMapping,
}

impl StallCause {
    /// Warns once about this cause when it opens or joins a stall.
    fn warn(self) {
        match self {
            Self::Backend(PresentFault::Unreported) => {
                log::warn!("engine: the backend presented no frame and reported no surface event")
            }
            // A skipped frame is not a fault: the backend abandoned it before
            // submitting anything and the previous image stays on screen.
            Self::Backend(PresentFault::Skipped(PresentSkip::AcquireTimeout)) => log::warn!(
                "engine: the display did not release a swapchain image within the \
                 presentation budget; this frame was skipped"
            ),
            Self::Backend(PresentFault::Surface(SurfaceEvent::Lost)) => log::warn!(
                "engine: the presentation surface was lost; this frame was not presented"
            ),
            Self::Backend(PresentFault::Surface(SurfaceEvent::OutOfDate)) => log::warn!(
                "engine: the presentation surface is out of date; rebuilding the swapchain"
            ),
            Self::Backend(PresentFault::Surface(SurfaceEvent::Restored)) => {
                log::warn!("engine: the surface was restored but this frame was not presented")
            }
            Self::Backend(PresentFault::Surface(SurfaceEvent::Resized(width, height))) => {
                log::warn!(
                    "engine: this frame was not presented after a resize to {width}x{height}"
                )
            }
            // The unfittable-size warning is emitted where the mapping is
            // rebuilt, once per unsupported size pair.
            Self::NoMapping => {}
        }
    }

    /// The phrase a stall summary uses for this cause.
    fn describe(self) -> String {
        match self {
            Self::Backend(PresentFault::Unreported) => {
                "the backend presented no frame and named no surface event".to_owned()
            }
            Self::Backend(PresentFault::Skipped(PresentSkip::AcquireTimeout)) => {
                "the display did not release a swapchain image within the presentation budget"
                    .to_owned()
            }
            Self::Backend(PresentFault::Surface(SurfaceEvent::Lost)) => {
                "the presentation surface was lost".to_owned()
            }
            Self::Backend(PresentFault::Surface(SurfaceEvent::OutOfDate)) => {
                "the presentation surface was out of date".to_owned()
            }
            Self::Backend(PresentFault::Surface(SurfaceEvent::Restored)) => {
                "the presentation surface was restored".to_owned()
            }
            Self::Backend(PresentFault::Surface(SurfaceEvent::Resized(width, height))) => {
                format!("the presentation surface was resized to {width}x{height}")
            }
            Self::NoMapping => "the game surface and the framebuffer cannot be fitted".to_owned(),
        }
    }
}

/// Joins the distinct causes of a stall into one log phrase.
fn describe_stall_causes(causes: &[StallCause]) -> String {
    causes
        .iter()
        .map(|cause| cause.describe())
        .collect::<Vec<String>>()
        .join("; ")
}

/// One run of consecutive frames the display did not accept.
///
/// The stall starts with the first frame whose image did not reach the display
/// and ends with the next presented frame. It keeps the distinct causes, so a
/// resume names every reason the display was starved rather than only the last
/// one, and it reports its own age while it lasts, so a display that stops
/// accepting frames is never silent between the first warning and the resume.
#[derive(Default)]
struct PresentationStall {
    /// Frames the display did not accept since the last presented frame.
    frames: u64,
    /// The frame clock the stall began at.
    started_ms: u64,
    /// Distinct causes in first-seen order.
    causes: Vec<StallCause>,
    /// The frame clock of the next age report.
    next_report_ms: Option<u64>,
}

impl PresentationStall {
    /// Whether the run has recorded a frame.
    fn active(&self) -> bool {
        self.frames > 0
    }

    /// Records a frame the display did not accept, returning the cause when it
    /// is new to this stall.
    fn record(&mut self, cause: StallCause, now_ms: u64) -> Option<StallCause> {
        if !self.active() {
            self.started_ms = now_ms;
            self.next_report_ms = Some(now_ms.saturating_add(STALL_REPORT_MS));
        }
        self.frames = self.frames.saturating_add(1);
        if self.causes.contains(&cause) {
            return None;
        }
        self.causes.push(cause);
        Some(cause)
    }

    /// Returns the stall's age when a report is due at `now_ms`.
    fn due_report(&mut self, now_ms: u64) -> Option<StallAge> {
        let due = self.next_report_ms?;
        if now_ms < due {
            return None;
        }
        self.next_report_ms = Some(now_ms.saturating_add(STALL_REPORT_INTERVAL_MS));
        Some(StallAge {
            frames: self.frames,
            elapsed_ms: now_ms.saturating_sub(self.started_ms),
        })
    }

    /// Ends the run at `now_ms`, returning what to report when it lasted.
    fn resumed(&mut self, now_ms: u64) -> Option<StallSummary> {
        if !self.active() {
            return None;
        }
        let summary = StallSummary {
            frames: self.frames,
            elapsed_ms: now_ms.saturating_sub(self.started_ms),
            causes: std::mem::take(&mut self.causes),
        };
        self.frames = 0;
        self.next_report_ms = None;
        Some(summary)
    }
}

/// The age of a presentation stall that is still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StallAge {
    /// Frames the display did not accept so far.
    frames: u64,
    /// Milliseconds since the stall began.
    elapsed_ms: u64,
}

/// The span of a presentation stall that ended.
#[derive(Debug, PartialEq, Eq)]
struct StallSummary {
    /// Frames the display did not accept.
    frames: u64,
    /// Milliseconds between the first skipped frame and the resumed one.
    elapsed_ms: u64,
    /// Distinct causes in first-seen order.
    causes: Vec<StallCause>,
}

/// Outcome of one [`Engine::step`] or [`Engine::frame`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameReport {
    /// Number of frames drawn so far, counting this one.
    pub frame_index: u64,
    /// The frame clock this frame ran at, in milliseconds.
    ///
    /// A clock advanced with [`Engine::run`] or [`Engine::step`] keeps its
    /// accumulated fractional part; [`Engine::frame`] sets it to the injected
    /// whole millisecond.
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
/// frames through [`Engine::run`] / [`Engine::step`] / [`Engine::frame`].
pub struct Engine {
    /// Stateless render service.
    pipeline: RenderPipeline,
    /// Layer tree owned by the engine.
    scene: Scene,
    /// The registered shader programs and the pipelines built from them.
    shader: ShaderLibrary,
    /// Projected render tree, rebuilt in place when
    /// [`Scene::structure_revision`] changes.
    render_scene: RenderScene,
    /// [`Scene::structure_revision`] value `render_scene` was projected from,
    /// or `None` when the projection must run regardless.
    ///
    /// `None` until the first projection, so the first frame always projects;
    /// the presentation mapping and the texture table are the other two
    /// projection inputs, and a change to either resets it to `None`.
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
    /// The image decode pool, or `None` when no decoder is installed.
    ///
    /// The pool is the engine's own state: it owns the worker threads, the
    /// queue and the byte accounting. It is absent until the composition
    /// root installs a decoder, and the frame's image barrier is a no-op
    /// while it is.
    decode_pool: Option<DecodePool>,
    /// The storage read pool, or `None` when no reader is installed.
    ///
    /// The pool is the engine's own state: it owns the worker threads, the
    /// queue and the reads in flight. The composition root installs it and
    /// hands it to the storage the script reads through, and the frame
    /// drains it before the hook runs.
    storage_pool: Option<Arc<StorageReadPool>>,
    /// Latest script-visible input state.
    input: crate::input::InputState,
    /// Current surface size in pixels.
    surface_size: (u32, u32),
    /// The frame clock of the previous frame, or `None` before the first one.
    last_now_ms: Option<u64>,
    /// Frames drawn so far.
    frame_index: u64,
    /// The frame clock: the time base the frame advances.
    ///
    /// `frame` sets it to the injected whole millisecond and `step`
    /// accumulates each delta, so sub-millisecond frame times add up instead
    /// of being discarded. Its whole-millisecond part is the `now_ms` the
    /// timer, hook and overlay receive.
    clock: FrameClock,
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
    /// The run of frames the display has not accepted, ended by the next
    /// presented frame.
    stall: PresentationStall,
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
        let mut scene = Scene::default();
        // A backend that draws the projected scene composites a running
        // transition's frame from the projection; the headless CPU path blends
        // the layer's display bitmap instead.
        scene.composite_transition_frames = pipeline.backend() != BackendKind::Null;
        let mut engine = Self {
            pipeline,
            scene,
            shader: ShaderLibrary::new(),
            render_scene: RenderScene::with_root_node(),
            render_scene_revision: None,
            drawn_layers: Vec::new(),
            surface,
            render_state,
            textures: map(),
            timer: TimerState::new(),
            decode_pool: None,
            storage_pool: None,
            input: crate::input::InputState::default(),
            surface_size,
            last_now_ms: None,
            frame_index: 0,
            clock: FrameClock::new(),
            headless,
            overlay,
            translation: None,
            last_window_client_size: None,
            pending_window_resize: None,
            presentation_skipped_logged: false,
            stall: PresentationStall::default(),
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
        engine.install_effect_pipelines();
        // The first frame draws: the surface holds nothing yet.
        engine.scene.mark_dirty();
        Ok(engine)
    }

    /// Creates the pipelines for the programs the engine ships.
    ///
    /// The choice is the backend's: a backend that cannot run effect
    /// pipelines gets none, and the transitions blend their display bitmap on
    /// the CPU, the way they always have. A backend that can, but refuses one
    /// of the programs, is reported and the transitions that need it fall
    /// back to the CPU as well; nothing is drawn with a program the driver
    /// rejected.
    fn install_effect_pipelines(&mut self) {
        if !capabilities(&self.pipeline).supports_effect_pipelines {
            log::info!(
                "transitions: this backend cannot run effect pipelines; the display bitmap is \
                 blended on the CPU"
            );
            return;
        }
        for program in [Program::CrossFade, Program::Scroll] {
            let shader = match self.shader.load(program.desc()) {
                Ok(shader) => shader,
                Err(error) => {
                    log::error!(
                        "transitions: the {:?} program was refused: {error}",
                        program.name()
                    );
                    return;
                }
            };
            let pipeline = match self.shader.pipeline(
                &self.pipeline,
                &mut self.render_state,
                shader,
                BlendMode::SourceOver,
            ) {
                Ok(pipeline) => pipeline,
                Err(error) => {
                    log::error!(
                        "transitions: the {:?} program has no pipeline: {error}",
                        program.name()
                    );
                    return;
                }
            };
            let handle = self.shader.pipeline_handle(pipeline).cloned();
            let Some(handle) = handle else {
                log::error!(
                    "transitions: the {:?} program has no pipeline handle",
                    program.name()
                );
                return;
            };
            self.scene.effect_pipelines.insert(program, handle);
        }
    }

    /// The backend's capabilities.
    pub fn capabilities(&self) -> GpuCapabilities {
        capabilities(&self.pipeline)
    }

    /// Installs the image decoder and starts the decode pool.
    ///
    /// The decoder is the bottom codec the pool drives; the composition root
    /// supplies it, because the concrete implementation lives in the bottom
    /// media layer. Installing a second decoder replaces the first pool, and
    /// the jobs the old pool had not finished are cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`DecodePoolError`] when the pool cannot be started.
    pub fn install_decoder(
        &mut self,
        decoder: Arc<dyn ImageDecoder>,
        config: DecodePoolConfig,
    ) -> Result<(), DecodePoolError> {
        self.decode_pool = Some(DecodePool::new(decoder, config)?);
        Ok(())
    }

    /// The image decode pool, or `None` when no decoder is installed.
    pub fn decode_pool(&self) -> Option<&DecodePool> {
        self.decode_pool.as_ref()
    }

    /// The image decode pool, mutably, or `None` when no decoder is
    /// installed.
    ///
    /// The pool's submit and cancel actions need the mutable borrow; the
    /// compatibility layer reaches the pool through the same attachment the
    /// scene and the timer use.
    pub fn decode_pool_mut(&mut self) -> Option<&mut DecodePool> {
        self.decode_pool.as_mut()
    }

    /// The decode accounting of the image decode pool, or `None` when no
    /// decoder is installed.
    ///
    /// The counters are the number of image decodes the pool ran and the time
    /// they took. A worker updates them under the pool's own lock, so the
    /// totals are read here on the thread that owns the engine.
    pub fn decode_stats(&self) -> Option<DecodeStats> {
        self.decode_pool.as_ref().map(DecodePool::stats)
    }

    /// Installs the storage reader and starts the storage read pool.
    ///
    /// The reader is the bottom backend the pool drives; the composition root
    /// supplies it. The pool is handed to the storage the script reads
    /// through, so the engine and the storage share it: the engine drains it
    /// every frame and the storage schedules the reads on it.
    ///
    /// # Errors
    ///
    /// Returns [`StoragePoolError`] when the pool cannot be started.
    pub fn install_storage_reader(
        &mut self,
        reader: Arc<dyn StorageReader>,
        config: StoragePoolConfig,
    ) -> Result<Arc<StorageReadPool>, StoragePoolError> {
        let pool = Arc::new(StorageReadPool::new(reader, config)?);
        self.storage_pool = Some(Arc::clone(&pool));
        Ok(pool)
    }

    /// The storage read pool, or `None` when no reader is installed.
    pub fn storage_pool(&self) -> Option<&Arc<StorageReadPool>> {
        self.storage_pool.as_ref()
    }

    /// Registers a shader program and returns its identity.
    ///
    /// Registration validates the descriptor and copies the stage code, so
    /// the program outlives the buffers it was loaded from. It does not touch
    /// the backend: the pipeline comes from [`Engine::effect_pipeline`].
    ///
    /// # Errors
    ///
    /// Returns [`ShaderError`] when the descriptor is not usable.
    pub fn load_shader(&mut self, desc: ProgramDesc<'_>) -> Result<ShaderId, ShaderError> {
        self.shader.load(desc)
    }

    /// The pipeline for `shader` built with `blend`, created on first use.
    ///
    /// The blend is part of the backend's pipeline object, so the same program
    /// drawn with two blends needs two pipelines.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderError`] when the program is unknown, when the backend
    /// cannot build effect pipelines at all, or when the driver refuses the
    /// program.
    pub fn effect_pipeline(
        &mut self,
        shader: ShaderId,
        blend: BlendMode,
    ) -> Result<PipelineId, ShaderError> {
        self.shader
            .pipeline(&self.pipeline, &mut self.render_state, shader, blend)
    }

    /// Advances the frame clock by the elapsed time `clock` reports and
    /// draws one frame.
    ///
    /// This is the frame's loop-level entry point: the caller chooses where
    /// the elapsed time comes from — the windowed loop injects the real time
    /// the platform measured, the headless runner injects the frame period it
    /// simulates — and every timeline the frame derives comes from that one
    /// value.
    ///
    /// A negative elapsed time counts as zero. The delta is accumulated with
    /// its fractional part, so sixty 16.6 ms frames advance the clock by
    /// 996 ms, not 960. The `now_ms` handed to the timer, hook and overlay is
    /// the accumulated clock's whole-millisecond part.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the frame hook, the overlay, or a render
    /// action fails.
    pub fn run(
        &mut self,
        clock: &dyn FrameTimeSource,
        hooks: &mut dyn GameFrame,
    ) -> Result<FrameReport, EngineError> {
        let dt_ms = clock.elapsed_ms().max(0.0);
        // The clock keeps the sub-millisecond remainder, so a 60 fps frame
        // does not lose most of a millisecond on every frame.
        let now_ms = self.clock.advance(dt_ms);
        self.run_frame(now_ms, dt_ms, hooks)
    }

    /// Advances the frame clock by `dt_ms` and draws one frame.
    ///
    /// A negative delta counts as zero. The delta is accumulated with its
    /// fractional part, so sixty `step(16.6)` calls advance the clock by
    /// 996 ms, not 960. The `now_ms` handed to the timer, hook and overlay is
    /// the accumulated clock's whole-millisecond part.
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
        let clock = InjectedFrameTime::new(f64::from(dt_ms.max(0.0)));
        self.run(&clock, hooks)
    }

    /// Draws one frame at the injected clock value `now_ms`.
    ///
    /// The clock is set to `now_ms` rather than accumulated, so a test can
    /// jump between frames deterministically and any fractional remainder left
    /// by [`Engine::step`] is discarded. The delta handed to the hook is the
    /// distance from the previous frame's clock, and zero on the first frame.
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
        self.clock.set(now_ms);
        self.run_frame(now_ms, dt_ms, hooks)
    }

    /// Runs one frame at `now_ms`, with `dt_ms` as the hook's delta.
    ///
    /// The clock is already set when this runs: `step` accumulated the delta
    /// into it and `frame` put it at the injected value.
    ///
    /// The order is the frame contract: fit the presentation, fire due
    /// timers, run the upward hook, fit the presentation again (the hook may
    /// have resized a window's primary layer), then refresh the overlay,
    /// upload, project, apply the presentation root transform and draw. A size
    /// pair that cannot be fitted skips everything that draws and reports no
    /// frame statistics.
    fn run_frame(
        &mut self,
        now_ms: u64,
        dt_ms: f64,
        hooks: &mut dyn GameFrame,
    ) -> Result<FrameReport, EngineError> {
        self.last_now_ms = Some(now_ms);
        self.sync_presentation();
        deliver_due(&mut self.timer, now_ms);
        // The storage barrier: the reads the previous frame scheduled are
        // published before the hook runs, so a read the script makes now
        // finds its result ready.
        if let Some(pool) = &self.storage_pool {
            pool.drain();
        }
        hooks
            .before_render(now_ms, dt_ms)
            .map_err(EngineError::Frame)?;
        // The image barrier: every image the hook queued is decoded before
        // the upload and the projection, so the frame draws decoded pixels.
        // The decodes run concurrently, so the wait is the time the last one
        // needs rather than the sum of them.
        self.scene.resolve_pending_images();
        self.sync_presentation();

        if self.translation.is_none() {
            self.frame_index = self.frame_index.saturating_add(1);
            self.note_presentation_stall(StallCause::NoMapping, now_ms);
            return Ok(FrameReport {
                frame_index: self.frame_index,
                elapsed_ms: self.clock.now_f64(),
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
        if self.translation.is_some()
            && self.render_scene_revision != Some(self.scene.structure_revision())
        {
            let project2_started = Instant::now();
            self.reproject_render_scene();
            project_ms += project2_started.elapsed().as_secs_f64() * 1000.0;
        }

        let render_started = Instant::now();
        // The frame draws the whole scene when anything changed. A frame with
        // nothing to show does not draw at all: the window keeps the content
        // of the last presented frame, and the change waits for a frame that
        // has something to show.
        let mut presented = false;
        if self.scene.dirty() {
            let outcome = match render(
                &self.pipeline,
                &self.surface,
                &mut self.render_state,
                &self.render_scene,
            ) {
                Ok(presentation) => present_outcome(presentation),
                // A swapchain that could not be rebuilt is reported here and
                // retried by the next frame's render call; the backend logged the
                // precise cause when it happened.
                Err(RenderError::SurfaceOutOfDate) => present_outcome(Presentation {
                    presented: false,
                    surface_event: Some(SurfaceEvent::OutOfDate),
                    skip: None,
                }),
                Err(error) => return Err(error.into()),
            };
            // A lost surface is recoverable: rebuild the surface and swapchain in
            // the bottom layer and keep the scene, textures and VM in place. The
            // recovered frame is skipped and the next frame presents normally.
            if outcome.surface_event == Some(SurfaceEvent::Lost) {
                recover_surface(&self.pipeline, &mut self.surface, &mut self.render_state)?;
                log::info!("engine: presentation surface recreated after it was lost");
                // The recreated surface holds nothing, so the whole frame is
                // drawn again.
                self.scene.mark_dirty();
            }
            if outcome.presented {
                self.finish_presentation_stall(now_ms);
                // The presented frame carries the change, so the next frame
                // starts clean. A frame the display did not accept keeps it:
                // the previous frame's content is still on screen.
                self.scene.clear_dirty();
                presented = true;
            } else {
                // `present_outcome` names a fault for every non-presented frame; a
                // frame without one is the backend's silence, reported as such.
                let fault = match outcome.fault {
                    Some(fault) => fault,
                    None => PresentFault::Unreported,
                };
                self.note_presentation_stall(StallCause::Backend(fault), now_ms);
            }
        }
        let render_ms = render_started.elapsed().as_secs_f64() * 1000.0;

        self.frame_index = self.frame_index.saturating_add(1);

        Ok(FrameReport {
            frame_index: self.frame_index,
            elapsed_ms: self.clock.now_f64(),
            presented,
            stats: self.last_frame(),
            sync_ms,
            upload_ms,
            prune_ms,
            project_ms,
            render_ms,
        })
    }

    /// Records a frame the display did not accept.
    ///
    /// A cause is reported the first time it joins the stall, and the stall's
    /// age is reported once it lasts long enough that a game frozen on screen
    /// would otherwise look like a hung process.
    fn note_presentation_stall(&mut self, cause: StallCause, now_ms: u64) {
        if let Some(cause) = self.stall.record(cause, now_ms) {
            cause.warn();
        }
        if let Some(age) = self.stall.due_report(now_ms) {
            log::warn!(
                "engine: the display has not accepted a frame for {:.1} s (frames skipped: {}); \
                 {}",
                age.elapsed_ms as f64 / 1000.0,
                age.frames,
                describe_stall_causes(&self.stall.causes),
            );
        }
    }

    /// Ends a presentation stall and reports how long it lasted and why.
    fn finish_presentation_stall(&mut self, now_ms: u64) {
        if let Some(summary) = self.stall.resumed(now_ms) {
            log::info!(
                "engine: presentation resumed after {} skipped frame(s) in {:.1} s: {}",
                summary.frames,
                summary.elapsed_ms as f64 / 1000.0,
                describe_stall_causes(&summary.causes),
            );
        }
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

    /// Rebuilds [`Engine::render_scene`] when the layer tree's structure or
    /// the presentation mapping is stale.
    ///
    /// The projection is skipped while [`Scene::structure_revision`] is
    /// unchanged, so a static tree does not reallocate the render tree every
    /// frame and a bitmap content write — which moves no node — does not
    /// re-project at all. A rebuild reuses the existing arena: the node list
    /// is cleared (retaining its capacity) and refilled, so only the
    /// projected nodes' own allocations are touched. The revision is `None`
    /// until the first projection, so the first frame always projects.
    ///
    /// The tree is projected in VM pixels and the render scene root carries the
    /// presentation transform, which is why a changed presentation also
    /// invalidates it (via `render_scene_revision = None`). The overlay is
    /// authored in device coordinates, so it is attached with the inverse root
    /// transform. Nothing is rebuilt while the two sizes cannot be fitted; the
    /// frame path draws nothing in that case.
    fn reproject_render_scene(&mut self) {
        let revision = self.scene.structure_revision();
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

        let root = self.render_scene.root;
        if let Some(root_node) = self.render_scene.node_mut(root) {
            root_node.transform = translation.root_transform();
        }
        self.attach_overlay(&translation);
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
            // A new mapping rescales the whole surface, so every pixel of the
            // previous frame's content is different.
            self.scene.mark_dirty();
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
    /// re-added after every rebuild and never on a skipped one. The node is
    /// authored in device pixels, so its transform is set to the inverse of the
    /// presentation root transform; its texture is stable across refreshes, so
    /// the node needs no refresh here.
    fn attach_overlay(&mut self, translation: &PixelTranslation) {
        let Some(overlay) = self.overlay.as_ref() else {
            return;
        };
        let root = self.render_scene.root;
        let mut node = overlay.node();
        node.parent = Some(root);
        node.transform = translation.inverse_root_transform();
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
        // The overlay is drawn in device coordinates, outside the screen the
        // scene describes, so the area it covers cannot be named. A frame with
        // an overlay draws the whole surface.
        self.scene.mark_dirty();
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

    /// Returns the frame clock.
    ///
    /// The clock is the engine's time base: the frame advances it, the timer
    /// registry and the upward hook read it, and the script reads it through
    /// `System.getTickCount`.
    pub fn clock(&self) -> &FrameClock {
        &self.clock
    }

    /// Returns the frame clock for mutation.
    pub fn clock_mut(&mut self) -> &mut FrameClock {
        &mut self.clock
    }

    /// Returns the most recently projected render tree.
    ///
    /// The nodes keep VM-space coordinates; the root carries the presentation
    /// transform that maps them into device pixels.
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
    ///
    /// A clock advanced with [`Engine::step`] carries the accumulated
    /// fractional part; [`Engine::frame`] sets it to the injected whole value.
    pub fn elapsed_ms(&self) -> f64 {
        self.clock.now_f64()
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
    ///
    /// A bitmap that changed in a rectangle uploads that rectangle alone;
    /// one that changed wholly uploads every pixel.
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
                        // A new texture holds no pixels: the copy has to
                        // cover the whole bitmap, or the rest of the texture
                        // stays undefined. Marking the bitmap wholly keeps a
                        // retry of that upload whole as well.
                        self.scene.mark_bitmap_whole(id);
                        // The projection resolves its textures through this
                        // table: a bitmap that had no texture when it was last
                        // projected now has one, so the node it was skipped
                        // for must be projected again even when the tree's
                        // structure is unchanged.
                        self.render_scene_revision = None;
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
            // The region every dirty bitmap uploads, with the packed pixels
            // of the partial ones.
            let mut planned: Vec<(u32, PixelRegion, Vec<u8>)> = Vec::with_capacity(ready.len());
            for id in &ready {
                let Some(bitmap) = self.scene.bitmap(*id) else {
                    continue;
                };
                let (region, pixels) = dirty_upload(bitmap);
                planned.push((*id, region, pixels));
            }
            let uploads: Vec<TextureUpload<'_>> = planned
                .iter()
                .filter_map(|(id, region, pixels)| {
                    let bitmap = self.scene.bitmap(*id)?;
                    let texture = self.textures.get(id)?;
                    let whole = PixelRegion::new(0, 0, bitmap.width, bitmap.height);
                    // The whole bitmap uploads its own buffer; a region
                    // uploads the packed copy of its rows.
                    let source = if *region == whole {
                        &bitmap.rgba[..]
                    } else {
                        pixels.as_slice()
                    };
                    Some(TextureUpload {
                        texture,
                        region: *region,
                        pixels: source,
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
            // The uploaded pixels are the ones the layers drawing the bitmap
            // show, so the picture changed.
            self.scene.mark_dirty();
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

/// The region a dirty bitmap uploads, with the packed pixels of a partial
/// region.
///
/// A bitmap that changed wholly uploads every pixel; a bitmap that changed
/// in a rectangle uploads that rectangle alone, with its rows packed
/// together. A region that no longer fits the bitmap (it was resized) or
/// that covers no pixel uploads every pixel.
fn dirty_upload(bitmap: &BitmapState) -> (PixelRegion, Vec<u8>) {
    let whole = PixelRegion::new(0, 0, bitmap.width, bitmap.height);
    let Some(region) = bitmap.dirty_region else {
        return (whole, Vec::new());
    };
    if region.is_empty() || region.right() > bitmap.width || region.bottom() > bitmap.height {
        return (whole, Vec::new());
    }
    if region == whole {
        return (whole, Vec::new());
    }
    match region_pixels(bitmap, region) {
        Some(pixels) => (region, pixels),
        None => (whole, Vec::new()),
    }
}

/// The pixels of `region` inside `bitmap`, packed row after row.
///
/// `None` when the region does not fit the bitmap's buffer.
fn region_pixels(bitmap: &BitmapState, region: PixelRegion) -> Option<Vec<u8>> {
    let width = usize::try_from(bitmap.width).ok()?;
    let row_bytes = usize::try_from(region.width).ok()?.checked_mul(4)?;
    let height = usize::try_from(region.height).ok()?;
    let mut pixels = Vec::with_capacity(row_bytes.checked_mul(height)?);
    for row in 0..height {
        let y = usize::try_from(region.y).ok()?.checked_add(row)?;
        let start = y
            .checked_mul(width)?
            .checked_add(usize::try_from(region.x).ok()?)?;
        let start = start.checked_mul(4)?;
        let end = start.checked_add(row_bytes)?;
        pixels.extend_from_slice(bitmap.rgba.get(start..end)?);
    }
    Some(pixels)
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
        Engine, EngineParts, GameFrame, PresentFault, PresentationStall, Scene, StallAge,
        StallCause, StallSummary, clear_uploaded_dirty, describe_stall_causes, present_outcome,
        reported_surface_size,
    };
    use api::geometry::PixelRegion;
    use api::host::{PresentSkip, Presentation, SurfaceEvent};
    use render::{NullRenderPipeline, PipelineConfig, begin_render_state, create_render_pipeline};
    use testkit::headless_window;

    /// A frame hook that changes nothing.
    struct NoFrame;

    impl GameFrame for NoFrame {
        fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
            Ok(())
        }
    }

    /// Builds a headless engine over the null backend.
    fn headless_engine(size: (u32, u32)) -> Engine {
        let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
        let handle = headless_window();
        let (surface, render_state) = begin_render_state(&pipeline, &handle).expect("surface");
        Engine::new(EngineParts {
            pipeline,
            surface,
            render_state,
            surface_size: size,
            headless: true,
            overlay: None,
        })
        .expect("engine")
    }

    /// A bitmap that changed in a rectangle uploads that rectangle alone.
    #[test]
    fn a_dirty_region_uploads_only_its_own_pixels() {
        let mut engine = headless_engine((64, 64));
        let mut hooks = NoFrame;
        let id = engine.scene_mut().add_bitmap(8, 8, vec![0u8; 8 * 8 * 4]);

        // The first frame creates the texture, so it takes every pixel.
        engine.frame(0, &mut hooks).expect("first frame");
        let texture = engine.textures().get(&id).expect("texture").clone();
        let uploaded = NullRenderPipeline::texture_uploaded_bytes(&engine.render_state, &texture)
            .expect("uploaded bytes");
        assert_eq!(uploaded, 8 * 8 * 4, "a new texture takes every pixel");

        // The next frame uploads the marked rectangle alone.
        engine
            .scene_mut()
            .bitmap_mut(id)
            .expect("bitmap")
            .mark_dirty_rect(PixelRegion::new(2, 2, 3, 3));
        engine.frame(16, &mut hooks).expect("second frame");
        let uploaded = NullRenderPipeline::texture_uploaded_bytes(&engine.render_state, &texture)
            .expect("uploaded bytes");
        assert_eq!(
            uploaded,
            8 * 8 * 4 + 3 * 3 * 4,
            "only the marked rectangle is uploaded"
        );
        assert!(
            !engine.scene().bitmap(id).expect("bitmap").dirty,
            "an uploaded bitmap is clean"
        );
    }

    /// A failed upload keeps the bitmap's dirty region for the retry.
    #[test]
    fn a_failed_upload_keeps_the_dirty_region_for_the_retry() {
        let mut scene = Scene::default();
        let id = scene.add_bitmap(4, 4, vec![0u8; 4 * 4 * 4]);
        scene.set_bitmap_clean(id);
        scene
            .bitmap_mut(id)
            .expect("bitmap")
            .mark_dirty_rect(PixelRegion::new(1, 1, 2, 2));

        clear_uploaded_dirty(&mut scene, &[id], false);
        let bitmap = scene.bitmap(id).expect("bitmap");
        assert!(bitmap.dirty, "a batch that failed is retried");
        assert_eq!(
            bitmap.dirty_region,
            Some(PixelRegion::new(1, 1, 2, 2)),
            "the failed region is uploaded again"
        );

        clear_uploaded_dirty(&mut scene, &[id], true);
        let bitmap = scene.bitmap(id).expect("bitmap");
        assert!(!bitmap.dirty, "an uploaded bitmap is clean");
        assert_eq!(bitmap.dirty_region, None, "a clean bitmap has no region");
    }

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
    fn a_frame_the_backend_did_not_present_is_reported() {
        // reference/cpp/core/visual/impl/VSyncTimingThread.cpp:114,141-142
        // The reference presents on every vsync wake; a wake that produced no
        // frame is a skipped, reported frame rather than a normal one.
        let outcome = present_outcome(Presentation {
            presented: false,
            surface_event: Some(SurfaceEvent::OutOfDate),
            skip: None,
        });
        assert!(!outcome.presented);
        assert_eq!(
            outcome.fault,
            Some(PresentFault::Surface(SurfaceEvent::OutOfDate))
        );

        let normal = present_outcome(Presentation {
            presented: true,
            surface_event: None,
            skip: None,
        });
        assert!(normal.presented);
        assert_eq!(normal.fault, None);
    }

    #[test]
    fn a_frame_the_display_never_released_is_skipped_and_reported() {
        // reference/cpp/core/visual/impl/BasicDrawDevice.cpp:244
        // The reference's present never blocks on the display, so an image the
        // presentation engine does not release is a missed frame, not a stall
        // and not a surface fault.
        let skipped = present_outcome(Presentation {
            presented: false,
            surface_event: None,
            skip: Some(PresentSkip::AcquireTimeout),
        });
        assert!(!skipped.presented);
        assert_eq!(skipped.surface_event, None);
        assert_eq!(
            skipped.fault,
            Some(PresentFault::Skipped(PresentSkip::AcquireTimeout))
        );
    }

    #[test]
    fn a_stall_names_every_cause_it_saw_and_its_span() {
        // reference/cpp/core/visual/impl/VSyncTimingThread.cpp:114,141-142
        // The reference presents on every vsync wake; a wake that produced no
        // frame is a skipped frame, so one stall is the run of such wakes and
        // its length is the frames they produced.
        let mut stall = PresentationStall::default();
        assert!(!stall.active());
        assert_eq!(
            stall.resumed(5_000),
            None,
            "a run that never skipped has no span to report"
        );

        let skip = StallCause::Backend(PresentFault::Skipped(PresentSkip::AcquireTimeout));
        assert_eq!(stall.record(skip, 1_000), Some(skip));
        assert_eq!(
            stall.record(skip, 1_008),
            None,
            "a cause already in the run is named once"
        );

        let out_of_date = StallCause::Backend(PresentFault::Surface(SurfaceEvent::OutOfDate));
        assert_eq!(stall.record(out_of_date, 1_016), Some(out_of_date));
        assert_eq!(stall.record(out_of_date, 1_024), None);
        // The unfittable mapping keeps the same frame count, so a run that
        // changes cause does not lose the frames it already skipped.
        assert_eq!(
            stall.record(StallCause::NoMapping, 1_032),
            Some(StallCause::NoMapping)
        );

        assert_eq!(
            stall.resumed(2_032),
            Some(StallSummary {
                frames: 5,
                elapsed_ms: 1_032,
                causes: vec![skip, out_of_date, StallCause::NoMapping],
            })
        );
        assert!(!stall.active(), "the resume ends the run");
        assert_eq!(stall.resumed(2_040), None);
    }

    #[test]
    fn a_stall_reports_its_age_on_a_bounded_schedule() {
        let mut stall = PresentationStall::default();
        let cause = StallCause::Backend(PresentFault::Skipped(PresentSkip::AcquireTimeout));
        assert_eq!(stall.record(cause, 0), Some(cause));

        assert_eq!(
            stall.due_report(999),
            None,
            "nothing is reported before the first interval"
        );
        assert_eq!(
            stall.due_report(1_000),
            Some(StallAge {
                frames: 1,
                elapsed_ms: 1_000,
            })
        );
        assert_eq!(
            stall.due_report(1_100),
            None,
            "an age is reported once per interval"
        );
        assert_eq!(stall.record(cause, 10_900), None);
        assert_eq!(
            stall.due_report(11_000),
            Some(StallAge {
                frames: 2,
                elapsed_ms: 11_000,
            })
        );
    }

    #[test]
    fn a_stall_summary_names_each_cause_once() {
        let causes = [
            StallCause::Backend(PresentFault::Skipped(PresentSkip::AcquireTimeout)),
            StallCause::NoMapping,
        ];
        assert_eq!(
            describe_stall_causes(&causes),
            "the display did not release a swapchain image within the presentation budget; \
             the game surface and the framebuffer cannot be fitted"
        );
    }

    #[test]
    fn a_lost_surface_is_reported_as_a_surface_fault_that_recovery_owns() {
        // The engine recovers from `surface_event`, never by rebuilding the
        // swapchain a second time inside the frame.
        let lost = present_outcome(Presentation {
            presented: false,
            surface_event: Some(SurfaceEvent::Lost),
            skip: None,
        });
        assert!(!lost.presented);
        assert_eq!(lost.fault, Some(PresentFault::Surface(SurfaceEvent::Lost)));

        let unreported = present_outcome(Presentation {
            presented: false,
            surface_event: None,
            skip: None,
        });
        assert_eq!(unreported.fault, Some(PresentFault::Unreported));
    }
}
