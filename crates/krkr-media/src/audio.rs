//! Audio decoding for the stateless [`Media`] engine.
//!
//! This is the `krkr-media` replacement for `tvp-sound/src/decode.rs`. It
//! keeps the reference's two decode shapes but expresses them through the
//! final middle-layer API:
//!
//! * [`probe_audio`] reads only the container header (sample rate, channels,
//!   frame count) so the middle layer can answer duration queries promptly.
//! * [`decode_audio`] materializes a whole file into interleaved `f32`.
//! * [`open_audio`] + [`read_audio`] + [`seek_audio`] stream from an
//!   [`AudioDecoderState`] that the middle layer owns and passes back on
//!   every call.
//!
//! # D9: stateless object, explicit state
//!
//! [`Media`] stores nothing here. The codec context lives inside the
//! [`AudioDecoderState`] value created by [`open_audio`] and owned by the
//! caller. There is no global codec registry and no lazy cache: the
//! symphonia [`CodecRegistry`] is built inside [`open_setup`] on each open,
//! and the codec it instantiates is moved into the state.
//!
//! # Codecs
//!
//! The default symphonia codecs are registered (`wav`/`pcm`, `ogg`/`vorbis`,
//! `flac`, `mp3`) plus the separately shipped
//! [`symphonia_adapter_libopus::OpusDecoder`]. When an Ogg Opus stream
//! cannot be opened or yields no samples, the reference's silent fallback is
//! preserved: a short (~60 ms) silent buffer is returned instead of an
//! error, so voice sequencing still observes a play→stop transition.

use std::fmt;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use krkr_api::error::MediaError;
use krkr_api::media::{AudioSpec, DecodedAudio};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::Media;

/// Audio format/codec names advertised by [`crate::capabilities`].
pub(crate) const AUDIO_FORMATS: [&str; 7] = ["wav", "pcm", "ogg", "vorbis", "flac", "mp3", "opus"];

/// Sample rate of the Opus-failure silent fallback.
const SILENT_RATE: u32 = 48_000;
/// Channel count of the Opus-failure silent fallback.
const SILENT_CHANNELS: u16 = 2;
/// Duration of the Opus-failure silent fallback in milliseconds.
const SILENT_MS: u64 = 60;

/// Frames in the Opus-failure silent fallback.
fn silent_frames() -> u64 {
    (u64::from(SILENT_RATE) / 1000) * SILENT_MS
}

/// Interleaved samples of the Opus-failure silent fallback.
fn silent_samples() -> Vec<f32> {
    vec![0.0; silent_frames() as usize * usize::from(SILENT_CHANNELS)]
}

/// [`AudioSpec`] of the Opus-failure silent fallback.
fn silent_spec() -> AudioSpec {
    AudioSpec {
        sample_rate: SILENT_RATE,
        channels: SILENT_CHANNELS,
        total_frames: Some(silent_frames()),
    }
}

/// Whole-file decode of the Opus-failure silent fallback.
fn silent_audio() -> DecodedAudio {
    DecodedAudio::new(silent_spec(), silent_samples())
}

/// Bottom-defined state for an incremental audio decode.
///
/// The middle layer owns this value and passes it back on every call; `Media`
/// never stores it. It is opaque on purpose: only this crate constructs or
/// mutates it, so the backend implementation can change without an API break.
///
/// The state holds the demuxer/codec context, the decode position (in sample
/// frames) and the [`Arc`]-shared compressed bytes needed to reseek. It is
/// `Send + Sync` (symphonia's `FormatReader` and `AudioDecoder` both are) so
/// the middle layer may run it on its worker pool.
#[non_exhaustive]
pub struct AudioDecoderState {
    /// Current stream format, refined from the first decoded packet.
    spec: AudioSpec,
    /// Opened format reader + decoder; `None` for the silent fallback.
    setup: Option<DecoderSetup>,
    /// Compressed bytes, retained for [`seek_audio`] reseeks.
    bytes: Arc<[u8]>,
    /// Original hint, retained for [`seek_audio`] reseeks.
    hint: Option<String>,
    /// Decoded-but-not-yet-returned interleaved samples.
    pending: Vec<f32>,
    /// Consumed offset into `pending`.
    pending_offset: usize,
    /// Frames still to drop after a seek (the reader restarts at 0).
    skip_frames: u64,
    /// Frames returned to the caller so far (the decode position).
    position_frames: u64,
    /// Set once the reader is exhausted.
    eof: bool,
    /// Template for the silent fallback; `Some` only in fallback states.
    fallback: Option<Vec<f32>>,
}

impl fmt::Debug for AudioDecoderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioDecoderState")
            .field("spec", &self.spec)
            .field("position_frames", &self.position_frames)
            .field("skip_frames", &self.skip_frames)
            .field("eof", &self.eof)
            .field("fallback", &self.fallback.is_some())
            .finish_non_exhaustive()
    }
}

/// An opened format reader + decoder, before any packet is consumed.
struct DecoderSetup {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
}

/// Everything [`open_setup`] produces: the decoder plus the probed [`AudioSpec`].
struct OpenedAudio {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    spec: AudioSpec,
}

/// Returns the hint used in error messages.
fn display_name(hint: Option<&str>) -> &str {
    hint.filter(|name| !name.is_empty()).unwrap_or("<audio>")
}

/// Extracts a lowercase container-extension hint from a storage name or a
/// bare extension (`"wav"` / `".wav"`).
fn extension_of(hint: &str) -> Option<String> {
    if let Some(ext) = Path::new(hint).extension().and_then(|ext| ext.to_str()) {
        return Some(ext.to_ascii_lowercase());
    }
    let bare = hint.strip_prefix('.').unwrap_or(hint);
    if !bare.is_empty() && !bare.contains(['/', '\\', '.']) {
        return Some(bare.to_ascii_lowercase());
    }
    None
}

/// Returns `true` when the bytes look like an Ogg Opus stream.
///
/// Opus carries a separate decoder adapter and gets the silent fallback; the
/// sniff is used both to select the fallback and to describe it in logs.
fn is_opus(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS") && bytes.windows(8).any(|window| window == b"OpusHead")
}

/// Builds a fresh codec registry with the default codecs plus the Opus
/// adapter.
///
/// Deliberately *not* cached in a global/lazy: the D9 contract forbids hidden
/// state, so every open constructs the small registry and hands the
/// instantiated decoder to the caller-owned state.
fn build_codec_registry() -> CodecRegistry {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    registry
}

/// Probes the container and builds the decoder without consuming samples.
///
/// Shared by [`probe_audio`], [`decode_audio`] and [`open_audio`]. The
/// extension hint only nudges the probe; the bytes always decide the format.
/// The caller's buffer is shared by refcount so multi-MB entries are never
/// copied.
fn open_setup(bytes: &Arc<[u8]>, hint: Option<&str>) -> Result<OpenedAudio, MediaError> {
    let name = display_name(hint);
    let mss = MediaSourceStream::new(Box::new(Cursor::new(Arc::clone(bytes))), Default::default());
    let mut symphonia_hint = Hint::new();
    if let Some(ext) = hint.and_then(extension_of) {
        symphonia_hint.with_extension(&ext);
    }
    let format = symphonia::default::get_probe()
        .probe(
            &symphonia_hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| MediaError::Decode {
            format: name.to_string(),
            message: format!("{error}"),
        })?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| MediaError::Unsupported {
            message: format!("{name}: no decodable audio track"),
        })?;
    let track_id = track.id;
    let codec_params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| MediaError::Unsupported {
            message: format!("{name}: no audio codec parameters"),
        })?;
    let sample_rate = codec_params.sample_rate.unwrap_or(0);
    let channels = codec_params
        .channels
        .as_ref()
        .map(|channels| channels.count())
        .unwrap_or(0) as u16;
    let duration_seconds = match (track.duration, track.time_base) {
        (Some(duration), Some(time_base)) => time_base
            .calc_duration(duration)
            .map(|time| time.as_secs_f64()),
        _ => None,
    };
    let total_frames = match (track.num_frames, duration_seconds, sample_rate) {
        (Some(frames), _, _) => Some(frames),
        (None, Some(seconds), rate) if rate > 0 => Some((seconds * f64::from(rate)).round() as u64),
        _ => None,
    };
    let spec = AudioSpec {
        sample_rate,
        channels,
        total_frames,
    };

    let decoder = build_codec_registry()
        .make_audio_decoder(codec_params, &AudioDecoderOptions::default())
        .map_err(|error| MediaError::Unsupported {
            message: format!("{name}: {error}"),
        })?;

    Ok(OpenedAudio {
        format,
        decoder,
        track_id,
        spec,
    })
}

/// Builds a fallback state that yields the ~60 ms silent buffer once.
fn fallback_state(bytes: Arc<[u8]>, hint: Option<String>) -> AudioDecoderState {
    let samples = silent_samples();
    AudioDecoderState {
        spec: silent_spec(),
        setup: None,
        bytes,
        hint,
        pending: samples.clone(),
        pending_offset: 0,
        skip_frames: 0,
        position_frames: 0,
        eof: false,
        fallback: Some(samples),
    }
}

/// Probes an audio stream for its [spec](AudioSpec) without decoding samples.
///
/// `hint` is the storage name or extension the caller used to find `bytes`;
/// it only steers backend selection and is never trusted over the content.
///
/// If the bytes are an Ogg Opus stream that cannot be opened, the reference's
/// silent fallback spec (~60 ms, 48 kHz stereo) is returned so the middle
/// layer can still sequence the sound instead of failing outright.
pub fn probe_audio(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<AudioSpec, MediaError> {
    let _ = m;
    let bytes: Arc<[u8]> = Arc::from(bytes);
    match open_setup(&bytes, hint) {
        Ok(opened) => Ok(opened.spec),
        Err(error) => {
            if is_opus(&bytes) {
                log::warn!(
                    "probe_audio: {}: Opus probe failed ({error}); reporting the silent fallback spec",
                    display_name(hint)
                );
                Ok(silent_spec())
            } else {
                Err(error)
            }
        }
    }
}

/// Decodes a whole audio file into interleaved `f32` samples.
///
/// The result carries the probed [`AudioSpec`]; the samples are interleaved
/// per channel (`interleaved[frame * channels + channel]`). A failed Ogg
/// Opus decode returns the ~60 ms silent fallback instead of an error so the
/// sequencing contract is preserved.
pub fn decode_audio(
    m: &Media,
    bytes: &[u8],
    hint: Option<&str>,
) -> Result<DecodedAudio, MediaError> {
    let (spec, mut state) = open_audio(m, Arc::from(bytes), hint)?;
    let channels = usize::from(spec.channels.max(1)).max(1);
    let mut samples: Vec<f32> = Vec::new();
    let mut chunk = vec![0.0f32; 4096 * channels];
    loop {
        match read_audio(m, &mut state, &mut chunk) {
            Ok(0) => break,
            Ok(written) => samples.extend_from_slice(&chunk[..written]),
            Err(MediaError::EndOfStream) => break,
            Err(error) => return Err(error),
        }
    }

    if samples.is_empty() {
        if is_opus(bytes) || state.fallback.is_some() {
            return Ok(silent_audio());
        }
        return Err(MediaError::Decode {
            format: display_name(hint).to_string(),
            message: "no audio data decoded".to_string(),
        });
    }

    Ok(DecodedAudio::new(state.spec, samples))
}

/// Opens an audio stream for incremental decoding.
///
/// On success returns the stream's [`AudioSpec`] plus a fresh
/// [`AudioDecoderState`] for [`read_audio`]/[`seek_audio`]. `bytes` is shared
/// by `Arc` so the caller can keep the compressed buffer alive (and so
/// [`seek_audio`] can restart the reader without re-copying it).
///
/// A failed Ogg Opus open yields a fallback state that returns ~60 ms of
/// silence once and then reports [`MediaError::EndOfStream`].
pub fn open_audio(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(AudioSpec, AudioDecoderState), MediaError> {
    let _ = m;
    let hint_owned = hint.map(str::to_string);
    match open_setup(&bytes, hint) {
        Ok(opened) => {
            let spec = opened.spec;
            let state = AudioDecoderState {
                spec,
                setup: Some(DecoderSetup {
                    format: opened.format,
                    decoder: opened.decoder,
                    track_id: opened.track_id,
                }),
                bytes,
                hint: hint_owned,
                pending: Vec::new(),
                pending_offset: 0,
                skip_frames: 0,
                position_frames: 0,
                eof: false,
                fallback: None,
            };
            Ok((spec, state))
        }
        Err(error) => {
            if is_opus(&bytes) {
                log::warn!(
                    "open_audio: {}: Opus open failed ({error}); using the silent fallback",
                    display_name(hint)
                );
                let state = fallback_state(bytes, hint_owned);
                Ok((state.spec, state))
            } else {
                Err(error)
            }
        }
    }
}

/// Decodes the next chunk of interleaved `f32` samples into `out`.
///
/// Returns the number of samples written, always a whole number of frames
/// (the tail of `out` that cannot hold a complete frame is left untouched).
/// [`MediaError::EndOfStream`] marks the end of the stream.
pub fn read_audio(
    m: &Media,
    state: &mut AudioDecoderState,
    out: &mut [f32],
) -> Result<usize, MediaError> {
    let _ = m;
    let channels = usize::from(state.spec.channels.max(1)).max(1);
    let frame_aligned = out.len() - (out.len() % channels);
    let mut written = 0usize;

    while written < frame_aligned {
        // Drain staged samples before touching the reader.
        if state.pending_offset < state.pending.len() {
            let available = state.pending.len() - state.pending_offset;
            let take = available.min(frame_aligned - written);
            out[written..written + take]
                .copy_from_slice(&state.pending[state.pending_offset..state.pending_offset + take]);
            state.pending_offset += take;
            written += take;
            if state.pending_offset == state.pending.len() {
                state.pending.clear();
                state.pending_offset = 0;
            }
            continue;
        }

        if state.eof {
            break;
        }
        if !decode_next_packet(state)? {
            state.eof = true;
            break;
        }
    }

    state.position_frames += (written / channels) as u64;
    if written == 0 {
        if state.eof {
            return Err(MediaError::EndOfStream);
        }
        // `out` is smaller than one frame; report "nothing written" so the
        // caller can retry with a larger buffer.
        return Ok(0);
    }
    Ok(written)
}

/// Decodes one packet into `state.pending`, honouring a pending seek skip.
///
/// Returns `Ok(false)` at end of stream and `Ok(true)` when samples were
/// staged (possibly zero samples from a corrupt packet, which is skipped).
fn decode_next_packet(state: &mut AudioDecoderState) -> Result<bool, MediaError> {
    let name = display_name(state.hint.as_deref()).to_string();
    let AudioDecoderState {
        spec,
        setup,
        pending,
        pending_offset,
        skip_frames,
        ..
    } = state;
    let Some(setup) = setup.as_mut() else {
        return Ok(false);
    };

    loop {
        let packet = match setup.format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => return Ok(false),
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                return Ok(false);
            }
            Err(SymphoniaError::ResetRequired) => return Ok(false),
            Err(error) => {
                return Err(MediaError::Decode {
                    format: name,
                    message: format!("{error}"),
                });
            }
        };
        if packet.track_id != setup.track_id {
            continue;
        }

        match setup.decoder.decode(&packet) {
            Ok(decoded) => {
                spec.sample_rate = decoded.spec().rate();
                spec.channels = decoded.spec().channels().count() as u16;
                let start = pending.len();
                pending.resize(start + decoded.samples_interleaved(), 0.0);
                decoded.copy_to_slice_interleaved(&mut pending[start..]);

                // Drop whole frames already seeked past. `pending` is empty on
                // entry (read_audio only decodes once it is drained), but the
                // offset is kept general.
                if *skip_frames > 0 {
                    let channels = usize::from(spec.channels.max(1)).max(1);
                    let available = (pending.len() - *pending_offset) / channels;
                    if (available as u64) <= *skip_frames {
                        *skip_frames -= available as u64;
                        pending.clear();
                        *pending_offset = 0;
                    } else {
                        let drop = *skip_frames as usize * channels;
                        pending.drain(*pending_offset..*pending_offset + drop);
                        *pending_offset = 0;
                        *skip_frames = 0;
                    }
                }
                return Ok(true);
            }
            // A corrupt frame: skip it and keep decoding the rest.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => {
                return Err(MediaError::Decode {
                    format: name,
                    message: format!("{error}"),
                });
            }
        }
    }
}

/// Seeks the stream so the next [`read_audio`] starts at `frame`.
///
/// The reader is reopened from the retained compressed bytes (the reference
/// `StreamDecoder` behaviour on seek/loop wrap), then decoded frames are
/// discarded until `frame` is reached. The decode position is updated
/// immediately; the silent-fallback path emulates the seek within its buffer.
pub fn seek_audio(m: &Media, state: &mut AudioDecoderState, frame: u64) -> Result<(), MediaError> {
    let _ = m;
    let bytes = Arc::clone(&state.bytes);
    let hint = state.hint.clone();
    match open_setup(&bytes, hint.as_deref()) {
        Ok(opened) => {
            state.spec = opened.spec;
            state.setup = Some(DecoderSetup {
                format: opened.format,
                decoder: opened.decoder,
                track_id: opened.track_id,
            });
            state.pending.clear();
            state.pending_offset = 0;
            state.skip_frames = frame;
            state.position_frames = frame;
            state.eof = false;
            state.fallback = None;
            Ok(())
        }
        Err(error) => {
            if is_opus(&bytes) {
                let template = silent_samples();
                let channels = usize::from(SILENT_CHANNELS);
                let offset = (frame as usize * channels).min(template.len());
                state.spec = silent_spec();
                state.setup = None;
                state.pending = template.clone();
                state.pending_offset = offset;
                state.skip_frames = 0;
                state.position_frames = frame;
                state.eof = false;
                state.fallback = Some(template);
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}
