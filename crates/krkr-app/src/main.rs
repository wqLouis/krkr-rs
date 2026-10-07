//! `krkr` CLI: headless frame runner and winit desktop host.
//!
//! ```text
//! krkr --headless [--frames N] [--backend null|vulkan]
//! krkr [--backend null|vulkan]
//! ```
//!
//! * `--headless` runs an empty scene for `N` frames (default 120) through
//!   [`krkr_app::Host`] and prints the null backend [`FrameStats`]. It needs
//!   neither a display nor a GPU.
//! * Without `--headless` the process opens a winit window and drives
//!   [`krkr_app::Host::step`] at roughly 60 fps.
//! * `--backend vulkan` requests the Vulkan pipeline. Vulkan bring-up is
//!   still a Phase 5 skeleton that returns `RenderError::Unsupported`, so a
//!   Vulkan run fails cleanly at startup; the default `--backend null`
//!   always works.
//! * `-h`/`--help` prints the usage text.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use krkr_app::{BackendKind, FrameStats, Host, HostConfig, StepReport};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::raw_window_handle::HasWindowHandle;
use winit::window::{Window, WindowId};

/// Usage text printed by `-h`/`--help` and on a parse error.
const USAGE: &str = "\
krkr-rs host

USAGE:
    krkr --headless [--frames N] [--backend null|vulkan]
    krkr [--backend null|vulkan]

OPTIONS:
    --headless             Run an empty scene offscreen (no window/GPU).
    --frames N             Headless frames to run (default 120).
    --backend null|vulkan  Render backend (default: null).
    -h, --help             Print this help.

NOTE: --backend vulkan currently returns RenderError::Unsupported; the Vulkan
      bring-up (Phase 5) is not implemented yet.
";

/// Default number of headless frames.
const DEFAULT_FRAMES: u32 = 120;
/// Default window/surface size in pixels.
const DEFAULT_SIZE: (u32, u32) = (1280, 720);
/// Target frame pacing for the windowed loop (~60 fps).
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Parsed command line.
#[derive(Clone, Debug)]
struct Cli {
    /// Run the offscreen frame loop instead of opening a window.
    headless: bool,
    /// Number of frames the headless loop renders.
    frames: u32,
    /// Requested render backend.
    backend: BackendKind,
    /// Whether `-h`/`--help` was requested.
    help: bool,
}

impl Cli {
    /// Parses `args` (without the program name).
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut cli = Self {
            headless: false,
            frames: DEFAULT_FRAMES,
            backend: BackendKind::Null,
            help: false,
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--headless" => cli.headless = true,
                "--frames" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--frames requires a value".to_owned())?;
                    cli.frames = value
                        .parse()
                        .map_err(|_| format!("invalid --frames value: {value}"))?;
                }
                "--backend" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--backend requires a value".to_owned())?;
                    cli.backend = parse_backend(&value)?;
                }
                "-h" | "--help" => cli.help = true,
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(cli)
    }
}

/// Maps a `--backend` value to [`BackendKind`].
fn parse_backend(value: &str) -> Result<BackendKind, String> {
    match value {
        "null" => Ok(BackendKind::Null),
        "vulkan" | "vk" => Ok(BackendKind::Vulkan),
        other => Err(format!(
            "unknown backend '{other}' (expected 'null' or 'vulkan')"
        )),
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

fn main() -> ExitCode {
    let cli = match Cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("krkr: {message}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if cli.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let result = if cli.headless {
        run_headless(&cli)
    } else {
        run_windowed(&cli)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("krkr: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Runs an empty scene offscreen and prints the final frame stats.
fn run_headless(cli: &Cli) -> Result<(), String> {
    let config = HostConfig {
        backend: cli.backend,
        ..HostConfig::default()
    };
    let mut host = Host::new(config).map_err(|error| error.to_string())?;

    println!(
        "krkr headless: backend={} frames={} size={}x{}",
        host.backend().name(),
        cli.frames,
        host.surface_size().0,
        host.surface_size().1,
    );

    let mut last = StepReport::default();
    for _ in 0..cli.frames {
        last = host
            .step(1000.0 / 60.0)
            .map_err(|error| error.to_string())?;
    }
    if let Some(stats) = last.stats {
        println!("final: frame={} {}", last.frame_index, format_stats(&stats));
    }
    Ok(())
}

/// Opens a winit window and drives [`Host::step`] at roughly 60 fps.
fn run_windowed(cli: &Cli) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = WindowedApp::new(cli.backend);
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())
}

/// winit application state for the desktop host.
struct WindowedApp {
    /// Backend requested on the command line.
    backend: BackendKind,
    /// The created window, if `resumed` has run.
    window: Option<Window>,
    /// The host, created once the window handle exists.
    host: Option<Host>,
    /// Timestamp of the previous frame (for `dt`).
    last_frame: Instant,
    /// Timestamp at which the next frame is due.
    next_frame: Instant,
}

impl WindowedApp {
    /// Creates an app that has not yet received `resumed`.
    fn new(backend: BackendKind) -> Self {
        let now = Instant::now();
        Self {
            backend,
            window: None,
            host: None,
            last_frame: now,
            next_frame: now,
        }
    }
}

impl ApplicationHandler for WindowedApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("krkr-rs")
            .with_inner_size(LogicalSize::new(
                f64::from(DEFAULT_SIZE.0),
                f64::from(DEFAULT_SIZE.1),
            ));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("krkr: failed to create window: {error}");
                event_loop.exit();
                return;
            }
        };

        let handle = window.window_handle().ok().map(|handle| handle.as_raw());
        let config = HostConfig {
            backend: self.backend,
            surface_size: DEFAULT_SIZE,
            window_handle: handle,
            ..HostConfig::default()
        };
        match Host::new(config) {
            Ok(host) => {
                self.host = Some(host);
            }
            Err(error) => {
                eprintln!("krkr: failed to create host: {error}");
                event_loop.exit();
                return;
            }
        }

        self.window = Some(window);
        self.next_frame = Instant::now();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                let size = (size.width.max(1), size.height.max(1));
                if let Some(host) = self.host.as_mut()
                    && let Err(error) = host.resize(size)
                {
                    eprintln!("krkr: resize failed: {error}");
                }
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt_ms = now.saturating_duration_since(self.last_frame).as_secs_f32() * 1000.0;
                self.last_frame = now;
                if let Some(host) = self.host.as_mut()
                    && let Err(error) = host.step(dt_ms)
                {
                    eprintln!("krkr: frame failed: {error}");
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame {
            self.next_frame = now + FRAME_INTERVAL;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}
