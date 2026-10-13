//! The emulator entry point and the L1/L2 composition it drives.
//!
//! A launcher resolves everything a run needs — the [`RuntimeConfig`] and the
//! [`GameInput`] — and calls [`run`]. The emulator core performs no
//! configuration I/O of its own: the desktop CLI reads its file, an Android
//! launcher reads platform storage, and both hand the finished value to the
//! same function so the core never learns where configuration comes from.

use std::path::PathBuf;
use std::time::Duration;

use api::config::{PresentationScaling, RuntimeConfig};
use engine::projection::ScalingMode;
use platform::{EventHandler, Flow, FramePacing, PlatformConfig, PlatformEvent, PlatformWindow};
use render::ViewTransform;

use crate::{
    BackendKind, FrameStats, GameHost, Host, HostConfig, StepReport, map_input_to_logical,
};

/// Default window/surface size in pixels.
const DEFAULT_SIZE: (u32, u32) = (1280, 720);

/// The game a launcher asks the emulator to open.
///
/// The launcher resolves the command line or the platform intent into this
/// value: it owns the app path and the ordered archive list. The emulator
/// only mounts what it is given.
#[derive(Clone, Debug)]
pub struct GameInput {
    /// App path for loose files and the base for relative archive paths.
    pub base_dir: PathBuf,
    /// Archives to mount, in order (later wins).
    pub archives: Vec<PathBuf>,
}

/// How a launched process runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// An offscreen frame loop that steps `frames` frames with no window or
    /// GPU.
    Headless {
        /// Number of frames to step.
        frames: u32,
    },
    /// A window and event loop driven by the platform backend.
    Windowed,
}

/// A fully resolved launch request.
///
/// The caller owns configuration loading: it builds this value from whatever
/// store its platform uses (a `key=value` file for the desktop CLI, platform
/// storage for an Android launcher) and then calls [`run`]. The emulator core
/// therefore performs no configuration I/O, and every frontend reaches the
/// engine through the same entry point.
#[derive(Clone, Debug)]
pub struct Launch {
    /// The validated runtime configuration to apply.
    pub runtime: RuntimeConfig,
    /// Render backend to create.
    pub backend: BackendKind,
    /// Game to open, or `None` for the empty-scene run.
    pub game: Option<GameInput>,
    /// Whether to run offscreen or in a window.
    pub mode: RunMode,
}

/// What a completed [`run`] produced.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunReport {
    /// The last frame the headless loop stepped, or `None` for a windowed run
    /// (and for a headless run of zero frames).
    pub last_frame: Option<StepReport>,
}

/// Runs the emulator with a launcher-resolved [`Launch`].
///
/// The caller owns configuration loading and game-path resolution; this
/// function only runs what `launch` carries, so each platform frontend can
/// supply its own configuration store without changing the core. It returns
/// after a headless run finishes or after the windowed event loop exits.
///
/// # Errors
///
/// A human-readable message when the host cannot be created or the game
/// cannot be opened, stepped or presented.
pub fn run(launch: Launch) -> Result<RunReport, String> {
    match launch.mode {
        RunMode::Headless { frames } => run_headless(&launch, frames),
        RunMode::Windowed => run_windowed(&launch).map(|()| RunReport::default()),
    }
}

/// Maps a resolved presentation scaling to the engine's projection mode.
fn map_scaling(scaling: PresentationScaling) -> ScalingMode {
    match scaling {
        PresentationScaling::Game => ScalingMode::Game,
        PresentationScaling::Fit => ScalingMode::Fit,
        PresentationScaling::FitSharp => ScalingMode::FitSharp,
        PresentationScaling::Fill => ScalingMode::Fill,
        PresentationScaling::Expand => ScalingMode::Expand,
    }
}

/// Formats the null backend counters on one line.
fn format_stats(stats: &FrameStats) -> String {
    format!(
        "visited={} draws={} (solid={} texture={} glyph={}) invisible={} missing={} depth={}",
        stats.nodes_visited,
        stats.draws,
        stats.solid_draws,
        stats.texture_draws,
        stats.glyph_draws,
        stats.invisible,
        stats.missing_textures,
        stats.max_depth,
    )
}

/// Runs offscreen and prints the final frame stats (or the game's scene size).
fn run_headless(launch: &Launch, frames: u32) -> Result<RunReport, String> {
    let config = HostConfig::headless().with_backend(launch.backend);
    let mut host = Host::new(config).map_err(|error| error.to_string())?;
    let dt_ms = launch.runtime.frame_rate_limit.period_ms();

    if let Some(input) = &launch.game {
        let mut game = GameHost::open_with_preload(
            host,
            input.archives.clone(),
            input.base_dir.clone(),
            launch.runtime.preload_archives,
        )
        .map_err(|error| error.to_string())?;
        println!(
            "krkr headless game: backend={} frames={} archives={}",
            game.host().backend().name(),
            frames,
            input.archives.len(),
        );
        let mut last = None;
        for _ in 0..frames {
            last = Some(game.step(dt_ms, &[]).map_err(|error| error.to_string())?);
        }
        if let Some(report) = last
            && let Some(stats) = report.stats
        {
            println!(
                "draws: frame={} {} textures={}",
                report.frame_index,
                format_stats(&stats),
                game.host().textures().len()
            );
        }
        let scene = game.scene();
        // Layers the window's root list can actually reach — the reference
        // draws exactly that tree (`tTVPLayerManager::RecreateOverallOrderIndex`),
        // so a scene with many unreached layers renders as an empty window.
        let reachable: Vec<String> = scene
            .windows
            .iter()
            .map(|window| {
                format!(
                    "win{}={}/{}",
                    window.id,
                    scene.window_layer_order(window.id).len(),
                    scene
                        .layers
                        .iter()
                        .filter(|layer| layer.window == window.id && !layer.shutdown)
                        .count()
                )
            })
            .collect();
        println!(
            "final: windows={} layers={} bitmaps={} reachable={}",
            scene.windows.len(),
            scene.layers.len(),
            scene.bitmaps.len(),
            reachable.join(",")
        );
        return Ok(RunReport { last_frame: last });
    }

    println!(
        "krkr headless: backend={} frames={} size={}x{}",
        host.backend().name(),
        frames,
        host.surface_size().0,
        host.surface_size().1,
    );

    let mut last = None;
    for _ in 0..frames {
        last = Some(host.step(dt_ms).map_err(|error| error.to_string())?);
    }
    if let Some(report) = last
        && let Some(stats) = report.stats
    {
        println!(
            "final: frame={} {}",
            report.frame_index,
            format_stats(&stats)
        );
    }
    Ok(RunReport { last_frame: last })
}

/// Opens a window through the L1 platform and drives [`Host::step`].
fn run_windowed(launch: &Launch) -> Result<(), String> {
    let mut config = PlatformConfig::default();
    config.title = "krkr-rs".to_owned();
    config.size = DEFAULT_SIZE;
    config.frame_interval = Duration::from_secs_f64(launch.runtime.frame_rate_limit.period_secs());
    platform::run(
        config,
        DesktopApp::new(
            launch.backend,
            map_scaling(launch.runtime.scaling),
            launch.game.clone(),
            launch.runtime.preload_archives,
        ),
    )
    .map_err(|error| error.to_string())
}

/// The composition root's bridge from L1 platform events to the L2 host.
///
/// The platform owns the window and drops the handler before the window (see
/// `platform`'s loop), so the Vulkan surface owned by the host is
/// destroyed while the display connection is still alive. The field order of
/// this struct is therefore irrelevant to teardown.
struct DesktopApp {
    /// Backend requested on the command line.
    backend: BackendKind,
    /// Presentation scaling mode requested on the command line.
    scaling: ScalingMode,
    /// The game to load, or `None` for the empty scene.
    game: Option<GameInput>,
    /// Whether every mounted archive is read into memory at mount.
    preload_archives: bool,
    /// The running host, created once the window handle exists.
    running: Option<Running>,
    /// Input and focus events received since the last frame, delivered to
    /// the VM.
    pending_events: Vec<PlatformEvent>,
    /// The window size a script most recently requested, so a request still
    /// in flight is not repeated on every frame.
    pending_size: Option<(u32, u32)>,
}

/// What the windowed loop is driving.
enum Running {
    /// An empty scene (no game).
    Empty(Box<Host>),
    /// A real game: the L4 VM host plus the L2 render host.
    Game(Box<GameHost>),
}

impl Running {
    /// Resizes the presentation surface.
    fn resize(&mut self, size: (u32, u32)) -> Result<(), String> {
        match self {
            Running::Empty(host) => host.resize(size).map(|_| ()).map_err(|e| e.to_string()),
            Running::Game(game) => game
                .host_mut()
                .resize(size)
                .map(|_| ())
                .map_err(|e| e.to_string()),
        }
    }

    /// The main window's declared client size, or `None` when the host owns
    /// no scene window (the empty-scene run).
    fn inner_size(&self) -> Option<(u32, u32)> {
        match self {
            Running::Empty(_) => None,
            Running::Game(game) => game.scene().windows.first().map(|window| window.inner_size),
        }
    }

    /// The logical size the host currently reports, or `None` when the host
    /// owns no scene window.
    fn logical_size(&self) -> Option<(u32, u32)> {
        match self {
            Running::Empty(_) => None,
            Running::Game(game) => Some(game.host().logical_size()),
        }
    }

    /// The framebuffer transform of the current presentation.
    fn view(&self) -> Option<ViewTransform> {
        match self {
            Running::Empty(host) => Some(host.view()),
            Running::Game(game) => Some(game.host().view()),
        }
    }

    /// Advances and presents one frame, delivering `events` to the VM.
    fn step(
        &mut self,
        dt_ms: f32,
        events: &[PlatformEvent],
        pacing: FramePacing,
    ) -> Result<(), String> {
        match self {
            Running::Empty(host) => host.step(dt_ms).map(|_| ()).map_err(|e| e.to_string()),
            Running::Game(game) => {
                game.step_with_pacing(dt_ms, events, pacing)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }
}

impl DesktopApp {
    /// Creates a handler that has not yet received `resumed`.
    fn new(
        backend: BackendKind,
        scaling: ScalingMode,
        game: Option<GameInput>,
        preload_archives: bool,
    ) -> Self {
        Self {
            backend,
            scaling,
            game,
            preload_archives,
            running: None,
            pending_events: Vec::new(),
            pending_size: None,
        }
    }

    /// Requests a new platform-window inner size when a script changed the
    /// main window's declared client size.
    ///
    /// The host rewrites the scene window's `inner_size` to the logical size
    /// of the current presentation, so the raw `inner_size` equals the
    /// host's logical size unless a script declared a new size. A window
    /// whose `inner_size` differs is a declaration and is applied to the OS
    /// window; a user resize changes the framebuffer instead and is left
    /// alone. When the backend applies the request synchronously (and
    /// therefore emits no `Resized`), the surface is resized here.
    fn apply_window_size(&mut self, window: &PlatformWindow) -> Result<(), String> {
        let Some(size) = self.running.as_ref().and_then(Running::inner_size) else {
            return Ok(());
        };
        let logical = self.running.as_ref().and_then(Running::logical_size);
        if logical == Some(size) {
            return Ok(());
        }
        if size.0 == 0 || size.1 == 0 {
            return Ok(());
        }
        if self.pending_size == Some(size) {
            return Ok(());
        }
        if size == window.size() {
            self.pending_size = None;
            if let Some(running) = self.running.as_mut() {
                running.resize(size)?;
            }
            return Ok(());
        }
        self.pending_size = Some(size);
        if let Some(applied) = window.set_size(size) {
            self.pending_size = None;
            if let Some(running) = self.running.as_mut() {
                running.resize(applied)?;
            }
        }
        Ok(())
    }
}

impl EventHandler for DesktopApp {
    fn resumed(&mut self, window: &PlatformWindow) -> Flow {
        // A missing handle is fatal, never replaced by a synthetic one.
        let window_handle = match window.raw_window_handle() {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("krkr: cannot obtain the window handle: {error}");
                return Flow::Exit;
            }
        };
        let display_handle = match window.raw_display_handle() {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("krkr: cannot obtain the display handle: {error}");
                return Flow::Exit;
            }
        };
        // Create the host at the window's real physical size rather than at
        // the requested logical default, so the initial swapchain matches.
        let config = HostConfig {
            backend: self.backend,
            surface_size: window.size(),
            window_handle: Some(window_handle),
            display_handle: Some(display_handle),
            scaling: self.scaling,
            ..Default::default()
        };
        match Host::new(config) {
            Ok(host) => {
                let running = match &self.game {
                    Some(input) => {
                        match GameHost::open_with_preload(
                            host,
                            input.archives.clone(),
                            input.base_dir.clone(),
                            self.preload_archives,
                        ) {
                            Ok(game) => Running::Game(Box::new(game)),
                            Err(error) => {
                                eprintln!("krkr: failed to open game: {error}");
                                return Flow::Exit;
                            }
                        }
                    }
                    None => Running::Empty(Box::new(host)),
                };
                self.running = Some(running);
                // Align the platform window with the client size the game
                // declared during startup (`Window.setInnerSize`); the
                // resulting `Resized` sizes the surface.
                if let Err(error) = self.apply_window_size(window) {
                    eprintln!("krkr: resize failed: {error}");
                    return Flow::Exit;
                }
                Flow::Continue
            }
            Err(error) => {
                eprintln!("krkr: failed to create host: {error}");
                Flow::Exit
            }
        }
    }

    fn event(&mut self, event: PlatformEvent) -> Flow {
        match event {
            PlatformEvent::Resized(width, height) => {
                self.pending_size = None;
                if let Some(running) = self.running.as_mut()
                    && let Err(error) = running.resize((width, height))
                {
                    eprintln!("krkr: resize failed: {error}");
                    return Flow::Exit;
                }
                Flow::Continue
            }
            // winit follows a scale-factor change with a `Resized` carrying
            // the new physical size, which is the event that resizes the
            // surface; the DPI value itself needs no host action because input
            // and sizes are physical.
            PlatformEvent::ScaleFactorChanged(_scale) => Flow::Continue,
            // Input and focus are buffered and delivered to the VM frame
            // body on the next frame, where the L3 event adapter folds them
            // into the L2 input state and dispatches the TJS events
            // (`onFocus`/`onBlur`). `ScaleFactorChanged` is informational
            // (winit follows it with `Resized`) and the adapter ignores it.
            PlatformEvent::Input(input) => {
                // The engine presents the logical scene scaled inside the
                // framebuffer, so a pointer/touch position is inverse-mapped
                // to logical coordinates before the layer manager hit-tests
                // it.
                let input = match self.running.as_ref().and_then(Running::view) {
                    Some(view) => map_input_to_logical(view, input),
                    None => input,
                };
                self.pending_events.push(PlatformEvent::Input(input));
                Flow::Continue
            }
            PlatformEvent::Focused(focused) => {
                self.pending_events.push(PlatformEvent::Focused(focused));
                Flow::Continue
            }
            PlatformEvent::CloseRequested => Flow::Exit,
        }
    }

    fn frame(&mut self, dt_ms: f32, window: &PlatformWindow, pacing: FramePacing) -> Flow {
        if let Some(running) = self.running.as_mut() {
            let events = std::mem::take(&mut self.pending_events);
            if let Err(error) = running.step(dt_ms, &events, pacing) {
                eprintln!("krkr: frame failed: {error}");
                return Flow::Exit;
            }
        }
        if let Err(error) = self.apply_window_size(window) {
            eprintln!("krkr: frame resize failed: {error}");
            return Flow::Exit;
        }
        Flow::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::config::FrameRateLimit;

    /// [`run`] applies the configuration the launcher passes it directly,
    /// with no file or environment variable involved.
    #[test]
    fn run_honours_a_directly_passed_config() {
        let runtime = RuntimeConfig {
            frame_rate_limit: FrameRateLimit::new("test", 500).expect("valid"),
            scaling: PresentationScaling::Fit,
            ..RuntimeConfig::default()
        };
        let report = run(Launch {
            runtime,
            backend: BackendKind::Null,
            game: None,
            mode: RunMode::Headless { frames: 2 },
        })
        .expect("headless run");

        let last = report.last_frame.expect("two frames were stepped");
        assert_eq!(last.frame_index, 2);
        // 500 fps is a 2 ms frame period, so two frames advance 4 ms; a
        // default 60 fps run would have advanced about 33 ms instead.
        assert!(
            (last.elapsed_ms - 4.0).abs() < 1e-9,
            "elapsed {} ms",
            last.elapsed_ms
        );
    }
}
