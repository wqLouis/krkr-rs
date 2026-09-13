//! Regression: a `WaveSoundBuffer` whose retained `self_obj` or
//! `action_owner` was invalidated must not be dispatched to forever after.
//!
//! The poll (`sound_poll`) fires `onStatusChanged`/`onFadeCompleted` on each
//! stream's retained `self_obj`, whose native handler forwards to the
//! retained `action_owner`'s `action(ev)`. When the script invalidates the
//! buffer or its action owner, a root `Invalidate()` (tjs2-sys commit
//! `3925289`) makes the member call return `TJSInvalidObject` ("The object
//! is already invalidated"); before the fix the poll logged that warning on
//! every status change, forever. The fix marks the stream dead on that
//! error, skips it, and reaps it (releases the retained objects and removes
//! the mixer channel).
//!
//! The buffer's own object being invalidated is already handled because
//! `Invalidate()` calls the native destroy callback (`ws_destroy`), which
//! removes the stream. The surviving case is the **action owner**: the
//! `WaveSoundBuffer` stays valid, so `sound_poll` dispatches `onStatusChanged`
//! to it, and only the nested `action(ev)` call fails. That is what this
//! test drives.
//!
//! The sound natives keep a process-global engine handle, so — like
//! `tests/class_shadowing.rs` — this lives in its own test binary.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use engine::Storage;
use tjs2_sys::{Tjs2Engine, TjsValue};
use tvp_sound::{active_stream_count, register_sound, sound_poll};

/// A unique temp directory that removes itself on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("tvp-inval-{}-{}", std::process::id(), stamp));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TestDir(dir)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A short 16-bit mono RIFF/WAVE sine fixture.
fn sine_wav_bytes(rate: u32, freq: f64, seconds: f64) -> Vec<u8> {
    let frames = (rate as f64 * seconds) as usize;
    let data_len = (frames * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for f in 0..frames {
        let v = ((2.0 * std::f64::consts::PI * freq * f as f64 / f64::from(rate)).sin()
            * 0.5
            * 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[test]
fn invalidated_stream_is_reaped_and_not_dispatched_again() {
    let dir = TestDir::new();
    std::fs::write(dir.0.join("one.wav"), sine_wav_bytes(44100, 440.0, 0.2)).expect("wav");
    let storage = Arc::new(Mutex::new(Storage::mount(&dir.0).expect("mount storage")));

    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();
    e.exec_script(
        r#"
        var lastStatus = "";
        var owner = %[action: function(ev) { lastStatus = ev.status; }];
        var sb = new WaveSoundBuffer(owner);
        sb.open("one.wav");
        sb.play();
        "#,
        "inval",
    )
    .unwrap();

    // Pump at clock 0 (dt = 0) until the async decode is ready and the
    // "play" transition has reached the owner.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        sound_poll(&e, 0.0);
        if e.eval("sb.status", "inval").unwrap() == TjsValue::String("play".into()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "play transition never fired: {:?}",
            e.eval("sb.status", "inval")
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(
        e.eval("lastStatus", "inval").unwrap(),
        TjsValue::String("play".into()),
        "one onStatusChanged(\"play\") should reach the action owner"
    );

    // The script drops the *action owner* (e.g. the game's scene/manager
    // object). The WaveSoundBuffer itself stays alive, so the poll's stream
    // entry (and its retained self_obj) is still there.
    e.exec_script("invalidate owner;", "inval").unwrap();
    let before = active_stream_count();
    assert!(
        before >= 1,
        "the invalidated stream must still be registered (got {before}); \
         otherwise this regression cannot be exercised"
    );

    // Advance past the track end: `sound_poll` dispatches the derived
    // "stop" event to the still-valid buffer, whose native handler forwards
    // to the now-invalidated owner. The nested action call returns
    // `TJSInvalidObject`; the poll must stop dispatching and reap the stream.
    sound_poll(&e, 0.5);
    assert_eq!(
        active_stream_count(),
        0,
        "invalidated stream must be reaped after the failed dispatch"
    );
    assert_eq!(
        e.eval("lastStatus", "inval").unwrap(),
        TjsValue::String("play".into()),
        "no event may reach the action owner after invalidation"
    );

    // Further polls are clean no-ops (no repeated dispatch/warning).
    sound_poll(&e, 0.6);
    sound_poll(&e, 0.7);
    assert_eq!(active_stream_count(), 0);
    assert_eq!(
        e.eval("lastStatus", "inval").unwrap(),
        TjsValue::String("play".into())
    );
}
