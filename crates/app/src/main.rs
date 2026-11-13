//! `krkr` CLI: headless frame runner and windowed desktop frontend.
//!
//! ```text
//! krkr [--headless] [--frames N] [--fps N] [--backend null|vulkan] [--preload-archives] [--graphic-cache-limit LIMIT] [--slow-report-ms N] [--vm-report] [--archive XP3 ...] [--base-dir DIR] [GAME]
//! ```
//!
//! * `--headless` runs an empty scene for `N` frames (default 120) through
//!   [`app::Host`] and prints the null backend [`render::FrameStats`]. It needs
//!   neither a display nor a GPU.
//! * Without `--headless` the process opens a window through the L1
//!   [`platform`] event loop and drives [`app::Host::step`] on the configured
//!   cadence. This binary is the composition root: it wires the L1
//!   platform to the L2 engine host and contains no platform logic.
//! * `--fps N` caps the frame rate (`N` in `1..=1000`, default 60). It paces
//!   both the windowed event loop and the headless step.
//! * `--archive XP3` (repeatable, order-significant, later wins) mounts the
//!   explicit game archives and runs the L2 startup sequence through the L4
//!   VM host. One positional path is also accepted: a directory becomes the app
//!   path (its loose `startup.tjs` decides bootability), a file is mounted as
//!   an archive.
//!   `--base-dir DIR` sets the app path; it defaults to the first archive's
//!   parent and every relative archive path resolves against it. With no
//!   `--archive` and a directory app path, every `*.xp3` under it mounts
//!   recursively; an explicit archive list suppresses that, and no directory
//!   is scanned for it.
//! * `--preload-archives` reads each mounted archive into memory in the
//!   background, trading the archive's size in resident memory for never
//!   reading it back from storage. Off by default; entries return the same
//!   bytes either way, and the mount does not wait for the bytes.
//! * `--debug-overlay` draws frame rate and its 1% low, CPU usage, disk I/O,
//!   resident memory and live object counts in the top-left corner. It is a
//!   render-only node (never a script-visible layer or bitmap) and off by
//!   default.
//! * `--graphic-cache-limit LIMIT` sets the decoded-image cache policy:
//!   `none` disables it, `0`/`unlimited` keeps every decoded image, and a
//!   positive integer is a byte cap. Without the option the cache stays
//!   disabled, and a startup script may still change it through
//!   `System.graphicCacheLimit`.
//! * `--dump-frame PATH` writes the presented frame to `PATH` as a PNG. The
//!   frame is the one the CPU rasterizer composited, so the file holds the
//!   picture the window shows; it is written again whenever a frame is
//!   presented, and the file therefore holds the most recent one. The frame
//!   comes from the null backend, so the option needs `--backend null`.
//! * `--backend vulkan` requests the Vulkan pipeline. It needs a real
//!   window with a display connection (Wayland/X11/XCB); the windowed host
//!   passes both the window and display handle so a surface can be created.
//!   The headless path has no display and uses `--backend null`.
//! * `--slow-report-ms N` bounds the wall-clock time the run may spend
//!   reporting slow frames. The option is absent by default, which leaves the
//!   reports unbounded, and `0` reports no slow frame at all. The budget is
//!   spent by the time the reports take: a run that has spent it stops
//!   reporting and says so once.
//! * `--vm-report` switches the VM's own measurement of the script
//!   compilation on and prints the analysis at the end of the run: the
//!   number of compiles, the size of the scripts they read, and the time the
//!   compiler's phases took. The measurement is off by default, because a
//!   clock read per token costs tens of milliseconds.
//! * Configuration is resolved **by this launcher** and handed to
//!   [`app::run`] as an already-loaded value: the `key=value` file named by
//!   the `KRKR_CONFIG` environment variable supplies overrides, the launch
//!   argument beats them field by field, and the built-in default fills the
//!   rest. The emulator core performs no configuration I/O, so another
//!   platform's launcher can load its own store and call the same entry
//!   point. An out-of-range or unparseable value is a typed error; nothing
//!   is clamped or defaulted, and nothing takes effect until the whole
//!   configuration validates.
//! * `-h`/`--help` prints the usage text.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use api::config::{
    CacheLimit, ConfigError, FrameRateLimit, RuntimeConfig, RuntimeConfigOverrides, parse_overrides,
};
use app::{BackendKind, GameInput, Launch, RunMode};

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
        flag: "--preload-archives",
        value: "",
        help: "Read every mounted archive into memory at mount, trading its size in resident memory for per-entry reads.",
    },
    OptionDoc {
        flag: "--debug-overlay",
        value: "",
        help: "Draw FPS/1% low, CPU, disk I/O, memory and object counts in the top-left corner.",
    },
    OptionDoc {
        flag: "--graphic-cache-limit",
        value: "LIMIT",
        help: "Decoded-image cache: none, 0/unlimited, or bytes (default: reference default).",
    },
    OptionDoc {
        flag: "--dump-frame",
        value: "PATH",
        help: "Write the presented frame to PATH as a PNG (the most recent frame; the null backend).",
    },
    OptionDoc {
        flag: "--slow-report-ms",
        value: "N",
        help: "Bound the wall-clock time the run spends reporting slow frames (default: unbounded; 0 reports no slow frame).",
    },
    OptionDoc {
        flag: "--vm-report",
        value: "",
        help: "Measure the VM's script compilation and print its analysis at the end of the run.",
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
        out.push_str(&format!("    {left:<30}{}\n", option.help));
    }
    out.push_str(&format!(
        "    {:<30}{}\n",
        "GAME", "Game directory (recursively scanned for *.xp3) or an XP3 file."
    ));
    out
}

/// Default number of headless frames.
const DEFAULT_FRAMES: u32 = 120;

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
    /// Whether `--preload-archives` was given.
    preload_archives: Option<bool>,
    /// Whether `--debug-overlay` was given.
    debug_overlay: Option<bool>,
    /// Decoded-image cache policy from `--graphic-cache-limit`, if given.
    graphic_cache_limit: Option<CacheLimit>,
    /// Where the presented frame is written as a PNG, if given.
    dump_frame: Option<PathBuf>,
    /// The whole-run budget for the slow-frame reports, in milliseconds, or
    /// `None` when the option was absent.
    slow_report_ms: Option<u64>,
    /// Whether `--vm-report` was given.
    vm_report: bool,
    /// Explicit `--archive` list, in mount order (later wins).
    archives: Vec<PathBuf>,
    /// App path override (`--base-dir`); defaults to the current directory.
    base_dir: Option<PathBuf>,
    /// The first positional argument: a game directory or an archive file.
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
            preload_archives: None,
            debug_overlay: None,
            graphic_cache_limit: None,
            dump_frame: None,
            slow_report_ms: None,
            vm_report: false,
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
                "--preload-archives" => cli.preload_archives = Some(true),
                "--debug-overlay" => cli.debug_overlay = Some(true),
                "--graphic-cache-limit" => {
                    let value = args.next().ok_or_else(|| {
                        CliError::Usage("--graphic-cache-limit requires a value".to_owned())
                    })?;
                    cli.graphic_cache_limit =
                        Some(CacheLimit::parse("--graphic-cache-limit", &value)?);
                }
                "--dump-frame" => {
                    let value = args.next().ok_or_else(|| {
                        CliError::Usage("--dump-frame requires a path".to_owned())
                    })?;
                    cli.dump_frame = Some(PathBuf::from(value));
                }
                "--slow-report-ms" => {
                    let value = args.next().ok_or_else(|| {
                        CliError::Usage("--slow-report-ms requires a value".to_owned())
                    })?;
                    cli.slow_report_ms = Some(value.parse().map_err(|_| {
                        ConfigError::invalid(
                            "--slow-report-ms",
                            value.clone(),
                            "a non-negative integer",
                        )
                    })?);
                }
                "--vm-report" => cli.vm_report = true,
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

/// Checks that the requested frame dump has a backend that can produce one.
///
/// The dumped frame is the one the CPU rasterizer composited, and only the
/// null backend composites one.
///
/// # Errors
///
/// Returns the message when the backend cannot produce a CPU frame.
fn check_dump_backend(cli: &Cli) -> Result<(), String> {
    if cli.dump_frame.is_some() && cli.backend != BackendKind::Null {
        return Err("--dump-frame needs the null backend (--backend null)".to_owned());
    }
    Ok(())
}

/// The slow-frame reporting budget the command line asks for.
///
/// The option is absent by default, and an absent option leaves the reports
/// unbounded. A budget of zero reports no slow frame at all.
fn slow_report_budget(cli: &Cli) -> Option<Duration> {
    cli.slow_report_ms.map(Duration::from_millis)
}

/// Environment variable naming the desktop configuration file.
///
/// When it is unset the desktop launcher holds no persistent configuration.
const CONFIG_PATH_ENV: &str = "KRKR_CONFIG";

/// Loads the desktop `key=value` configuration file named by
/// [`CONFIG_PATH_ENV`].
///
/// The desktop launcher owns this file; the emulator core never sees it. An
/// unset variable means no configuration, but a configured file that cannot
/// be read is a typed error, never an empty result (no silent fallback).
fn load_desktop_config() -> Result<RuntimeConfigOverrides, ConfigError> {
    match std::env::var_os(CONFIG_PATH_ENV) {
        Some(path) => load_config_file(Path::new(&path)),
        None => Ok(RuntimeConfigOverrides::default()),
    }
}

/// Reads and parses one `key=value` configuration file.
///
/// Nothing is applied until the whole file parses: an unknown key or a bad
/// value is a typed error naming the key and its accepted range.
fn load_config_file(path: &Path) -> Result<RuntimeConfigOverrides, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|error| ConfigError::Source {
        message: format!("cannot read {}: {error}", path.display()),
    })?;
    parse_overrides(&text)
}

/// Resolves the runtime configuration with the precedence
/// **launch argument > desktop configuration file > built-in default**.
///
/// Both the argument and the file have already been validated, so the merge
/// cannot fail; a field neither source holds falls back to
/// [`RuntimeConfig::default`].
fn resolve_runtime_config(cli: &Cli, file: RuntimeConfigOverrides) -> RuntimeConfig {
    let defaults = RuntimeConfig::default();
    RuntimeConfig {
        frame_rate_limit: cli
            .frame_rate
            .or(file.frame_rate_limit)
            .unwrap_or(defaults.frame_rate_limit),
        preload_archives: cli
            .preload_archives
            .or(file.preload_archives)
            .unwrap_or(defaults.preload_archives),
        debug_overlay: cli
            .debug_overlay
            .or(file.debug_overlay)
            .unwrap_or(defaults.debug_overlay),
        graphic_cache_limit: cli.graphic_cache_limit.or(file.graphic_cache_limit),
    }
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

    let file = match load_desktop_config() {
        Ok(file) => file,
        Err(error) => {
            eprintln!("krkr: {error}");
            return ExitCode::from(2);
        }
    };
    let runtime = resolve_runtime_config(&cli, file);

    let game = match resolve_game(&cli) {
        Ok(game) => game,
        Err(message) => {
            eprintln!("krkr: {message}");
            return ExitCode::FAILURE;
        }
    };

    // The dumped frame is the one the CPU rasterizer composited, and only
    // the null backend composites one.
    if let Err(message) = check_dump_backend(&cli) {
        eprintln!("krkr: {message}");
        return ExitCode::from(2);
    }

    let slow_report_budget = slow_report_budget(&cli);
    // The VM's own measurement of the script compilation: the run asks for
    // it, so the switch goes on before the run starts and the startup's own
    // compiles are measured with the rest.
    if cli.vm_report {
        compat::set_compile_stats_enabled(true);
    }
    let launch = Launch {
        runtime,
        backend: cli.backend,
        game,
        mode: if cli.headless {
            RunMode::Headless { frames: cli.frames }
        } else {
            RunMode::Windowed
        },
        dump_frame: cli.dump_frame,
        slow_report_budget,
    };

    let outcome = app::run(launch);
    if cli.vm_report {
        println!("{}", app::vm_report(&compat::compile_stats()));
    }
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("krkr: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Resolves the command line into the game to load, or `None` when no game
/// input was given (the empty-scene run).
///
/// The game is discovered from the first command-line argument: a directory is
/// the app path whose loose `startup.tjs` decides bootability, a file is opened
/// as an archive. There is no default folder scan for `*.xp3`. `--archive`
/// stays authoritative and is mounted in addition to a positional archive.
///
/// The app path is `--base-dir` (or a positional directory) when given;
/// otherwise it is the first archive's parent, falling back to the working
/// directory. Relative archive paths resolve against the app path, so it is
/// the single root for archives and loose files.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn parse(args: &[&str]) -> Result<Cli, CliError> {
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
        assert_eq!(cli.frame_rate, None);
        assert_eq!(cli.debug_overlay, None);
        assert_eq!(cli.graphic_cache_limit, None);
    }

    #[test]
    fn parse_frame_rate() {
        let cli = parse(&["--fps", "30"]).expect("parse");
        assert_eq!(
            cli.frame_rate,
            Some(FrameRateLimit::new("--fps", 30).expect("valid"))
        );
    }

    #[test]
    fn parse_debug_overlay() {
        assert_eq!(parse(&["--headless"]).expect("parse").debug_overlay, None);
        assert_eq!(
            parse(&["--headless", "--debug-overlay"])
                .expect("parse")
                .debug_overlay,
            Some(true)
        );
    }

    #[test]
    fn parse_graphic_cache_limit() {
        let cli = parse(&["--headless", "--graphic-cache-limit", "none"]).expect("parse");
        assert_eq!(cli.graphic_cache_limit, Some(CacheLimit::Disabled));

        let cli = parse(&["--headless", "--graphic-cache-limit", "0"]).expect("parse");
        assert_eq!(cli.graphic_cache_limit, Some(CacheLimit::Unlimited));

        let cli = parse(&["--headless", "--graphic-cache-limit", "1048576"]).expect("parse");
        assert_eq!(cli.graphic_cache_limit, Some(CacheLimit::bytes(1_048_576)));
    }

    #[test]
    fn parse_dump_frame() {
        let cli = parse(&["--headless", "--dump-frame", "frame.png"]).expect("parse");
        assert_eq!(cli.dump_frame, Some(PathBuf::from("frame.png")));
        assert!(check_dump_backend(&cli).is_ok());

        // The dumped frame comes from the CPU rasterizer, so a backend that
        // presents to a swapchain cannot produce one.
        let cli = parse(&["--backend", "vulkan", "--dump-frame", "frame.png"]).expect("parse");
        assert!(check_dump_backend(&cli).is_err());
    }

    #[test]
    fn parse_rejects_every_invalid_configuration_value() {
        for args in [
            &["--fps"][..],
            &["--fps", "0"],
            &["--fps", "1001"],
            &["--fps", "fast"],
            &["--graphic-cache-limit"],
            &["--graphic-cache-limit", "-1"],
            &["--graphic-cache-limit", "sometimes"],
        ] {
            assert!(parse(args).is_err(), "accepted {args:?}");
        }
    }

    /// The slow-frame budget is absent by default, and the option takes a
    /// whole number of milliseconds.
    #[test]
    fn parse_slow_report_ms() {
        assert_eq!(parse(&["--headless"]).expect("parse").slow_report_ms, None);
        assert_eq!(
            parse(&["--headless", "--slow-report-ms", "250"])
                .expect("parse")
                .slow_report_ms,
            Some(250)
        );
        assert_eq!(
            parse(&["--headless", "--slow-report-ms", "0"])
                .expect("parse")
                .slow_report_ms,
            Some(0)
        );
    }

    /// A budget that is not a whole number of milliseconds is a usage error,
    /// and the message names the option.
    #[test]
    fn parse_rejects_a_bad_slow_report_ms() {
        for args in [
            &["--slow-report-ms"][..],
            &["--slow-report-ms", "-1"],
            &["--slow-report-ms", "fast"],
            &["--slow-report-ms", "1.5"],
        ] {
            assert!(parse(args).is_err(), "accepted {args:?}");
        }
        let error = parse(&["--slow-report-ms", "-1"])
            .expect_err("must fail")
            .to_string();
        assert!(error.contains("--slow-report-ms"), "{error}");
    }

    /// The option's value is the run's budget: an absent option leaves the
    /// reports unbounded, and a budget of zero reports no slow frame.
    #[test]
    fn the_slow_report_budget_follows_the_option() {
        let unbounded = slow_report_budget(&parse(&["--headless"]).expect("parse"));
        assert_eq!(unbounded, None);

        let bounded =
            slow_report_budget(&parse(&["--headless", "--slow-report-ms", "250"]).expect("parse"));
        assert_eq!(bounded, Some(Duration::from_millis(250)));

        let silent =
            slow_report_budget(&parse(&["--headless", "--slow-report-ms", "0"]).expect("parse"));
        assert_eq!(silent, Some(Duration::ZERO));
    }

    /// The help names the option and its default, so a run that asks for a
    /// budget is told what the absent option means.
    #[test]
    fn the_help_documents_the_slow_report_budget() {
        let text = usage();
        assert!(text.contains("--slow-report-ms N"), "{text}");
        assert!(text.contains("default: unbounded"), "{text}");
    }

    #[test]
    fn invalid_value_error_names_the_option_and_the_range() {
        let error = parse(&["--fps", "0"]).expect_err("must fail").to_string();
        assert!(error.contains("--fps"), "{error}");
        assert!(error.contains("1..=1000"), "{error}");
    }

    #[test]
    fn resolution_prefers_argument_then_file_then_default() {
        let file = RuntimeConfigOverrides {
            frame_rate_limit: Some(FrameRateLimit::new("frame_rate", 30).expect("valid")),
            debug_overlay: Some(true),
            graphic_cache_limit: Some(CacheLimit::bytes(1_048_576)),
            ..RuntimeConfigOverrides::default()
        };

        // The file beats the built-in default.
        let cli = parse(&["--headless"]).expect("parse");
        let config = resolve_runtime_config(&cli, file);
        assert_eq!(config.frame_rate_limit.fps(), 30);
        assert!(config.debug_overlay);
        assert_eq!(
            config.graphic_cache_limit,
            Some(CacheLimit::bytes(1_048_576))
        );

        // The launch argument beats the file, field by field.
        let cli =
            parse(&["--headless", "--fps", "120", "--graphic-cache-limit", "0"]).expect("parse");
        let config = resolve_runtime_config(&cli, file);
        assert_eq!(config.frame_rate_limit.fps(), 120);
        // The file's other fields are still in place.
        assert!(config.debug_overlay);
        assert_eq!(config.graphic_cache_limit, Some(CacheLimit::Unlimited));
    }

    #[test]
    fn unresolved_fields_take_the_built_in_default() {
        let cli = parse(&["--headless"]).expect("parse");
        let config = resolve_runtime_config(&cli, RuntimeConfigOverrides::default());
        assert_eq!(config, RuntimeConfig::default());
        assert_eq!(config.frame_rate_limit.fps(), 60);
        assert!(!config.debug_overlay);
        assert_eq!(config.graphic_cache_limit, None);
    }

    #[test]
    fn debug_overlay_resolves_argument_then_file_then_default() {
        // The built-in default is off.
        let cli = parse(&["--headless"]).expect("parse");
        assert!(!resolve_runtime_config(&cli, RuntimeConfigOverrides::default()).debug_overlay);

        // The configuration file turns it on.
        let file = RuntimeConfigOverrides {
            debug_overlay: Some(true),
            ..RuntimeConfigOverrides::default()
        };
        assert!(resolve_runtime_config(&cli, file).debug_overlay);

        // The launch argument beats a file that leaves it off.
        let file = RuntimeConfigOverrides {
            debug_overlay: Some(false),
            ..RuntimeConfigOverrides::default()
        };
        let cli = parse(&["--headless", "--debug-overlay"]).expect("parse");
        assert!(resolve_runtime_config(&cli, file).debug_overlay);
    }

    /// A unique path under the system temp directory for a config file.
    fn temp_config(text: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("krkr-config-{}-{seq}.conf", std::process::id()));
        std::fs::write(&path, text).expect("write temp config");
        path
    }

    #[test]
    fn config_file_parses_keys_and_rejects_bad_values() {
        let path = temp_config(
            "frame_rate = 30\ndebug_overlay=true\ngraphic_cache_limit=1048576\n# comment\n",
        );
        let overrides = load_config_file(&path).expect("parse");
        assert_eq!(
            overrides.frame_rate_limit,
            Some(FrameRateLimit::new("frame_rate", 30).expect("valid"))
        );
        assert_eq!(overrides.debug_overlay, Some(true));
        assert_eq!(
            overrides.graphic_cache_limit,
            Some(CacheLimit::bytes(1_048_576))
        );
        std::fs::remove_file(&path).ok();

        let bad = temp_config("frame_rate=0\n");
        let error = load_config_file(&bad).expect_err("must reject").to_string();
        assert!(error.contains("frame_rate"), "{error}");
        assert!(error.contains("1..=1000"), "{error}");
        std::fs::remove_file(&bad).ok();

        let unknown = temp_config("teleport=on\n");
        let error = load_config_file(&unknown)
            .expect_err("must reject")
            .to_string();
        assert!(error.contains("teleport"), "{error}");
        assert!(error.contains("frame_rate"), "{error}");
        std::fs::remove_file(&unknown).ok();
    }

    #[test]
    fn config_file_that_cannot_be_read_is_an_error() {
        let path = std::env::temp_dir().join("krkr-config-does-not-exist.conf");
        assert!(load_config_file(&path).is_err());
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
