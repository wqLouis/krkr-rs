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
//! The mixer is **render-driven**: [`sound_render`] advances it by the frames
//! it produces, so the playback cursor is always the end of the audio rendered
//! so far. Each frame renders one chunk of `carry + dt_seconds × sample_rate`
//! frames, keeping the sub-sample remainder in `carry` for the next frame. The
//! carry makes the mapping sample-accurate over time — no rounding drift
//! accumulates (a 60 fps loop at 44.1 kHz renders 735 frames per frame
//! forever; an odd rate like 48 kHz/60 alternates without losing a sample).
//!
//! Because the cursor is what was rendered, consecutive chunks tile gaplessly
//! whatever the frame jitter: no dropped audio, no repeated audio, and the L1
//! queue depth (and therefore latency) stays constant. L1 pre-fills that queue
//! with roughly 150 ms before starting the device and, on a real underrun,
//! fades the last per-channel sample to silence over a few milliseconds (see
//! `platform`'s `audio_queue`), so the constant depth is the reservoir plus one
//! frame — independent of how the device callback is sized.
//!
//! **When a frame runs long**, the L1 device drains its pre-fill and underruns
//! while the frame is stalled; the resumed chunk is still contiguous with the
//! last rendered audio (the cursor did not move during the stall), so the only
//! artifact is the inserted silence, smoothed by the L1 fade. One frame may
//! advance the cursor by at most the configured maximum ([`DEFAULT_MAX_CHUNK_MS`]
//! by default, `KRKR_RS_AUDIO_MAX_CHUNK_MS`): the elapsed time beyond the cap is
//! reported with a `warn`, and playback simply falls behind the wall clock by
//! that much (it does not catch up), so no audio is dropped or repeated and a
//! single write cannot jump the queue past its bound. A **small** cap keeps the
//! post-hitch latency low but rebuilds only a small reservoir; a **large** cap
//! covers more of the hitch in one write at the cost of more latency. The cap
//! bounds one frame's playback advance; it is not a substitute for the L1
//! pre-fill.
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

use std::rc::Rc;
use std::sync::Arc;

use api::error::{AudioError, MediaError};
use api::media::AudioSpec;
use compat::{
    CompatHost, add_audio_decoder, set_output_latency, set_sound_render_driven, sound_render,
};
use engine::sound::{AudioStream, SoundError, SoundSource};
use media::{AudioDecoderState, Media, open_audio, read_audio, seek_audio};
use platform::{AudioDevice, AudioOutputState, PlatformError};
use vm::VmHost;

use crate::GameError;
use crate::startup::StartupStats;

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

/// The default most playback one frame may advance (see the module docs).
///
/// A longer frame advances at most this much and reports the excess with a
/// `warn`; playback falls that far behind the wall clock instead of the chunk
/// dropping or repeating any audio. `KRKR_RS_AUDIO_MAX_CHUNK_MS` overrides it.
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
    /// Sub-frame remainder to carry into the next frame.
    carry_frames: f64,
    /// Milliseconds of this frame deliberately not advanced (0 unless the
    /// frame exceeded the cap).
    behind_ms: f64,
}

/// Plans one frame's mixer chunk.
///
/// `dt_ms` is the frame's elapsed time, `carry_frames` the sub-frame remainder
/// from the previous frame, and `max_chunk_ms` the most playback one frame may
/// advance. The frame is clamped to the cap *before* the carry is added, so a
/// long frame advances at most the cap and the excess is reported in
/// `behind_ms` rather than rendered or carried.
fn plan_chunk(dt_ms: f64, carry_frames: f64, max_chunk_ms: f64, rate: f64) -> ChunkPlan {
    let dt_ms = dt_ms.max(0.0);
    let behind_ms = (dt_ms - max_chunk_ms).max(0.0);
    let due = carry_frames + dt_ms.min(max_chunk_ms) * rate / 1000.0;
    let frames = due.floor();
    ChunkPlan {
        frames: frames as usize,
        carry_frames: due - frames,
        behind_ms,
    }
}

/// One game audio stream over the bottom decoder.
///
/// `open_audio` probes the container without decoding; this value decodes and
/// reseeks on demand as [`SoundSource::frame`] drives it.
struct MediaAudioStream {
    /// The stateless decode backend.
    media: Media,
    /// The decoder state this stream owns across reads and seeks.
    state: AudioDecoderState,
    /// The container format reported at open.
    spec: AudioSpec,
}

impl AudioStream for MediaAudioStream {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, SoundError> {
        match read_audio(&self.media, &mut self.state, out) {
            Ok(written) => Ok(written),
            // The decoder reports the end as `EndOfStream`; a zero-sample
            // read is the end-of-window signal.
            Err(MediaError::EndOfStream) => Ok(0),
            Err(error) => Err(SoundError::Decode {
                message: error.to_string(),
            }),
        }
    }

    fn seek(&mut self, frame: u64) -> Result<(), SoundError> {
        seek_audio(&self.media, &mut self.state, frame).map_err(|error| SoundError::Decode {
            message: error.to_string(),
        })
    }
}

/// Install the `media` audio decoder on `host`.
///
/// Call once, before any script runs. Opening a sound probes the container
/// (sample rate, channels, total frames) and installs an incremental decoder;
/// samples are decoded only as playback reads them. The decoder owns a clone
/// of the stateless [`Media`] backend and its own explicit
/// [`AudioDecoderState`].
pub fn install_audio_decoder(host: &CompatHost, media: &Media) {
    let stats = StartupStats::new();
    install_audio_decoder_metered(host, media, &stats);
}

/// Installs the `media` audio decoder with the shared [`StartupStats`]
/// counter set.
pub(crate) fn install_audio_decoder_metered(
    host: &CompatHost,
    media: &Media,
    stats: &Rc<StartupStats>,
) {
    let media = media.clone();
    let stats = Rc::clone(stats);
    set_sound_render_driven(host.services(), true);
    add_audio_decoder(
        host.services(),
        Box::new(move |bytes, loops, hint| {
            let bytes: Arc<[u8]> = Arc::from(bytes);
            let (spec, state) =
                open_audio(&media, bytes, hint).map_err(|error| error.to_string())?;
            stats.record_audio_decoder();
            let stream = MediaAudioStream {
                media: media.clone(),
                state,
                spec,
            };
            let source = SoundSource::streaming(spec, Box::new(stream), loops)
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
    /// Most playback one frame may advance, in milliseconds (the configured
    /// cap; see the module docs).
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

    /// The most playback one frame may advance, in milliseconds.
    ///
    /// A movie's picture advances by the same bound, so a frame longer than
    /// this leaves the picture and the sound equally far behind instead of
    /// pulling them apart.
    pub(crate) fn max_chunk_ms(&self) -> f64 {
        self.max_chunk_ms
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
        if !dt_ms.is_finite() {
            self.carry_frames = 0.0;
            log::warn!("audio: non-finite frame duration; skipping this buffer");
            return Ok(());
        }
        let rate = f64::from(self.spec.sample_rate);
        let plan = plan_chunk(dt_ms, self.carry_frames, self.max_chunk_ms, rate);
        self.carry_frames = plan.carry_frames;
        if plan.behind_ms > 0.0 {
            log::warn!(
                "audio: {dt_ms:.1} ms frame exceeds the {:.0} ms playback cap; \
                 playback falls {:.1} ms behind the wall clock",
                self.max_chunk_ms,
                plan.behind_ms
            );
        }
        if plan.frames == 0 {
            // Sub-frame remainder only: carry it and render nothing.
            return Ok(());
        }
        let frames = plan.frames;

        let channels = usize::try_from(self.spec.channels).map_err(|_| {
            PlatformError::from(AudioError::InvalidSpec {
                message: format!(
                    "output channel count {} does not fit in memory",
                    self.spec.channels
                ),
            })
        })?;
        self.buffer.resize(frames * channels, 0.0);
        let ctx = vm.compat().ctx().map_err(vm::VmError::from)?;
        sound_render(&ctx, &mut self.buffer, self.spec).map_err(vm::VmError::from)?;
        self.device.write(&mut self.state, &self.buffer)?;
        // The device holds what was written, so the sound reaches the listener
        // that much later than the mixer rendered it. The mixer needs the
        // number to place a picture that follows the sound.
        set_output_latency(&ctx, self.device.queued_seconds(&self.state));
        log::trace!(
            "audio: wrote {frames} frames ({:.1} ms) to the {} device",
            frames as f64 * 1000.0 / rate,
            if self.null { "null" } else { "output" },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_MAX_CHUNK_MS, plan_chunk};

    /// A 1 kHz stream, so one millisecond is exactly one frame.
    const RATE: f64 = 1_000.0;
    /// A 10 ms cap keeps the 0/1x/5x arithmetic exact.
    const CAP_MS: f64 = 10.0;

    #[test]
    fn the_default_cap_is_250_ms() {
        assert_eq!(DEFAULT_MAX_CHUNK_MS, 250.0);
    }

    #[test]
    fn a_frame_shorter_than_one_frame_renders_nothing() {
        let plan = plan_chunk(0.0, 0.0, CAP_MS, RATE);
        assert_eq!(plan.frames, 0);
        assert_eq!(plan.carry_frames, 0.0);
        assert_eq!(plan.behind_ms, 0.0);
    }

    #[test]
    fn a_frame_at_the_cap_renders_the_whole_chunk() {
        let plan = plan_chunk(CAP_MS, 0.0, CAP_MS, RATE);
        assert_eq!(plan.frames, CAP_MS as usize);
        assert_eq!(plan.carry_frames, 0.0);
        assert_eq!(plan.behind_ms, 0.0);
    }

    #[test]
    fn a_frame_five_times_the_cap_advances_only_the_cap() {
        let plan = plan_chunk(CAP_MS * 5.0, 0.0, CAP_MS, RATE);
        assert_eq!(plan.frames, CAP_MS as usize);
        assert!(
            (plan.behind_ms - CAP_MS * 4.0).abs() < 1e-9,
            "expected {:.1} ms behind, got {:.1}",
            CAP_MS * 4.0,
            plan.behind_ms
        );
    }

    #[test]
    fn the_carry_preserves_the_sub_frame_remainder() {
        // 0.4 ms at 1 kHz is 0.4 frames; three of them render one frame and
        // carry 0.2.
        let first = plan_chunk(0.4, 0.0, CAP_MS, RATE);
        assert_eq!(first.frames, 0);
        assert!((first.carry_frames - 0.4).abs() < 1e-9);
        let second = plan_chunk(0.4, first.carry_frames, CAP_MS, RATE);
        assert_eq!(second.frames, 0);
        assert!((second.carry_frames - 0.8).abs() < 1e-9);
        let third = plan_chunk(0.4, second.carry_frames, CAP_MS, RATE);
        assert_eq!(third.frames, 1);
        assert!((third.carry_frames - 0.2).abs() < 1e-9);
    }

    #[test]
    fn a_long_frame_does_not_carry_the_excess() {
        // The excess over the cap is behind, not carried: the next frame
        // starts fresh.
        let plan = plan_chunk(CAP_MS * 3.0, 0.0, CAP_MS, RATE);
        assert_eq!(plan.frames, CAP_MS as usize);
        assert_eq!(plan.carry_frames, 0.0);
        assert!((plan.behind_ms - CAP_MS * 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_different_cap_changes_the_advance() {
        let plan = plan_chunk(CAP_MS * 5.0, 0.0, CAP_MS * 2.0, RATE);
        assert_eq!(plan.frames, (CAP_MS * 2.0) as usize);
        assert!((plan.behind_ms - CAP_MS * 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_negative_frame_renders_only_the_carry() {
        let plan = plan_chunk(-5.0, 0.7, CAP_MS, RATE);
        assert_eq!(plan.frames, 0);
        assert!((plan.carry_frames - 0.7).abs() < 1e-9);
        assert_eq!(plan.behind_ms, 0.0);
    }
}
