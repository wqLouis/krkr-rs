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

use engine::Storage;
use tjs2_sys::{Tjs2Engine, TjsValue};
use tvp_sound::{advance, register_sound};

/// The real game installed on this dev machine (context: `PlayBgm("BGM02")`
/// resolves to `bgm/bgm02.ogg` inside `data.xp3`).
const REAL_GAME_DIR: &str = "/mnt/DATA/Games/Others/test";

/// A real BGM file that actually exists in the game (verified via
/// `scripts/extract_xp3.py`).
const REAL_BGM: &str = "bgm/bgm02.ogg";

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
    //    SoundChannel()` → `ch.play(name)` decodes on the fly and hands the
    //    channel the source. Advance the clock 1.5s so the channel is
    //    mid-playback.
    e.exec_script(
        r#"
        var ch = new SoundChannel();
        ch.setVolume(1.0);
        ch.play("bgm/bgm02.ogg");
        "#,
        "realbgm",
    )
    .unwrap();
    advance(1.5);
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
