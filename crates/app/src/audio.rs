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
//! with roughly 150 ms before starting the device and, on underrun, fades the
//! last per-channel sample to silence over a few milliseconds (see `platform`'s
//! `audio_queue`), so the constant depth is the reservoir plus one frame —
//! independent of how the device callback is sized.
//!
//! **When a frame runs long**, the mixer clock (`sound_poll`) is wall-driven
//! and has already skipped the elapsed time while the L1 device drained its
//! pre-fill and faded to silence (see the L1 queue policy). The audio for
//! the stalled interval is therefore unrecoverable: a hitch is an audible
//! skip, not something this layer can render back. The chunk written after the
//! stall is capped at the configured maximum ([`DEFAULT_MAX_CHUNK_MS`] by
//! default, `KRKR_RS_AUDIO_MAX_CHUNK_MS`):
//!
//! * the elapsed time beyond the cap is *dropped*, reported with a `warn`
//!   (never silently), and the fractional carry is reset with it;
//! * because the capped chunk is rendered from the mixer's *current*
//!   (post-stall) position, at most the cap of already-banked content is
//!   replayed once right after such a frame. The repeat and the post-hitch
//!   latency are bounded by the cap instead of growing with the stall, and the
//!   L1 queue additionally drops oldest samples beyond its two-second bound.
//!
//! The cap is a latency/repeat trade-off, not a cure for the stall. A **small**
//! cap keeps the one-time repeat short, but rebuilds only a small reservoir
//! after the hitch, so the next slow frame underruns sooner. A **large** cap
//! covers more of the stall in one write and rebuilds a bigger reservoir, at
//! the cost of a longer repeat and more post-hitch latency. Removing the repeat
//! would mean letting the device drive the mixer's playback cursor instead of
//! the wall clock; the same cursor also drives fade/status timing, so that
//! change is not local to this module.
//!
//! The cap defaults to [`DEFAULT_MAX_CHUNK_MS`] and is overridden by the
//! `KRKR_RS_AUDIO_MAX_CHUNK_MS` environment variable (a positive, finite number
//! of milliseconds). An invalid value fails the device open with a typed error
//! rather than being ignored.
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

use api::error::AudioError;
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

/// The default longest single chunk one frame may render (see the module
/// docs).
///
/// A stall longer than this drops the excess (with a `warn`) instead of
/// banking an arbitrarily large buffer, so post-hitch latency and the one-time
/// content repeat are both bounded. `KRKR_RS_AUDIO_MAX_CHUNK_MS` overrides it.
const DEFAULT_MAX_CHUNK_MS: f64 = 250.0;

/// The environment variable that overrides [`DEFAULT_MAX_CHUNK_MS`].
const MAX_CHUNK_ENV: &str = "KRKR_RS_AUDIO_MAX_CHUNK_MS";

/// Reads and validates the configured chunk cap.
///
/// # Errors
///
/// [`PlatformError::Audio`] wrapping [`AudioError::InvalidSpec`] when the
/// variable is set to something other than a positive, finite number of
/// milliseconds. A configuration error is surfaced, never silently ignored.
fn configured_max_chunk_ms() -> Result<f64, PlatformError> {
    match std::env::var(MAX_CHUNK_ENV) {
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_MAX_CHUNK_MS),
        Err(std::env::VarError::NotUnicode(_)) => Err(AudioError::InvalidSpec {
            message: format!("{MAX_CHUNK_ENV} must be valid Unicode"),
        }
        .into()),
        Ok(raw) => {
            let ms: f64 = raw.parse().map_err(|_| AudioError::InvalidSpec {
                message: format!("{MAX_CHUNK_ENV} is not a number: {raw:?}"),
            })?;
            if !ms.is_finite() || ms <= 0.0 {
                return Err(AudioError::InvalidSpec {
                    message: format!(
                        "{MAX_CHUNK_ENV} must be a positive finite number of milliseconds: {raw:?}"
                    ),
                }
                .into());
            }
            Ok(ms)
        }
    }
}

/// One frame's planned mixer chunk.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ChunkPlan {
    /// Whole frames to render.
    frames: usize,
    /// Milliseconds of elapsed output deliberately not rendered (0 unless the
    /// frame exceeded the cap).
    dropped_ms: f64,
}

/// Plans one frame's mixer chunk.
///
/// `due_frames` is the carried sub-frame remainder plus this frame's elapsed
/// output, at `rate` Hz; `max_chunk_ms` bounds a single chunk. Returns the
/// whole frames to render and the milliseconds deliberately not rendered when
/// the frame exceeds the cap.
fn plan_chunk(due_frames: f64, max_chunk_ms: f64, rate: f64) -> ChunkPlan {
    let frames = due_frames.floor();
    if frames < 1.0 {
        return ChunkPlan {
            frames: 0,
            dropped_ms: 0.0,
        };
    }
    let cap = rate * max_chunk_ms / 1000.0;
    if frames > cap {
        ChunkPlan {
            frames: cap as usize,
            dropped_ms: (frames - cap) * 1000.0 / rate,
        }
    } else {
        ChunkPlan {
            frames: frames as usize,
            dropped_ms: 0.0,
        }
    }
}

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
    /// Longest single chunk one frame may render, in milliseconds (the
    /// configured cap; see the module docs).
    max_chunk_ms: f64,
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
    /// backend is the only no-op), or when `KRKR_RS_AUDIO_MAX_CHUNK_MS` is not
    /// a positive, finite number of milliseconds.
    pub(crate) fn open(headless: bool) -> Result<Self, PlatformError> {
        let max_chunk_ms = configured_max_chunk_ms()?;
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
            max_chunk_ms,
        })
    }

    /// Renders one dt-derived chunk of the shared mixer and queues it.
    ///
    /// `vm` supplies the bound compat context the L3 `sound_render` seam
    /// borrows; `dt_ms` is the frame's elapsed time. The chunk policy is
    /// documented on the module: `carry + dt` frames, capped at the
    /// configured maximum with a `warn` when the frame runs long.
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
        let plan = plan_chunk(due, self.max_chunk_ms, rate);
        if plan.frames == 0 {
            // Sub-sample remainder only: carry it and render nothing.
            self.carry_frames = due;
            return Ok(());
        }
        self.carry_frames = due - plan.frames as f64;
        if plan.dropped_ms > 0.0 {
            log::warn!(
                "audio: {dt_ms:.1} ms frame exceeds the {:.0} ms chunk cap; \
                 dropping {:.1} ms of output",
                self.max_chunk_ms,
                plan.dropped_ms
            );
            self.carry_frames = 0.0;
        }
        let frames = plan.frames;

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

#[cfg(test)]
mod tests {
    use super::{ChunkPlan, DEFAULT_MAX_CHUNK_MS, plan_chunk};

    /// A 1 kHz stream, so one millisecond is exactly one frame.
    const RATE: f64 = 1_000.0;
    /// A 10 ms cap keeps the 0/1x/5x arithmetic exact.
    const CAP_MS: f64 = 10.0;

    fn plan_for(frame_ms: f64) -> ChunkPlan {
        plan_chunk(frame_ms * RATE / 1000.0, CAP_MS, RATE)
    }

    #[test]
    fn the_default_cap_is_250_ms() {
        assert_eq!(DEFAULT_MAX_CHUNK_MS, 250.0);
    }

    #[test]
    fn a_frame_shorter_than_one_frame_renders_nothing() {
        // 0x the cap: the elapsed time is sub-frame and stays in the carry.
        let plan = plan_for(0.0);
        assert_eq!(plan.frames, 0);
        assert_eq!(plan.dropped_ms, 0.0);
    }

    #[test]
    fn a_frame_at_the_cap_renders_the_whole_chunk() {
        // 1x the cap: the whole frame renders, nothing is dropped.
        let plan = plan_for(CAP_MS);
        assert_eq!(plan.frames, CAP_MS as usize);
        assert_eq!(plan.dropped_ms, 0.0);
    }

    #[test]
    fn a_frame_five_times_the_cap_drops_the_excess() {
        // 5x the cap: render one cap and drop the other four.
        let plan = plan_for(CAP_MS * 5.0);
        assert_eq!(plan.frames, CAP_MS as usize);
        assert!(
            (plan.dropped_ms - CAP_MS * 4.0).abs() < 1e-9,
            "expected {:.1} ms dropped, got {:.1}",
            CAP_MS * 4.0,
            plan.dropped_ms
        );
    }

    #[test]
    fn a_different_cap_changes_the_rendered_chunk() {
        // The cap is a parameter, so a larger one renders more before dropping.
        let plan = plan_chunk(CAP_MS * 5.0 * RATE / 1000.0, CAP_MS * 2.0, RATE);
        assert_eq!(plan.frames, (CAP_MS * 2.0) as usize);
        assert!((plan.dropped_ms - CAP_MS * 3.0).abs() < 1e-9);
    }
}
