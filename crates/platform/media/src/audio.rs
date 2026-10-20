//! Audio decoding for the stateless [`Media`] engine.
//!
//! This is the `media` replacement for `tvp-sound/src/decode.rs`. It
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
//! # Stateless object, explicit state
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
//! `flac`, `mp3`, `mp4`/`aac`) plus the separately shipped
//! [`symphonia_adapter_libopus::OpusDecoder`]. A stream that cannot be
//! probed or decoded returns a typed [`MediaError`]; no silence or other
//! placeholder is substituted.

use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use api::error::MediaError;
use api::media::{AudioSpec, DecodedAudio};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;

use crate::Media;
use crate::hint::{display_name, extension_of};

/// Audio format/codec names advertised by [`crate::capabilities`].
pub(crate) const AUDIO_FORMATS: [&str; 9] = [
    "wav", "pcm", "ogg", "vorbis", "flac", "mp3", "opus", "mp4", "aac",
];

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
    /// Opened format reader + decoder.
    setup: Option<DecoderSetup<'static>>,
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
}

impl fmt::Debug for AudioDecoderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioDecoderState")
            .field("spec", &self.spec)
            .field("position_frames", &self.position_frames)
            .field("skip_frames", &self.skip_frames)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

/// An opened format reader + decoder, before any packet is consumed.
///
/// The reader borrows its byte source for `'a`; the streaming state stores a
/// `'static` reader (backed by an `Arc<[u8]>`), while whole-file
/// probe/decode use a `&[u8]`-backed reader that never escapes the call.
struct DecoderSetup<'a> {
    format: Box<dyn FormatReader + 'a>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
}

/// Everything [`open_setup`] produces: the decoder plus the probed [`AudioSpec`].
struct OpenedAudio<'a> {
    format: Box<dyn FormatReader + 'a>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    spec: AudioSpec,
}

/// Builds a fresh codec registry with the default codecs plus the Opus
/// adapter.
///
/// Deliberately *not* cached in a global/lazy: hidden
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
/// Shared by [`probe_audio`], [`decode_audio`], [`open_audio`] and
/// [`seek_audio`]. The extension hint only nudges the probe; the bytes always
/// decide the format. `source` may be a borrowed `Cursor<&[u8]>` (whole-file
/// probe/decode) or an owned `Cursor<Arc<[u8]>>` (the streaming state), so no
/// whole-buffer copy is needed on any path.
fn open_setup<'a, S, F>(source: F, hint: Option<&str>) -> Result<OpenedAudio<'a>, MediaError>
where
    S: MediaSource + 'a,
    F: Fn() -> S,
{
    let name = display_name(hint, "<audio>");
    // The hint only steers the probe. A name whose extension does not match
    // the content sends the probe down the wrong path, so a failed hinted
    // probe is retried without the hint: the bytes decide the format.
    match probe_source(source(), hint) {
        Ok(opened) => Ok(opened),
        Err(hinted) => match probe_source(source(), None) {
            Ok(opened) => Ok(opened),
            Err(unhinted) => Err(MediaError::Decode {
                format: String::new(),
                message: format!("{name}: {hinted}; without the name hint: {unhinted}"),
            }),
        },
    }
}

/// Probes the container with `hint` and builds the decoder.
///
/// The error is the probe's own message; [`open_setup`] combines the hinted
/// and the unhinted attempt.
fn probe_source<'a, S>(source: S, hint: Option<&str>) -> Result<OpenedAudio<'a>, String>
where
    S: MediaSource + 'a,
{
    let name = display_name(hint, "<audio>");
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    let mut symphonia_hint = Hint::new();
    if let Some(ext) = hint.and_then(extension_of) {
        symphonia_hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
        .probe(
            &symphonia_hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| error.to_string())?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| format!("{name}: no decodable audio track"))?;
    let track_id = track.id;
    let codec_params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| format!("{name}: no audio codec parameters"))?;
    let sample_rate = codec_params.sample_rate.unwrap_or(0);
    let channels = match codec_params.channels.as_ref() {
        Some(channels) => {
            let count = channels.count();
            u32::try_from(count).map_err(|_| {
                format!("{name}: audio channel count {count} does not fit in 32 bits")
            })?
        }
        None => 0,
    };
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
        .map_err(|error| format!("{name}: {error}"))?;

    Ok(OpenedAudio {
        format,
        decoder,
        track_id,
        spec,
    })
}

/// Probes an audio stream for its [spec](AudioSpec) without decoding samples.
///
/// `hint` is the storage name or extension the caller used to find `bytes`;
/// it only steers backend selection and is never trusted over the content.
/// A stream that cannot be probed returns a typed [`MediaError`].
pub fn probe_audio(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<AudioSpec, MediaError> {
    let _ = m;
    open_setup(|| Cursor::new(bytes), hint).map(|opened| opened.spec)
}

/// Decodes a whole audio file into interleaved `f32` samples.
///
/// The result carries the probed [`AudioSpec`]; the samples are interleaved
/// per channel (`interleaved[frame * channels + channel]`). A stream that
/// cannot be decoded returns a typed [`MediaError`] — no silence is
/// substituted.
pub fn decode_audio(
    m: &Media,
    bytes: &[u8],
    hint: Option<&str>,
) -> Result<DecodedAudio, MediaError> {
    let _ = m;
    let opened = open_setup(|| Cursor::new(bytes), hint)?;
    decode_all(opened, hint)
}

/// Drains every packet of an opened stream into interleaved `f32` samples.
///
/// Used by [`decode_audio`], which decodes straight from the caller's
/// borrowed buffer (via a `Cursor<&[u8]>` reader) and therefore never needs
/// an [`AudioDecoderState`] or an owned copy of the compressed bytes.
fn decode_all(opened: OpenedAudio<'_>, hint: Option<&str>) -> Result<DecodedAudio, MediaError> {
    let OpenedAudio {
        format,
        decoder,
        track_id,
        mut spec,
    } = opened;
    let mut setup = DecoderSetup {
        format,
        decoder,
        track_id,
    };
    let name = display_name(hint, "<audio>");
    let mut samples: Vec<f32> = Vec::new();
    let mut pending: Vec<f32> = Vec::new();
    let mut pending_offset = 0usize;
    let mut skip_frames = 0u64;

    loop {
        if pending_offset < pending.len() {
            samples.extend_from_slice(&pending[pending_offset..]);
            pending.clear();
            pending_offset = 0;
            continue;
        }
        match decode_packet_into(
            &mut spec,
            &mut setup,
            &mut pending,
            &mut pending_offset,
            &mut skip_frames,
            name,
        ) {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => return Err(error),
        }
    }

    if samples.is_empty() {
        return Err(MediaError::Decode {
            format: String::new(),
            message: format!("{name}: no audio data decoded"),
        });
    }

    Ok(DecodedAudio::new(spec, samples))
}

/// Opens an audio stream for incremental decoding.
///
/// On success returns the stream's [`AudioSpec`] plus a fresh
/// [`AudioDecoderState`] for [`read_audio`]/[`seek_audio`]. `bytes` is shared
/// by `Arc` so the caller can keep the compressed buffer alive (and so
/// [`seek_audio`] can restart the reader without re-copying it).
///
/// The first packet is decoded here, so the returned spec is the format the
/// decoder really produces. A track whose decoder produces a different rate
/// than the container declares (an AAC track with the SBR extension, whose
/// decoder has no SBR synthesis) is reported at the open instead of playing
/// at the wrong speed; the decoded packet is staged in the state, so nothing
/// is lost.
///
/// A stream that cannot be opened returns a typed [`MediaError`].
pub fn open_audio(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(AudioSpec, AudioDecoderState), MediaError> {
    let _ = m;
    let hint_owned = hint.map(str::to_string);
    let opened = open_setup(|| Cursor::new(Arc::clone(&bytes)), hint)?;
    let spec = opened.spec;
    let mut state = AudioDecoderState {
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
    };
    if !decode_next_packet(&mut state)? {
        return Err(MediaError::Decode {
            format: String::new(),
            message: format!("{}: no audio data decoded", display_name(hint, "<audio>")),
        });
    }
    // The decoder's own format, not the container's.
    let spec = state.spec;
    Ok((spec, state))
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
    let channels =
        usize::try_from(state.spec.channels.max(1)).map_err(|_| MediaError::InvalidData {
            message: format!(
                "audio channel count {} does not fit in memory",
                state.spec.channels
            ),
        })?;
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
    let AudioDecoderState {
        spec,
        setup,
        pending,
        pending_offset,
        skip_frames,
        hint,
        ..
    } = state;
    let Some(setup) = setup.as_mut() else {
        return Ok(false);
    };
    let name = display_name(hint.as_deref(), "<audio>");
    decode_packet_into(spec, setup, pending, pending_offset, skip_frames, name)
}

/// Decodes one packet of `setup` into `pending`, honouring a pending seek
/// skip. Shared by the streaming path ([`decode_next_packet`]) and the
/// whole-file path ([`decode_all`]).
///
/// Returns `Ok(false)` at end of stream and `Ok(true)` when samples were
/// staged (possibly zero samples from a corrupt packet, which is skipped).
/// `name` is the caller's display hint; the detected container name is
/// reported in [`MediaError::Decode::format`].
fn decode_packet_into(
    spec: &mut AudioSpec,
    setup: &mut DecoderSetup<'_>,
    pending: &mut Vec<f32>,
    pending_offset: &mut usize,
    skip_frames: &mut u64,
    name: &str,
) -> Result<bool, MediaError> {
    let format = setup.format.format_info().short_name;

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
                    format: format.to_string(),
                    message: format!("{name}: {error}"),
                });
            }
        };
        if packet.track_id != setup.track_id {
            continue;
        }

        match setup.decoder.decode(&packet) {
            Ok(decoded) => {
                let rate = decoded.spec().rate();
                if rate != spec.sample_rate {
                    // The decoder produces a different rate than the container
                    // declares. An AAC track with the SBR extension declares
                    // the extension's rate while the decoder produces the
                    // core's, and the decoder has no SBR synthesis, so the
                    // track would play at the wrong speed. The stream is
                    // reported instead of approximated.
                    return Err(MediaError::Unsupported {
                        message: format!(
                            "{name}: the decoder produces {rate} Hz while the container \
                             declares {} Hz: the track uses a feature the decoder does \
                             not provide",
                            spec.sample_rate
                        ),
                    });
                }
                spec.sample_rate = decoded.spec().rate();
                let count = decoded.spec().channels().count();
                spec.channels = u32::try_from(count).map_err(|_| MediaError::InvalidData {
                    message: format!("{name}: audio channel count {count} does not fit in 32 bits"),
                })?;
                let start = pending.len();
                pending.resize(start + decoded.samples_interleaved(), 0.0);
                decoded.copy_to_slice_interleaved(&mut pending[start..]);

                // Drop whole frames already seeked past. `pending` is empty on
                // entry (read_audio only decodes once it is drained), but the
                // offset is kept general.
                if *skip_frames > 0 {
                    let channels = usize::try_from(spec.channels.max(1)).map_err(|_| {
                        MediaError::InvalidData {
                            message: format!(
                                "audio channel count {} does not fit in memory",
                                spec.channels
                            ),
                        }
                    })?;
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
                    format: format.to_string(),
                    message: format!("{name}: {error}"),
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
/// immediately. A stream that cannot be reopened returns a typed
/// [`MediaError`].
pub fn seek_audio(m: &Media, state: &mut AudioDecoderState, frame: u64) -> Result<(), MediaError> {
    let _ = m;
    let hint = state.hint.clone();
    match open_setup(|| Cursor::new(Arc::clone(&state.bytes)), hint.as_deref()) {
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
            Ok(())
        }
        Err(error) => Err(error),
    }
}
