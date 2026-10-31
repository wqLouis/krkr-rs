//! The emulator entry point and the L1/L2 composition it drives.
//!
//! A launcher resolves everything a run needs — the [`RuntimeConfig`] and the
//! [`GameInput`] — and calls [`run`]. The emulator core performs no
//! configuration I/O of its own: the desktop CLI reads its file, an Android
//! launcher reads platform storage, and both hand the finished value to the
//! same function so the core never learns where configuration comes from.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use api::config::{CacheLimit, RuntimeConfig};
use engine::FrameTimeSource;
use platform::{
    EventHandler, Flow, FramePacing, PlatformConfig, PlatformError, PlatformEvent, PlatformWindow,
    Ticker,
};
use render::FrameStats;

use crate::{
    BackendKind, GameError, GameHost, Host, HostConfig, HostError, PlatformFrameTime, StepReport,
};

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

/// The failures a [`run`] reports.
///
/// Each source keeps its own typed error: the host's configuration and frame
/// failures, the game's mount and startup failures, and the platform's window
/// and event-loop failures. A launcher prints the error; nothing is flattened
/// into a message.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// The host could not be created, or a frame action failed.
    #[error(transparent)]
    Host(#[from] HostError),
    /// The game could not be opened, stepped or presented.
    #[error(transparent)]
    Game(#[from] GameError),
    /// The platform window or its event loop failed.
    #[error(transparent)]
    Platform(#[from] PlatformError),
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
/// Returns [`RunError`] when the host cannot be created, when the game
/// cannot be opened, stepped or presented, or when the platform window and
/// its event loop fail.
pub fn run(launch: Launch) -> Result<RunReport, RunError> {
    match launch.mode {
        RunMode::Headless { frames } => run_headless(&launch, frames),
        RunMode::Windowed => run_windowed(&launch).map(|()| RunReport::default()),
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

/// The headless loop's frame pacing.
///
/// The loop waits for each frame's deadline, so the frames are as far apart
/// as the frame-rate limit says. The engine's clock advances by the time the
/// loop measured between the frames, so a paced loop makes that clock the
/// wall clock: a run that paced nothing would advance the clock by the frame
/// period for every frame while the wall clock ran on as fast as the host
/// could step, and every duration the script measures would collapse into
/// that gap.
struct HeadlessPacer {
    /// The frame deadlines, paced by the frame-rate limit.
    ticker: Ticker,
}

impl HeadlessPacer {
    /// A pacer whose frames are due every `period_secs` seconds.
    fn new(period_secs: f64) -> Self {
        Self {
            ticker: Ticker::new(Duration::from_secs_f64(period_secs), Instant::now()),
        }
    }

    /// Waits for the frame's deadline.
    fn wait(&mut self) {
        loop {
            let now = Instant::now();
            if self.ticker.poll(now) {
                return;
            }
            std::thread::sleep(self.ticker.deadline().saturating_duration_since(now));
        }
    }
}

/// The headless loop's frame clock.
///
/// The engine's clock advances by the time the loop measured between the
/// frames, so the script's tick count is the wall clock: it moves with the
/// time the run really took, and not with the nominal frame period. A
/// nominal period would leave the script's clock behind the wall clock by the
/// accumulated cost of the frames themselves.
struct HeadlessClock {
    /// The instant the run started: the origin the reading counts from.
    started: Instant,
    /// The milliseconds elapsed since the previous frame.
    elapsed_ms: f64,
    /// The instant the previous frame started.
    ///
    /// The first frame measures from the run's start, the way the windowed
    /// loop measures its first frame from the moment its window was created:
    /// the time the run spent before the first frame is part of the elapsed
    /// time the script sees.
    previous_at: Instant,
}

impl HeadlessClock {
    /// A clock whose first frame measures from `started`.
    fn new(started: Instant) -> Self {
        Self {
            started,
            elapsed_ms: 0.0,
            previous_at: started,
        }
    }

    /// Measures the time since the previous frame and returns the frame time.
    ///
    /// The measurement happens where the frame starts, like the platform's
    /// own measurement in the windowed loop, so a frame's own cost is part of
    /// the next frame's elapsed time.
    fn advance(&mut self, now: Instant) -> &Self {
        self.elapsed_ms = now
            .saturating_duration_since(self.previous_at)
            .as_secs_f64()
            * 1000.0;
        self.previous_at = now;
        self
    }
}

impl FrameTimeSource for HeadlessClock {
    fn elapsed_ms(&self) -> f64 {
        self.elapsed_ms
    }

    /// The run's real elapsed time, read when the script asks for a tick.
    ///
    /// It is the same clock the frame deltas are measured from, so the script's
    /// tick count moves inside a frame and the headless run's clock is the wall
    /// clock.
    fn now_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }
}

/// Runs offscreen and prints the final frame stats (or the game's scene size).
fn run_headless(launch: &Launch, frames: u32) -> Result<RunReport, RunError> {
    let started = Instant::now();
    // Boxed: the script's tick count reads this source, so its address is
    // handed to the game and must stay stable for the whole run.
    let mut clock = Box::new(HeadlessClock::new(started));
    let config = HostConfig::headless()
        .with_backend(launch.backend)
        .with_debug_overlay(launch.runtime.debug_overlay)
        .with_graphic_cache_limit(launch.runtime.graphic_cache_limit);
    // The source is boxed before the host, so the address the game attaches is
    // stable for the whole run; the host is stepped with the same source.
    let source: &(dyn FrameTimeSource + 'static) = clock.as_ref();
    let source = source as *const (dyn FrameTimeSource + 'static);
    let mut host = Host::new(config)?.with_frame_time_source(source);
    // The headless step is paced by the frame-rate limit, like the windowed
    // loop, and the engine's clock advances by the time the loop measured
    // since the previous frame: the script's tick count is the wall clock, so
    // a duration the script measures lasts here as long as it lasts in a
    // windowed run. The period the limit names paces the frames; the clock
    // counts the time that actually passed, which is that period plus the
    // frame's own cost.
    let mut pacer = HeadlessPacer::new(launch.runtime.frame_rate_limit.period_secs());

    if let Some(input) = &launch.game {
        let mut game = GameHost::open_with_preload(
            host,
            input.archives.clone(),
            input.base_dir.clone(),
            launch.runtime.preload_archives,
        )?;
        println!(
            "krkr headless game: backend={} frames={} archives={}",
            game.host().backend().name(),
            frames,
            input.archives.len(),
        );
        let mut last = None;
        for _ in 0..frames {
            pacer.wait();
            let frame_time = clock.advance(Instant::now());
            last = Some(game.step(frame_time, &[])?);
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
        // Layers the window's root list can actually reach: the window draws
        // exactly that tree, so a scene with many unreached layers renders as an
        // empty window.
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
        pacer.wait();
        let frame_time = clock.advance(Instant::now());
        last = Some(host.step(frame_time)?);
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
fn run_windowed(launch: &Launch) -> Result<(), RunError> {
    let frame_interval = Duration::from_secs_f64(launch.runtime.frame_rate_limit.period_secs());
    let mut config = PlatformConfig::default();
    config.title = "krkr-rs".to_owned();
    config.frame_interval = frame_interval;
    Ok(platform::run(
        config,
        DesktopApp::new(
            launch.backend,
            launch.game.clone(),
            launch.runtime.preload_archives,
            launch.runtime.debug_overlay,
            launch.runtime.graphic_cache_limit,
            frame_interval,
        ),
    )?)
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
    /// The game to load, or `None` for the empty scene.
    game: Option<GameInput>,
    /// Whether every mounted archive is read into memory at mount.
    preload_archives: bool,
    /// Whether the debug overlay is drawn.
    debug_overlay: bool,
    /// Decoded-image cache policy, or `None` for the engine default.
    graphic_cache_limit: Option<CacheLimit>,
    /// Frame period the loop paces at, which bounds the presentation wait.
    frame_interval: Duration,
    /// The frame's clock source: the real time the platform measured, and the
    /// reading the script's tick count takes on demand.
    ///
    /// Boxed so the address the game attaches with `Host::with_frame_time_source`
    /// stays stable when the handler moves.
    frame_time: Box<PlatformFrameTime>,
    /// The running host, created once the window handle exists.
    running: Option<Running>,
    /// Input and focus events received since the last frame, delivered to
    /// the VM.
    pending_events: Vec<PlatformEvent>,
    /// The newest window size an OS resize reported since the last frame.
    ///
    /// A compositor can report several sizes for one layout change; the
    /// surface follows the newest one at the frame boundary, so one change
    /// costs at most one swapchain rebuild and none is rebuilt from a size a
    /// later report already replaced.
    pending_surface_size: Option<(u32, u32)>,
    /// Frames delivered since the last heartbeat line.
    heartbeat_frames: u64,
    /// Frames since the last heartbeat line whose scene changed.
    changed_frames: u64,
    /// The scene revisions the previous frame ended on, or `None` before the
    /// first frame.
    last_revisions: Option<SceneRevisions>,
    /// Instant of the most recent heartbeat line.
    heartbeat_at: Instant,
}

/// The scene's two revision counters.
///
/// The counters are the engine's own: the structure revision moves when the
/// layer tree changes and the content revision when a bitmap's pixels change.
/// A frame that moves neither drew the same image as the frame before it.
///
/// The debug overlay is not part of the scene, so a frame that only refreshed
/// the overlay leaves both counters where they were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SceneRevisions {
    /// The layer tree's structure revision.
    structure: u64,
    /// The bitmaps' content revision.
    content: u64,
}

/// What the windowed loop is driving.
enum Running {
    /// An empty scene (no game).
    Empty(Box<Host>),
    /// A real game: the L4 VM host plus the L2 engine host.
    Game(Box<GameHost>),
}

impl Running {
    /// The scene revisions the last frame ended on.
    fn scene_revisions(&self) -> SceneRevisions {
        let scene = match self {
            Running::Empty(host) => host.scene(),
            Running::Game(game) => game.scene(),
        };
        SceneRevisions {
            structure: scene.structure_revision(),
            content: scene.content_revision(),
        }
    }

    /// Resizes the render surface.
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

    /// Takes the script-side window size the OS window still has to adopt.
    ///
    /// The empty-scene run has no scene window, so it never asks for one.
    fn take_window_resize(&mut self) -> Option<(u32, u32)> {
        match self {
            Running::Empty(_) => None,
            Running::Game(game) => game.host_mut().take_window_resize(),
        }
    }

    /// Advances and presents one frame, delivering `events` to the VM.
    fn step(
        &mut self,
        clock: &dyn FrameTimeSource,
        events: &[PlatformEvent],
        pacing: FramePacing,
    ) -> Result<StepReport, String> {
        match self {
            Running::Empty(host) => host.step(clock).map_err(|e| e.to_string()),
            Running::Game(game) => game
                .step_with_pacing(clock, events, pacing)
                .map_err(|e| e.to_string()),
        }
    }
}

impl DesktopApp {
    /// Creates a handler that has not yet received `resumed`.
    fn new(
        backend: BackendKind,
        game: Option<GameInput>,
        preload_archives: bool,
        debug_overlay: bool,
        graphic_cache_limit: Option<CacheLimit>,
        frame_interval: Duration,
    ) -> Self {
        Self {
            backend,
            game,
            preload_archives,
            debug_overlay,
            graphic_cache_limit,
            frame_interval,
            running: None,
            pending_events: Vec::new(),
            pending_surface_size: None,
            frame_time: Box::new(PlatformFrameTime::new()),
            heartbeat_frames: 0,
            changed_frames: 0,
            last_revisions: None,
            heartbeat_at: Instant::now(),
        }
    }

    /// Logs one heartbeat line per second of delivered frames.
    ///
    /// The line carries the frame index, the frame gap and whether the frame
    /// reached the surface; the platform loop logs the ticker deadline for the
    /// same second, so an alive loop that presents nothing is distinguishable
    /// from one that stopped delivering frames.
    fn log_heartbeat(&mut self, report: &StepReport, dt_ms: f32, now: Instant) {
        self.heartbeat_frames = self.heartbeat_frames.saturating_add(1);
        let elapsed = now.saturating_duration_since(self.heartbeat_at);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        log::debug!(
            "app: frame={} (+{} in {:.1} s), dt={dt_ms:.1} ms, presented={}, \
             scene_changed={}/{}",
            report.frame_index,
            self.heartbeat_frames,
            elapsed.as_secs_f64(),
            report.presented,
            self.changed_frames,
            self.heartbeat_frames,
        );
        self.heartbeat_at = now;
        self.heartbeat_frames = 0;
        self.changed_frames = 0;
    }
}

impl EventHandler for DesktopApp {
    fn resumed(&mut self, window: &PlatformWindow) -> Flow {
        // A missing handle is fatal; no synthetic one stands in for it.
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
            debug_overlay: self.debug_overlay,
            graphic_cache_limit: self.graphic_cache_limit,
            frame_interval: self.frame_interval,
            ..Default::default()
        };
        let source: &(dyn FrameTimeSource + 'static) = self.frame_time.as_ref();
        match Host::new(config) {
            Ok(host) => {
                // The source is a field of this handler, so it outlives the
                // game the host is opened over, and it is the source every frame
                // is stepped with.
                let source = source as *const (dyn FrameTimeSource + 'static);
                let host = host.with_frame_time_source(source);
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
                // A window size the startup scripts declared
                // (`Window.setInnerSize`) is surfaced by the first frame and
                // applied there; the resulting `Resized` sizes the surface.
                self.running = Some(running);
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
                if self.running.is_none() {
                    return Flow::Continue;
                }
                // The surface follows the newest size at the next frame
                // boundary. A compositor can report several sizes for one
                // layout change; rebuilding between them would build a
                // swapchain nothing is presented from.
                self.pending_surface_size = Some((width, height));
                // The VM learns about the new size as a script event: the
                // window posts `onResize` to script, and the frame body
                // dispatches it after the surface follows the size.
                self.pending_events
                    .push(PlatformEvent::Resized(width, height));
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
            // Positions arrive in device pixels; the game host maps them into
            // VM pixels before the VM sees them.
            PlatformEvent::Input(input) => {
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
        // The platform measured the real time since the previous frame; that
        // measurement is the elapsed time the engine's clock advances by.
        self.frame_time.elapsed_ms = f64::from(dt_ms);
        let (report, revisions) = {
            let Some(running) = self.running.as_mut() else {
                return Flow::Continue;
            };
            // An OS resize is applied once, with the newest size the event
            // loop reported, before the frame body and the draw see it.
            if let Some(size) = self.pending_surface_size.take()
                && let Err(error) = running.resize(size)
            {
                eprintln!("krkr: resize failed: {error}");
                return Flow::Exit;
            }
            let events = std::mem::take(&mut self.pending_events);
            let report = match running.step(self.frame_time.as_ref(), &events, pacing) {
                Ok(report) => report,
                Err(error) => {
                    eprintln!("krkr: frame failed: {error}");
                    return Flow::Exit;
                }
            };
            // A script `Window.setInnerSize` asks the OS window to adopt the
            // new client size. The engine cleared the request when it was
            // taken, so a size that does not change again is not applied
            // twice, and a request the window already shows needs no action.
            // A backend that applies it synchronously emits no `Resized`, so
            // the surface is resized here from the size the backend reports;
            // when the compositor keeps the final size it reports it as a
            // `Resized` event instead, which the next frame applies.
            if let Some(size) = running.take_window_resize()
                && size.0 != 0
                && size.1 != 0
                && size != window.size()
                && let Some(applied) = window.set_size(size)
                && let Err(error) = running.resize(applied)
            {
                eprintln!("krkr: frame resize failed: {error}");
                return Flow::Exit;
            }
            (report, running.scene_revisions())
        };
        // A frame that moves neither revision drew the same image as the
        // frame before it. The count is a measurement, not a decision: the
        // frame still ran, and the script still saw the same clock.
        if self.last_revisions != Some(revisions) {
            self.changed_frames = self.changed_frames.saturating_add(1);
        }
        self.last_revisions = Some(revisions);
        self.log_heartbeat(&report, dt_ms, Instant::now());
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
            ..RuntimeConfig::default()
        };
        let started = Instant::now();
        let report = run(Launch {
            runtime,
            backend: BackendKind::Null,
            game: None,
            mode: RunMode::Headless { frames: 2 },
        })
        .expect("headless run");
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;

        let last = report.last_frame.expect("two frames were stepped");
        assert_eq!(last.frame_index, 2);
        // The engine's clock counts the time the run really took: it is the
        // wall clock, and not the sum of the nominal frame periods.
        assert!(
            last.elapsed_ms > 0.0 && last.elapsed_ms <= wall_ms,
            "the clock advanced {} ms while the wall clock advanced {wall_ms} ms",
            last.elapsed_ms
        );
    }

    /// The headless step waits for the frame's deadline, so the frames are as
    /// far apart as the frame-rate limit says.
    ///
    /// The script's tick count is the wall clock
    /// (`reference/cpp/core/utils/TickCount.cpp:110-118`), and the windowed
    /// loop measures the same way
    /// (`reference/cpp/core/base/impl/EventImpl.cpp:161`); a headless loop
    /// that paced nothing would advance the engine's clock by the frame
    /// period for every frame while the wall clock ran on as fast as the
    /// host could step, and every duration the script measures would
    /// collapse into that gap.
    #[test]
    fn the_headless_step_waits_for_the_frame_deadline() {
        let runtime = RuntimeConfig {
            frame_rate_limit: FrameRateLimit::new("test", 500).expect("valid"),
            ..RuntimeConfig::default()
        };
        let started = Instant::now();
        run(Launch {
            runtime,
            backend: BackendKind::Null,
            game: None,
            mode: RunMode::Headless { frames: 5 },
        })
        .expect("headless run");
        let elapsed = started.elapsed();
        // 500 fps is a 2 ms frame period: the first frame is due at once and
        // the four that follow wait for their deadlines, so the five frames
        // span at least 8 ms.
        assert!(
            elapsed >= Duration::from_millis(8),
            "five paced frames took {elapsed:?}"
        );
    }
}
