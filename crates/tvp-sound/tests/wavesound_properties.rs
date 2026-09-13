//! Reference-surface probe for the `WaveSoundBuffer` native class:
//! metadata (`totalTime`/`frequency`/`bits`/`channels`), millisecond seek
//! semantics (`position`/`samplePosition`), the secondary volume
//! (`volume2`), pause-before-play, `.sli` loop points, and the stop rewind.
//!
//! The `WaveSoundBuffer` natives keep a process-global engine handle
//! (`ENGINE`, set by the first `register_sound`), so — like
//! `tests/class_shadowing.rs` — this lives in its own test binary. Only one
//! test registers the natives; the other is a pure storage/parser probe.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use engine::Storage;
use tjs2_sys::{Tjs2Engine, TjsValue};
use tvp_sound::{advance, open_track, register_sound, sound_poll};

/// A unique temp directory that removes itself on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("tvp-wsprops-{}-{}", std::process::id(), stamp));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TestDir(dir)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A 16-bit mono RIFF/WAVE sine fixture.
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

fn mount_dir(dir: &TestDir) -> Arc<Mutex<Storage>> {
    Arc::new(Mutex::new(Storage::mount(&dir.0).expect("mount storage")))
}

/// Wait until the sound poll has derived the buffer's status.
fn wait_status(e: &Tjs2Engine, var: &str, want: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        sound_poll(e, 0.0);
        if matches!(
            e.eval(&format!("{var}.status"), "wait"),
            Ok(TjsValue::String(s)) if s == want
        ) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{var} never reached status {want:?}: {:?}",
            e.eval(&format!("{var}.status"), "wait")
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn eval_i(e: &Tjs2Engine, expr: &str) -> i64 {
    match e.eval(expr, "probe").expect("eval succeeds") {
        TjsValue::Integer(v) => v,
        other => panic!("expected Integer for {expr:?}, got {other:?}"),
    }
}

/// Pure probe: `open_track` reads the `<name>.sli` side-car and exposes the
/// loop link (the game's `bgm/*.ogg.sli` loop points).
#[test]
fn open_track_reads_sli_loop_points() {
    let dir = TestDir::new();
    std::fs::write(dir.0.join("one.wav"), sine_wav_bytes(44100, 440.0, 0.5)).expect("wav");
    std::fs::write(
        dir.0.join("one.wav.sli"),
        b"#2.00\n# Sound Loop Information (utf-8)\nLink { From=22050; To=11025; Smooth=False; Condition=no; RefValue=0; CondVar=0; }\n",
    )
    .expect("sli");
    let storage = mount_dir(&dir);

    let track = open_track(&storage, "one.wav").expect("open with sli");
    let link = track.loop_link().expect("sli loop link parsed");
    assert_eq!(link.from, 22050);
    assert_eq!(link.to, 11025);
    assert!(!link.smooth);
}

/// The reference `WaveSoundBuffer` property surface the game's
/// `system/sound.tjs` + `selectitem.tjs` drive.
#[test]
fn wavesoundbuffer_reference_property_surface() {
    let dir = TestDir::new();
    std::fs::write(dir.0.join("one.wav"), sine_wav_bytes(44100, 440.0, 1.0)).expect("wav");
    let storage = mount_dir(&dir);

    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();
    e.exec_script(
        r#"
        var ws = new WaveSoundBuffer(null);
        ws.open("one.wav");
        "#,
        "wsprops",
    )
    .unwrap();
    wait_status(&e, "ws", "stop");

    // Reference metadata.
    assert_eq!(eval_i(&e, "ws.totalTime"), 1000, "1s = 1000 ms");
    assert_eq!(eval_i(&e, "ws.frequency"), 44100);
    assert_eq!(eval_i(&e, "ws.bits"), 16);
    assert_eq!(eval_i(&e, "ws.channels"), 1);
    // Default volume2 is 100000 (the reference ctor).
    assert_eq!(eval_i(&e, "ws.volume2"), 100000);
    assert_eq!(eval_i(&e, "ws.position"), 0);
    assert_eq!(eval_i(&e, "ws.samplePosition"), 0);

    // volume2 is independent of `volume` and clamps.
    e.exec_script("ws.volume = 100000; ws.volume2 = 50000;", "wsprops")
        .unwrap();
    assert_eq!(eval_i(&e, "ws.volume"), 100000);
    assert_eq!(eval_i(&e, "ws.volume2"), 50000);
    e.exec_script("ws.volume2 = 200000;", "wsprops").unwrap();
    assert_eq!(eval_i(&e, "ws.volume2"), 100000, "clamped high");
    e.exec_script("ws.volume2 = -5;", "wsprops").unwrap();
    assert_eq!(eval_i(&e, "ws.volume2"), 0, "clamped low");

    // position is milliseconds; samplePosition is sample granules.
    e.exec_script("ws.position = 250;", "wsprops").unwrap();
    assert_eq!(eval_i(&e, "ws.position"), 250);
    assert_eq!(eval_i(&e, "ws.samplePosition"), 11025);
    e.exec_script("ws.samplePosition = 22050;", "wsprops")
        .unwrap();
    assert_eq!(eval_i(&e, "ws.position"), 500);

    // Pause before play must stick (the eyecatch jingle pattern).
    e.exec_script("ws.paused = true; ws.play();", "wsprops")
        .unwrap();
    assert_eq!(eval_i(&e, "ws.paused"), 1, "play keeps the pause flag");
    advance(0.1); // global clock -> 0.1s
    assert_eq!(
        eval_i(&e, "ws.position"),
        0,
        "a paused buffer does not advance"
    );

    // Unpause and advance; position moves in real time.
    e.exec_script("ws.paused = false;", "wsprops").unwrap();
    advance(0.35); // dt = 0.25s
    let pos = eval_i(&e, "ws.position");
    assert!(
        (240..=260).contains(&pos),
        "expected ~250 ms after 0.25 s, got {pos}"
    );

    // Calling `play()` while already playing must not restart at 0
    // (reference `Play()` returns when `BufferPlaying` is set).
    e.exec_script("ws.play();", "wsprops").unwrap();
    let pos2 = eval_i(&e, "ws.position");
    assert!(
        (230..=270).contains(&pos2),
        "play() while playing restarted the track: {pos2} ms"
    );

    // The reference `Stop()` rewinds to 0.
    e.exec_script("ws.stop();", "wsprops").unwrap();
    assert_eq!(eval_i(&e, "ws.position"), 0, "stop rewinds the position");
    assert_eq!(
        e.eval("ws.status", "wsprops").unwrap(),
        TjsValue::String("stop".into())
    );

    // frequency is a real playback-rate control (round-trips and applies).
    e.exec_script("ws.frequency = 22050;", "wsprops").unwrap();
    assert_eq!(eval_i(&e, "ws.frequency"), 22050);
    assert_eq!(eval_i(&e, "ws.channels"), 1, "metadata still readable");
}
