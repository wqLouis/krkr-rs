//! The script-observable frame timing that an idle or low-power change
//! must not move.
//!
//! A script timer and a `System.addContinuousHandler` callback are both
//! delivered by the engine's frame: the timer fires on the frame clock and
//! the continuous handler runs once per frame. Their rates are the
//! script-visible contract of the frame rate, so any change that makes the
//! frame run less often — a skipped draw, a reduced tick rate, a wake-up
//! driven by the next timer deadline — must keep them exactly as they are.
//!
//! The test drives a real game host over a temporary directory, so the
//! counters come from the same path a game runs: the L4 VM host, the L3
//! compatibility surface and the L2 engine frame.

use std::path::PathBuf;

use app::{GameHost, Host, HostConfig, InjectedFrameTime};
use testkit::vm_lock;
use tjs::TjsValue;
use vm::VmHost;

/// The startup script: a 1 ms timer and a continuous handler, each counting
/// its own deliveries in a global.
const SCRIPT: &str = r#"
var fires = 0;
var ticks = 0;
var timer = new Timer(function() { fires = fires + 1; }, '');
timer.interval = 1;
timer.enabled = true;
System.addContinuousHandler(function(tick) { ticks = ticks + 1; });
"#;

/// The number of frames the test steps.
const FRAMES: i64 = 60;

/// The frame period the test steps at, in milliseconds.
const FRAME_MS: f32 = 1000.0 / 60.0;

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

/// A script timer and a continuous handler are delivered once per frame.
///
/// The 1 ms timer is due at every frame, and the registry delivers a
/// repeating timer once per advance — a late frame fires it exactly once,
/// never as a catch-up burst — so both counters must equal the frame count.
/// The frame period is injected, so the count does not depend on how fast
/// the machine is: 60 steps of 1000/60 ms deliver exactly 60 of each.
///
/// The count is what any idle or low-power work must not move: a frame that
/// stopped running, a timer that fired twice, or a tick rate that was halved
/// all show up here.
#[test]
fn a_script_timer_and_a_continuous_handler_fire_once_per_frame() {
    let _lock = vm_lock();
    let (mut game, base) = open_game("script-timing", SCRIPT);

    for _ in 0..FRAMES {
        game.step(&InjectedFrameTime::new(f64::from(FRAME_MS)), &[])
            .expect("frame");
    }

    let fires = eval_int(game.vm(), "fires");
    let ticks = eval_int(game.vm(), "ticks");
    assert_eq!(
        fires, FRAMES,
        "the timer must fire once per frame: {fires} fires over {FRAMES} frames"
    );
    assert_eq!(
        ticks, FRAMES,
        "the continuous handler must run once per frame: {ticks} ticks over {FRAMES} frames"
    );

    drop(game);
    let _ = std::fs::remove_dir_all(&base);
}
