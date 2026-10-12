//! krkr-rs host assembly: owns the middle-layer state values and the
//! per-frame step, wired to the L1 platform by the `krkr` binary.
//!
//! [`Host`] is the engine-driving frontend. It owns the stateless bottom
//! objects ([`RenderPipeline`], [`FileSystem`], [`Media`]) together with all
//! of the evolving middle-layer state:
//!
//! * the TVP layer system [`Scene`];
//! * the projected [`RenderScene`] consumed by the pipeline;
//! * the [`RenderState`]/[`Surface`] pair created by
//!   [`begin_render_state`];
//! * the bitmap-id → [`Texture`] table and the raw [`InputState`].
//!
//! There are no globals, statics or hidden caches: every value is a local
//! field of [`Host`] and is passed to the bottom objects explicitly. Two
//! hosts can therefore run in the same process. The per-frame work is one
//! projection pass: sync dirty bitmaps, upload textures, project the tree and
//! make one `render` call.

// The composition root installs the VM's raw-pointer binding to the L2 scene
// and storage (`compat::CompatServices::attach_scene`/`attach_storage`), which
// is inherently `unsafe`. That wiring is the crate's only `unsafe`, confined
// to `game` and covered by SAFETY comments; every other module is safe.
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod audio;
mod game;
mod image_codec;
mod runner;
mod video;
mod window_ex;

use std::collections::HashMap;

use api::error::RenderError;
use api::geometry::PixelRegion;
use api::host::{InputState, SurfaceEvent};
use engine::projection::{Presentation, ScalingMode, project_scene};
use engine::scene::{Rect, Scene};
use fs::FileSystem;
use media::Media;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use render::{
    PipelineConfig, RenderPipeline, RenderScene, RenderState, Surface, Texture, TextureDesc,
    TextureFormat, TextureUpload, ViewTransform, begin_render_state,
    begin_render_state_with_display, create_render_pipeline, create_texture, drop_texture, render,
    resize as resize_render, upload_textures,
};

pub use audio::install_audio_decoder;
pub use game::{GameError, GameHost};
pub use image_codec::{install_image_codec, install_image_codec_on};
pub use render::{BackendKind, FrameStats};
// The emulator entry point. A launcher resolves `Launch` (including the
// `RuntimeConfig` it loaded its own way) and calls `run`; see the `runner`
// module for the entry point contract.
pub use runner::{GameInput, Launch, RunMode, RunReport, run};
pub use video::install_video_source;
pub use window_ex::install_window_ex_facts;

/// Errors surfaced by [`Host`].
///
/// The only fallible host operations are the render actions, so the host
/// error is currently a transparent render error. It exists as its own type
/// so file-system/media errors can be added without changing signatures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HostError {
    /// A bottom-layer render action failed.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// The host configuration is invalid (for example, a non-headless host
    /// with no window handle).
    #[error("host config: {0}")]
    Config(String),
}

/// Outcome of one [`Host::step`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StepReport {
    /// Number of frames rendered so far, counting this one.
    pub frame_index: u64,
    /// Total simulated time advanced through `step`, in milliseconds.
    pub elapsed_ms: f64,
    /// Per-frame counters from the null backend, or `None` on a GPU backend.
    pub stats: Option<FrameStats>,
}

/// Immutable configuration for [`Host::new`].
///
/// The type is `#[non_exhaustive]`: start from [`HostConfig::default`] or
/// [`HostConfig::headless`] and assign the fields you need.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HostConfig {
    /// Render backend to create.
    pub backend: BackendKind,
    /// Initial surface size in pixels.
    pub surface_size: (u32, u32),
    /// Application name passed to the graphics API.
    pub application_name: String,
    /// Whether backend validation should be enabled.
    pub validation: bool,
    /// Whether the host runs offscreen with no native window.
    ///
    /// When `true` the host uses [`platform::headless_window_handle`]
    /// and requires the null backend. When `false`, [`window_handle`]
    /// must be `Some`; a real backend with no handle is an error.
    ///
    /// [`window_handle`]: HostConfig::window_handle
    pub headless: bool,
    /// Window handle used to create the surface.
    ///
    /// Required unless [`HostConfig::headless`] is set. The windowed host
    /// passes the real platform window handle here so a Vulkan surface can
    /// bind to it.
    pub window_handle: Option<RawWindowHandle>,
    /// Display handle (Wayland/X11/XCB connection) used with `window_handle`.
    ///
    /// `VK_KHR_surface` needs the platform display connection in addition to
    /// the window handle. When this is `Some`, the host creates the surface
    /// through `begin_render_state_with_display`; `None` uses the
    /// window-handle-only entry point (or the null backend).
    pub display_handle: Option<RawDisplayHandle>,
    /// How the engine presents the logical scene inside the framebuffer.
    ///
    /// [`ScalingMode::Game`] is the untransformed Win32 presentation and the
    /// default, so an unconfigured run matches the reference exactly.
    pub scaling: ScalingMode,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            backend: BackendKind::Null,
            surface_size: (1280, 720),
            application_name: "krkr-rs".to_owned(),
            validation: false,
            headless: false,
            window_handle: None,
            display_handle: None,
            scaling: ScalingMode::Game,
        }
    }
}

impl HostConfig {
    /// Returns the default offscreen configuration.
    ///
    /// This is [`Default`] with [`headless`](HostConfig::headless) set, so a
    /// host can be created without a window or display handle.
    pub fn headless() -> Self {
        Self {
            headless: true,
            ..Self::default()
        }
    }

    /// Returns this configuration with a different backend.
    pub fn with_backend(mut self, backend: BackendKind) -> Self {
        self.backend = backend;
        self
    }

    /// Returns this configuration with a different surface size.
    pub fn with_surface_size(mut self, size: (u32, u32)) -> Self {
        self.surface_size = size;
        self
    }

    /// Returns this configuration bound to a real window handle.
    pub fn with_window_handle(mut self, handle: RawWindowHandle) -> Self {
        self.window_handle = Some(handle);
        self
    }

    /// Returns this configuration bound to a display handle.
    pub fn with_display_handle(mut self, display: RawDisplayHandle) -> Self {
        self.display_handle = Some(display);
        self
    }

    /// Returns this configuration with a different presentation scaling mode.
    pub fn with_scaling(mut self, scaling: ScalingMode) -> Self {
        self.scaling = scaling;
        self
    }
}

/// Resizes a window's client area to `size`.
///
/// The logical screen size and, when the window has one, its primary layer
/// are resized; the layer stays at the origin because the reference forbids
/// moving the primary layer.
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

/// Inverse-maps a platform input event from framebuffer to logical
/// coordinates.
///
/// The engine presents the logical scene scaled inside the framebuffer, so a
/// pointer or touch position must be divided by the presentation transform
/// before the layer manager hit-tests it. Keyboard and text events carry no
/// position and pass through unchanged.
pub fn map_input_to_logical(
    view: ViewTransform,
    input: platform::InputEvent,
) -> platform::InputEvent {
    match input {
        platform::InputEvent::PointerMoved { x, y } => {
            let (x, y) = view.inverse_map((x, y));
            platform::InputEvent::PointerMoved { x, y }
        }
        platform::InputEvent::Touch { id, phase, x, y } => {
            let (x, y) = view.inverse_map((x, y));
            platform::InputEvent::Touch { id, phase, x, y }
        }
        platform::InputEvent::PointerLeft
        | platform::InputEvent::MouseButton { .. }
        | platform::InputEvent::Wheel { .. }
        | platform::InputEvent::Key { .. }
        | platform::InputEvent::Text(_) => input,
    }
}

/// The assembled host: bottom objects, middle-layer state and frame step.
///
/// See the crate documentation for the ownership model. The fields are
/// deliberately private; callers reach the middle-layer state through
/// [`Host::scene`] / [`Host::scene_mut`] and drive frames through
/// [`Host::step`].
pub struct Host {
    /// Stateless render service.
    pipeline: RenderPipeline,
    /// Stateless file-system service (constructed, not yet exercised).
    fs: FileSystem,
    /// Stateless media service (constructed, not yet exercised).
    media: Media,
    /// TVP layer system owned by the host.
    scene: Scene,
    /// Projected render tree, rebuilt in place by [`Host::step`] only when the
    /// [`Scene`] revision changes.
    render_scene: RenderScene,
    /// [`Scene::revision`] value that `render_scene` was projected from.
    ///
    /// `None` until the first projection, so the first [`Host::step`] (and any
    /// forced redraw) always projects.
    render_scene_revision: Option<u64>,
    /// Presentation surface for `render_state`.
    surface: Surface,
    /// GPU/staging/texture bookkeeping owned by the host.
    render_state: RenderState,
    /// Bitmap id → uploaded texture handle.
    textures: HashMap<u32, Texture>,
    /// Latest raw input snapshot.
    input: InputState,
    /// Whether the host runs offscreen with no window (from
    /// [`HostConfig::headless`]).
    ///
    /// The game host reads this at open time to pick the L1 audio backend:
    /// a headless host cannot assume an audio server exists and opens the
    /// null device (see `audio` module docs).
    headless: bool,
    /// Frames rendered so far.
    frame_index: u64,
    /// Simulated time advanced through [`Host::step`].
    elapsed_ms: f64,
    /// Current surface size in pixels.
    surface_size: (u32, u32),
    /// Presentation scaling mode (from [`HostConfig::scaling`]).
    scaling: ScalingMode,
    /// The game's declared design size, captured from the scene window when a
    /// script changes it. `None` until the first resize.
    design_size: Option<(u32, u32)>,
    /// The logical size the host currently reports to scripts.
    logical_size: (u32, u32),
    /// Framebuffer transform for the current presentation.
    view: ViewTransform,
}

impl Host {
    /// Creates a host and its surface from `config`.
    ///
    /// The file system is an in-memory root and the media object uses its
    /// default configuration; the host does not exercise either yet.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Render`] when the requested backend cannot be
    /// created (currently `--backend vulkan`, whose Phase 5 skeleton returns
    /// [`RenderError::Unsupported`]) or when surface creation fails.
    pub fn new(config: HostConfig) -> Result<Self, HostError> {
        let pipeline = create_render_pipeline(PipelineConfig {
            backend: config.backend,
            application_name: config.application_name,
            preferred_format: TextureFormat::Rgba8Srgb,
            validation: config.validation,
        })?;
        let fs = FileSystem::memory();
        let media = Media::new();
        let scene = Scene::default();
        let render_scene = RenderScene::with_root_node();

        let headless_handle;
        let handle: &RawWindowHandle = if config.headless {
            if config.backend != BackendKind::Null {
                return Err(HostError::Config(
                    "headless hosts require the null backend".to_owned(),
                ));
            }
            headless_handle = platform::headless_window_handle();
            &headless_handle
        } else {
            config.window_handle.as_ref().ok_or_else(|| {
                HostError::Config(
                    "no window handle; set `headless` for an offscreen host".to_owned(),
                )
            })?
        };
        let (surface, mut render_state) = match &config.display_handle {
            Some(display) => begin_render_state_with_display(&pipeline, display, handle)?,
            None => begin_render_state(&pipeline, handle)?,
        };
        let surface_size = (config.surface_size.0.max(1), config.surface_size.1.max(1));
        let _ = resize_render(&pipeline, &surface, &mut render_state, surface_size)?;
        let view = Presentation::compute(config.scaling, surface_size, surface_size).view;

        Ok(Self {
            pipeline,
            fs,
            media,
            scene,
            render_scene,
            render_scene_revision: None,
            surface,
            render_state,
            textures: HashMap::new(),
            input: InputState::default(),
            headless: config.headless,
            frame_index: 0,
            elapsed_ms: 0.0,
            surface_size,
            scaling: config.scaling,
            design_size: None,
            logical_size: surface_size,
            view,
        })
    }

    /// Advances the host by one frame and presents it.
    ///
    /// This performs the projection seam in order: dirty bitmaps are synced
    /// to textures, the layer tree is projected into the persistent
    /// [`RenderScene`] (skipped while [`Scene::revision`] is unchanged), and
    /// the pipeline renders it in a single call. `dt_ms` is accumulated into
    /// [`Host::elapsed_ms`] for callers that animate.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Render`] when the render action fails.
    pub fn step(&mut self, dt_ms: f32) -> Result<StepReport, HostError> {
        self.elapsed_ms += f64::from(dt_ms).max(0.0);
        self.sync_dirty_bitmaps();
        self.prune_stale_textures();

        self.reproject_render_scene();

        render(
            &self.pipeline,
            &self.surface,
            &mut self.render_state,
            &self.render_scene,
        )?;
        self.frame_index = self.frame_index.saturating_add(1);

        Ok(StepReport {
            frame_index: self.frame_index,
            elapsed_ms: self.elapsed_ms,
            stats: self.last_frame(),
        })
    }

    /// Rebuilds [`Host::render_scene`] from the layer tree when it is stale.
    ///
    /// The projection is skipped while [`Scene::revision`] is unchanged, so a
    /// static scene does not reallocate the render tree every frame. A
    /// rebuild reuses the existing arena: [`RenderScene::nodes`] is cleared
    /// (retaining its capacity) and refilled, so the persistent allocation is
    /// kept and only the projected nodes' own allocations are touched. The
    /// revision is `None` until the first projection, so the first frame (and
    /// any forced redraw) always projects.
    fn reproject_render_scene(&mut self) {
        // The framebuffer transform can change on a resize that does not
        // change the logical layout, so it is refreshed even when the layer
        // tree is rebuilt from the persistent arena below.
        self.render_scene.view = self.view;

        let revision = self.scene.revision();
        if self.render_scene_revision == Some(revision) {
            return;
        }

        let textures = &self.textures;
        let projected = project_scene(&self.scene, self.view, |bitmap| {
            textures.get(&bitmap).cloned()
        });

        // Reuse the arena instead of replacing the whole `RenderScene`.
        self.render_scene.nodes.clear();
        self.render_scene.root = projected.root;
        self.render_scene.background = projected.background;
        self.render_scene.view = projected.view;
        self.render_scene.nodes.extend(projected.nodes);

        self.render_scene_revision = Some(revision);
    }

    /// Replaces the stored raw input snapshot.
    ///
    /// TVP hit-testing and gesture synthesis stay in the middle layer; the
    /// host only captures the device state and hands it over.
    pub fn handle_input(&mut self, input: InputState) {
        self.input = input;
    }

    /// Reports a new surface size in physical pixels.
    ///
    /// The physical size is the framebuffer. The logical size reported to
    /// scripts is derived from it according to the configured
    /// [`ScalingMode`]: the framebuffer size for [`ScalingMode::Game`], the
    /// game's declared design size for `Fit`/`Fill`, and the design height at
    /// the window's aspect for `Expand`.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Render`] if the backend rejects the resize.
    pub fn resize(&mut self, size: (u32, u32)) -> Result<SurfaceEvent, HostError> {
        let size = (size.0.max(1), size.1.max(1));
        let event = resize_render(&self.pipeline, &self.surface, &mut self.render_state, size)?;
        self.surface_size = size;

        // A scene window whose `inner_size` differs from the logical size the
        // host last wrote is a script declaration (`Window.setSize` /
        // `setInnerSize`); capture it as the design size the presentation
        // scales from.
        let scripted = self.scene.windows.first().map(|window| window.inner_size);
        if let Some(scripted) = scripted
            && (self.design_size.is_none() || scripted != self.logical_size)
        {
            self.design_size = Some(scripted);
        }
        let design = self.design_size.unwrap_or(size);
        let presentation = Presentation::compute(self.scaling, design, size);
        self.view = presentation.view;
        self.logical_size = presentation.logical_size;

        // The logical screen size and, when the window has one, its primary
        // layer follow the presentation; the layer stays at the origin
        // because the reference forbids moving the primary layer.
        if let Some(window) = self.scene.windows.first().map(|window| window.id) {
            resize_window_client(&mut self.scene, window, self.logical_size);
        }
        Ok(event)
    }

    /// Returns the stateless render pipeline.
    pub fn pipeline(&self) -> &RenderPipeline {
        &self.pipeline
    }

    /// Returns the backend the pipeline was created with.
    pub fn backend(&self) -> BackendKind {
        self.pipeline.backend()
    }

    /// Returns the file-system service (constructed, not yet used).
    pub fn fs(&self) -> &FileSystem {
        &self.fs
    }

    /// Returns the media service (constructed, not yet used).
    pub fn media(&self) -> &Media {
        &self.media
    }

    /// Returns the TVP layer system.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Returns the TVP layer system for script-driven mutation.
    pub fn scene_mut(&mut self) -> &mut Scene {
        &mut self.scene
    }

    /// Returns the most recently projected render tree.
    pub fn render_scene(&self) -> &RenderScene {
        &self.render_scene
    }

    /// Returns the bitmap-id → texture table.
    pub fn textures(&self) -> &HashMap<u32, Texture> {
        &self.textures
    }

    /// Returns the latest raw input snapshot.
    pub fn input(&self) -> &InputState {
        &self.input
    }

    /// Whether this host runs offscreen with no window.
    ///
    /// Mirrors [`HostConfig::headless`]: the game host uses it to select the
    /// L1 null audio backend instead of opening a real device.
    pub fn is_headless(&self) -> bool {
        self.headless
    }

    /// Returns the current surface size in pixels.
    pub fn surface_size(&self) -> (u32, u32) {
        self.surface_size
    }

    /// Returns the presentation scaling mode.
    pub fn scaling(&self) -> ScalingMode {
        self.scaling
    }

    /// Returns the logical scene size the host reports to scripts.
    pub fn logical_size(&self) -> (u32, u32) {
        self.logical_size
    }

    /// Returns the framebuffer transform for the current presentation.
    pub fn view(&self) -> ViewTransform {
        self.view
    }

    /// Returns the number of frames rendered so far.
    pub fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Returns the simulated time advanced through [`Host::step`].
    pub fn elapsed_ms(&self) -> f64 {
        self.elapsed_ms
    }

    /// Returns the null backend's counters for the last frame, if available.
    pub fn last_frame(&self) -> Option<FrameStats> {
        render::NullRenderPipeline::last_frame(&self.render_state)
    }

    /// Uploads every bitmap whose dirty flag is set and clears the flag.
    ///
    /// A bitmap whose dimensions changed recreates its texture; one with an
    /// inconsistent pixel length is skipped with a warning so one bad bitmap
    /// cannot abort the frame. All dirty bitmaps are uploaded in one batch,
    /// so the frame pays a single GPU wait instead of one per bitmap.
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
