//! # tvp-sound — decoding + clock-driven playback + the TVP sound natives
//!
//! The audio module of krkr-rs, in three layers:
//!
//! 1. **[`decode`]** — `decode_audio(storage, name)` turns a storage entry
//!    (wav / ogg / vorbis / flac / mp3) into fully-decoded interleaved
//!    `f32` PCM ([`DecodedAudio`]).
//! 2. **[`mixer`]** — a global clock-driven [`Mixer`] of [`Channel`]s.
//!    Playback is **pure computation**: the app's update loop calls
//!    [`advance`] (or `Mixer::advance(dt)`) each frame, which moves
//!    positions, applies fades, and sets per-channel "done" state. No audio
//!    device is involved — this machine is headless and everything must
//!    work without one. [`player`] is the optional rodio output half.
//! 3. **[`natives`]** — `register_sound(engine, storage)` registers the
//!    `SoundBuffer` and `SoundChannel` native classes. `SoundBuffer`
//!    decodes a storage entry; `SoundChannel` is a thin handle onto a mixer
//!    channel (`play`/`stop`/`pause`/`resume`, position, volume `0..1`,
//!    pan `-1..1`, loop, fades, `isPlaying`/`isDone`).
//!
//! # Decoder choice (documented)
//!
//! The task allows rodio for playback with symphonia for decode (or
//! symphonia directly when rodio's feature surface is awkward). We do
//! **both**: `symphonia` directly for decode (its API fits headless
//! decode-to-memory, and its feature names are precise: `wav`/`pcm`/`ogg`/
//! `vorbis`/`flac`/`mp3`), and `rodio = "0.20"` for the output half. rodio
//! 0.20 uses the same symphonia 0.5, so Cargo unifies the crates and the
//! features enabled here also upgrade rodio's copy.
//!
//! # Volume/pan scale
//!
//! This wave's script surface uses `volume 0..=1` and `pan -1..=1` (the
//! task's spec). The reference engine uses `0..=100000` for both
//! (`WaveSoundBuffer`/`SoundChannel`); porting the game's `system/sound.tjs`
//! later means dividing/multiplying by 100000 at that boundary.
//!
//! # Testing
//!
//! Headless by construction: the mixer and decode tests never open a
//! device, and [`player`] is exercised only by compilation. The VM is not
//! thread-safe, so tests that spin up a `Tjs2Engine` must run with
//! `--test-threads=1` (workspace convention).

pub mod decode;
pub mod mixer;
pub mod natives;
pub mod player;

mod ffi;

pub use decode::{DecodeError, DecodedAudio, decode_audio, decode_audio_bytes};
pub use mixer::{Channel, Fade, Mixer};
pub use natives::register_sound;

use std::sync::{Arc, LazyLock, Mutex};

/// The process-wide mixer that [`advance`] drives.
///
/// Created by [`register_sound`]; the app's update loop calls [`advance`]
/// once per frame with a monotonic timestamp. `None` before any
/// `register_sound` (or in pure-mixer tests that build their own mixer).
static GLOBAL_MIXER: LazyLock<Mutex<Option<Arc<Mutex<Mixer>>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Point [`advance`] at a mixer (called by [`register_sound`]).
pub fn set_global_mixer(mixer: Arc<Mutex<Mixer>>) {
    *mixer::lock_ok(&GLOBAL_MIXER) = Some(mixer);
}

/// The current global mixer, if one has been set.
pub fn global_mixer() -> Option<Arc<Mutex<Mixer>>> {
    mixer::lock_ok(&GLOBAL_MIXER).clone()
}

/// Drive the global mixer forward to `now_seconds`.
///
/// The app calls this once per frame with a **monotonic** timestamp in
/// seconds (e.g. `Instant::elapsed().as_secs_f64()`); the mixer advances
/// each playing channel by the elapsed delta and flips per-channel "done"
/// state. No-op before [`register_sound`] set a mixer.
pub fn advance(now_seconds: f64) {
    if let Some(m) = global_mixer() {
        mixer::lock_ok(&m).advance_to(now_seconds);
    }
}
