//! The frame-time distribution of the real game, in actual gameplay.
//!
//! The title menu is idle: nothing is loaded, nothing moves. This drives
//! the game into a scenario with clicks and measures the frames there.
//!
//! Run: KRKR_TEST_GAME=<dir> cargo run -p app --example gameplay_frames -- <clicks>

use std::path::Path;
use std::time::Instant;

use app::{FrameTimeSource, GameHost, Host, HostConfig};
use platform::{InputEvent, MouseButton, PlatformEvent};

/// A clock that measures the real time between the frames.
///
/// The gap the slow-frame report reads is this one, so it must be the wall
/// clock and not an injected frame period.
///
/// The engine reads the frame's own gap, and the clock hands the value
/// over: the engine adds it to its clock every frame, so handing it the
/// time since the run started would run the scenario ahead of the wall
/// clock - every frame would carry the whole elapsed time's worth of
/// timers, and the frames would measure the scenario's work instead of
/// the game's real rate.
///
/// `KRKR_FRAME_CLOCK=cumulative` hands the engine the cumulative time
/// instead, which is the fast-forward workload: the game runs ahead of the
/// wall clock and the frames carry the scenario's work every frame. The
/// engine's slow-frame report reads the cumulative time as its gap there,
/// so its gap and its unaccounted residual mean nothing in that mode; the
/// phase costs stay the frame's own.
struct WallClock {
    start: Instant,
    last: std::cell::Cell<Instant>,
    elapsed: std::cell::Cell<f64>,
    cumulative: bool,
}

impl WallClock {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last: std::cell::Cell::new(now),
            elapsed: std::cell::Cell::new(0.0),
            cumulative: cumulative_clock(),
        }
    }

    /// The gap since the previous frame, and moves the clock's own boundary.
    fn take_gap(&mut self) -> f64 {
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

impl FrameTimeSource for WallClock {
    fn elapsed_ms(&self) -> f64 {
        self.elapsed.get()
    }

    fn now_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
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

    // The frames are driven as fast as the host runs them, so the gap is the
    // engine's cost and not the presentation cadence.
    let mut clock = WallClock::new();
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
        game.step(&clock, &events).expect("a frame");
        let now = Instant::now();
        gaps.push(now.duration_since(last).as_secs_f64() * 1000.0);
        last = now;
        clock.take_gap();
    }

    // The startup gap is the first one; it spans the engine load.
    let startup = gaps.first().copied().unwrap_or(0.0);
    let mut rest: Vec<f64> = gaps[1..].to_vec();
    rest.sort_by(|left, right| left.total_cmp(right));
    let at = |fraction: f64| -> f64 {
        let index = ((rest.len() as f64 - 1.0) * fraction).round() as usize;
        rest[index]
    };
    let p50 = at(0.50);
    let p90 = at(0.90);
    let p99 = at(0.99);
    let worst = *rest.last().unwrap_or(&0.0);
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
