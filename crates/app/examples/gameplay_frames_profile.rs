//! The gameplay frame-time distribution with every frame's bounds on stdout.
//!
//! The `gameplay_frames` harness measures the frames but keeps their bounds
//! to itself. A sampling profiler needs to know which frame a sample landed
//! in, so this harness prints one line per frame: the frame's index, the gap
//! since the previous frame, the time the frame's own work took, and the
//! wall-clock nanosecond the frame started at.
//!
//! The lines are `FRAME <index> <gap-ms> <step-ms> <start-ns>`, and they go
//! to stdout; the run's own log goes to stderr.
//!
//! Run: KRKR_TEST_GAME=<dir> cargo run -p app --example gameplay_frames_profile -- <frames>

use std::cell::Cell;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use app::{FrameTimeSource, GameHost, Host, HostConfig};
use platform::{InputEvent, MouseButton, PlatformEvent};

/// A clock that measures the real time between the frames.
///
/// The engine reads the gap when the frame starts, so the harness measures
/// the gap before the step and the clock hands the value over. The boundary
/// moves at the frame's start, so the gap the engine reads spans the frame
/// it is about to run.
///
/// `KRKR_FRAME_CLOCK=cumulative` hands the engine the time since the run
/// started instead of the frame's own gap. The engine adds that to its clock
/// every frame, so the scenario runs ahead of the wall clock: the frames
/// then carry the scenario's work every frame instead of waiting for the
/// next event. The gap the engine's slow-frame report reads is the
/// cumulative time in that mode, so the report's gap and its unaccounted
/// residual mean nothing there; the phase costs stay the frame's own.
struct WallClock {
    start: Instant,
    last: Cell<Instant>,
    elapsed: Cell<f64>,
    cumulative: bool,
}

impl WallClock {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last: Cell::new(now),
            elapsed: Cell::new(0.0),
            cumulative: cumulative_clock(),
        }
    }

    /// The gap since the previous frame, and the value the engine reads.
    fn take_gap(&self) -> f64 {
        let now = Instant::now();
        let gap = now.duration_since(self.last.get()).as_secs_f64() * 1000.0;
        self.last.set(now);
        let handed = if self.cumulative {
            self.start.elapsed().as_secs_f64() * 1000.0
        } else {
            gap
        };
        self.elapsed.set(handed);
        gap
    }
}

impl FrameTimeSource for WallClock {
    fn elapsed_ms(&self) -> f64 {
        self.elapsed.get()
    }

    fn now_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
}

/// The clock's mode, read from the environment.
///
/// The mode that is not named is the frame delta, so an absent variable is
/// the default and a variable that cannot be read is an error.
fn cumulative_clock() -> bool {
    match std::env::var("KRKR_FRAME_CLOCK") {
        Ok(value) => value == "cumulative",
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => {
            eprintln!("the frame clock mode is not readable: {error}");
            std::process::exit(2);
        }
    }
}

/// The wall-clock nanoseconds, the clock the frame lines carry.
fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the wall clock")
        .as_nanos()
}

fn main() {
    // The slow-frame report is a log line, so the run needs a logger.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let Ok(dir) = std::env::var("KRKR_TEST_GAME") else {
        eprintln!("set KRKR_TEST_GAME to the game directory");
        std::process::exit(2);
    };
    let dir = Path::new(&dir);
    let archives = testkit::game_archives(dir).expect("the archives");
    let host = Host::new(HostConfig::headless()).expect("the host");
    let mut game = GameHost::open(host, archives, dir.to_path_buf()).expect("the game");
    game = game.with_slow_report_budget(None);

    let frames: usize = match std::env::args().nth(2) {
        Some(value) => match value.parse() {
            Ok(frames) => frames,
            Err(error) => {
                eprintln!("the frame count is not a number: {error}");
                std::process::exit(2);
            }
        },
        None => 6000,
    };

    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    // The frames are driven as fast as the host runs them, so the gap is the
    // engine's cost and not the presentation cadence.
    let clock = WallClock::new();
    let mut gaps: Vec<f64> = Vec::with_capacity(frames);
    let mut last = Instant::now();

    // The title menu: a click in the middle of the screen advances it.
    let mut click_at: Option<usize> = None;
    for frame in 0..frames {
        let mut events: Vec<PlatformEvent> = Vec::new();
        if frame == 30 {
            // The first click: the title's "start".
            events.push(PlatformEvent::Input(InputEvent::PointerMoved {
                x: 640.0,
                y: 360.0,
            }));
            events.push(PlatformEvent::Input(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            }));
            events.push(PlatformEvent::Input(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            }));
            click_at = Some(frame);
        }
        if let Some(at) = click_at
            && frame >= at
            && (frame - at) % 600 == 599
        {
            // A click every ten seconds of game time: the scenario advances.
            events.push(PlatformEvent::Input(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            }));
            events.push(PlatformEvent::Input(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            }));
        }
        let start_ns = now_ns();
        clock.take_gap();
        game.step(&clock, &events).expect("a frame");
        let now = Instant::now();
        let gap = now.duration_since(last).as_secs_f64() * 1000.0;
        let step = now_ns().saturating_sub(start_ns) as f64 / 1_000_000.0;
        last = now;
        writeln!(out, "FRAME {frame} {gap:.3} {step:.3} {start_ns}").expect("the frame line");
        gaps.push(gap);
    }
    out.flush().expect("the frame lines");

    // The startup gap is the first one; it spans the engine load.
    let startup = match gaps.first() {
        Some(gap) => *gap,
        // The run had no frames at all, so there is no startup gap.
        None => 0.0,
    };
    let mut rest: Vec<f64> = gaps[1..].to_vec();
    rest.sort_by(|left, right| left.total_cmp(right));
    let at = |fraction: f64| -> f64 {
        let index = ((rest.len() as f64 - 1.0) * fraction).round() as usize;
        rest[index]
    };
    let p50 = at(0.50);
    let p90 = at(0.90);
    let p99 = at(0.99);
    let worst = match rest.last() {
        Some(gap) => *gap,
        // The run had no frames after the startup one.
        None => 0.0,
    };
    // The 1% low: the rate the slowest 1% of the frames run at.
    let slow: Vec<f64> = rest.iter().copied().filter(|gap| *gap >= p99).collect();
    let one_low = slow.iter().sum::<f64>() / slow.len().max(1) as f64;
    println!("the gameplay frames: {}", gaps.len());
    println!("the startup gap: {startup:.1} ms");
    println!("the p50: {p50:.2} ms ({:.1} fps)", 1000.0 / p50);
    println!("the p90: {p90:.2} ms ({:.1} fps)", 1000.0 / p90);
    println!("the p99: {p99:.2} ms ({:.1} fps)", 1000.0 / p99);
    println!("the 1% low: {one_low:.2} ms ({:.1} fps)", 1000.0 / one_low);
    println!("the worst: {worst:.2} ms ({:.1} fps)", 1000.0 / worst);
    println!(
        "the frames over 33 ms: {}",
        rest.iter().filter(|g| **g > 33.3).count()
    );
    println!(
        "the frames over 100 ms: {}",
        rest.iter().filter(|g| **g > 100.0).count()
    );
}
