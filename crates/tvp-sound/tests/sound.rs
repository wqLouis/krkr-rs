//! Headless integration tests for tvp-sound: decode, the clock-driven
//! mixer, and the SoundBuffer/SoundChannel native classes end-to-end.
//!
//! The TJS2 VM is not thread-safe, so these must run with
//! `--test-threads=1` (workspace convention); each test creates its own
//! engine and re-registers the natives (which installs a fresh global
//! mixer, so the global clock starts at 0 for every test).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use engine::Storage;
use tjs2_sys::{Tjs2Engine, TjsValue};
use tvp_sound::{advance, decode_audio, register_sound};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A unique temp directory that removes itself on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("tvp-sound-test-{}-{}", std::process::id(), stamp));
        std::fs::create_dir_all(&dir).expect("create temp game dir");
        TestDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Build a RIFF/WAVE container for 16-bit PCM samples.
fn wav_pcm16(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(2 * channels).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// A sine-wave WAV fixture (`seconds` long, `freq` Hz, amplitude 0.5).
fn sine_wav_bytes(rate: u32, channels: u16, freq: f64, seconds: f64) -> Vec<u8> {
    let frames = (rate as f64 * seconds) as usize;
    let mut samples = Vec::with_capacity(frames * usize::from(channels));
    for f in 0..frames {
        let v = ((2.0 * std::f64::consts::PI * freq * f as f64 / f64::from(rate)).sin()
            * 0.5
            * 32767.0) as i16;
        for _ in 0..channels {
            samples.push(v);
        }
    }
    wav_pcm16(rate, channels, &samples)
}

/// Write `name` into `dir` and mount it as game storage.
fn mounted(dir: &TestDir, name: &str, bytes: &[u8]) -> Arc<Mutex<Storage>> {
    std::fs::write(dir.path().join(name), bytes).expect("write fixture");
    Arc::new(Mutex::new(
        Storage::mount(dir.path()).expect("mount storage"),
    ))
}

// ---------------------------------------------------------------------------
// decode
// ---------------------------------------------------------------------------

#[test]
fn decode_wav_sine_amplitude_and_frequency() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "test.wav", &sine_wav_bytes(44100, 1, 440.0, 1.0));

    let audio = decode_audio(&storage, "test.wav").expect("decode wav");
    assert_eq!(audio.sample_rate, 44100);
    assert_eq!(audio.channels, 1);
    assert_eq!(audio.samples.len(), 44100, "one second of mono samples");
    assert!((audio.duration_seconds() - 1.0).abs() < 1e-9);

    // amplitude ≈ 0.5 (the sine is written at half scale)
    let peak = audio.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!((peak - 0.5).abs() < 0.02, "peak amplitude {peak}");

    // frequency ≈ 440 Hz via zero crossings (880 crossings per second)
    let crossings = audio
        .samples
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    let freq = crossings as f64 / 2.0;
    assert!((freq - 440.0).abs() < 440.0 * 0.05, "measured {freq} Hz");
}

#[test]
fn decode_wav_stereo_interleaves() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "stereo.wav", &sine_wav_bytes(22050, 2, 330.0, 0.5));

    let audio = decode_audio(&storage, "stereo.wav").expect("decode stereo wav");
    assert_eq!(audio.sample_rate, 22050);
    assert_eq!(audio.channels, 2);
    assert_eq!(audio.samples.len(), 22050, "0.5s * 2 channels * 22050");
    // interleaving: left and right channels carry the same tone
    let (l, r) = (audio.samples[0], audio.samples[1]);
    assert!(
        (l - r).abs() < 1e-6,
        "interleaved channels equal: {l} vs {r}"
    );
}

#[test]
fn decode_missing_file_errors() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "a.wav", &sine_wav_bytes(44100, 1, 440.0, 0.1));

    let err = decode_audio(&storage, "nope.wav").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("nope.wav") && msg.contains("not found"),
        "{msg}"
    );
}

#[test]
fn decode_garbage_bytes_errors() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "junk.wav", b"this is not audio at all........");
    let err = decode_audio(&storage, "junk.wav").unwrap_err();
    assert!(err.to_string().contains("junk.wav"), "{err}");
}

/// OGG/Vorbis decode, verified with an ffmpeg-generated fixture. Skipped
/// (with a log line) when ffmpeg is unavailable.
#[test]
fn decode_ogg_vorbis_fixture() {
    let dir = TestDir::new();
    let src = dir.path().join("src.wav");
    let ogg = dir.path().join("tone.ogg");
    std::fs::write(&src, sine_wav_bytes(44100, 1, 440.0, 0.5)).expect("write src wav");
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(&src)
        .args(["-c:a", "libvorbis", "-q:a", "1"])
        .arg(&ogg)
        .status();
    match status {
        Ok(s) if s.success() => {
            let storage = mounted(&dir, "tone.ogg", &std::fs::read(&ogg).expect("read ogg"));
            let audio = decode_audio(&storage, "tone.ogg").expect("decode ogg");
            assert_eq!(audio.sample_rate, 44100);
            assert_eq!(audio.channels, 1);
            let dur = audio.duration_seconds();
            assert!(
                (0.4..0.6).contains(&dur),
                "ogg duration {dur}s should be ~0.5s"
            );
        }
        other => eprintln!("ffmpeg unavailable ({other:?}) — skipping the ogg fixture test"),
    }
}

// ---------------------------------------------------------------------------
// natives: SoundBuffer
// ---------------------------------------------------------------------------

#[test]
fn soundbuffer_decodes_from_storage() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "test.wav", &sine_wav_bytes(44100, 1, 440.0, 1.0));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    e.exec_script(
        r#"
        var b = new SoundBuffer();
        b.open("test.wav");
        var info = b.getBufferInfo();
        var bid = b.getBufferId();
        var status = b.getStatus();
        "#,
        "sb1",
    )
    .unwrap();

    assert_eq!(
        e.eval("info", "sb1").unwrap(),
        TjsValue::String("(44100,1,1.00)".into())
    );
    assert_eq!(
        e.eval("status", "sb1").unwrap(),
        TjsValue::String("ready".into())
    );
    match e.eval("bid", "sb1").unwrap() {
        TjsValue::Integer(id) => assert!(id > 0, "buffer id must be positive"),
        other => panic!("buffer id should be an integer, got {other:?}"),
    }
}

#[test]
fn soundbuffer_unloaded_state() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "a.wav", &sine_wav_bytes(44100, 1, 440.0, 0.1));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    e.exec_script("var b = new SoundBuffer();", "sb2").unwrap();
    assert_eq!(
        e.eval("b.getStatus()", "sb2").unwrap(),
        TjsValue::String("unload".into())
    );
    assert_eq!(
        e.eval("b.getBufferId()", "sb2").unwrap(),
        TjsValue::Integer(0)
    );
    assert_eq!(e.eval("b.getBufferInfo()", "sb2").unwrap(), TjsValue::Void);
}

#[test]
fn soundbuffer_open_missing_file_throws() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "a.wav", &sine_wav_bytes(44100, 1, 440.0, 0.1));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    let err = e
        .exec_script("var b = new SoundBuffer(); b.open(\"nope.wav\");", "sb3")
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("nope.wav") && msg.contains("not found"),
        "{msg}"
    );
}

// ---------------------------------------------------------------------------
// natives: SoundChannel + the global clock
// ---------------------------------------------------------------------------

#[test]
fn channel_play_advance_position_and_done() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "one.wav", &sine_wav_bytes(44100, 1, 440.0, 1.0));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    e.exec_script(
        r#"
        var ch = new SoundChannel();
        var b = new SoundBuffer();
        b.open("one.wav");
        ch.play(b);
        "#,
        "ch1",
    )
    .unwrap();
    assert_eq!(
        e.eval("ch.isPlaying()", "ch1").unwrap(),
        TjsValue::Integer(1)
    );

    // The global clock starts at 0: advance(1.0) moves the channel 1s.
    advance(1.0);
    assert_eq!(
        e.eval("ch.getPosition()", "ch1").unwrap(),
        TjsValue::Real(1.0)
    );
    assert_eq!(
        e.eval("ch.isDone()", "ch1").unwrap(),
        TjsValue::Integer(0),
        "exactly at the end is not done yet"
    );
    assert_eq!(
        e.eval("ch.isPlaying()", "ch1").unwrap(),
        TjsValue::Integer(1)
    );

    // Past the end: done, not playing, position pinned at the duration.
    advance(1.5); // dt = 0.5
    assert_eq!(e.eval("ch.isDone()", "ch1").unwrap(), TjsValue::Integer(1));
    assert_eq!(
        e.eval("ch.isPlaying()", "ch1").unwrap(),
        TjsValue::Integer(0)
    );
    assert_eq!(
        e.eval("ch.getPosition()", "ch1").unwrap(),
        TjsValue::Real(1.0)
    );
    assert_eq!(
        e.eval("ch.getStatus()", "ch1").unwrap(),
        TjsValue::String("stop".into())
    );
}

#[test]
fn channel_loop_wraps_position() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "one.wav", &sine_wav_bytes(44100, 1, 440.0, 1.0));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    e.exec_script(
        r#"
        var ch = new SoundChannel();
        ch.setLoop(1);
        var b = new SoundBuffer();
        b.open("one.wav");
        ch.play(b);
        "#,
        "ch2",
    )
    .unwrap();
    advance(1.2); // one full second, then 0.2s into the loop
    assert_eq!(
        e.eval("ch.isPlaying()", "ch2").unwrap(),
        TjsValue::Integer(1),
        "looping keeps playing"
    );
    assert_eq!(e.eval("ch.isDone()", "ch2").unwrap(), TjsValue::Integer(0));
    let pos = match e.eval("ch.getPosition()", "ch2").unwrap() {
        TjsValue::Real(p) => p,
        other => panic!("position is a real, got {other:?}"),
    };
    assert!((pos - 0.2).abs() < 1e-6, "position wraps to {pos}");

    advance(2.5); // 1.3 more seconds -> wraps to 0.5
    let pos = match e.eval("ch.getPosition()", "ch2").unwrap() {
        TjsValue::Real(p) => p,
        other => panic!("position is a real, got {other:?}"),
    };
    assert!((pos - 0.5).abs() < 1e-6, "position wraps to {pos}");
}

#[test]
fn channel_pause_resume_stop_and_status() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "two.wav", &sine_wav_bytes(44100, 1, 220.0, 2.0));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    e.exec_script(
        r#"
        var ch = new SoundChannel();
        var b = new SoundBuffer();
        b.open("two.wav");
        ch.play(b);
        "#,
        "ch3",
    )
    .unwrap();
    advance(1.0);
    e.exec_script("ch.pause();", "ch3").unwrap();
    assert_eq!(
        e.eval("ch.getStatus()", "ch3").unwrap(),
        TjsValue::String("pause".into())
    );
    assert_eq!(
        e.eval("ch.isPlaying()", "ch3").unwrap(),
        TjsValue::Integer(0)
    );
    advance(0.5); // paused: no movement
    assert_eq!(
        e.eval("ch.getPosition()", "ch3").unwrap(),
        TjsValue::Real(1.0)
    );

    e.exec_script("ch.resume();", "ch3").unwrap();
    advance(2.5); // +1.0s of movement (clock was 1.5)
    assert_eq!(
        e.eval("ch.getPosition()", "ch3").unwrap(),
        TjsValue::Real(2.0)
    );

    e.exec_script("ch.stop();", "ch3").unwrap();
    assert_eq!(
        e.eval("ch.isPlaying()", "ch3").unwrap(),
        TjsValue::Integer(0)
    );
    assert_eq!(e.eval("ch.isDone()", "ch3").unwrap(), TjsValue::Integer(0));
    assert_eq!(
        e.eval("ch.getStatus()", "ch3").unwrap(),
        TjsValue::String("stop".into())
    );
}

#[test]
fn channel_volume_pan_clamp_and_source_switch() {
    let dir = TestDir::new();
    std::fs::write(
        dir.path().join("one.wav"),
        sine_wav_bytes(44100, 1, 440.0, 1.0),
    )
    .expect("write one.wav");
    std::fs::write(
        dir.path().join("two.wav"),
        sine_wav_bytes(44100, 1, 220.0, 2.0),
    )
    .expect("write two.wav");
    let storage = Arc::new(Mutex::new(
        Storage::mount(dir.path()).expect("mount storage"),
    ));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    // two buffers; each play() immediately follows its buffer's open() so
    // the object-argument resolution (most recently opened buffer) is exact.
    e.exec_script(
        r#"
        var ch = new SoundChannel();
        var b1 = new SoundBuffer(); b1.open("one.wav");
        ch.play(b1);
        var b2 = new SoundBuffer(); b2.open("two.wav");
        ch.play(b2);
        "#,
        "ch4",
    )
    .unwrap();

    // volume clamps to 0..1
    e.exec_script("ch.setVolume(2.0);", "ch4").unwrap();
    assert_eq!(
        e.eval("ch.getVolume()", "ch4").unwrap(),
        TjsValue::Real(1.0)
    );
    e.exec_script("ch.setVolume(-1.0);", "ch4").unwrap();
    assert_eq!(
        e.eval("ch.getVolume()", "ch4").unwrap(),
        TjsValue::Real(0.0)
    );
    e.exec_script("ch.setVolume(0.5);", "ch4").unwrap();
    assert_eq!(
        e.eval("ch.getVolume()", "ch4").unwrap(),
        TjsValue::Real(0.5)
    );

    // pan clamps to -1..1
    e.exec_script("ch.setPan(5.0);", "ch4").unwrap();
    assert_eq!(e.eval("ch.getPan()", "ch4").unwrap(), TjsValue::Real(1.0));
    e.exec_script("ch.setPan(-5.0);", "ch4").unwrap();
    assert_eq!(e.eval("ch.getPan()", "ch4").unwrap(), TjsValue::Real(-1.0));
    e.exec_script("ch.setPan(0.25);", "ch4").unwrap();
    assert_eq!(e.eval("ch.getPan()", "ch4").unwrap(), TjsValue::Real(0.25));

    // the second play() switched the source: at 1.5s the 1s buffer would be
    // done, the 2s buffer is still playing.
    advance(1.5);
    assert_eq!(
        e.eval("ch.isDone()", "ch4").unwrap(),
        TjsValue::Integer(0),
        "source switched to the 2s buffer"
    );
    assert_eq!(
        e.eval("ch.isPlaying()", "ch4").unwrap(),
        TjsValue::Integer(1)
    );

    // stop() ends playback
    e.exec_script("ch.stop();", "ch4").unwrap();
    assert_eq!(
        e.eval("ch.isPlaying()", "ch4").unwrap(),
        TjsValue::Integer(0)
    );
}

#[test]
fn channel_play_by_name_and_by_id() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "one.wav", &sine_wav_bytes(44100, 1, 440.0, 1.0));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    // play by storage name (decoded on the fly)
    e.exec_script("var ch = new SoundChannel(); ch.play(\"one.wav\");", "ch5")
        .unwrap();
    assert_eq!(
        e.eval("ch.isPlaying()", "ch5").unwrap(),
        TjsValue::Integer(1)
    );

    // play by buffer id
    e.exec_script(
        r#"
        var b = new SoundBuffer(); b.open("one.wav");
        var id = b.getBufferId();
        ch.play(id);
        "#,
        "ch5",
    )
    .unwrap();
    assert_eq!(
        e.eval("ch.isPlaying()", "ch5").unwrap(),
        TjsValue::Integer(1)
    );
}

#[test]
fn channel_fade_ramps_volume() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "two.wav", &sine_wav_bytes(44100, 1, 220.0, 2.0));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    e.exec_script(
        r#"
        var ch = new SoundChannel();
        var b = new SoundBuffer();
        b.open("two.wav");
        ch.play(b);
        ch.fade(0.0, 1000);
        "#,
        "ch6",
    )
    .unwrap();
    advance(0.5); // halfway through the 1s fade
    let v = match e.eval("ch.getVolume()", "ch6").unwrap() {
        TjsValue::Real(v) => v,
        other => panic!("volume is a real, got {other:?}"),
    };
    assert!((v - 0.5).abs() < 0.01, "halfway fade volume {v}");

    advance(1.0); // fade completes
    assert_eq!(
        e.eval("ch.getVolume()", "ch6").unwrap(),
        TjsValue::Real(0.0)
    );
    assert_eq!(
        e.eval("ch.isPlaying()", "ch6").unwrap(),
        TjsValue::Integer(1),
        "fade does not stop playback"
    );
}

#[test]
fn channel_set_volume_cancels_fade() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "two.wav", &sine_wav_bytes(44100, 1, 220.0, 2.0));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    e.exec_script(
        r#"
        var ch = new SoundChannel();
        var b = new SoundBuffer();
        b.open("two.wav");
        ch.play(b);
        ch.fadeIn(1000);
        ch.setVolume(0.25);
        "#,
        "ch7",
    )
    .unwrap();
    advance(0.5); // the cancelled fade must not move the volume
    assert_eq!(
        e.eval("ch.getVolume()", "ch7").unwrap(),
        TjsValue::Real(0.25)
    );
}

#[test]
fn channel_play_errors() {
    let dir = TestDir::new();
    let storage = mounted(&dir, "a.wav", &sine_wav_bytes(44100, 1, 440.0, 0.1));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();

    let err = e
        .exec_script("var ch = new SoundChannel(); ch.play();", "ch8")
        .unwrap_err();
    assert!(err.to_string().contains("requires an argument"), "{err}");

    let err = e
        .exec_script(
            "var ch = new SoundChannel(); ch.play(\"missing.wav\");",
            "ch8",
        )
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");

    // An object argument resolves to the most recently opened buffer (the
    // ABI hands us an opaque object handle, not the object's identity; the
    // heuristic is documented in the crate docs). With a buffer open this
    // plays instead of erroring.
    e.exec_script(
        r#"
        var b = new SoundBuffer(); b.open("a.wav");
        ch.play(%[]);
        "#,
        "ch8",
    )
    .unwrap();
    assert_eq!(
        e.eval("ch.isPlaying()", "ch8").unwrap(),
        TjsValue::Integer(1)
    );
}
