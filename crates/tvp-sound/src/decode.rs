//! Audio decoding for the TVP sound module: turn a storage entry (wav, ogg,
//! vorbis, flac, mp3) into interleaved `f32` PCM.
//!
//! Two decode shapes live here:
//!
//! * [`decode_audio`] / [`decode_audio_bytes`] — the original *synchronous,
//!   whole-file* decode into a [`DecodedAudio`]. Short effects and voices
//!   still use this path; it is also what tests and callers that want a
//!   materialized buffer use.
//! * [`probe_audio`] + [`StreamDecoder`] — the building blocks of the
//!   asynchronous/streaming path in [`crate::source`]. `probe_audio` reads
//!   only the container metadata (sample rate, channels, frame count) so
//!   `open` can return promptly; `StreamDecoder` then feeds a bounded
//!   decode-ahead ring buffer a chunk at a time.
//!
//! # Why symphonia directly instead of rodio's `Decoder`?
//!
//! rodio's decode API is tied to the device-driven streaming model (it is
//! built to feed `Sink`s on top of a `cpal` output stream) and its feature
//! names are awkward (`symphonia-vorbis` enables only the vorbis *codec*,
//! not the ogg container). This module decodes with `symphonia` directly —
//! the exact same crate rodio uses internally, so Cargo unifies the
//! dependency and the features enabled here (`wav`, `pcm`, `ogg`, `vorbis`,
//! `flac`, `mp3`) also upgrade rodio's copy. Playback output lives in
//! [`crate::player`] on top of rodio.

use std::io::Cursor;
use std::sync::{Arc, LazyLock, Mutex};

use engine::Storage;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Fully-decoded, interleaved `f32` PCM audio.
///
/// `samples` is interleaved per the reference's internal PCM convention:
/// `[frame0ch0, frame0ch1, frame1ch0, frame1ch1, ...]` (i.e.
/// `samples[frame * channels + channel]`).
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    /// Samples per second.
    pub sample_rate: u32,
    /// Channel count (1 = mono, 2 = stereo; the decoder's native layout).
    pub channels: u16,
    /// Interleaved `f32` samples in `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
}

impl DecodedAudio {
    /// Duration in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate.max(1))
    }

    /// Number of sample frames (sample groups, one per channel).
    pub fn frames(&self) -> u64 {
        self.samples.len() as u64 / u64::from(self.channels.max(1))
    }
}

/// Container metadata known from a probe, before any sample is decoded.
///
/// This is what lets `open` return promptly: the sample rate, channels and
/// (when the container states it) the total frame count / duration are
/// available without touching the expensive packet decode loop.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioMetadata {
    /// Samples per second.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Total number of playable frames, when the container states it.
    pub total_frames: Option<u64>,
    /// Duration in seconds, when the container states it.
    pub duration_seconds: Option<f64>,
}

impl AudioMetadata {
    /// Duration from the frame count and sample rate when it was not stated
    /// directly by the container.
    pub fn duration_seconds_or_frames(&self) -> Option<f64> {
        self.duration_seconds.or_else(|| {
            self.total_frames
                .map(|f| f as f64 / f64::from(self.sample_rate.max(1)))
        })
    }
}

/// Errors produced while decoding an audio file.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The storage layer could not produce the entry.
    #[error("storage: {0}")]
    Storage(#[from] engine::storage::ReadError),
    /// The bytes could not be opened as any supported audio format.
    #[error("'{name}': {detail}")]
    Format {
        /// Storage name that failed to probe.
        name: String,
        /// Symphonia's probe/format error message.
        detail: String,
    },
    /// The container opened, but has no decodable audio track.
    #[error("'{0}': no audio track")]
    NoTrack(String),
    /// The container opened but decoding produced no samples.
    #[error("'{0}': no audio data decoded")]
    Empty(String),
}

/// Global codec registry that includes the Opus decoder via
/// `symphonia-adapter-libopus`. Opus is not in symphonia's default
/// registry (it ships separately as `symphonia-adapter-libopus`, which
/// needs the C libopus). The game's voice files are Ogg Opus
/// (`OpusHead`), while BGM are Vorbis. With this registry, real Opus
/// voices decode to full PCM so the logo→title sequencing can wait on
/// actual voice durations instead of the ~60 ms silent fallback.
static CODEC_REGISTRY: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    registry
});

fn is_opus(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS") && bytes.windows(8).any(|w| w == b"OpusHead")
}

fn silent_fallback(name: &str) -> DecodedAudio {
    log::warn!(
        "decode_audio: {name}: Opus decode failed or produced no samples; returning a short silent buffer so voice sequencing still drives onStatusChanged(\"stop\")"
    );
    let rate = 48000u32;
    let channels = 2u16;
    // ~60 ms of silence: enough for the mixer to report a play→stop
    // transition on the next poll, even when real decode is unavailable.
    let n = (rate as usize / 1000 * 60) * channels as usize;
    DecodedAudio {
        sample_rate: rate,
        channels,
        samples: vec![0.0; n],
    }
}

/// An opened format reader + decoder, before any packet is consumed.
struct DecoderSetup {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    metadata: AudioMetadata,
    opus: bool,
}

/// Probe the container and build the decoder. Shared by the whole-file
/// decode, [`probe_audio`] and [`StreamDecoder`].
fn open_setup(bytes: &[u8], name: &str) -> Result<DecoderSetup, DecodeError> {
    // Probe the container. The extension hint only nudges the probe; the
    // bytes themselves decide the format.
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = name.rsplit('.').next()
        && !ext.is_empty()
    {
        hint.with_extension(ext);
    }
    let opus = is_opus(bytes);
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| DecodeError::Format {
            name: name.to_string(),
            detail: format!("{e}"),
        })?;

    // Pick the default audio track.
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| DecodeError::NoTrack(name.to_string()))?;
    let track_id = track.id;
    let codec_params = track
        .codec_params
        .as_ref()
        .and_then(|cp| cp.audio())
        .ok_or_else(|| DecodeError::NoTrack(name.to_string()))?;
    let sample_rate = codec_params.sample_rate.unwrap_or(0);
    let channels = codec_params
        .channels
        .as_ref()
        .map(|c| c.count())
        .unwrap_or(0) as u16;
    let total_frames = track.num_frames;
    let duration_seconds = match (track.duration, track.time_base) {
        (Some(d), Some(tb)) => tb.calc_duration(d).map(|t| t.as_secs_f64()),
        _ => None,
    };
    let decoder = CODEC_REGISTRY
        .make_audio_decoder(codec_params, &AudioDecoderOptions::default())
        .map_err(|e| DecodeError::Format {
            name: name.to_string(),
            detail: format!("codec: {e}"),
        })?;
    Ok(DecoderSetup {
        format,
        decoder,
        track_id,
        metadata: AudioMetadata {
            sample_rate,
            channels,
            total_frames,
            duration_seconds,
        },
        opus,
    })
}

/// Probe a container and return its metadata without decoding any samples.
///
/// This is the synchronous part of the asynchronous open path: it reads the
/// whole compressed entry (small) and inspects its headers, so the caller
/// can answer `getBufferInfo()`/duration queries immediately.
pub fn probe_audio(bytes: &[u8], name: &str) -> Result<AudioMetadata, DecodeError> {
    open_setup(bytes, name).map(|s| s.metadata)
}

/// Decode a storage entry by name (wav/ogg/vorbis/flac/mp3).
///
/// Locks `storage` briefly to read the entry's bytes, then fully decodes
/// them into memory. `name` is normalized by the storage layer exactly like
/// every other TVP storage access.
pub fn decode_audio(
    storage: &Arc<Mutex<Storage>>,
    name: &str,
) -> Result<DecodedAudio, DecodeError> {
    let bytes = {
        let mut storage = storage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        storage.read(name)?
    };
    decode_audio_bytes(&bytes, name)
}

/// Decode an in-memory audio file into a fully materialized buffer.
///
/// Public mainly so tests can feed fixtures without a mounted storage.
pub fn decode_audio_bytes(bytes: &[u8], name: &str) -> Result<DecodedAudio, DecodeError> {
    let setup = match open_setup(bytes, name) {
        Ok(s) => s,
        Err(e) => {
            if is_opus(bytes) {
                log::warn!(
                    "decode_audio: {name}: Opus open failed ({e}), falling back to silent buffer"
                );
                return Ok(silent_fallback(name));
            }
            return Err(e);
        }
    };
    let DecoderSetup {
        mut format,
        mut decoder,
        track_id,
        metadata,
        opus,
    } = setup;
    let mut samples: Vec<f32> = Vec::new();
    let mut sample_rate = metadata.sample_rate;
    let mut channels = metadata.channels;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            // End of stream: symphonia 0.6 signals EOF with Ok(None).
            Ok(None) => break,
            // Legacy end-of-stream I/O error (some containers).
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            // Track layout changed mid-stream (rare); not worth re-wiring a
            // new decoder here — stop and keep what we have.
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => {
                return Err(DecodeError::Format {
                    name: name.to_string(),
                    detail: format!("{e}"),
                });
            }
        };
        if packet.track_id != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                sample_rate = decoded.spec().rate();
                channels = decoded.spec().channels().count() as u16;
                // Convert whatever the codec produced into interleaved f32.
                let start = samples.len();
                samples.resize(start + decoded.samples_interleaved(), 0.0);
                decoded.copy_to_slice_interleaved(&mut samples[start..]);
            }
            // A corrupt frame: skip it, keep decoding the rest.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => {
                return Err(DecodeError::Format {
                    name: name.to_string(),
                    detail: format!("{e}"),
                });
            }
        }
    }

    if samples.is_empty() {
        if opus {
            return Ok(silent_fallback(name));
        }
        return Err(DecodeError::Empty(name.to_string()));
    }

    let audio = DecodedAudio {
        sample_rate,
        channels,
        samples,
    };
    log::info!(
        "decode_audio: {name}: decoded {} Hz, {} ch, {:.2}s ({} frames)",
        audio.sample_rate,
        audio.channels,
        audio.duration_seconds(),
        audio.frames()
    );
    Ok(audio)
}

/// A streaming audio decoder: probes a container once, then yields
/// interleaved `f32` chunks from [`Self::next_chunk`] until EOF.
///
/// `StreamDecoder` owns its format reader/decoder, so it runs entirely on
/// the decode worker thread. It performs no locking and no device access.
pub struct StreamDecoder {
    setup: DecoderSetup,
    name: String,
    eof: bool,
}

impl StreamDecoder {
    /// Open `bytes` as a streaming decoder. `name` is used for the format
    /// hint and error messages.
    pub fn open(bytes: &[u8], name: &str) -> Result<Self, DecodeError> {
        Ok(StreamDecoder {
            setup: open_setup(bytes, name)?,
            name: name.to_string(),
            eof: false,
        })
    }

    /// Samples per second (may be refined by the first decoded packet).
    pub fn sample_rate(&self) -> u32 {
        self.setup.metadata.sample_rate
    }

    /// Channel count (may be refined by the first decoded packet).
    pub fn channels(&self) -> u16 {
        self.setup.metadata.channels
    }

    /// Total frames, when the container states it.
    pub fn total_frames(&self) -> Option<u64> {
        self.setup.metadata.total_frames
    }

    /// Duration in seconds, when available.
    pub fn duration_seconds(&self) -> Option<f64> {
        self.setup.metadata.duration_seconds_or_frames()
    }

    /// Decode and return at most `max_frames` sample frames of interleaved
    /// `f32` PCM, or `None` at end of stream. Corrupt packets are skipped.
    pub fn next_chunk(&mut self, max_frames: usize) -> Result<Option<Vec<f32>>, DecodeError> {
        let mut out: Vec<f32> = Vec::new();
        while !self.eof {
            let channels = usize::from(self.setup.metadata.channels.max(1));
            if out.len() / channels >= max_frames {
                break;
            }
            let packet = match self.setup.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.eof = true;
                    break;
                }
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.eof = true;
                    break;
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.eof = true;
                    break;
                }
                Err(e) => {
                    return Err(DecodeError::Format {
                        name: self.name.clone(),
                        detail: format!("{e}"),
                    });
                }
            };
            if packet.track_id != self.setup.track_id {
                continue;
            }
            match self.setup.decoder.decode(&packet) {
                Ok(decoded) => {
                    self.setup.metadata.sample_rate = decoded.spec().rate();
                    self.setup.metadata.channels = decoded.spec().channels().count() as u16;
                    let start = out.len();
                    out.resize(start + decoded.samples_interleaved(), 0.0);
                    decoded.copy_to_slice_interleaved(&mut out[start..]);
                }
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => {
                    return Err(DecodeError::Format {
                        name: self.name.clone(),
                        detail: format!("{e}"),
                    });
                }
            }
        }
        if out.is_empty() {
            Ok(None)
        } else {
            Ok(Some(out))
        }
    }
}
