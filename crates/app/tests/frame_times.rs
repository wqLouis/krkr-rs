//! The run reports the frame-time distribution it measured.
//!
//! The report is the run's last line: the number of frames, the fastest and
//! the slowest frame, the percentiles and the 1% low — the rate the slowest
//! 1% of the frames run at. Every number in it comes from the gaps the run
//! measured, and a run that paced itself says so, because a paced run's gaps
//! include the wait for the frame's deadline.

use api::config::{FrameRateLimit, RuntimeConfig};
use app::{BackendKind, Launch, RunMode};

/// The frames the run steps.
const FRAMES: u32 = 30;

/// The frame rate the run paces at.
///
/// A one-millisecond period keeps the run short and leaves the frame's own
/// cost a visible part of every gap.
fn runtime() -> RuntimeConfig {
    RuntimeConfig {
        frame_rate_limit: FrameRateLimit::new("test", 1000).expect("valid"),
        ..RuntimeConfig::default()
    }
}

/// A headless run of the empty scene.
fn launch(frame_report: bool) -> Launch {
    Launch {
        runtime: runtime(),
        backend: BackendKind::Null,
        game: None,
        mode: RunMode::Headless { frames: FRAMES },
        dump_frame: None,
        slow_report_budget: None,
        frame_report,
    }
}

/// The run reports the distribution it measured.
///
/// The report counts the frames the run stepped and the gaps between them,
/// and it names the pacing: the gaps of a paced run include the wait for the
/// frame's deadline.
#[test]
fn a_run_reports_the_frame_times_it_measured() {
    let report = app::run(launch(true)).expect("headless run");
    let times = report
        .frame_times
        .expect("the run reported its frame times");

    assert_eq!(times.frames, u64::from(FRAMES));
    // The first gap spans the run's start-up and not a frame, so the
    // frame-to-frame gaps are one less than the frames.
    assert_eq!(times.gaps, u64::from(FRAMES - 1));
    assert_eq!(
        times.kept,
        usize::try_from(FRAMES - 1).expect("the frames fit")
    );
    assert!(times.paced, "the run paced itself");
    assert_eq!(times.period_ms, Some(1.0));
    assert!(times.startup_ms.is_some(), "the run measured its start-up");

    // The percentiles are ordered, and every one of them is measured.
    let fastest = times.fastest_ms.expect("the fastest frame");
    let p50 = times.p50_ms.expect("the median");
    let p90 = times.p90_ms.expect("the 90th percentile");
    let p99 = times.p99_ms.expect("the 99th percentile");
    let slowest = times.slowest_ms.expect("the slowest frame");
    assert!(fastest <= p50 && p50 <= p90 && p90 <= p99 && p99 <= slowest);
    assert!(fastest > 0.0, "the fastest frame took {fastest} ms");
    // The run paced at a one-millisecond period, so every gap covers the
    // wait for the deadline and the frame's own cost.
    assert!(fastest >= 1.0, "the fastest gap was {fastest} ms");

    // The 1% low is the rate the slowest 1% of the frames run at.
    let low = times.one_percent_low_fps.expect("the 1% low");
    assert!(low > 0.0, "the 1% low was {low} fps");
    let low_ms = times.one_percent_low_ms.expect("the 1% low's frame time");
    assert!(
        (1000.0 / low_ms - low).abs() < 1e-6,
        "{low} fps is {low_ms} ms"
    );
}

/// The report's line names the distribution and the 1% low.
#[test]
fn the_report_line_names_the_one_percent_low() {
    let report = app::run(launch(true)).expect("headless run");
    let line = report
        .frame_times
        .expect("the run reported its frame times")
        .line();
    assert!(line.starts_with("frame times: frames=30 gaps=29"), "{line}");
    assert!(line.contains("paced=yes(1.0ms)"), "{line}");
    assert!(line.contains("p50="), "{line}");
    assert!(line.contains("p90="), "{line}");
    assert!(line.contains("p99="), "{line}");
    assert!(line.contains("1%low="), "{line}");
}

/// A run that does not report the distribution measures no history at all.
#[test]
fn a_run_without_the_report_measures_nothing() {
    let report = app::run(launch(false)).expect("headless run");
    assert!(
        report.frame_times.is_none(),
        "the run reported its frame times"
    );
    // The run itself is unchanged: it stepped the same frames.
    let last = report.last_frame.expect("the last frame");
    assert_eq!(last.frame_index, u64::from(FRAMES));
}
