//! Tests for the asynchronous/streaming audio path ([`tvp_sound::source`]):
//! background decode, play-before-ready, cancellation across
//! stop/reopen/destroy, and bounded memory for long tracks.
//!
//! These tests drive `open_track` + the clock-driven [`Mixer`] directly (no
//! TJS VM), so they are deterministic and avoid the process-global engine
//! constraint. They share the global `active_decode_workers()` counter, so a
//! process-wide lock serializes them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use engine::Storage;
use tvp_sound::{Mixer, active_decode_workers, open_track};

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("tvp-sound-async-{}-{}", std::process::id(), stamp));
        std::fs::create_dir_all(&dir).expect("create temp dir");
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

/// A 16-bit PCM RIFF/WAVE container.
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

fn sine_wav_bytes(rate: u32, channels: u16, freq: f64, seconds: f64) -> Vec<u8> {
    let frames = (f64::from(rate) * seconds) as usize;
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

fn mounted(dir: &TestDir, name: &str, bytes: &[u8]) -> Arc<Mutex<Storage>> {
    std::fs::write(dir.path().join(name), bytes).expect("write fixture");
    Arc::new(Mutex::new(
        Storage::mount(dir.path()).expect("mount storage"),
    ))
}

fn wait_for<F: FnMut() -> bool>(timeout: Duration, mut pred: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    pred()
}

/// `open_track` must return promptly (before the whole track is decoded)
/// and the streaming source must become playable through the mixer.
#[test]
fn async_open_streams_and_plays() {
    let _lock = test_lock();
    let dir = TestDir::new();
    // 60 s of 44.1 kHz mono is ~10.6 MB decoded PCM, above the 8 MB
    // whole-file cap, so it streams.
    let storage = mounted(&dir, "long.wav", &sine_wav_bytes(44100, 1, 440.0, 60.0));

    let start = Instant::now();
    let track = open_track(&storage, "long.wav").expect("open track");
    let open_elapsed = start.elapsed();
    assert!(
        track.is_streaming(),
        "long track must stream, not materialize"
    );
    assert_eq!(
        track.resident_pcm_bytes(),
        0,
        "no whole-file PCM is materialized for a streaming track"
    );
    assert!(
        open_elapsed < Duration::from_secs(5),
        "open must return promptly, took {open_elapsed:?}"
    );

    // Wait until the worker has buffered some audio, then render it.
    assert!(
        wait_for(Duration::from_secs(30), || track.buffered_frames() > 0),
        "streaming worker never produced frames"
    );

    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play_track(track.clone());
    assert!(
        mixer.channel_ref(id).unwrap().is_playing(),
        "a ready streaming track must be playable"
    );
    let mut out = vec![0.0f32; 1024];
    mixer.advance(0.0);
    mixer.render_mix(&mut out, 44100, 1);
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.1, "streamed audio renders non-silent: peak {peak}");
}

/// A decode in flight must survive `stop()` (the worker keeps buffering),
/// and dropping/reopening the track must cancel the old worker.
#[test]
fn decode_survives_stop_reopen_and_destroy() {
    let _lock = test_lock();
    let dir = TestDir::new();
    let storage = mounted(&dir, "long.wav", &sine_wav_bytes(44100, 1, 220.0, 60.0));
    let baseline = active_decode_workers();

    let track = open_track(&storage, "long.wav").expect("open track");
    assert!(
        wait_for(Duration::from_secs(10), || active_decode_workers()
            > baseline),
        "decode worker did not start"
    );

    // stop() must not cancel the decode.
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play_track(track.clone());
    mixer.channel(id).unwrap().stop();
    assert!(!mixer.channel_ref(id).unwrap().is_playing());
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        track.buffered_frames() > 0,
        "the worker kept decoding after stop()"
    );

    // Dropping the channel and the track destroys the source: the worker
    // must exit.
    mixer.remove_channel(id);
    drop(mixer);
    drop(track);
    assert!(
        wait_for(Duration::from_secs(10), || active_decode_workers()
            == baseline),
        "worker not cancelled on destroy (active={})",
        active_decode_workers()
    );

    // Reopening starts a fresh worker and works.
    let track2 = open_track(&storage, "long.wav").expect("reopen track");
    assert!(
        wait_for(Duration::from_secs(10), || active_decode_workers()
            > baseline),
        "reopened worker did not start"
    );
    assert!(
        wait_for(Duration::from_secs(30), || track2.buffered_frames() > 0),
        "reopened worker never produced frames"
    );

    // Destroying the reopened track also cancels its worker.
    drop(track2);
    assert!(
        wait_for(Duration::from_secs(10), || active_decode_workers()
            == baseline),
        "worker not cancelled on destroy (active={})",
        active_decode_workers()
    );
}

/// A long track's resident PCM must stay bounded by the ring capacity, not
/// grow with the track length.
#[test]
fn long_track_memory_is_bounded() {
    let _lock = test_lock();
    let dir = TestDir::new();
    let storage = mounted(&dir, "long.wav", &sine_wav_bytes(44100, 1, 440.0, 120.0));
    let track = open_track(&storage, "long.wav").expect("open track");

    assert!(track.is_streaming());
    let total = track.total_frames().expect("wav frame count");
    let cap = track.ring_capacity_frames();
    assert_eq!(
        cap,
        44100 * u64::from(tvp_sound::source::STREAM_AHEAD_SECONDS),
        "ring capacity is the decode-ahead window"
    );
    assert!(
        total > cap * 10,
        "test track ({total} frames) must be much longer than the ring ({cap})"
    );

    // Let the worker fill the ring and stay blocked on a full buffer.
    assert!(
        wait_for(Duration::from_secs(30), || track.buffered_frames() >= cap),
        "ring never filled (buffered={})",
        track.buffered_frames()
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        track.buffered_frames() <= cap,
        "ring overflowed: {} > {cap}",
        track.buffered_frames()
    );
    assert!(
        track.buffered_frames() < total,
        "the whole track was materialized despite streaming"
    );
    assert_eq!(
        track.resident_pcm_bytes(),
        0,
        "streaming tracks hold no whole-file buffer"
    );
}

/// Regression for the async streaming-decode change: a long track that is
/// `open`ed and `play`ed *before* its worker has decoded anything must start
/// at the beginning (position 0), never let the mixer clock run past the
/// decoded watermark, stay audible through the whole track, and finish at
/// the real end.
///
/// This drives [`Mixer::render_mix_advancing`] (the device/audio-callback
/// path) rather than `advance` + `render_mix`: with the old
/// `render_mix`-then-`advance` mix a channel whose ring was empty (the
/// `open`→`play` race, or a worker slower than the clock) advanced its
/// position anyway, dropping every frame the worker later published and
/// ending in permanent silence.
#[test]
fn streaming_playback_starts_at_zero_stays_audible_and_ends() {
    let _lock = test_lock();
    let dir = TestDir::new();
    let seconds = 60.0;
    let storage = mounted(&dir, "long.wav", &sine_wav_bytes(44100, 1, 440.0, seconds));
    let track = open_track(&storage, "long.wav").expect("open track");
    assert!(
        track.is_streaming(),
        "60 s of 44.1 kHz PCM must exceed the whole-file cap"
    );

    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    // Play immediately: this is the game's `open`→`play` sequence.
    mixer.channel(id).unwrap().play_track(track.clone());

    let rate = 44100u32;
    let chunk_frames = 4410usize; // 100 ms
    let mut out = vec![0.0f32; chunk_frames];
    let mut first_sound_at: Option<f64> = None;
    let mut audible_blocks = 0usize;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        // The playback position must never lead the decoded audio.
        let pos = mixer.channel_ref(id).unwrap().position_seconds;
        let available = track
            .available_end_seconds()
            .expect("streaming track has a watermark");
        assert!(
            pos <= available + 1e-6,
            "position {pos:.6}s ran past the decoded watermark {available:.6}s"
        );
        if mixer.channel_ref(id).unwrap().done {
            break;
        }
        if available <= pos {
            // The worker has not published the next frames yet: wait for it,
            // like the real device callback would just render silence.
            assert!(
                Instant::now() < deadline,
                "streaming worker never produced the rest of the track"
            );
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        mixer.render_mix_advancing(&mut out, rate, 1);
        let rms = (out
            .iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>()
            / out.len() as f64)
            .sqrt();
        assert!(
            rms > 1e-4,
            "streaming playback went silent at position {pos:.3}s"
        );
        if first_sound_at.is_none() {
            first_sound_at = Some(mixer.channel_ref(id).unwrap().position_seconds);
        }
        audible_blocks += 1;
        assert!(
            Instant::now() < deadline,
            "streaming playback never reached the end"
        );
    }

    let first = first_sound_at.expect("stream produced no audio at all");
    assert!(
        first < 0.5,
        "stream skipped its head: first audio at {first:.3}s instead of 0"
    );
    assert!(
        audible_blocks > 500,
        "stream had only {audible_blocks} audible 100 ms blocks of 600"
    );
    let duration = track.known_duration_seconds().expect("wav duration");
    let final_pos = mixer.channel_ref(id).unwrap().position_seconds;
    assert!(
        (final_pos - duration).abs() < 0.2,
        "stream ended at {final_pos:.3}s, expected {duration:.3}s"
    );
}

/// End-to-end sample correctness for the streaming ring: the fixture is a
/// 60 s ramp whose sample value is `frame / total_frames`, so a mixer that
/// reads the wrong ring slot (the original `push` wrote modulo capacity but
/// `frame` read linearly from `base`) or skips the head produces values that
/// do not match the playback position. This is the end-to-end counterpart of
/// [`source::tests::ring_reads_back_written_frames_across_release_and_wrap`].
#[test]
fn streaming_rendered_samples_match_playback_position() {
    let _lock = test_lock();
    let dir = TestDir::new();
    let rate = 44100u32;
    let seconds = 60.0;
    let frames = (f64::from(rate) * seconds) as usize;
    let samples: Vec<i16> = (0..frames)
        .map(|i| ((i as f64 / frames as f64) * 32767.0) as i16)
        .collect();
    let storage = mounted(&dir, "ramp.wav", &wav_pcm16(rate, 1, &samples));
    let track = open_track(&storage, "ramp.wav").expect("open track");
    assert!(track.is_streaming());

    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play_track(track.clone());

    let chunk_frames = 4410usize;
    let mut out = vec![0.0f32; chunk_frames];
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut checked = 0usize;
    while checked < 200 {
        let pos = mixer.channel_ref(id).unwrap().position_seconds;
        let available = track.available_end_seconds().unwrap();
        if available <= pos {
            assert!(Instant::now() < deadline, "streaming worker stalled");
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        mixer.render_mix_advancing(&mut out, rate, 1);
        // The first sample of the chunk must encode the playback position.
        let expected = (pos / seconds) as f32;
        assert!(
            (out[0] - expected).abs() < 0.02,
            "sample at {pos:.3}s was {} but the ramp expects {expected:.3} \
             (wrong ring slot or skipped head)",
            out[0]
        );
        checked += 1;
    }
}
