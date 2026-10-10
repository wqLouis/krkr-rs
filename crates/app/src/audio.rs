//! L4 audio: install the L1 `media` decoder, own the L1 output device and
//! drain the L3 mixer to it each frame.
//!
//! `compat` (L3) never links a codec and never sees a device. Two seams cross
//! the boundary and this module wires both, playing the composition-root role:
//!
//! * [`install_audio_decoder`] installs the L1 `media` decoder on the
//!   `add_audio_decoder` chain so `WaveSoundBuffer` (and the legacy
//!   `CDDASoundBuffer`/`MIDISoundBuffer` classes) can decode and play;
//! * [`AudioOutput`] opens the L1 [`AudioDevice`] at game start and, once per
//!   frame, renders `dt` worth of the shared mixer through
//!   [`compat::sound_render`] and [`AudioDevice::write`]s it.
//!
//! The split is strict: L1 owns the device, L2 owns the mixing,
//! L3 renders on request, L4 only shuttles buffers between them.
//!
//! # Buffer/dt policy (the documented decision)
//!
//! The buffer is **dt-derived, not fixed-size**: each frame renders exactly
//! one chunk of `carry + dt_seconds × sample_rate` frames, keeping the
//! sub-sample remainder in `carry` for the next frame. The carry makes the
//! mapping sample-accurate over time — no rounding drift accumulates (a
//! 60 fps loop at 44.1 kHz renders 735 frames per frame forever; an odd rate
//! like 48 kHz/60 alternates without losing a sample).
//!
//! This is the correct tiling for this clock arrangement: `sound_poll` owns
//! the mixer clock (it advances to the L1 monotonic clock), so consecutive
//! chunks must cover exactly the elapsed `dt` for the output stream to be
//! contiguous with the mixer's positions. In steady state the chunks tile
//! gaplessly: no dropped audio, no repeated audio, and the L1 queue depth (and
//! therefore latency) stays constant — no silent drift. L1 pre-fills that queue
//! with roughly 150 ms before starting the device and holds the last per-channel
//! sample on underrun (see `platform`'s `audio_queue`), so the constant depth is
//! the reservoir plus one frame — independent of how the device callback is
//! sized.
//!
//! **When a frame runs long**, the chunk is capped at [`MAX_CHUNK_MS`]:
//!
//! * the mixer clock (`sound_poll`) is wall-driven and has already skipped
//!   the elapsed time, so content for the stall is unrecoverable anyway — the
//!   stream skips forward instead of queueing a backlog;
//! * the elapsed time beyond the cap is *dropped*, reported with a `warn`
//!   (never silently), and the fractional carry is reset with it;
//! * because the capped chunk is rendered from the mixer's *current* position,
//!   at most `MAX_CHUNK_MS` of already-banked content is replayed once right
//!   after such a frame. The repeat is bounded by the cap rather than growing
//!   with the stall, and the L1 queue additionally drops oldest samples beyond
//!   its two-second bound (documented in `platform`). So a hitch costs a
//!   bounded one-time skip/repeat, never unbounded latency growth.
//!
//! A `dt ≤ 0` renders nothing (the carry alone is under one frame); a
//! non-finite `dt` is warned about and skipped rather than turned into a
//! giant allocation.
//!
//! # Headless
//!
//! A headless host cannot assume an audio server exists, so it opens the L1
//! [`AudioDevice::null`] mock at [`HEADLESS_SPEC`]: it validates and exercises
//! the exact same call path (render + frame-aligned write) and discards
//! samples. It is the one legitimate no-op: it never panics and never blocks.
//! A windowed host asks the device for its own preferred format
//! ([`AudioDevice::default_spec`]); a device that cannot be queried or opened
//! is a typed [`PlatformError`] surfaced through [`crate::GameError::Audio`] —
//! never silence.

use std::sync::Arc;

use api::media::AudioSpec;
use compat::{CompatHost, add_audio_decoder, sound_render};
use engine::sound::SoundSource;
use media::{Media, decode_audio};
use platform::{AudioDevice, AudioOutputState, PlatformError};
use vm::VmHost;

use crate::GameError;

/// The format the headless (null) output device is opened with.
///
/// A headless host has no hardware to ask, so this is **chosen** rather than
/// inherited: 48 kHz stereo is the native rate of Android and most modern
/// desktop devices, so a headless run exercises the same device-rate resampling
/// a real run takes. The null device never reaches a speaker, so the value only
/// fixes the render/buffer arithmetic — and that arithmetic is rate-independent
/// (`AudioQueue` pre-fills by milliseconds and the chunk size is `dt × rate`).
const HEADLESS_SPEC: AudioSpec = AudioSpec {
    sample_rate: 48_000,
    channels: 2,
    total_frames: None,
};

/// Longest single chunk one frame may render (see the module docs).
///
/// A stall longer than this drops the excess (with a `warn`) instead of
/// banking an arbitrarily large buffer, so post-hitch latency and the one-time
/// content repeat are both bounded by this value.
const MAX_CHUNK_MS: f64 = 250.0;

/// Install the `media` audio decoder on `host`.
///
/// Call once, before any script runs. The decoder owns a clone of the
/// stateless [`Media`] backend.
pub fn install_audio_decoder(host: &CompatHost, media: &Media) {
    let media = media.clone();
    add_audio_decoder(
        host.services(),
        Box::new(move |bytes, loops, hint| {
            let audio = decode_audio(&media, bytes, hint).map_err(|error| error.to_string())?;
            let source = SoundSource::with_loops(Arc::new(audio), loops)
                .map_err(|error| error.to_string())?;
            Ok(Arc::new(source))
        }),
    );
}

/// The L1 audio device plus the per-frame render buffer (see the module docs
/// for the dt/buffer policy).
///
/// Owned by [`crate::GameHost`], whose field order drops this **before** the
/// VM: the device is stopped before the engine dies.
pub(crate) struct AudioOutput {
    /// The stateless L1 device handle (spec + backend).
    device: AudioDevice,
    /// The L1 stream/queue state the device writes through.
    state: AudioOutputState,
    /// The actual output spec (`AudioDevice::spec`), chosen by the device (or
    /// [`HEADLESS_SPEC`] when headless).
    spec: AudioSpec,
    /// Whether this is the null (headless) backend, for log context.
    null: bool,
    /// Reused per-frame render buffer (avoids a per-frame allocation).
    buffer: Vec<f32>,
    /// Sub-sample frame carry for the dt-derived chunk size (no drift).
    carry_frames: f64,
}

impl AudioOutput {
    /// Opens the output device at the format it prefers.
    ///
    /// `headless` selects the L1 null backend (the documented no-op for hosts
    /// with no audio hardware), opened at [`HEADLESS_SPEC`]. A windowed host
    /// asks the device for its default format ([`AudioDevice::default_spec`])
    /// and opens that — the mixer resamples every source to it — so no output
    /// rate is hardcoded here.
    ///
    /// # Errors
    ///
    /// [`PlatformError::Audio`] — the typed L1 error when the device cannot be
    /// queried or opened (a device that cannot open fails loudly; the null
    /// backend is the only no-op).
    pub(crate) fn open(headless: bool) -> Result<Self, PlatformError> {
        let requested = if headless {
            HEADLESS_SPEC
        } else {
            AudioDevice::default_spec()?
        };
        let (device, state) = if headless {
            AudioDevice::null(requested)?
        } else {
            AudioDevice::open(requested)?
        };
        let spec = device.spec();
        log::info!(
            "audio: opened {} device at {} Hz/{} ch",
            if headless { "null" } else { "output" },
            spec.sample_rate,
            spec.channels,
        );
        Ok(Self {
            device,
            state,
            spec,
            null: headless,
            buffer: Vec::new(),
            carry_frames: 0.0,
        })
    }

    /// Renders one dt-derived chunk of the shared mixer and queues it.
    ///
    /// `vm` supplies the bound compat context the L3 `sound_render` seam
    /// borrows; `dt_ms` is the frame's elapsed time. The chunk policy is
    /// documented on the module: `carry + dt` frames, capped at
    /// [`MAX_CHUNK_MS`] with a `warn` when the frame runs long.
    ///
    /// # Errors
    ///
    /// [`GameError::Vm`] when the compat context is not bound or L3 rendering
    /// fails (a malformed output spec), [`GameError::Audio`] when the L1
    /// device rejects the write — both typed, never silent.
    pub(crate) fn pump(&mut self, vm: &VmHost, dt_ms: f64) -> Result<(), GameError> {
        let rate = f64::from(self.spec.sample_rate);
        let due = self.carry_frames + dt_ms.max(0.0) * rate / 1000.0;
        if !due.is_finite() {
            self.carry_frames = 0.0;
            log::warn!("audio: non-finite frame duration; skipping this buffer");
            return Ok(());
        }
        let mut frames = due.floor();
        if frames < 1.0 {
            // Sub-sample remainder only: carry it and render nothing.
            self.carry_frames = due;
            return Ok(());
        }
        self.carry_frames = due - frames;

        let cap = rate * MAX_CHUNK_MS / 1000.0;
        if frames > cap {
            let dropped_ms = (frames - cap) * 1000.0 / rate;
            log::warn!(
                "audio: {:.1} ms frame exceeds the {MAX_CHUNK_MS:.0} ms chunk cap; \
                 dropping {dropped_ms:.1} ms of output",
                dt_ms
            );
            frames = cap;
            self.carry_frames = 0.0;
        }
        let frames = frames as usize;

        self.buffer
            .resize(frames * usize::from(self.spec.channels), 0.0);
        let ctx = vm.compat().ctx().map_err(vm::VmError::from)?;
        sound_render(&ctx, &mut self.buffer, self.spec).map_err(vm::VmError::from)?;
        self.device.write(&mut self.state, &self.buffer)?;
        log::debug!(
            "audio: wrote {frames} frames ({:.1} ms) to the {} device",
            frames as f64 * 1000.0 / rate,
            if self.null { "null" } else { "output" },
        );
        Ok(())
    }
}
