//! Integration test: drive the real `VideoOverlay` native class through the
//! TJS VM against a movie extracted from the reference game archive.
//!
//! The game's `.mpg` files are H.264/AAC MP4 containers; this test proves the
//! native reports the real duration/dimensions and decodes actual RGBA pixels
//! and audio samples (exposed through the read-only diagnostic properties
//! `frameWidth`/`frameHeight`/`frameBytes`/`frameChecksum` and
//! `audioSampleCount`/`audioSampleRate`/`audioChannels`).
//!
//! The test skips (rather than fails) when the reference archive or the
//! extraction script is not present, so `cargo test` stays portable.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use engine::Storage;
use tjs2_sys::{Tjs2Engine, TjsValue};
use tvp_natives::{register_all, set_video_storage};

/// Reference game archive used by the repository's manual verification.
const GAME_XP3: &str = "/mnt/DATA/Games/Others/test/data.xp3";
/// The H.264/AAC clip the task singled out (1280x720, ~5 s).
const MOVIE_ENTRY: &str = "effect/watch_long.mpg";

fn repo_root() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Some(manifest.parent()?.parent()?.to_path_buf())
}

/// Extract `MOVIE_ENTRY` into `dir`; `None` means "skip".
fn extract_movie(dir: &std::path::Path) -> Option<()> {
    let root = repo_root()?;
    let script = root.join("scripts/extract_xp3.py");
    if !script.exists() || !std::path::Path::new(GAME_XP3).exists() {
        return None;
    }
    let output = Command::new("python3")
        .arg(&script)
        .arg(GAME_XP3)
        .arg(MOVIE_ENTRY)
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    std::fs::write(dir.join("watch_long.mpg"), &output.stdout).ok()?;
    Some(())
}

#[test]
fn video_overlay_decodes_real_movie() {
    let dir = std::env::temp_dir().join(format!("tvp_vo_movie_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    if extract_movie(&dir).is_none() {
        eprintln!("skipping: {GAME_XP3} / scripts/extract_xp3.py not available");
        return;
    }

    let storage = Storage::mount(dir.to_str().expect("utf-8 path")).expect("mount storage");
    set_video_storage(Some(Arc::new(Mutex::new(storage))));

    let engine = Tjs2Engine::new().expect("create engine");
    register_all(&engine).expect("register natives");

    engine
        .exec_script(
            r#"
            var v = new VideoOverlay(null);
            v.open("watch_long.mpg");
            var ow = v.originalWidth;
            var oh = v.originalHeight;
            var ofps = v.fps;
            var otf = v.totalFrame;
            var ott = v.totalTime;
            var oas = v.numberOfAudioStream;
            var frameBytes0 = v.frameBytes;
            var checksum0 = v.frameChecksum;
            // Frames 0..=6 are identical in this clip; frame 30 (~1 s) is not.
            v.frame = 30;
            var frameBytesMid = v.frameBytes;
            var checksumMid = v.frameChecksum;
            var audioCount = v.audioSampleCount;
            var audioRate = v.audioSampleRate;
            var audioCh = v.audioChannels;
            var frameW = v.frameWidth;
            var frameH = v.frameHeight;

            // Real decoder + the existing clock-driven event surface: play
            // fires onStatusChanged("play"); jumping past the end at the next
            // poll fires onStatusChanged("stop") and the transition callback
            // (what MovieLayer uses to run onStopMovie).
            var events = "";
            var done = 0;
            v.onStatusChanged = function(status) { events += status + ","; };
            v.setTransitionCompleteCall(function() { done++; });
            v.play();
            v.position = 6000;
            "#,
            "video_overlay_movie",
        )
        .expect("VideoOverlay script must run");

    // The position is already past the 5 s end, so one poll completes it.
    tvp_natives::video_overlay_poll(&engine);

    let int = |name: &str| match engine.eval(name, "test").expect("eval") {
        TjsValue::Integer(value) => value,
        other => panic!("{name} -> {other:?}"),
    };
    let real = |name: &str| match engine.eval(name, "test").expect("eval") {
        TjsValue::Real(value) => value,
        other => panic!("{name} -> {other:?}"),
    };

    // -- metadata ----------------------------------------------------------
    assert_eq!(int("ow"), 1280, "originalWidth");
    assert_eq!(int("oh"), 720, "originalHeight");
    assert!((real("ofps") - 30.0).abs() < 0.01, "fps {}", real("ofps"));
    assert_eq!(int("otf"), 150, "totalFrame");
    assert!(
        (int("ott") - 5000).abs() < 200,
        "totalTime {} ms should be ~5.0 s",
        int("ott")
    );
    assert_eq!(int("oas"), 1, "numberOfAudioStream");

    // -- decoded RGBA layer bitmap ----------------------------------------
    assert_eq!(int("frameW"), 1280, "frameWidth");
    assert_eq!(int("frameH"), 720, "frameHeight");
    assert_eq!(int("frameBytes0"), 1280 * 720 * 4, "frame 0 RGBA bytes");
    assert_eq!(int("frameBytesMid"), 1280 * 720 * 4, "frame 30 RGBA bytes");
    assert_ne!(int("checksum0"), 0, "frame 0 must have pixels");
    assert_ne!(
        int("checksumMid"),
        int("checksum0"),
        "frame 30 must differ from frame 0"
    );

    // -- decoded audio -----------------------------------------------------
    assert!(int("audioCount") > 0, "audio must yield samples");
    assert_eq!(int("audioRate"), 48_000, "audioSampleRate");
    assert_eq!(int("audioCh"), 2, "audioChannels");

    // -- clock-driven events still fire with a real decoder ----------------
    let events = match engine.eval("events", "test").expect("eval") {
        TjsValue::String(s) => s,
        other => panic!("events -> {other:?}"),
    };
    assert!(events.contains("play"), "onStatusChanged(play): {events:?}");
    assert!(events.contains("stop"), "onStatusChanged(stop): {events:?}");
    assert_eq!(int("done"), 1, "transition callback must fire once");

    set_video_storage(None);
    let _ = std::fs::remove_dir_all(&dir);
}
