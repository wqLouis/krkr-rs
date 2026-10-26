//! The headless runner's script clock is the wall clock.
//!
//! The script's tick count is a wall clock: the reference's tick counter is
//! `timeGetTime()` with a bias for the 32-bit overflow
//! (`reference/cpp/core/utils/TickCount.cpp:110-118`), and the script reads
//! it through `System.getTickCount()`
//! (`reference/cpp/core/base/impl/SystemImpl.cpp:621-629`). A headless run
//! that paced its frames but advanced the script's clock by the nominal
//! frame period would leave the clock behind the wall clock by the
//! accumulated cost of the frames themselves.
//!
//! The script reports its own tick count on every frame through
//! `System.inform`, and the test reads the reports back from the logger.
//! The instant a report carries is the wall clock at the moment the script
//! read its tick count, so the script's series and the wall clock's series
//! can be compared against each other.

use std::path::PathBuf;
use std::sync::{Mutex, Once};
use std::time::Instant;

use api::config::{FrameRateLimit, RuntimeConfig};
use app::{BackendKind, GameInput, Launch, RunMode};
use log::{Level, LevelFilter, Metadata, Record};

/// The startup script: a continuous handler that reports the script's tick
/// count on every frame.
const SCRIPT: &str = r#"
System.addContinuousHandler(function(tick) {
    System.inform("tick=" + System.getTickCount());
});
"#;

/// The frames the run steps.
///
/// The run takes about seven tenths of a second, which keeps the wall clock
/// long enough for the frames' own jitter to stay a small share of it.
const FRAMES: u32 = 600;

/// The frame rate the run paces at.
///
/// A 1 ms frame period keeps the frames' own cost a measurable part of the
/// wall clock they are spread over, so a clock that advanced by the nominal
/// period instead of the measured one would show up as a lag.
const FPS: u32 = 1000;

/// The mark the script's report carries its tick count behind.
const TICK_MARK: &str = "tick=";

/// The share the two clocks may differ by.
const TOLERANCE: f64 = 0.02;

/// The reports the script made, with the instant each one was logged.
static REPORTS: Mutex<Vec<(Instant, String)>> = Mutex::new(Vec::new());

/// The logger that keeps the script's reports.
static LOGGER: ReportLogger = ReportLogger;

/// The logger that keeps the script's reports.
struct ReportLogger;

impl log::Log for ReportLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = record.args().to_string();
        if !line.contains(TICK_MARK) {
            return;
        }
        let mut reports = REPORTS.lock().unwrap_or_else(|poison| poison.into_inner());
        reports.push((Instant::now(), line));
    }

    fn flush(&self) {}
}

/// Installs the logger that keeps the script's reports.
fn install_logger() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        log::set_logger(&LOGGER).expect("the test binary installs one logger");
        log::set_max_level(LevelFilter::Info);
    });
}

/// The tick counts the script reported, with the instant of each report.
fn reported_ticks() -> Vec<(Instant, i64)> {
    let reports = REPORTS.lock().unwrap_or_else(|poison| poison.into_inner());
    reports
        .iter()
        .filter_map(|(at, line)| {
            let (_, tick) = line.split_once(TICK_MARK)?;
            Some((
                *at,
                tick.trim()
                    .parse::<i64>()
                    .expect("a tick report carries a number"),
            ))
        })
        .collect()
}

/// A unique temporary directory, removed by the caller.
fn unique_temp_dir(tag: &str) -> PathBuf {
    let mut base = std::env::temp_dir();
    base.push(format!(
        "krkr-app-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    base
}

/// The headless runner advances the script's clock by the time its frames
/// really took.
///
/// The script's tick count is the wall clock
/// (`reference/cpp/core/utils/TickCount.cpp:110-118`, exposed as
/// `System.getTickCount()` at
/// `reference/cpp/core/base/impl/SystemImpl.cpp:621-629`), so a headless
/// run that advanced the clock by the nominal frame period would leave the
/// script's clock behind the wall clock by the accumulated cost of the
/// frames.
///
/// The script reports its tick count on every frame, and the reports carry
/// the wall clock's instant, so the two series are compared directly. The
/// ratio stays within the tolerance because the clock counts the measured
/// time; a clock that counted the nominal period would report the nominal
/// sum, which is below the measured one by the frames' own cost.
#[test]
fn the_headless_script_clock_tracks_the_wall_clock() {
    install_logger();
    let base = unique_temp_dir("headless-clock");
    std::fs::create_dir_all(&base).expect("game dir");
    std::fs::write(base.join("startup.tjs"), SCRIPT).expect("startup script");

    let runtime = RuntimeConfig {
        frame_rate_limit: FrameRateLimit::new("test", FPS).expect("valid"),
        ..RuntimeConfig::default()
    };
    let started = Instant::now();
    let report = app::run(Launch {
        runtime,
        backend: BackendKind::Null,
        game: Some(GameInput {
            base_dir: base.clone(),
            archives: Vec::new(),
        }),
        mode: RunMode::Headless { frames: FRAMES },
    })
    .expect("headless run");

    let ticks = reported_ticks();
    let first = ticks
        .first()
        .expect("the script reported a tick on the first frame");
    let last = ticks
        .last()
        .expect("the script reported a tick on the last frame");

    // The script's clock and the wall clock moved together: between the two
    // reports the tick count advanced by the time the frames really took.
    let tick_delta = (last.1 - first.1) as f64;
    let wall_delta = last.0.duration_since(first.0).as_secs_f64() * 1000.0;
    let ratio = tick_delta / wall_delta;
    assert!(
        (ratio - 1.0).abs() <= TOLERANCE,
        "the script's clock advanced {tick_delta} ms while the wall clock advanced {wall_delta} ms"
    );

    // The tick count is the wall clock since the run started, the way the
    // windowed loop's clock is: the time the run spent before its first
    // frame counts too. The report's own instant carries the frame's start
    // to report delay, so this comparison allows a wider share.
    let script_ms = last.1 as f64;
    let wall_at_last = last.0.duration_since(started).as_secs_f64() * 1000.0;
    let ratio = script_ms / wall_at_last;
    assert!(
        (ratio - 1.0).abs() <= 0.05,
        "the script's clock read {script_ms} ms after {wall_at_last} ms of wall clock"
    );

    // The engine's clock is the clock the script read.
    let engine_ms = report
        .last_frame
        .expect("the run stepped its frames")
        .elapsed_ms;
    assert!(
        (engine_ms - script_ms).abs() <= 1.5,
        "the engine's clock read {engine_ms} ms while the script read {script_ms} ms"
    );

    let _ = std::fs::remove_dir_all(&base);
}
