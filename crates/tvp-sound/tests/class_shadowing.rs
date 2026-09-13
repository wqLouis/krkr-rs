//! One-engine probe for the `WaveSoundBuffer` script-subclass surface.
//!
//! The sound natives keep a process-global engine handle and a
//! process-global stream map, so WaveSoundBuffer tests must live one per
//! process — hence this dedicated test binary with a single `#[test]` (see
//! the note in `tests/real_game_opus_wavesound.rs`).
//!
//! Covered:
//! - `class SoundBuffer extends WaveSoundBuffer` shadowing (`instanceof`);
//! - the reference status-event flow: [`sound_poll`] posts
//!   `onStatusChanged` to the buffer **instance**, the script override runs
//!   first, and its `super.onStatusChanged(...)` reaches the native handler,
//!   which forwards `%[type,status]` to the action owner's `action(ev)` —
//!   the exact chain the game's `SoundBuffer` (BGM/SE) and
//!   `WaveSoundBuffer(this)` (AttentionVoice) rely on.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use engine::Storage;
use tjs2_sys::{Tjs2Engine, TjsValue};
use tvp_sound::{advance, register_sound, sound_poll};

/// A short 16-bit mono RIFF/WAVE sine fixture (no external deps).
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
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
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
fn wavesoundbuffer_subclass_shadowing_and_status_events() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("tvp-shadow-{}-{}", std::process::id(), stamp));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("one.wav"), sine_wav_bytes(44100, 440.0, 0.2)).expect("write wav");
    let storage = Arc::new(Mutex::new(Storage::mount(&dir).expect("mount storage")));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    let script = r#"
        var overrideCount = 0;
        var actionType = "";
        var actionStatus = "";
        var owner = %[action: function(ev) { actionType = ev.type; actionStatus = ev.status; }];
        class SoundBuffer extends WaveSoundBuffer {
            function SoundBuffer(owner){ WaveSoundBuffer(owner); }
            function open(){ super.open("one.wav"); }
            function onStatusChanged(st){
                overrideCount++;
                super.onStatusChanged(...);
            }
            function test(){ return this.getStatus(); }
        }
        var sb = new SoundBuffer(owner);
        sb.open();
        sb.play();
    "#;
    e.exec_script(script, "probe").expect("subclass script");

    // `instanceof` against an ABI-registered native class is not reliable in
    // this TJS2 build, so the subclass relationship is proven functionally:
    // the no-argument `open()` above reached the override (which forwards to
    // `super.open("one.wav")`), and the events below reach the override and
    // its `super`.

    // play() transition -> onStatusChanged override -> super -> action(ev).
    // The 0.2s WAV decodes asynchronously; pump the poll at clock 0 until
    // the play transition fires.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        advance(0.0);
        sound_poll(&e, 0.0);
        if e.eval("actionStatus", "probe").unwrap() == TjsValue::String("play".into()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "play transition never fired: {:?}",
            e.eval("actionStatus", "probe")
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        e.eval("overrideCount >= 1", "probe").unwrap(),
        TjsValue::Integer(1),
        "the script subclass onStatusChanged override must run"
    );
    assert_eq!(
        e.eval("actionType", "probe").unwrap(),
        TjsValue::String("onStatusChanged".into())
    );
    assert_eq!(
        e.eval("actionStatus", "probe").unwrap(),
        TjsValue::String("play".into()),
        "super.onStatusChanged must forward to the action owner"
    );

    // Natural end -> "stop" through the same override + super chain.
    advance(0.5);
    sound_poll(&e, 0.55);
    assert_eq!(
        e.eval("actionStatus", "probe").unwrap(),
        TjsValue::String("stop".into()),
        "the stop transition must reach the action owner too"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
