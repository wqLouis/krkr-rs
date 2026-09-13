//! Probe test: decode a real `.ogg` from the actual game at
//! `/mnt/DATA/Games/Others/test` (mounted like the app does — the storage
//! layer auto-opens every `*.xp3`), play it on the clock-driven mixer
//! through the native `WaveSoundBuffer` path, advance the clock, and prove
//! that non-silent PCM actually comes out of [`tvp_sound::Mixer::render_mix`].
//!
//! This is the end-to-end "decode + mixer" probe: if this passes, the
//! pipeline from `PlayBgm("BGM02")` → storage → symphonia decode → mixer →
//! PCM is fully working, and the only thing between the PCM and the
//! speakers is the (separately wired) rodio output half.
//!
//! Like [`sound.rs`], the TJS2 VM is single-threaded and the natives
//! install a process-global mixer, so this test takes the same process-wide
//! [`VM_LOCK`]. The real game dir is a stable absolute path on the dev
//! machine; the test fails loudly (with the dir name in the message) if it
//! is missing rather than silently passing.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::Storage;
use tjs2_sys::{Tjs2Engine, TjsValue};
use tvp_sound::{advance, register_sound};

/// The real game installed on this dev machine (context: `PlayBgm("BGM02")`
/// resolves to `bgm/bgm02.ogg` inside `data.xp3`).
const REAL_GAME_DIR: &str = "/mnt/DATA/Games/Others/test";

/// A real BGM file that actually exists in the game (verified via
/// `scripts/extract_xp3.py`).
const REAL_BGM: &str = "bgm/bgm02.ogg";

/// A real Ogg Opus voice file that actually exists in the game. The game's
/// `voice/*.ogg` are Opus (`OpusHead`), not Vorbis — this is the regression
/// fixture for the `symphonia-adapter-libopus` decoder registration.
const REAL_VOICE_OPUS: &str = "voice/azs000001.ogg";

/// One process-wide lock serializing every test: the TJS2 VM and the
/// global mixer are not thread-safe (same pattern as tests/sound.rs).
static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn vm_lock() -> std::sync::MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Mount the real game directory (auto-opens `data.xp3` + `patch.xp3`).
fn mount_real_game() -> Arc<Mutex<Storage>> {
    let dir = Path::new(REAL_GAME_DIR);
    assert!(
        dir.is_dir(),
        "real game dir {REAL_GAME_DIR:?} not found — this probe test needs it mounted"
    );
    Arc::new(Mutex::new(Storage::mount(dir).expect("mount real game")))
}

#[test]
fn real_game_bgm_ogg_decodes_to_audible_pcm() {
    let _vm_lock = vm_lock();
    let storage = mount_real_game();

    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage.clone()).unwrap();

    // 1. Decode the real BGM through the storage layer (the exact path
    //    `WaveSoundBuffer.open`/`SoundChannel.play(name)` take).
    let audio = tvp_sound::decode_audio(&storage, REAL_BGM).expect("decode real bgm ogg");
    assert!(
        audio.sample_rate > 0 && audio.channels > 0,
        "decoded BGM must have a sample rate and channels: {audio:?}"
    );
    let duration = audio.duration_seconds();
    assert!(
        duration > 1.0,
        "real BGM should be more than a second of audio, got {duration}s"
    );
    eprintln!(
        "real BGM {REAL_BGM}: {} Hz, {} ch, {:.2}s, {} samples",
        audio.sample_rate,
        audio.channels,
        duration,
        audio.samples.len()
    );

    // 2. Play it on a mixer channel the way the game does: `new
    //    SoundChannel()` → `ch.play(name)` starts the background decode and
    //    hands the channel the source. A long BGM streams, so wait until its
    //    decoder has buffered enough for the 1.5s playback position before
    //    asserting/rendering.
    e.exec_script(
        r#"
        var ch = new SoundChannel();
        ch.setVolume(1.0);
        ch.play("bgm/bgm02.ogg");
        "#,
        "realbgm",
    )
    .unwrap();
    {
        let mixer = tvp_sound::global_mixer().expect("register_sound set a global mixer");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            advance(1.5);
            let mut probe = vec![0.0f32; 4410 * 2];
            {
                let m = mixer.lock().unwrap_or_else(|p| p.into_inner());
                m.render_mix(&mut probe, 44100, 2);
            }
            let peak = probe.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            if peak > 0.01 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "real BGM never buffered audio at 1.5s (peak {peak})"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    assert_eq!(
        e.eval("ch.isPlaying()", "realbgm").unwrap(),
        TjsValue::Integer(1),
        "BGM channel must be playing after 1.5s"
    );
    assert_eq!(
        e.eval("ch.isDone()", "realbgm").unwrap(),
        TjsValue::Integer(0)
    );

    // 3. Render 1s of the live mix at the device-ish rate and prove real,
    //    non-silent PCM comes out.
    let mut pcm = vec![0.0f32; 44100 * 2]; // 1s stereo @ 44.1kHz
    {
        let mixer = tvp_sound::global_mixer().expect("register_sound set a global mixer");
        let mixer = mixer.lock().unwrap_or_else(|p| p.into_inner());
        // The global mixer's clock is at 1.5s here; rendering the current
        // mix samples the channels' decoded PCM at their playback position.
        mixer.render_mix(&mut pcm, 44100, 2);
    }

    // Real BGM is not silent: assert a healthy RMS over the rendered second.
    let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
    let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    eprintln!("rendered 1s: peak={peak:.4} rms={rms:.4}");
    assert!(
        peak > 0.01,
        "real BGM renders audible PCM: peak {peak} is silence"
    );
    assert!(
        rms > 0.001,
        "real BGM renders audible PCM: rms {rms} is silence"
    );
}

/// Regression test for real **Ogg Opus** decoding through [`tvp_sound::decode_audio`].
///
/// The game's voices are `OpusHead` Ogg files; symphonia's default registry has
/// no Opus decoder, so before `symphonia-adapter-libopus` was registered these
/// decoded to a ~60 ms silent fallback buffer (48000 Hz / 2 ch). Asserting the
/// real voice's actual shape (mono, multi-second, non-silent) proves the real
/// Opus decoder is wired and produces audible PCM.
#[test]
fn real_game_opus_voice_decodes_to_audible_pcm() {
    let _vm_lock = vm_lock();
    let storage = mount_real_game();

    let audio = tvp_sound::decode_audio(&storage, REAL_VOICE_OPUS)
        .unwrap_or_else(|e| panic!("decode real Opus voice {REAL_VOICE_OPUS}: {e}"));

    // Not the silent fallback: it would be 2ch stereo at ~60 ms. The real
    // voice is mono and 3+ seconds long.
    assert_eq!(
        audio.channels, 1,
        "Opus voice must decode to its native mono, got {audio:?} (silent fallback?)"
    );
    let duration = audio.duration_seconds();
    assert!(
        duration > 3.0,
        "real Opus voice should be >3s of audio, got {duration}s (silent fallback?)"
    );
    eprintln!(
        "real Opus voice {REAL_VOICE_OPUS}: {} Hz, {} ch, {:.2}s, {} frames",
        audio.sample_rate,
        audio.channels,
        duration,
        audio.frames()
    );

    // Audible: a meaningful share of samples must be non-zero (the probe
    // found all 155551 samples non-zero; allow headroom for quiet passages).
    let nonzero = audio.samples.iter().filter(|&&s| s != 0.0).count();
    assert!(
        nonzero * 10 >= audio.samples.len() * 9,
        "Opus voice PCM is mostly zero ({nonzero}/{}) — silent decode?",
        audio.samples.len()
    );
    let peak = audio.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.01, "Opus voice peak {peak} is silence");

    // End-to-end: play it on a mixer channel like the game's voice sequencing
    // does and prove non-silent PCM comes out of the live mix.
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage.clone()).unwrap();
    e.exec_script(
        &format!(r#"var vch = new SoundChannel(); vch.play("{REAL_VOICE_OPUS}");"#),
        "realvoice",
    )
    .unwrap();
    // The voice decodes asynchronously; wait until it is ready to play.
    let mut ready = false;
    for _ in 0..10_000 {
        if matches!(
            e.eval("vch.isPlaying()", "realvoice"),
            Ok(TjsValue::Integer(1))
        ) {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(ready, "Opus voice never became ready");
    advance(1.0);
    assert_eq!(
        e.eval("vch.isPlaying()", "realvoice").unwrap(),
        TjsValue::Integer(1),
        "Opus voice channel must still be playing after 1s (silent fallback would be done)"
    );
    let mut pcm = vec![0.0f32; 44100 * 2];
    {
        let mixer = tvp_sound::global_mixer().expect("register_sound set a global mixer");
        let mixer = mixer.lock().unwrap_or_else(|p| p.into_inner());
        mixer.render_mix(&mut pcm, 44100, 2);
    }
    let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
    eprintln!("rendered 1s of Opus voice mix: rms={rms:.4}");
    assert!(
        rms > 0.0005,
        "rendered Opus voice mix is silence: rms {rms}"
    );
}

/// Regression for the async streaming-decode change on the **real game
/// BGM**: `open`+`play` before the worker has produced anything must not let
/// the mixer clock run past the decoded watermark (the old
/// `render_mix`→`advance` order could race ahead, drop every frame and play
/// permanent silence), and playback must start at the beginning of the track
/// instead of skipping its head.
#[test]
fn real_game_bgm_streams_from_the_start() {
    let _vm_lock = vm_lock();
    let storage = mount_real_game();
    let track = tvp_sound::open_track(&storage, REAL_BGM).expect("open real bgm");
    assert!(
        track.is_streaming(),
        "the 122 s real BGM must take the bounded streaming path"
    );

    let mut mixer = tvp_sound::Mixer::new();
    let id = mixer.spawn_channel();
    // Play immediately (the game's `open`→`play` sequence, no wait).
    mixer.channel(id).unwrap().play_track(track.clone());

    let rate = 44100u32;
    let mut out = vec![0.0f32; 4410 * 2]; // 100 ms stereo
    let mut first_sound_at: Option<f64> = None;
    let mut audible_blocks = 0usize;
    let mut rendered_blocks = 0usize;
    let deadline = Instant::now() + Duration::from_secs(30);
    while rendered_blocks < 50 {
        let pos = mixer.channel_ref(id).unwrap().position_seconds;
        let available = track
            .available_end_seconds()
            .expect("streaming BGM has a decoded watermark");
        assert!(
            pos <= available + 1e-6,
            "BGM position {pos:.6}s ran past the decoded watermark {available:.6}s"
        );
        if available <= pos {
            assert!(
                Instant::now() < deadline,
                "BGM streaming worker never produced the next frames"
            );
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        mixer.render_mix_advancing(&mut out, rate, 2);
        rendered_blocks += 1;
        let rms = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        if rms > 1e-4 {
            if first_sound_at.is_none() {
                first_sound_at = Some(mixer.channel_ref(id).unwrap().position_seconds);
            }
            audible_blocks += 1;
        }
    }

    let first = first_sound_at.expect("real BGM never produced any audio (permanent silence)");
    assert!(
        first < 1.0,
        "real BGM skipped its head: first audio at {first:.3}s"
    );
    assert!(
        audible_blocks > 20,
        "real BGM had only {audible_blocks} audible 100 ms blocks out of 50"
    );
}

/// Regression: a real short Opus voice (a whole-file decode) plays to its
/// end on the device clock, with non-silent PCM throughout and a final
/// position at the real duration.
#[test]
fn real_game_opus_voice_plays_fully() {
    let _vm_lock = vm_lock();
    let storage = mount_real_game();
    let track = tvp_sound::open_track(&storage, REAL_VOICE_OPUS).expect("open real voice");
    assert!(
        !track.is_streaming(),
        "the 3 s voice must take the whole-file path"
    );
    let duration = track.known_duration_seconds().expect("voice duration");

    let mut mixer = tvp_sound::Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play_track(track.clone());

    // Wait for the background decode to publish the whole buffer.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !track.is_ready() {
        assert!(Instant::now() < deadline, "voice decode never finished");
        std::thread::sleep(Duration::from_millis(1));
    }

    let rate = 44100u32;
    let mut out = vec![0.0f32; 4410]; // 100 ms mono
    let mut audible_blocks = 0usize;
    while !mixer.channel_ref(id).unwrap().done {
        mixer.render_mix_advancing(&mut out, rate, 1);
        let rms = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        if rms > 1e-4 {
            audible_blocks += 1;
        }
        assert!(Instant::now() < deadline, "voice never reached its end");
    }

    // 3.24 s in 100 ms blocks is ~32; the last block is partial.
    assert!(
        audible_blocks >= 25,
        "voice had only {audible_blocks} audible 100 ms blocks"
    );
    let final_pos = mixer.channel_ref(id).unwrap().position_seconds;
    assert!(
        (final_pos - duration).abs() < 0.2,
        "voice ended at {final_pos:.3}s, expected {duration:.3}s"
    );
}
