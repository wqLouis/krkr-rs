//! krkr-rs host assembly: the middle-layer engine plus the stateless services
//! the composition root holds, wired to the L1 platform by the `krkr` binary.
//!
//! [`Host`] wraps the middle-layer [`engine::Engine`] together with the two
//! stateless bottom objects the composition root also keeps ([`FileSystem`]
//! and [`Media`]). Every evolving value — the TVP layer system [`Scene`], the
//! projected [`RenderScene`], the [`RenderState`]/[`Surface`] pair, the
//! bitmap-id → [`Texture`] table, the timer registry and the script-visible
//! input state — belongs to the engine, so the host holds no frame state of
//! its own and drives one engine frame per [`Host::step`].
//!
//! There are no globals, statics or hidden caches: every value is a local
//! field of [`Host`] and is passed to the bottom objects explicitly. Two
//! hosts can therefore run in the same process.

// The composition root installs the VM's raw-pointer binding to the L2 scene,
// storage, timer and input (`compat::CompatServices::attach_*`), which is
// inherently `unsafe`. That wiring is the crate's only `unsafe`, confined to
// `game` and covered by SAFETY comments; every other module is safe.
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod audio;
mod game;
mod image_codec;
mod overlay;
mod runner;
mod startup;
mod video;
mod window_ex;

use api::error::RenderError;
use api::host::SurfaceEvent;
use engine::Scene;
use fs::FileSystem;
use media::Media;
use overlay::Overlay;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use render::{
    FrameTimings, PipelineConfig, RenderPipeline, RenderScene, RenderState, Surface, Texture,
    TextureFormat, begin_render_state, begin_render_state_with_display, create_render_pipeline,
};

pub use audio::install_audio_decoder;
pub(crate) use audio::install_audio_decoder_metered;
pub use game::{GameError, GameHost};
pub(crate) use image_codec::install_image_codec_metered;
pub use image_codec::{install_image_codec, install_image_codec_on};
pub use render::{BackendKind, FrameStats};
// The emulator entry point. A launcher resolves `Launch` (including the
// `RuntimeConfig` it loaded its own way) and calls `run`; see the `runner`
// module for the entry point contract.
pub use runner::{GameInput, Launch, RunMode, RunReport, run};
pub use video::install_video_source;
pub(crate) use video::install_video_source_metered;
pub use window_ex::install_window_ex_facts;

/// Errors surfaced by [`Host`].
///
/// The host reports the engine's own frame, configuration and overlay failures
/// alongside its own configuration check.
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
    /// The debug overlay could not be built or refreshed.
    #[error("debug overlay: {0}")]
    Overlay(String),
    /// The frame hook failed; the frame was abandoned before drawing.
    #[error("frame: {0}")]
    Frame(String),
}

impl From<engine::EngineError> for HostError {
    fn from(error: engine::EngineError) -> Self {
        match error {
            engine::EngineError::Render(error) => Self::Render(error),
            engine::EngineError::Overlay(message) => Self::Overlay(message),
            engine::EngineError::Frame(message) => Self::Frame(message),
            // `EngineError` is non-exhaustive, so a variant this build cannot
            // classify keeps its message as a frame failure.
            other => Self::Frame(other.to_string()),
        }
    }
}

/// Outcome of one [`Host::step`].
pub use engine::FrameReport as StepReport;

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
    /// Whether the host draws the debug overlay (frame rate, host usage,
    /// memory and live object counts) in the top-left corner.
    ///
    /// The overlay is a render-only node: it is never a script-visible layer
    /// or bitmap, so it cannot change the scene a script observes.
    pub debug_overlay: bool,
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
            debug_overlay: false,
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

    /// Returns this configuration with the debug overlay enabled or disabled.
    pub fn with_debug_overlay(mut self, debug_overlay: bool) -> Self {
        self.debug_overlay = debug_overlay;
        self
    }
}

/// The assembled host: the middle-layer engine plus the stateless services
/// the composition root also holds.
///
/// See the crate documentation for the ownership model. Callers reach the
/// layer system through [`Host::scene`] / [`Host::scene_mut`] and drive
/// frames through [`Host::step`].
pub struct Host {
    /// The middle-layer engine: scene, projection, textures, timers, input
    /// and the frame clock.
    engine: engine::Engine,
    /// Stateless file-system service (constructed, not yet exercised).
    fs: FileSystem,
    /// Stateless media service (constructed, not yet exercised).
    media: Media,
}

/// A frame hook that does nothing, for a host with no game above it.
struct EmptyFrame;

impl engine::GameFrame for EmptyFrame {
    fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
        Ok(())
    }
}

impl Host {
    /// Creates a host and its surface from `config`.
    ///
    /// The file system is an in-memory root and the media object uses its
    /// default configuration; the host does not exercise either yet.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Config`] for a headless configuration with a real
    /// backend and for a non-headless one with no window handle. Returns
    /// [`HostError::Render`] when the
    /// requested backend cannot be created or when surface creation fails. A
    /// `--backend vulkan` host reports [`RenderError::Unsupported`] for a
    /// backend this build does not implement, and
    /// [`RenderError::DeviceUnavailable`] when the Vulkan loader, instance or
    /// device cannot be brought up.
    pub fn new(config: HostConfig) -> Result<Self, HostError> {
        let pipeline = create_render_pipeline(PipelineConfig {
            backend: config.backend,
            application_name: config.application_name,
            preferred_format: TextureFormat::Rgba8Srgb,
            validation: config.validation,
        })?;
        let fs = FileSystem::memory();
        let media = Media::new();

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
        let (surface, render_state): (Surface, RenderState) = match &config.display_handle {
            Some(display) => begin_render_state_with_display(&pipeline, display, handle)?,
            None => begin_render_state(&pipeline, handle)?,
        };
        let overlay = if config.debug_overlay {
            Some(Overlay::new(&pipeline).map_err(|error| HostError::Overlay(error.to_string()))?)
        } else {
            None
        };

        let engine = engine::Engine::new(engine::EngineParts {
            pipeline,
            surface,
            render_state,
            surface_size: config.surface_size,
            headless: config.headless,
            overlay: overlay.map(|overlay| Box::new(overlay) as Box<dyn engine::EngineOverlay>),
        })?;

        Ok(Self { engine, fs, media })
    }

    /// Advances the host by one frame and presents it.
    ///
    /// The engine drives the whole frame: it fits the script-visible size into
    /// the framebuffer, advances its clock, fires due timers, runs the (here
    /// empty) frame hook, fits the presentation again (the hook may have
    /// changed a window size), refreshes the debug overlay, uploads dirty
    /// bitmaps, projects the layer tree when it changed, translates it into
    /// device pixels and renders the result. `dt_ms` advances the engine's
    /// frame clock.
    ///
    /// # Errors
    ///
    /// Returns [`HostError`] when the frame hook, the overlay or a render
    /// action fails.
    pub fn step(&mut self, dt_ms: f32) -> Result<StepReport, HostError> {
        self.step_with_frame(dt_ms, &mut EmptyFrame)
    }

    /// Advances the host by one frame, running `hooks` before the projection.
    ///
    /// This is the composition root's entry point: the game host passes its
    /// VM/audio hook so its per-frame work runs between the timer advance and
    /// the projection.
    ///
    /// # Errors
    ///
    /// As [`Host::step`], plus a hook failure reported as
    /// [`HostError::Frame`].
    pub(crate) fn step_with_frame(
        &mut self,
        dt_ms: f32,
        hooks: &mut dyn engine::GameFrame,
    ) -> Result<StepReport, HostError> {
        Ok(self.engine.step(dt_ms, hooks)?)
    }

    /// The middle-layer engine, for the composition root's attachments and
    /// presentation reads.
    pub(crate) fn engine_mut(&mut self) -> &mut engine::Engine {
        &mut self.engine
    }

    /// The middle-layer engine, for reading the presentation the frame uses.
    pub(crate) fn engine(&self) -> &engine::Engine {
        &self.engine
    }

    /// Reports a new framebuffer size in physical pixels.
    ///
    /// The surface size is the device framebuffer, which is independent of the
    /// script-visible size: the scene keeps its window and layer sizes and the
    /// presentation refits them into the new framebuffer, so an OS resize moves
    /// no value a script observes. A script `Window.setInnerSize` is the other
    /// direction and is surfaced through [`Host::take_window_resize`].
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Render`] if the backend rejects the resize.
    pub fn resize(&mut self, size: (u32, u32)) -> Result<SurfaceEvent, HostError> {
        Ok(self.engine.resize(size)?)
    }

    /// Returns the stateless render pipeline.
    pub fn pipeline(&self) -> &RenderPipeline {
        self.engine.pipeline()
    }

    /// Returns the backend the pipeline was created with.
    pub fn backend(&self) -> BackendKind {
        self.engine.backend()
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
        self.engine.scene()
    }

    /// Returns the TVP layer system for script-driven mutation.
    pub fn scene_mut(&mut self) -> &mut Scene {
        self.engine.scene_mut()
    }

    /// Returns the most recently projected render tree.
    pub fn render_scene(&self) -> &RenderScene {
        self.engine.render_scene()
    }

    /// Returns the bitmap-id → texture table.
    pub fn textures(&self) -> &engine::Map<u32, Texture> {
        self.engine.textures()
    }

    /// Whether this host runs offscreen with no window.
    ///
    /// Mirrors [`HostConfig::headless`]: the game host uses it to select the
    /// L1 null audio backend instead of opening a real device.
    pub fn is_headless(&self) -> bool {
        self.engine.is_headless()
    }

    /// Returns the current surface size in pixels.
    pub fn surface_size(&self) -> (u32, u32) {
        self.engine.surface_size()
    }

    /// Returns the VM-pixel → device-pixel mapping of the presented frame, or
    /// `None` when the script-visible size cannot be fitted into the
    /// framebuffer.
    pub fn translation(&self) -> Option<engine::PixelTranslation> {
        self.engine.translation()
    }

    /// Returns the logical screen size the presentation fits into the window:
    /// the first scene window's primary layer when it has one, and the
    /// framebuffer size otherwise.
    pub fn vm_size(&self) -> (u32, u32) {
        self.engine.vm_size()
    }

    /// Takes the script-side window size the platform window still has to
    /// adopt, if any.
    ///
    /// The engine produces a request when the script creates a window whose
    /// client size differs from the framebuffer and whenever that size changes
    /// afterwards. Taking it clears it, so a size that does not change again is
    /// applied once.
    pub fn take_window_resize(&mut self) -> Option<(u32, u32)> {
        self.engine.take_window_resize()
    }

    /// Whether the host draws the debug overlay.
    pub fn debug_overlay_enabled(&self) -> bool {
        self.engine.debug_overlay_enabled()
    }

    /// Returns the number of frames rendered so far.
    pub fn frame_index(&self) -> u64 {
        self.engine.frame_index()
    }

    /// Returns the frame clock of the most recent frame.
    pub fn elapsed_ms(&self) -> f64 {
        self.engine.elapsed_ms()
    }

    /// Returns the null backend's counters for the last frame, if available.
    pub fn last_frame(&self) -> Option<FrameStats> {
        self.engine.last_frame()
    }

    /// Returns the presentation calls' costs for the last presented frame.
    ///
    /// The Vulkan backend reports the acquire/present/fence/recreate costs it
    /// measured; the null backend reports zeros. `None` means no frame has
    /// been presented through this state yet.
    pub fn last_frame_timings(&self) -> Option<FrameTimings> {
        self.engine.last_frame_timings()
    }
}
