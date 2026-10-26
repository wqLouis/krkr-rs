//! The script-observable frame timing that an idle or low-power change
//! must not move.
//!
//! A script timer and a `System.addContinuousHandler` callback are both
//! delivered by the engine's frame, and they follow different clocks.
//!
//! The timer follows the *clock*: a frame that took longer than the timer's
//! interval replays the intervals it missed, so the script sees one fire per
//! elapsed interval however the frames are spread.
//!
//! reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the timer thread
//! computes n = (curtick - NextTick) / Interval, increments it and triggers
//! the timer n times, re-arming from the deadline it missed. Our registry
//! replays the same n intervals in the frame that covered them.
//!
//! The continuous handler follows the *frames*: it runs once per frame.
//!
//! reference/cpp/core/base/EventIntf.cpp:868 — the continuous events are
//! delivered in the event-delivery phase, and the frame's draw flags that
//! phase for the next idle (reference/cpp/core/visual/impl/
//! VSyncTimingThread.cpp:141-142), so the handler runs once per frame.
//!
//! The test drives a real game host over a temporary directory, so the
//! counters come from the same path a game runs: the L4 VM host, the L3
//! compatibility surface and the L2 engine frame.

use std::path::PathBuf;

use app::{GameHost, Host, HostConfig, InjectedFrameTime};
use testkit::vm_lock;
use tjs::TjsValue;
use vm::VmHost;

/// The startup script: a 10 ms timer and a continuous handler, each counting
/// its own deliveries in a global.
const SCRIPT: &str = r#"
var fires = 0;
var ticks = 0;
var timer = new Timer(function() { fires = fires + 1; }, '');
timer.interval = 10;
timer.enabled = true;
System.addContinuousHandler(function(tick) { ticks = ticks + 1; });
"#;

/// The number of frames the test steps.
const FRAMES: i64 = 60;

/// The frame period the test steps at, in milliseconds.
const FRAME_MS: f32 = 1000.0 / 60.0;

/// The fires a 10 ms timer makes over the test's frames: its first deadline
/// is one interval out (10 ms), and it fires once for every deadline the
/// clock reaches, so the count is the number of whole intervals in the
/// elapsed time.
///
/// The frame period is a `f32`, so sixty frames of 1000/60 ms accumulate to
/// 999.99996 ms — just short of a second — and the clock's whole-millisecond
/// value is 999. The deadlines 10, 20, ... 990 are therefore due and the one
/// at 1000 ms is not: ninety-nine fires.
///
/// reference/cpp/core/utils/impl/TimerImpl.cpp:118-148 — the timer fires once
/// per elapsed interval, so the count follows the clock and not the frame
/// count.
const FIRES_PER_SECOND: i64 = 99;

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

/// Boots a headless game host over a temporary directory holding `script`,
/// returning the host and the directory to remove.
fn open_game(tag: &str, script: &str) -> (GameHost, PathBuf) {
    let base = unique_temp_dir(tag);
    std::fs::create_dir_all(&base).expect("game dir");
    std::fs::write(base.join("startup.tjs"), script).expect("startup script");
    let host = Host::new(HostConfig::headless()).expect("headless host");
    let game = GameHost::open(host, Vec::new(), base.clone()).expect("open game");
    (game, base)
}

/// Evaluates `expression` and returns its integer result.
fn eval_int(vm: &VmHost, expression: &str) -> i64 {
    match vm.eval(expression, "t").expect("eval succeeds") {
        TjsValue::Integer(value) => value,
        other => panic!("expected Integer for {expression:?}, got {other:?}"),
    }
}

/// Runs `frames` frames of `frame_ms` each and returns the timer's fire
/// count and the continuous handler's tick count.
fn run_frames(tag: &str, frames: i64, frame_ms: f32) -> (i64, i64) {
    let _lock = vm_lock();
    let (mut game, base) = open_game(tag, SCRIPT);

    for _ in 0..frames {
        game.step(&InjectedFrameTime::new(f64::from(frame_ms)), &[])
            .expect("frame");
    }

    let fires = eval_int(game.vm(), "fires");
    let ticks = eval_int(game.vm(), "ticks");

    drop(game);
    let _ = std::fs::remove_dir_all(&base);
    (fires, ticks)
}

/// The script timer follows the clock, the continuous handler follows the
/// frames.
///
/// The same elapsed second is run twice: once in 60 frames and once in 30.
///
/// The timer fires once per elapsed 10 ms interval: its first deadline is
/// one interval out and the clock reaches 1000 ms, so a hundred deadlines
/// are due and the timer fires a hundred times — in both runs, because the
/// registry replays the intervals a frame covered. A frame covers one or two
/// intervals in the fine split and three or four in the coarse one, so no
/// frame's backlog reaches the capacity of six postings and none is
/// dropped.
///
/// reference/cpp/core/utils/TimerIntf.cpp:69-111 — the capacity bounds the
/// `onTimer` postings still outstanding for a timer.
///
/// The continuous handler runs once per frame, so its count is the frame
/// count.
///
/// The frame period is injected, so the counts do not depend on how fast the
/// machine is.
///
/// The counts are what any idle or low-power work must not move: a frame
/// that stopped running, a timer that fired twice for one interval, or a
/// tick rate that was halved all show up here.
#[test]
fn a_script_timer_follows_the_clock_and_a_continuous_handler_follows_the_frames() {
    let (fires_fine, ticks_fine) = run_frames("script-timing-fine", FRAMES, FRAME_MS);
    let (fires_coarse, ticks_coarse) =
        run_frames("script-timing-coarse", FRAMES / 2, FRAME_MS * 2.0);

    assert_eq!(
        ticks_fine, FRAMES,
        "the continuous handler must run once per frame: {ticks_fine} ticks over \
         {FRAMES} frames"
    );
    assert_eq!(
        ticks_coarse,
        FRAMES / 2,
        "the continuous handler must run once per frame: {ticks_coarse} ticks over \
         {} frames",
        FRAMES / 2
    );

    assert_eq!(
        fires_fine, FIRES_PER_SECOND,
        "the timer must fire once per elapsed interval: {fires_fine} fires over \
         {FRAMES} frames"
    );
    assert_eq!(
        fires_coarse,
        FIRES_PER_SECOND,
        "the timer must fire once per elapsed interval: {fires_coarse} fires over \
         {} frames",
        FRAMES / 2
    );
}
