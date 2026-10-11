//! `krkr` CLI: headless frame runner and windowed desktop frontend.
//!
//! ```text
//! krkr [--headless] [--frames N] [--fps N] [--backend null|vulkan] [--scaling game|fit|fill|expand] [--archive XP3 ...] [--base-dir DIR] [GAME]
//! ```
//!
//! * `--headless` runs an empty scene for `N` frames (default 120) through
//!   [`app::Host`] and prints the null backend [`FrameStats`]. It needs
//!   neither a display nor a GPU.
//! * Without `--headless` the process opens a window through the L1
//!   [`platform`] event loop, drives [`app::Host::step`] on the configured
//!   cadence. This binary is the composition root: it wires the L1
//!   platform to the L2 engine host and contains no platform logic.
//! * `--fps N` caps the frame rate (`N` in `1..=1000`, default 60). It paces
//!   both the windowed event loop and the headless step.
//! * `--archive XP3` (repeatable, order-significant, later wins) mounts the
//!   explicit game archives and runs the L2 startup sequence through the L4
//!   VM host. One positional path is also accepted, mirroring the reference
//!   `TVPCheckStartupArg`: a directory becomes the app path (its loose
//!   `startup.tjs` decides bootability), a file is mounted as an archive.
//!   `--base-dir DIR` sets the app path; it defaults to the first archive's
//!   parent and every relative archive path resolves against it. **No folder
//!   is ever scanned** for `*.xp3` (the reference has no such scan).
//! * `--backend vulkan` requests the Vulkan pipeline. It needs a real
//!   window with a display connection (Wayland/X11/XCB); the windowed host
//!   passes both the window and display handle so a surface can be created.
//!   The headless path has no display and uses `--backend null`.
//! * `--scaling` selects how the engine presents the logical scene inside
//!   the framebuffer (`game` by default, plus `fit`, `fill` and `expand`).
//! * Configuration has the precedence **launch argument > platform-provided
//!   configuration > built-in default**. The platform supplies its own
//!   [`api::config::ConfigSource`] through [`platform::config_source`], so
//!   this composition root never chooses a configuration file or branches on
//!   the target OS. An out-of-range or unparseable value is a typed error;
//!   nothing is clamped or defaulted, and nothing takes effect until the
//!   whole configuration validates.
//! * `-h`/`--help` prints the usage text.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use api::config::{ConfigError, ConfigSource, FrameRateLimit, PresentationScaling, RuntimeConfig};
use app::{BackendKind, FrameStats, GameHost, Host, HostConfig, StepReport, map_input_to_logical};
use engine::projection::ScalingMode;
use platform::{EventHandler, Flow, PlatformConfig, PlatformEvent, PlatformWindow};
use render::ViewTransform;

/// One documented launch option.
struct OptionDoc {
    /// The flag, as written on the command line.
    flag: &'static str,
    /// The value placeholder, or an empty string for a flag with no value.
    value: &'static str,
    /// Description including the default and accepted range.
    help: &'static str,
}

/// Every launch option.
///
/// [`usage`] prints this table and the `--help` completeness test walks it, so
/// an option cannot be added without help text.
const OPTIONS: &[OptionDoc] = &[
    OptionDoc {
        flag: "--headless",
        value: "",
        help: "Run offscreen (no window/GPU).",
    },
    OptionDoc {
        flag: "--frames",
        value: "N",
        help: "Headless frames to run (default: 120).",
    },
    OptionDoc {
        flag: "--fps",
        value: "N",
        help: "Frame-rate limit in frames per second (default: 60, range: 1..=1000).",
    },
    OptionDoc {
        flag: "--backend",
        value: "null|vulkan",
        help: "Render backend (default: null).",
    },
    OptionDoc {
        flag: "--scaling",
        value: "MODE",
        help: "Presentation scaling (default: game, one of: game, fit, fill, expand).",
    },
    OptionDoc {
        flag: "--archive",
        value: "XP3",
        help: "Mount an XP3 archive (repeatable, later wins); with none, every *.xp3 under the base directory mounts.",
    },
    OptionDoc {
        flag: "--base-dir",
        value: "DIR",
        help: "App path (default: first archive's parent).",
    },
    OptionDoc {
        flag: "-h, --help",
        value: "",
        help: "Print this help.",
    },
];

/// Returns the usage text printed by `-h`/`--help` and on a parse error.
fn usage() -> String {
    let mut out = String::from("krkr-rs host\n\nUSAGE:\n    krkr [OPTIONS] [GAME]\n\nOPTIONS:\n");
    for option in OPTIONS {
        let left = if option.value.is_empty() {
            option.flag.to_owned()
        } else {
            format!("{} {}", option.flag, option.value)
        };
        out.push_str(&format!("    {left:<24}{}\n", option.help));
    }
    out.push_str(&format!(
        "    {:<24}{}\n",
        "GAME", "Game directory (recursively scanned for *.xp3) or an XP3 file."
    ));
    out
}

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
    /// Frame-rate limit from `--fps`, or `None` when the argument was absent.
    frame_rate: Option<FrameRateLimit>,
    /// Presentation scaling from `--scaling`, or `None` when absent.
    scaling: Option<PresentationScaling>,
    /// Explicit `--archive` list, in mount order (later wins).
    archives: Vec<PathBuf>,
    /// App path override (`--base-dir`); defaults to the current directory.
    base_dir: Option<PathBuf>,
    /// The first positional argument (reference `argv[1]`): a game directory
    /// or an archive file.
    positional: Option<PathBuf>,
    /// Whether `-h`/`--help` was requested.
    help: bool,
}

/// A command-line parse or resolution failure.
#[derive(Debug, thiserror::Error)]
enum CliError {
    /// The command line is structurally malformed.
    #[error("{0}")]
    Usage(String),
    /// A value failed validation.
    #[error(transparent)]
    Config(#[from] ConfigError),
}

impl Cli {
    /// Parses `args` (without the program name).
    ///
    /// Every value is parsed and validated here, so a malformed command line
    /// has no partial effect.
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, CliError> {
        let mut cli = Self {
            headless: false,
            frames: DEFAULT_FRAMES,
            backend: BackendKind::Null,
            frame_rate: None,
            scaling: None,
            archives: Vec::new(),
            base_dir: None,
            positional: None,
            help: false,
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--headless" => cli.headless = true,
                "--frames" => {
                    let value = args
                        .next()
                        .ok_or_else(|| CliError::Usage("--frames requires a value".to_owned()))?;
                    cli.frames = value.parse().map_err(|_| {
                        ConfigError::invalid("--frames", value.clone(), "a non-negative integer")
                    })?;
                }
                "--fps" => {
                    let value = args
                        .next()
                        .ok_or_else(|| CliError::Usage("--fps requires a value".to_owned()))?;
                    cli.frame_rate = Some(FrameRateLimit::parse("--fps", &value)?);
                }
                "--backend" => {
                    let value = args
                        .next()
                        .ok_or_else(|| CliError::Usage("--backend requires a value".to_owned()))?;
                    cli.backend = parse_backend(&value).map_err(CliError::Usage)?;
                }
                "--scaling" => {
                    let value = args
                        .next()
                        .ok_or_else(|| CliError::Usage("--scaling requires a value".to_owned()))?;
                    cli.scaling = Some(PresentationScaling::parse("--scaling", &value)?);
                }
                "--archive" => {
                    let value = args
                        .next()
                        .ok_or_else(|| CliError::Usage("--archive requires a path".to_owned()))?;
                    cli.archives.push(PathBuf::from(value));
                }
                "--base-dir" => {
                    let value = args.next().ok_or_else(|| {
                        CliError::Usage("--base-dir requires a directory".to_owned())
                    })?;
                    cli.base_dir = Some(PathBuf::from(value));
                }
                "-h" | "--help" => cli.help = true,
                other if !other.starts_with('-') => {
                    if cli.positional.is_some() {
                        return Err(CliError::Usage(format!(
                            "unexpected extra argument: {other}"
                        )));
                    }
                    cli.positional = Some(PathBuf::from(other));
                }
                other => return Err(CliError::Usage(format!("unknown argument: {other}"))),
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

/// Maps a resolved presentation scaling to the engine's projection mode.
fn map_scaling(scaling: PresentationScaling) -> ScalingMode {
    match scaling {
        PresentationScaling::Game => ScalingMode::Game,
        PresentationScaling::Fit => ScalingMode::Fit,
        PresentationScaling::Fill => ScalingMode::Fill,
        PresentationScaling::Expand => ScalingMode::Expand,
    }
}

/// Resolves the runtime configuration with the precedence
/// **launch argument > platform configuration > built-in default**.
///
/// The platform's overrides are loaded and validated before any value is
/// used; the launch argument then wins field by field, and a field neither
/// source holds falls back to [`RuntimeConfig::default`].
fn resolve_runtime_config(
    cli: &Cli,
    source: &impl ConfigSource,
) -> Result<RuntimeConfig, ConfigError> {
    let platform = source.load()?;
    let defaults = RuntimeConfig::default();
    Ok(RuntimeConfig {
        frame_rate_limit: cli
            .frame_rate
            .or(platform.frame_rate_limit)
            .unwrap_or(defaults.frame_rate_limit),
        scaling: cli.scaling.or(platform.scaling).unwrap_or(defaults.scaling),
    })
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

/// Initializes the process logger.
///
/// The `log` crate is a facade; this is the one place the composition root
/// installs a backend. `RUST_LOG` selects the filter (default `info`), so a
/// plain run is quiet but `RUST_LOG=debug krkr …` surfaces the VM, natives and
/// host diagnostics on stderr. Initializing twice is a no-op.
fn init_logger() {
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    builder.format_timestamp_millis();
    let _ = builder.try_init();
}

fn main() -> ExitCode {
    init_logger();
    let cli = match Cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("krkr: {message}");
            eprint!("{}", usage());
            return ExitCode::from(2);
        }
    };
    if cli.help {
        print!("{}", usage());
        return ExitCode::SUCCESS;
    }

    let source = platform::config_source();
    let runtime = match resolve_runtime_config(&cli, &source) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("krkr: {error}");
            return ExitCode::from(2);
        }
    };

    let result = if cli.headless {
        run_headless(&cli, &runtime)
    } else {
        run_windowed(&cli, &runtime)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("krkr: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Runs offscreen and prints the final frame stats (or the game's scene size).
fn run_headless(cli: &Cli, runtime: &RuntimeConfig) -> Result<(), String> {
    let input = resolve_game(cli)?;
    let config = HostConfig::headless().with_backend(cli.backend);
    let mut host = Host::new(config).map_err(|error| error.to_string())?;
    let dt_ms = runtime.frame_rate_limit.period_ms();

    if let Some(input) = input {
        let mut game = GameHost::open(host, input.archives.clone(), input.base_dir.clone())
            .map_err(|error| error.to_string())?;
        println!(
            "krkr headless game: backend={} frames={} archives={}",
            game.host().backend().name(),
            cli.frames,
            input.archives.len(),
        );
        let mut last = StepReport::default();
        for _ in 0..cli.frames {
            last = game.step(dt_ms, &[]).map_err(|error| error.to_string())?;
        }
        if let Some(stats) = last.stats {
            println!(
                "draws: frame={} {} textures={}",
                last.frame_index,
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
        return Ok(());
    }

    println!(
        "krkr headless: backend={} frames={} size={}x{}",
        host.backend().name(),
        cli.frames,
        host.surface_size().0,
        host.surface_size().1,
    );

    let mut last = StepReport::default();
    for _ in 0..cli.frames {
        last = host.step(dt_ms).map_err(|error| error.to_string())?;
    }
    if let Some(stats) = last.stats {
        println!("final: frame={} {}", last.frame_index, format_stats(&stats));
    }
    Ok(())
}

/// A game resolved from the command line: the app path plus the ordered
/// archive list.
#[derive(Clone, Debug)]
struct GameInput {
    /// App path for loose files and the base for relative archive paths.
    base_dir: PathBuf,
    /// Archives to mount, in order (later wins).
    archives: Vec<PathBuf>,
}

/// Resolves the command line into the game to load, or `None` when no game
/// input was given (the empty-scene run).
///
/// The reference discovers the game from its first command-line argument
/// (`TVPCheckStartupArg`): a directory is the app path whose loose
/// `startup.tjs` decides bootability, a file is opened as an archive. There is
/// no default folder scan for `*.xp3`. `--archive` stays authoritative and is
/// mounted in addition to a positional archive.
///
/// The app path is `--base-dir` (or a positional directory) when given;
/// otherwise it is the first archive's parent (the reference `TVPGetAppPath`
/// convention), falling back to the working directory. Relative archive paths
/// resolve against the app path, so it is the single root for archives and
/// loose files.
fn resolve_game(cli: &Cli) -> Result<Option<GameInput>, String> {
    let mut raw_archives = cli.archives.clone();
    let mut positional_dir = None;
    if let Some(path) = &cli.positional {
        let meta = std::fs::metadata(path)
            .map_err(|error| format!("cannot access {}: {error}", path.display()))?;
        if meta.is_dir() {
            positional_dir = Some(path.clone());
        } else {
            raw_archives.push(path.clone());
        }
    }

    if cli.base_dir.is_some() && positional_dir.is_some() {
        return Err("--base-dir and a game directory argument cannot both be given".to_owned());
    }

    let explicit_base = match cli.base_dir.as_ref().or(positional_dir.as_ref()) {
        Some(dir) => Some(absolute(dir)?),
        None => None,
    };

    if raw_archives.is_empty() && explicit_base.is_none() {
        return Ok(None);
    }

    let (base_dir, archives) = match explicit_base {
        Some(base_dir) => {
            let archives = raw_archives
                .into_iter()
                .map(|archive| resolve_against(&base_dir, archive))
                .collect();
            (base_dir, archives)
        }
        None => {
            let cwd = current_dir()?;
            let archives: Vec<PathBuf> = raw_archives
                .into_iter()
                .map(|archive| resolve_against(&cwd, archive))
                .collect();
            let base_dir = archives
                .first()
                .and_then(|archive| archive.parent())
                .map(Path::to_path_buf)
                .unwrap_or(cwd);
            (base_dir, archives)
        }
    };
    Ok(Some(GameInput { base_dir, archives }))
}

/// Resolves `archive` against `root`; absolute paths are used as given.
fn resolve_against(root: &Path, archive: PathBuf) -> PathBuf {
    if archive.is_absolute() {
        archive
    } else {
        root.join(archive)
    }
}

/// The current working directory as a typed error.
fn current_dir() -> Result<PathBuf, String> {
    std::env::current_dir()
        .map_err(|error| format!("cannot determine the current directory: {error}"))
}

/// Makes `path` absolute against the current directory. Existence is not
/// checked here; a missing base directory is reported when the storage mounts.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        current_dir().map(|cwd| cwd.join(path))
    }
}

/// Opens a window through the L1 platform and drives [`Host::step`].
fn run_windowed(cli: &Cli, runtime: &RuntimeConfig) -> Result<(), String> {
    let mut config = PlatformConfig::default();
    config.title = "krkr-rs".to_owned();
    config.size = DEFAULT_SIZE;
    config.frame_interval = Duration::from_secs_f64(runtime.frame_rate_limit.period_secs());
    platform::run(
        config,
        DesktopApp::new(
            cli.backend,
            map_scaling(runtime.scaling),
            resolve_game(cli)?,
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
    fn step(&mut self, dt_ms: f32, events: &[PlatformEvent]) -> Result<(), String> {
        match self {
            Running::Empty(host) => host.step(dt_ms).map(|_| ()).map_err(|e| e.to_string()),
            Running::Game(game) => {
                game.step(dt_ms, events).map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }
}

impl DesktopApp {
    /// Creates a handler that has not yet received `resumed`.
    fn new(backend: BackendKind, scaling: ScalingMode, game: Option<GameInput>) -> Self {
        Self {
            backend,
            scaling,
            game,
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
        let mut config = HostConfig::default();
        config.backend = self.backend;
        // Create the host at the window's real physical size rather than at
        // the requested logical default, so the initial swapchain matches.
        config.surface_size = window.size();
        config.window_handle = Some(window_handle);
        config.display_handle = Some(display_handle);
        config.scaling = self.scaling;
        match Host::new(config) {
            Ok(host) => {
                let running = match &self.game {
                    Some(input) => {
                        match GameHost::open(host, input.archives.clone(), input.base_dir.clone()) {
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

    fn frame(&mut self, dt_ms: f32, window: &PlatformWindow) -> Flow {
        if let Some(running) = self.running.as_mut() {
            let events = std::mem::take(&mut self.pending_events);
            if let Err(error) = running.step(dt_ms, &events) {
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
    use api::config::{NoConfigSource, RuntimeConfigOverrides};

    fn parse(args: &[&str]) -> Result<Cli, CliError> {
        Cli::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    /// A platform configuration source with fixed overrides.
    struct FakeSource(RuntimeConfigOverrides);

    impl ConfigSource for FakeSource {
        fn load(&self) -> Result<RuntimeConfigOverrides, ConfigError> {
            Ok(self.0)
        }
    }

    fn overrides(fps: u32, scaling: PresentationScaling) -> FakeSource {
        FakeSource(RuntimeConfigOverrides {
            frame_rate_limit: Some(FrameRateLimit::new("frame_rate", fps).expect("valid")),
            scaling: Some(scaling),
        })
    }

    #[test]
    fn parse_defaults_to_the_empty_scene() {
        let cli = parse(&["--headless"]).expect("parse");
        assert!(cli.headless);
        assert!(cli.archives.is_empty());
        assert!(cli.base_dir.is_none());
        assert!(cli.positional.is_none());
        assert_eq!(cli.frames, DEFAULT_FRAMES);
        assert_eq!(cli.frame_rate, None);
        assert_eq!(cli.scaling, None);
    }

    #[test]
    fn parse_frame_rate_and_scaling() {
        let cli = parse(&["--fps", "30", "--scaling", "fit"]).expect("parse");
        assert_eq!(
            cli.frame_rate,
            Some(FrameRateLimit::new("--fps", 30).expect("valid"))
        );
        assert_eq!(cli.scaling, Some(PresentationScaling::Fit));
    }

    #[test]
    fn parse_rejects_every_invalid_configuration_value() {
        for args in [
            &["--fps"][..],
            &["--fps", "0"],
            &["--fps", "1001"],
            &["--fps", "fast"],
            &["--scaling"],
            &["--scaling", "stretch"],
        ] {
            assert!(parse(args).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn invalid_value_error_names_the_option_and_the_range() {
        let error = parse(&["--fps", "0"]).expect_err("must fail").to_string();
        assert!(error.contains("--fps"), "{error}");
        assert!(error.contains("1..=1000"), "{error}");
        let error = parse(&["--scaling", "stretch"])
            .expect_err("must fail")
            .to_string();
        assert!(error.contains("--scaling"), "{error}");
        assert!(error.contains("game, fit, fill, expand"), "{error}");
    }

    #[test]
    fn resolution_prefers_argument_then_platform_then_default() {
        let platform = overrides(30, PresentationScaling::Fit);

        // The platform beats the built-in default.
        let cli = parse(&["--headless"]).expect("parse");
        let config = resolve_runtime_config(&cli, &platform).expect("resolve");
        assert_eq!(config.frame_rate_limit.fps(), 30);
        assert_eq!(config.scaling, PresentationScaling::Fit);

        // The launch argument beats the platform, field by field.
        let cli = parse(&["--headless", "--fps", "120", "--scaling", "fill"]).expect("parse");
        let config = resolve_runtime_config(&cli, &platform).expect("resolve");
        assert_eq!(config.frame_rate_limit.fps(), 120);
        assert_eq!(config.scaling, PresentationScaling::Fill);

        // An argument one field overrides only that field.
        let cli = parse(&["--headless", "--fps", "120"]).expect("parse");
        let config = resolve_runtime_config(&cli, &platform).expect("resolve");
        assert_eq!(config.frame_rate_limit.fps(), 120);
        assert_eq!(config.scaling, PresentationScaling::Fit);
    }

    #[test]
    fn resolution_falls_back_to_the_built_in_default() {
        let cli = parse(&["--headless"]).expect("parse");
        let config = resolve_runtime_config(&cli, &NoConfigSource).expect("resolve");
        assert_eq!(config, RuntimeConfig::default());
        assert_eq!(config.frame_rate_limit.fps(), 60);
        assert_eq!(config.scaling, PresentationScaling::Game);
    }

    /// A platform source whose store exists but cannot be read.
    struct FailingSource;

    impl ConfigSource for FailingSource {
        fn load(&self) -> Result<RuntimeConfigOverrides, ConfigError> {
            Err(ConfigError::Source {
                message: "boom".to_owned(),
            })
        }
    }

    #[test]
    fn resolution_validates_the_platform_source_before_any_argument() {
        let cli = parse(&["--headless", "--fps", "120", "--scaling", "fill"]).expect("parse");
        let error = resolve_runtime_config(&cli, &FailingSource).expect_err("must fail");
        assert!(error.to_string().contains("boom"), "{error}");
    }

    #[test]
    fn help_lists_every_option_with_default_and_range() {
        let text = usage();
        for option in OPTIONS {
            assert!(
                text.contains(option.flag),
                "help is missing {}",
                option.flag
            );
        }
        assert!(text.contains("GAME"), "{text}");
        assert!(text.contains("default: 60"), "{text}");
        assert!(text.contains("1..=1000"), "{text}");
        assert!(text.contains("default: game"), "{text}");
        assert!(text.contains("game, fit, fill, expand"), "{text}");
        assert!(text.contains("default: 120"), "{text}");
    }

    #[test]
    fn parse_archive_and_base_dir() {
        let cli = parse(&["--archive", "data.xp3", "--base-dir", "/game"]).expect("parse");
        assert_eq!(cli.archives, vec![PathBuf::from("data.xp3")]);
        assert_eq!(cli.base_dir, Some(PathBuf::from("/game")));
    }

    #[test]
    fn parse_positional_game() {
        let cli = parse(&["/path/to/game"]).expect("parse");
        assert_eq!(cli.positional, Some(PathBuf::from("/path/to/game")));
    }

    #[test]
    fn parse_rejects_a_second_positional() {
        assert!(parse(&["game", "extra"]).is_err());
    }

    #[test]
    fn parse_rejects_unknown_flag() {
        assert!(parse(&["--nope"]).is_err());
    }

    #[test]
    fn resolve_is_none_without_game_input() {
        assert!(
            resolve_game(&parse(&["--headless"]).expect("parse"))
                .expect("resolve")
                .is_none()
        );
    }

    #[test]
    fn resolve_base_dir_is_absolute() {
        let input = resolve_game(&parse(&["--base-dir", "game"]).expect("parse"))
            .expect("resolve")
            .expect("game input");
        assert!(input.base_dir.is_absolute());
        assert!(input.base_dir.ends_with("game"));
        assert!(input.archives.is_empty());
    }

    #[test]
    fn resolve_archive_defaults_base_dir_to_its_parent() {
        let cwd = std::env::current_dir().expect("cwd");
        let input = resolve_game(&parse(&["--archive", "data.xp3"]).expect("parse"))
            .expect("resolve")
            .expect("game input");
        assert_eq!(input.base_dir, cwd);
        assert_eq!(input.archives, vec![cwd.join("data.xp3")]);
    }

    /// `--archive sub/data.xp3` must not re-join `sub` onto the app path it
    /// derived from that same archive.
    #[test]
    fn resolve_archive_with_a_directory_component_uses_its_parent() {
        let cwd = std::env::current_dir().expect("cwd");
        let input = resolve_game(&parse(&["--archive", "sub/data.xp3"]).expect("parse"))
            .expect("resolve")
            .expect("game input");
        assert_eq!(input.base_dir, cwd.join("sub"));
        assert_eq!(input.archives, vec![cwd.join("sub/data.xp3")]);
    }

    #[test]
    fn resolve_base_dir_roots_relative_archives() {
        let input =
            resolve_game(&parse(&["--base-dir", "/game", "--archive", "data.xp3"]).expect("parse"))
                .expect("resolve")
                .expect("game input");
        assert_eq!(input.base_dir, PathBuf::from("/game"));
        assert_eq!(input.archives, vec![PathBuf::from("/game/data.xp3")]);
    }

    #[test]
    fn resolve_positional_directory_becomes_the_base_dir() {
        let dir = std::env::temp_dir().join(format!("krkr-cli-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let input = resolve_game(&parse(&[dir.to_str().expect("utf-8 path")]).expect("parse"))
            .expect("resolve")
            .expect("game input");
        assert_eq!(input.base_dir, dir);
        assert!(input.archives.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_positional_file_becomes_an_archive() {
        let path = std::env::temp_dir().join(format!("krkr-cli-{}.xp3", std::process::id()));
        std::fs::write(&path, b"not an xp3 yet").expect("write temp file");
        let input = resolve_game(&parse(&[path.to_str().expect("utf-8 path")]).expect("parse"))
            .expect("resolve")
            .expect("game input");
        assert_eq!(input.archives, vec![path.clone()]);
        assert!(input.base_dir.is_absolute());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn resolve_rejects_base_dir_with_a_directory_argument() {
        let dir = std::env::temp_dir().join(format!("krkr-cli-both-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let args = [
            "--base-dir",
            dir.to_str().expect("utf-8 path"),
            dir.to_str().expect("utf-8 path"),
        ];
        assert!(resolve_game(&parse(&args).expect("parse")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
