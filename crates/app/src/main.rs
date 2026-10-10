//! `krkr` CLI: headless frame runner and windowed desktop frontend.
//!
//! ```text
//! krkr [--headless] [--frames N] [--backend null|vulkan] [--archive XP3 ...] [--base-dir DIR] [GAME]
//! ```
//!
//! * `--headless` runs an empty scene for `N` frames (default 120) through
//!   [`app::Host`] and prints the null backend [`FrameStats`]. It needs
//!   neither a display nor a GPU.
//! * Without `--headless` the process opens a window through the L1
//!   [`platform`] event loop and drives [`app::Host::step`] at
//!   roughly 60 fps. This binary is the composition root: it wires the L1
//!   platform to the L2 engine host and contains no platform logic.
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
//! * `-h`/`--help` prints the usage text.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use app::{BackendKind, FrameStats, GameHost, Host, HostConfig, StepReport};
use platform::{EventHandler, Flow, PlatformConfig, PlatformEvent, PlatformWindow};

/// Usage text printed by `-h`/`--help` and on a parse error.
const USAGE: &str = "\
krkr-rs host

USAGE:
    krkr [--headless] [--frames N] [--backend null|vulkan] [--archive XP3 ...] [--base-dir DIR] [GAME]

OPTIONS:
    --headless             Run offscreen (no window/GPU).
    --frames N             Headless frames to run (default 120).
    --backend null|vulkan  Render backend (default: null).
    --archive XP3          Mount an XP3 archive (repeatable, later wins).
    --base-dir DIR         App path (default: first archive's parent).
    GAME                   Game directory (loose startup.tjs) or an XP3 file.
    -h, --help             Print this help.
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

impl Cli {
    /// Parses `args` (without the program name).
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut cli = Self {
            headless: false,
            frames: DEFAULT_FRAMES,
            backend: BackendKind::Null,
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
                "--archive" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--archive requires a path".to_owned())?;
                    cli.archives.push(PathBuf::from(value));
                }
                "--base-dir" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--base-dir requires a directory".to_owned())?;
                    cli.base_dir = Some(PathBuf::from(value));
                }
                "-h" | "--help" => cli.help = true,
                other if !other.starts_with('-') => {
                    if cli.positional.is_some() {
                        return Err(format!("unexpected extra argument: {other}"));
                    }
                    cli.positional = Some(PathBuf::from(other));
                }
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

/// Runs offscreen and prints the final frame stats (or the game's scene size).
fn run_headless(cli: &Cli) -> Result<(), String> {
    let input = resolve_game(cli)?;
    let config = HostConfig::headless().with_backend(cli.backend);
    let mut host = Host::new(config).map_err(|error| error.to_string())?;

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
            last = game
                .step(1000.0 / 60.0, &[])
                .map_err(|error| error.to_string())?;
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
        last = host
            .step(1000.0 / 60.0)
            .map_err(|error| error.to_string())?;
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
fn run_windowed(cli: &Cli) -> Result<(), String> {
    let mut config = PlatformConfig::default();
    config.title = "krkr-rs".to_owned();
    config.size = DEFAULT_SIZE;
    platform::run(config, DesktopApp::new(cli.backend, resolve_game(cli)?))
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
    /// The game to load, or `None` for the empty scene.
    game: Option<GameInput>,
    /// The running host, created once the window handle exists.
    running: Option<Running>,
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

    /// Advances and presents one frame.
    fn step(&mut self, dt_ms: f32) -> Result<(), String> {
        match self {
            Running::Empty(host) => host.step(dt_ms).map(|_| ()).map_err(|e| e.to_string()),
            Running::Game(game) => {
                game.step(dt_ms, &[]).map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }
}

impl DesktopApp {
    /// Creates a handler that has not yet received `resumed`.
    fn new(backend: BackendKind, game: Option<GameInput>) -> Self {
        Self {
            backend,
            game,
            running: None,
        }
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
            PlatformEvent::CloseRequested | PlatformEvent::Focused(_) | PlatformEvent::Input(_) => {
                Flow::Continue
            }
        }
    }

    fn frame(&mut self, dt_ms: f32) -> Flow {
        if let Some(running) = self.running.as_mut()
            && let Err(error) = running.step(dt_ms)
        {
            eprintln!("krkr: frame failed: {error}");
            return Flow::Exit;
        }
        Flow::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        Cli::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parse_defaults_to_the_empty_scene() {
        let cli = parse(&["--headless"]).expect("parse");
        assert!(cli.headless);
        assert!(cli.archives.is_empty());
        assert!(cli.base_dir.is_none());
        assert!(cli.positional.is_none());
        assert_eq!(cli.frames, DEFAULT_FRAMES);
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
