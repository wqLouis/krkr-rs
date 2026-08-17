//! Audio decoding for the TVP sound module: turn a storage entry (wav, ogg,
//! vorbis, flac, mp3) into fully-decoded, interleaved `f32` PCM.
//!
//! # Why symphonia directly instead of rodio's `Decoder`?
//!
//! rodio 0.20's decode API (`rodio::Decoder`) is tied to the device-driven
//! streaming model (it is built to feed `Sink`s on top of a `cpal` output
//! stream) and its feature names are awkward (`symphonia-vorbis` enables
//! only the vorbis *codec*, not the ogg container). This module decodes
//! with `symphonia` directly — the exact same crate rodio 0.20 uses
//! internally, so Cargo unifies the dependency and the features enabled
//! here (`wav`, `pcm`, `ogg`, `vorbis`, `flac`, `mp3`) also upgrade rodio's
//! copy. Playback output lives in [`crate::player`] on top of rodio.

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use engine::Storage;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

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

/// Decode an in-memory audio file.
///
/// Public mainly so tests can feed fixtures without a mounted storage.
pub fn decode_audio_bytes(bytes: &[u8], name: &str) -> Result<DecodedAudio, DecodeError> {
    // The game's voice files are Ogg Opus (symphonia 0.5 has no Opus
    // decoder — voices decode as "unsupported codec"). The voices drive
    // the script's sequencing via onStatusChanged("stop"), so a short
    // silent buffer lets them "play and finish" immediately and keeps the
    // logo→title chain moving. Real Opus decoding is a follow-up
    // (symphonia 0.6 + symphonia-adapter-libopus).
    if bytes.starts_with(b"OggS") && bytes.windows(8).any(|w| w == b"OpusHead") {
        log::warn!("decode_audio: {name}: Ogg Opus is not decoded yet; returning a short silent buffer (voice sequencing still works)");
        let rate = 48000u32;
        let channels = 2u16;
        // ~60 ms of silence: enough for the mixer to report a play→stop
        // transition on the next poll.
        let n = (rate as usize / 1000 * 60) * channels as usize;
        return Ok(DecodedAudio {
            sample_rate: rate,
            channels,
            samples: vec![0.0; n],
        });
    }
    // Probe the container. The extension hint only nudges the probe; the
    // bytes themselves decide the format.
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = name.rsplit('.').next()
        && !ext.is_empty()
    {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| DecodeError::Format {
            name: name.to_string(),
            detail: format!("{e}"),
        })?;

    let mut format = probed.format;

    // Pick the default (first) audio track.
    let track = format
        .default_track()
        .ok_or_else(|| DecodeError::NoTrack(name.to_string()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| DecodeError::Format {
            name: name.to_string(),
            detail: format!("codec: {e}"),
        })?;

    let mut samples: Vec<f32> = Vec::new();
    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(0) as u16;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // End of stream: the container signals EOF as an UnexpectedEof
            // I/O error.
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
        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                sample_rate = decoded.spec().rate;
                channels = decoded.spec().channels.count() as u16;
                // Convert whatever the codec produced (u8/i16/i32/f32/...)
                // into interleaved f32.
                let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
                buf.copy_interleaved_ref(decoded);
                samples.extend_from_slice(buf.samples());
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
        return Err(DecodeError::Empty(name.to_string()));
    }

    Ok(DecodedAudio {
        sample_rate,
        channels,
        samples,
    })
}
