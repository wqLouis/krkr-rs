//! The Android event loop: it drives the shared emulator host from the
//! `GameActivity` lifecycle and input callbacks.
//!
//! `android-activity` owns the loop on the `android_main` thread: it delivers
//! [`MainEvent`]s through [`AndroidApp::poll_events`] and the queued input
//! through [`AndroidApp::input_events_iter`]. This module normalizes both into
//! the same [`PlatformEvent`]s the desktop backend emits and calls the same
//! [`EventHandler`] the Linux loop calls, so the engine above the platform
//! boundary cannot tell the two hosts apart.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use android_activity::{AndroidApp, MainEvent, PollEvent};
use app::{BackendKind, GameHost, Host, HostConfig};
use engine::projection::ScalingMode;
use platform::{EventHandler, Flow, PlatformError, PlatformEvent, PlatformWindow, Ticker};

use crate::input::translate_input;
use crate::state;

/// The host handler bound to one `GameActivity` surface.
///
/// It mirrors the desktop host: the L2 render host is created once the
/// `ANativeWindow` exists and is torn down before that window is destroyed.
/// Android always launches a game, so there is no empty-scene variant.
pub(crate) struct AndroidHost {
    /// Render backend requested by the composition root.
    backend: BackendKind,
    /// Presentation scaling mode (phones always fit).
    scaling: ScalingMode,
    /// The folder the launcher selected; mounted when the window appears.
    game_dir: PathBuf,
    /// Whether every mounted archive is read into memory at mount, from the
    /// launcher's resolved settings.
    preload_archives: bool,
    /// The running game, created by [`EventHandler::resumed`].
    running: Option<GameHost>,
    /// Input and focus events received since the last frame.
    pending_events: Vec<PlatformEvent>,
}

impl AndroidHost {
    /// Creates a handler that has not yet received a window.
    pub(crate) fn new(
        backend: BackendKind,
        scaling: ScalingMode,
        game_dir: PathBuf,
        preload_archives: bool,
    ) -> Self {
        Self {
            backend,
            scaling,
            game_dir,
            preload_archives,
            running: None,
            pending_events: Vec::new(),
        }
    }

    /// Drops the host (and its Vulkan surface) before the `ANativeWindow` is
    /// destroyed.
    pub(crate) fn teardown(&mut self) {
        self.running = None;
    }

    /// Delivers a translated input event through the normal event path.
    pub(crate) fn send_input(&mut self, input: platform::InputEvent) {
        let _ = self.event(PlatformEvent::Input(input));
    }

    /// Re-applies the presentation when a script declared a new client size.
    ///
    /// On Android the surface is the display, so a `Window.setInnerSize` cannot
    /// resize the OS window; the host recomputes the fitted presentation over
    /// the same framebuffer instead.
    fn sync_script_size(&mut self) -> Result<(), String> {
        let Some(game) = self.running.as_mut() else {
            return Ok(());
        };
        let declared = game.scene().windows.first().map(|window| window.inner_size);
        let Some(declared) = declared else {
            return Ok(());
        };
        if declared == game.host().logical_size() {
            return Ok(());
        }
        let surface = game.host().surface_size();
        game.host_mut()
            .resize(surface)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

impl EventHandler for AndroidHost {
    fn resumed(&mut self, window: &PlatformWindow) -> Flow {
        // A missing handle is fatal, never replaced by a synthetic one.
        let window_handle = match window.raw_window_handle() {
            Ok(handle) => handle,
            Err(error) => {
                log::error!("cannot obtain the Android window handle: {error}");
                return Flow::Exit;
            }
        };
        let display_handle = match window.raw_display_handle() {
            Ok(handle) => handle,
            Err(error) => {
                log::error!("cannot obtain the Android display handle: {error}");
                return Flow::Exit;
            }
        };
        let mut config = HostConfig::default();
        config.backend = self.backend;
        config.surface_size = window.size();
        config.window_handle = Some(window_handle);
        config.display_handle = Some(display_handle);
        config.scaling = self.scaling;
        let host = match Host::new(config) {
            Ok(host) => host,
            Err(error) => {
                log::error!("cannot create the render host: {error}");
                return Flow::Exit;
            }
        };
        // A directory-only launch mounts every `*.xp3` under the folder. The
        // launcher's archive-preload choice is passed through the same mount
        // path the desktop build uses.
        match GameHost::open_with_preload(
            host,
            Vec::new(),
            self.game_dir.clone(),
            self.preload_archives,
        ) {
            Ok(game) => {
                self.running = Some(game);
                Flow::Continue
            }
            Err(error) => {
                log::error!("cannot open the game: {error}");
                let message = error.to_string();
                state::write_state(state::State::Failed, Some(&message));
                Flow::Exit
            }
        }
    }

    fn event(&mut self, event: PlatformEvent) -> Flow {
        match event {
            PlatformEvent::Resized(width, height) => {
                if let Some(game) = self.running.as_mut()
                    && let Err(error) = game.host_mut().resize((width, height))
                {
                    log::error!("resize failed: {error}");
                    return Flow::Exit;
                }
                Flow::Continue
            }
            // Input and sizes are physical, so the density value needs no host
            // action; the accompanying physical resize carries the new size.
            PlatformEvent::ScaleFactorChanged(_scale) => Flow::Continue,
            PlatformEvent::Input(input) => {
                // The engine presents the logical scene scaled inside the
                // framebuffer, so a pointer or touch position is inverse-mapped
                // to logical coordinates before hit-testing.
                let input = match self.running.as_ref() {
                    Some(game) => app::map_input_to_logical(game.host().view(), input),
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

    fn frame(&mut self, dt_ms: f32, _window: &PlatformWindow) -> Flow {
        if let Some(game) = self.running.as_mut() {
            let events = std::mem::take(&mut self.pending_events);
            if let Err(error) = game.step(dt_ms, &events) {
                log::error!("frame failed: {error}");
                let message = error.to_string();
                state::write_state(state::State::Failed, Some(&message));
                return Flow::Exit;
            }
        }
        Flow::Continue
    }
}

/// Runs the `android-activity` loop until the Activity is destroyed.
///
/// Frames are paced by a [`Ticker`] and rendered at the deadline; the Android
/// `RedrawNeeded` and content-rect callbacks also request a frame, so the
/// first frame and a surface change are presented promptly. While paused, the
/// ticker keeps running but no frame is stepped.
pub(crate) fn run_loop(app: AndroidApp, mut handler: AndroidHost, frame_period: Duration) {
    let mut ticker = Ticker::new(frame_period, Instant::now());
    let mut last_frame = Instant::now();
    let mut window: Option<PlatformWindow> = None;
    let mut paused = false;
    let mut frame_requested = true;
    let mut exit = false;
    let event_source = app.clone();

    while !exit {
        let now = Instant::now();
        let timeout = ticker.deadline().saturating_duration_since(now);
        event_source.poll_events(Some(timeout), |poll_event| {
            if exit {
                return;
            }
            let PollEvent::Main(event) = poll_event else {
                return;
            };
            match event {
                MainEvent::InitWindow { .. } => match create_window(&app) {
                    Ok(created) => {
                        window = Some(created);
                        if let Some(created) = window.as_ref()
                            && handler.resumed(created) == Flow::Exit
                        {
                            exit = true;
                        }
                        frame_requested = true;
                    }
                    Err(error) => {
                        log::error!("cannot open the Android window: {error}");
                        exit = true;
                    }
                },
                MainEvent::TerminateWindow { .. } => {
                    handler.teardown();
                    window = None;
                }
                MainEvent::WindowResized { .. } => {
                    if let Some(created) = &window {
                        let (width, height) = created.size();
                        if handler.event(PlatformEvent::Resized(width, height)) == Flow::Exit {
                            exit = true;
                        }
                        frame_requested = true;
                    }
                }
                MainEvent::ConfigChanged { .. } => {
                    if let Some(created) = &window {
                        let _ = handler
                            .event(PlatformEvent::ScaleFactorChanged(created.scale_factor()));
                        let (width, height) = created.size();
                        if handler.event(PlatformEvent::Resized(width, height)) == Flow::Exit {
                            exit = true;
                        }
                        frame_requested = true;
                    }
                }
                MainEvent::RedrawNeeded { .. }
                | MainEvent::ContentRectChanged { .. }
                | MainEvent::InsetsChanged { .. } => {
                    frame_requested = true;
                }
                MainEvent::GainedFocus => {
                    if handler.event(PlatformEvent::Focused(true)) == Flow::Exit {
                        exit = true;
                    }
                }
                MainEvent::LostFocus => {
                    if handler.event(PlatformEvent::Focused(false)) == Flow::Exit {
                        exit = true;
                    }
                }
                MainEvent::Resume { .. } => {
                    paused = false;
                    frame_requested = true;
                    if handler.event(PlatformEvent::Focused(true)) == Flow::Exit {
                        exit = true;
                    }
                }
                MainEvent::Pause => {
                    paused = true;
                    if handler.event(PlatformEvent::Focused(false)) == Flow::Exit {
                        exit = true;
                    }
                }
                MainEvent::Destroy => {
                    let _ = handler.event(PlatformEvent::CloseRequested);
                    exit = true;
                }
                MainEvent::SaveState { .. } => {
                    state::write_state(state::State::Running, None);
                }
                MainEvent::LowMemory => log::warn!("the system reported low memory"),
                MainEvent::InputAvailable => translate_input(&app, &mut handler),
                // `MainEvent` is non-exhaustive; the lifecycle events the
                // engine observes are handled above and the rest need no host
                // action.
                _ => {}
            }
        });

        let now = Instant::now();
        if ticker.poll(now) {
            frame_requested = true;
        }
        if frame_requested && !paused && !exit {
            frame_requested = false;
            if let Some(created) = &window {
                let dt_ms = now.saturating_duration_since(last_frame).as_secs_f32() * 1000.0;
                last_frame = now;
                if handler.frame(dt_ms, created) == Flow::Exit {
                    exit = true;
                }
                if let Err(error) = handler.sync_script_size() {
                    log::error!("cannot apply the script window size: {error}");
                    exit = true;
                }
            }
        }
    }
}

/// Wraps the `ANativeWindow` the activity handed over, with its display
/// density.
fn create_window(app: &AndroidApp) -> Result<PlatformWindow, PlatformError> {
    let native = app.native_window().ok_or_else(|| {
        PlatformError::Window("the activity reported no native window".to_owned())
    })?;
    let density_dpi = match app.config().density() {
        Some(dpi) => match i32::try_from(dpi) {
            Ok(dpi) => dpi,
            Err(_) => {
                log::warn!("display density {dpi} does not fit in i32; using the 160 dpi baseline");
                160
            }
        },
        None => {
            log::warn!("the display reported no density; using the 160 dpi baseline");
            160
        }
    };
    Ok(PlatformWindow::new(native, density_dpi))
}
