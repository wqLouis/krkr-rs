//! End-to-end probe: the game's **real Ogg Opus voice** through the real
//! `WaveSoundBuffer` native class — `open("voice/…")` → `play()` → clock
//! advance → status transitions. This is exactly the path the game's
//! AttentionVoice sequencing drives; with the old ~60 ms silent Opus
//! fallback it would report `status == "stop"` after a single frame.
//!
//! This lives in its **own test binary** on purpose: the `WaveSoundBuffer`
//! natives keep a process-global engine handle (`ENGINE` OnceLock, set by
//! the first `register_sound`), so the TJS2 VM + sound natives must be
//! single-engine-per-process (same constraint as `tests/sound.rs`, taken
//! one step further).

use std::path::Path;
use std::sync::{Arc, Mutex};

use engine::Storage;
use tjs2_sys::{Tjs2Engine, TjsValue};
use tvp_sound::{advance, register_sound, sound_poll};

/// The real game installed on this dev machine.
const REAL_GAME_DIR: &str = "/mnt/DATA/Games/Others/test";

/// A real Ogg Opus voice file that actually exists in the game
/// (`voice/azs000001.ogg`: 48000 Hz mono, 3.24 s).
const REAL_VOICE_OPUS: &str = "voice/azs000001.ogg";

#[test]
fn real_game_opus_voice_plays_via_wavesoundbuffer() {
    let dir = Path::new(REAL_GAME_DIR);
    assert!(
        dir.is_dir(),
        "real game dir {REAL_GAME_DIR:?} not found — this probe test needs it mounted"
    );
    let storage = Arc::new(Mutex::new(Storage::mount(dir).expect("mount real game")));

    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage.clone()).unwrap();
    e.exec_script(
        &format!(
            r#"
        var lastType = "";
        var lastStatus = "";
        var owner = %[action: function(ev) {{ lastType = ev.type; lastStatus = ev.status; }}];
        var ws = new WaveSoundBuffer(owner);
        ws.open("{REAL_VOICE_OPUS}");
        ws.play();
        "#
        ),
        "wsopus",
    )
    .unwrap();

    // 0.5s in: the ~60 ms silent fallback would already be "stop" here.
    // The app drives `advance` + `sound_poll` once per frame; mirror that.
    advance(0.5);
    sound_poll(&e, 0.5);
    assert_eq!(
        e.eval("ws.status", "wsopus").unwrap(),
        TjsValue::String("play".into()),
        "Opus voice must still be playing after 0.5s (silent fallback stops at ~60ms)"
    );
    // The reference delivers `onStatusChanged` to the buffer instance, whose
    // native handler forwards `%[type, status]` to the action owner's
    // `action(ev)` — exactly what the game's `AttentionVoice` implements.
    assert_eq!(
        e.eval("lastType", "wsopus").unwrap(),
        TjsValue::String("onStatusChanged".into()),
        "the action owner must receive the onStatusChanged event type"
    );
    assert_eq!(
        e.eval("lastStatus", "wsopus").unwrap(),
        TjsValue::String("play".into()),
        "the action owner must have received action({{status:\"play\"}})"
    );

    // Past the real end of the 3.24s voice: status flips to "stop", which is
    // exactly the onStatusChanged("stop") the game's voice chain waits for.
    advance(4.0);
    sound_poll(&e, 4.5);
    assert_eq!(
        e.eval("ws.status", "wsopus").unwrap(),
        TjsValue::String("stop".into()),
        "Opus voice must reach its natural end (stop) after ~4.5s"
    );
    assert_eq!(
        e.eval("lastStatus", "wsopus").unwrap(),
        TjsValue::String("stop".into()),
        "the action owner must have received the final action({{status:\"stop\"}})"
    );
}
