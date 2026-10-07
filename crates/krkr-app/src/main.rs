//! `krkr` CLI: headless frame runner and windowed desktop frontend.
//!
//! ```text
//! krkr --headless [--frames N] [--backend null|vulkan]
//! krkr [--backend null|vulkan]
//! ```
//!
//! * `--headless` runs an empty scene for `N` frames (default 120) through
//!   [`krkr_app::Host`] and prints the null backend [`FrameStats`]. It needs
//!   neither a display nor a GPU.
//! * Without `--headless` the process opens a window through the L1
//!   [`krkr_platform`] event loop and drives [`krkr_app::Host::step`] at
//!   roughly 60 fps. This binary is the composition root: it wires the L1
//!   platform to the L2 engine host and contains no platform logic.
//! * `--backend vulkan` requests the Vulkan pipeline. It needs a real
//!   window with a display connection (Wayland/X11/XCB); the windowed host
//!   passes both the window and display handle so a surface can be created.
//!   The headless path has no display and uses `--backend null`.
//! * `-h`/`--help` prints the usage text.

use std::process::ExitCode;

use krkr_app::{BackendKind, FrameStats, Host, HostConfig, StepReport};
use krkr_platform::{EventHandler, Flow, PlatformConfig, PlatformEvent, PlatformWindow};

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

/// Opens a window through the L1 platform and drives [`Host::step`].
fn run_windowed(cli: &Cli) -> Result<(), String> {
    let config = PlatformConfig {
        title: "krkr-rs".to_owned(),
        size: DEFAULT_SIZE,
        ..PlatformConfig::default()
    };
    krkr_platform::run(config, DesktopApp::new(cli.backend)).map_err(|error| error.to_string())
}

/// The composition root's bridge from L1 platform events to the L2 host.
///
/// Field order matters for teardown: the Vulkan swapchain/surface must be
/// destroyed while the window's display connection is still alive. The
/// platform owns the window and drops it after the handler (and therefore
/// `host`) is dropped, so `host` is declared here without owning the window.
struct DesktopApp {
    /// Backend requested on the command line.
    backend: BackendKind,
    /// The host, created once the window handle exists.
    host: Option<Host>,
}

impl DesktopApp {
    /// Creates a handler that has not yet received `resumed`.
    fn new(backend: BackendKind) -> Self {
        Self {
            backend,
            host: None,
        }
    }
}

impl EventHandler for DesktopApp {
    fn resumed(&mut self, window: &PlatformWindow) -> Flow {
        let config = HostConfig {
            backend: self.backend,
            surface_size: DEFAULT_SIZE,
            window_handle: window.raw_window_handle(),
            display_handle: window.raw_display_handle(),
            ..HostConfig::default()
        };
        match Host::new(config) {
            Ok(host) => {
                self.host = Some(host);
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
                if let Some(host) = self.host.as_mut()
                    && let Err(error) = host.resize((width, height))
                {
                    eprintln!("krkr: resize failed: {error}");
                    return Flow::Exit;
                }
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    fn frame(&mut self, dt_ms: f32) -> Flow {
        if let Some(host) = self.host.as_mut()
            && let Err(error) = host.step(dt_ms)
        {
            eprintln!("krkr: frame failed: {error}");
            return Flow::Exit;
        }
        Flow::Continue
    }
}
