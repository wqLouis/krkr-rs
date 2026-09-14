//! # tvp-sound — decoding + clock-driven playback + the TVP sound natives
//!
//! The audio module of krkr-rs, in four layers:
//!
//! 1. **[`decode`]** — `decode_audio(storage, name)` turns a storage entry
//!    (wav / ogg / vorbis / flac / mp3) into fully-decoded interleaved
//!    `f32` PCM ([`DecodedAudio`]). It also exposes `probe_audio` and
//!    [`StreamDecoder`], the building blocks of the asynchronous path.
//! 2. **[`source`]** — [`AudioTrack`]: asynchronous loading. `open_track`
//!    reads and probes on the calling thread, then a background worker
//!    decodes short sounds whole and long tracks into a bounded decode-ahead
//!    ring, so `open` never blocks on the packet decode and long BGM uses
//!    bounded memory.
//! 3. **[`mixer`]** — a global clock-driven [`Mixer`] of [`Channel`]s.
//!    Playback is **pure computation**: the app's update loop calls
//!    [`advance`] (or `Mixer::advance(dt)`) each frame, which moves
//!    positions, applies fades, and sets per-channel "done" state. No audio
//!    device is involved — this machine is headless and everything must
//!    work without one. [`player`] is the optional rodio output half.
//! 4. **[`natives`]** — `register_sound(engine, storage)` registers the
//!    `SoundBuffer` and `SoundChannel` native classes. `SoundBuffer`
//!    loads a storage entry asynchronously; `SoundChannel` is a thin handle
//!    onto a mixer channel (`play`/`stop`/`pause`/`resume`, position, volume
//!    `0..1`, pan `-1..1`, loop, fades, `isPlaying`/`isDone`).
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
//! # Volume/pan scale and loop points
//!
//! The game's `WaveSoundBuffer` surface (`system/sound.tjs` derives
//! `SoundBuffer` from it) uses the reference's `0..=100000` scale for
//! `volume`/`volume2`/`pan`, and `position`/`totalTime` in **milliseconds**.
//! The task-level `SoundChannel` class keeps the simpler `0..=1` /
//! `-1..=1` scale and seconds-based positions.
//!
//! `WaveSoundBuffer.open(name)` also loads the `<name>.sli` side-car
//! ([`sli`]); a looping channel then loops the link's `To..From` region
//! instead of the whole file, matching the reference `Open` + loop manager.
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
pub mod sli;
pub mod source;
pub mod wavesound;

mod ffi;
mod waveflags;

pub use decode::{
    AudioMetadata, DecodeError, DecodedAudio, StreamDecoder, decode_audio, decode_audio_bytes,
    probe_audio,
};
pub use mixer::{Channel, Fade, Mixer};
pub use natives::register_sound;
pub use natives::set_sound_output_enabled;
pub use sli::{LoopCondition, LoopLink, SliInfo, WaveLabel};
pub use source::{AudioTrack, active_decode_workers, open_track, open_track_bytes};
pub use wavesound::{active_stream_count, sound_poll};

pub use player::{
    MainThreadOutputGuard, MixerSource, OutputError, OutputGuard, OutputStatus, start_output,
    start_output_on_main_thread,
};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// Whether a real audio device is streaming the mixer. When it is, the
/// audio callback drives playback positions (rendering chunk-by-chunk) and
/// [`advance`] must not also jump them to wall time — otherwise the two
/// clocks fight and the output stutters.
static AUDIO_OUTPUT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Mark whether a real audio device is streaming (see [`AUDIO_OUTPUT_ACTIVE`]).
pub fn set_audio_output_active(on: bool) {
    AUDIO_OUTPUT_ACTIVE.store(on, Ordering::Relaxed);
}

/// Whether a real audio device is currently streaming the mixer.
pub fn audio_output_active() -> bool {
    AUDIO_OUTPUT_ACTIVE.load(Ordering::Relaxed)
}

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
    // With a live device the callback is the clock; jumping positions to
    // wall time would double-advance and stutter. Headless still uses this.
    if audio_output_active() {
        return;
    }
    if let Some(m) = global_mixer() {
        mixer::lock_ok(&m).advance_to(now_seconds);
    }
}
