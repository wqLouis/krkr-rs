//! Boot a real KiriKiri game through the CLI's [`GameHost`], end to end.
//! Ignored by default.
//!
//! Set `KRKR_TEST_GAME` to the game directory and run:
//!
//! ```text
//! KRKR_TEST_GAME=/path/to/game cargo test -p app --test boot_game -- --ignored --nocapture
//! ```
//!
//! This is the compatibility oracle: it mounts the game's archives (explicit
//! list, later wins), runs the L2 startup sequence through the TJS VM with the
//! compat natives registered, and drives the frame body. Whatever it reports
//! (a missing native, a script error) is the next thing to fix.
//!
//! The test harness derives the archive list from the directory; the product
//! load path (`krkr --archive …`) never scans (doc 09 §1).

use std::path::Path;

use app::{GameHost, Host, HostConfig, InjectedFrameTime};

#[test]
#[ignore = "needs KRKR_TEST_GAME pointing at a real game"]
fn boots_the_real_game() {
    let Ok(dir) = std::env::var("KRKR_TEST_GAME") else {
        eprintln!("skipping: set KRKR_TEST_GAME to a game directory");
        return;
    };
    let dir = Path::new(&dir);

    // The product takes an explicit list; the test derives one for convenience.
    let archives = testkit::game_archives(dir).expect("list game directory");

    let host = Host::new(HostConfig::headless()).expect("create host");
    let mut game = GameHost::open(host, archives, dir.to_path_buf()).expect("open game");

    for _ in 0..60 {
        game.step(&InjectedFrameTime::new(1000.0 / 60.0), &[]).expect("step");
    }
    let scene = game.scene();
    assert!(
        !scene.windows.is_empty(),
        "the game should have created a window"
    );
    eprintln!(
        "booted: windows={} layers={} bitmaps={}",
        scene.windows.len(),
        scene.layers.len(),
        scene.bitmaps.len(),
    );
}
