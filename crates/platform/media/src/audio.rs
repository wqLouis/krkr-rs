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
    /// Interleaved samples still to drop (the container's delay, or the
    /// samples a seek restarts at).
    skip_samples: u64,
    /// The timing the container declares, when it declares any.
    timing: Option<EditTiming>,
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
            .field("skip_samples", &self.skip_samples)
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
    /// The timing the container declares, when it declares any.
    timing: Option<EditTiming>,
}

/// The timing a container declares for a track.
///
/// A track is encoded with a delay: the first frames hold the encoder's
/// warm-up, not the recording, and the container states how many samples
/// they are. It also states how long the track is presented, so the samples
/// the encoder added at the end are not part of it. The values are counted
/// in the samples of the decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditTiming {
    /// The leading samples the encoder inserted; the decoder skips them.
    delay: u64,
    /// The samples the container presents.
    total: u64,
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
///
/// `edit` is the timing the container declares for its track, read from the
/// container's own bytes; a container that declares none leaves the track's
/// length as the decoder produces it.
fn open_setup<'a, S, F>(
    source: F,
    hint: Option<&str>,
    edit: Option<Mp4Edit>,
) -> Result<OpenedAudio<'a>, MediaError>
where
    S: MediaSource + 'a,
    F: Fn() -> S,
{
    let name = display_name(hint, "<audio>");
    // The hint only steers the probe. A name whose extension does not match
    // the content sends the probe down the wrong path, so a failed hinted
    // probe is retried without the hint: the bytes decide the format.
    match probe_source(source(), hint, edit) {
        Ok(opened) => Ok(opened),
        Err(hinted) => match probe_source(source(), None, edit) {
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
fn probe_source<'a, S>(
    source: S,
    hint: Option<&str>,
    edit: Option<Mp4Edit>,
) -> Result<OpenedAudio<'a>, String>
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
    // The container's own timing decides the length of the track when it
    // declares one: the samples the encoder added at the end are not part of
    // the presentation.
    let timing = match edit {
        Some(edit) => Some(
            edit.timing(sample_rate)
                .map_err(|error| error.to_string())?,
        ),
        None => None,
    };
    let total_frames = match timing {
        Some(timing) => Some(timing.total),
        None => total_frames,
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
        timing,
    })
}

/// Probes an audio stream for its [spec](AudioSpec) without decoding samples.
///
/// `hint` is the storage name or extension the caller used to find `bytes`;
/// it only steers backend selection and is never trusted over the content.
/// A stream that cannot be probed returns a typed [`MediaError`].
pub fn probe_audio(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<AudioSpec, MediaError> {
    let _ = m;
    let edit = mp4_edit(bytes);
    open_setup(|| Cursor::new(bytes), hint, edit).map(|opened| opened.spec)
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
    let edit = mp4_edit(bytes);
    let opened = open_setup(|| Cursor::new(bytes), hint, edit)?;
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
        timing,
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
    // The container's delay is the priming the encoder inserted: it is not
    // part of the recording and is skipped.
    let mut skip_samples = timing
        .map_or(0, |timing| timing.delay)
        .saturating_mul(u64::from(spec.channels.max(1)));

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
            &mut skip_samples,
            name,
        ) {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => return Err(error),
        }
    }

    // The samples the encoder added at the end are not part of the
    // presentation: the container states its length, and the track stops
    // there.
    if let Some(timing) = timing {
        let channels = usize::try_from(spec.channels).map_err(|_| MediaError::InvalidData {
            message: format!(
                "{name}: audio channel count {} does not fit in memory",
                spec.channels
            ),
        })?;
        let frames = usize::try_from(timing.total).map_err(|_| MediaError::InvalidData {
            message: format!("{name}: the container's length does not fit in memory"),
        })?;
        let presented = frames
            .checked_mul(channels)
            .ok_or_else(|| MediaError::InvalidData {
                message: format!("{name}: the container's length does not fit in memory"),
            })?;
        samples.truncate(presented.min(samples.len()));
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
    let edit = mp4_edit(&bytes);
    let opened = open_setup(|| Cursor::new(Arc::clone(&bytes)), hint, edit)?;
    let spec = opened.spec;
    let timing = opened.timing;
    // The container's delay is the priming the encoder inserted: it is not
    // part of the recording and is skipped.
    let skip_samples = timing
        .map_or(0, |timing| timing.delay)
        .saturating_mul(u64::from(spec.channels.max(1)));
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
        skip_samples,
        timing,
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
        // The container states how long the track is presented: the samples
        // the encoder added at the end are not part of it, so the track stops
        // there.
        if let Some(timing) = state.timing
            && state.position_frames + (written / channels) as u64 >= timing.total
        {
            state.eof = true;
            break;
        }
        // Drain staged samples before touching the reader.
        if state.pending_offset < state.pending.len() {
            let available = state.pending.len() - state.pending_offset;
            let mut take = available.min(frame_aligned - written);
            // The container states how long the track is presented: the
            // samples the encoder added at the end are not part of it, so the
            // track stops there even inside a decoded packet.
            if let Some(timing) = state.timing {
                let returned = state.position_frames + (written / channels) as u64;
                let remaining = timing.total.saturating_sub(returned);
                let remaining_samples = remaining.saturating_mul(channels as u64);
                if remaining_samples < take as u64 {
                    // The remaining length is smaller than the buffer, so it
                    // fits in memory.
                    take = usize::try_from(remaining_samples).map_err(|_| {
                        MediaError::InvalidData {
                            message: "the container's length does not fit in memory".to_owned(),
                        }
                    })?;
                }
                if take == 0 {
                    state.eof = true;
                    break;
                }
            }
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
        skip_samples,
        hint,
        ..
    } = state;
    let Some(setup) = setup.as_mut() else {
        return Ok(false);
    };
    let name = display_name(hint.as_deref(), "<audio>");
    decode_packet_into(spec, setup, pending, pending_offset, skip_samples, name)
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
    skip_samples: &mut u64,
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

                // Drop the samples already seeked past or already declared as
                // the container's delay. `pending` is empty on entry
                // (read_audio only decodes once it is drained), but the
                // offset is kept general.
                if *skip_samples > 0 {
                    let available = (pending.len() - *pending_offset) as u64;
                    if available <= *skip_samples {
                        *skip_samples -= available;
                        pending.clear();
                        *pending_offset = 0;
                    } else {
                        let drop = usize::try_from(*skip_samples).map_err(|_| {
                            MediaError::InvalidData {
                                message: format!(
                                    "{name}: the number of samples to skip does not fit in \
                                     memory"
                                ),
                            }
                        })?;
                        pending.drain(*pending_offset..*pending_offset + drop);
                        *pending_offset = 0;
                        *skip_samples = 0;
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
    let edit = mp4_edit(&state.bytes);
    match open_setup(
        || Cursor::new(Arc::clone(&state.bytes)),
        hint.as_deref(),
        edit,
    ) {
        Ok(opened) => {
            state.spec = opened.spec;
            state.timing = opened.timing;
            state.setup = Some(DecoderSetup {
                format: opened.format,
                decoder: opened.decoder,
                track_id: opened.track_id,
            });
            state.pending.clear();
            state.pending_offset = 0;
            // The seek's frame is counted in the presentation, so the
            // container's delay comes before it.
            let delay = state.timing.map_or(0, |timing| timing.delay);
            state.skip_samples = delay
                .saturating_add(frame)
                .saturating_mul(u64::from(state.spec.channels.max(1)));
            state.position_frames = frame;
            state.eof = false;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// The timing an MP4 container declares for its audio track.
///
/// An MP4 states the timing of a track in its edit list: the media time the
/// presentation starts at (the encoder's delay, the priming the decoder must
/// skip) and the length of the presentation (the samples the encoder added at
/// the end are not part of it). The values are in the container's own
/// timescales.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mp4Edit {
    /// The media time the presentation starts at, in media timescale ticks.
    media_time: i64,
    /// The sum of the edits' segment durations, in movie timescale ticks.
    segment_duration: u64,
    /// The movie's timescale, in ticks per second.
    movie_timescale: u32,
    /// The media's timescale, in ticks per second.
    media_timescale: u32,
}

impl Mp4Edit {
    /// The edit list's timing in the samples of a decoder running at
    /// `sample_rate`.
    ///
    /// The media time and the segment duration are in the container's
    /// timescales; the decoder's samples are counted at its own rate.
    fn timing(&self, sample_rate: u32) -> Result<EditTiming, MediaError> {
        let delay = match u64::try_from(self.media_time) {
            Ok(ticks) if ticks > 0 => ticks_to_samples(ticks, sample_rate, self.media_timescale)?,
            _ => 0,
        };
        let total = ticks_to_samples(self.segment_duration, sample_rate, self.movie_timescale)?;
        Ok(EditTiming { delay, total })
    }
}

/// Converts container ticks to the samples of a decoder running at `rate`.
fn ticks_to_samples(ticks: u64, rate: u32, timescale: u32) -> Result<u64, MediaError> {
    if timescale == 0 {
        return Err(MediaError::InvalidData {
            message: "the container declares a timescale of zero".to_owned(),
        });
    }
    let numerator = u128::from(ticks) * u128::from(rate);
    let denominator = u128::from(timescale);
    // Rounded to the nearest sample.
    let samples = (numerator + denominator / 2) / denominator;
    u64::try_from(samples).map_err(|_| MediaError::InvalidData {
        message: format!("the container's timing does not fit in 64 bits: {samples} samples"),
    })
}

/// Reads the edit list of the first sound track of an ISO-MP4 file.
///
/// A file that is not an MP4, or that declares no edit list, states no
/// timing: `None` is returned and the track keeps the length the decoder
/// produces.
fn mp4_edit(bytes: &[u8]) -> Option<Mp4Edit> {
    let moov = mp4_child(bytes, b"moov")?;
    let movie_timescale = mp4_timescale(mp4_child(moov, b"mvhd")?)?;
    for (kind, trak) in mp4_boxes(moov) {
        if kind != *b"trak" {
            continue;
        }
        let Some(mdia) = mp4_child(trak, b"mdia") else {
            continue;
        };
        // The handler type tells the sound track from the other tracks.
        let Some(handler) = mp4_child(mdia, b"hdlr").and_then(|hdlr| hdlr.get(8..12)) else {
            continue;
        };
        if handler != b"soun".as_slice() {
            continue;
        }
        let Some(media_timescale) = mp4_child(mdia, b"mdhd").and_then(mp4_timescale) else {
            continue;
        };
        let Some(elst) = mp4_child(trak, b"edts").and_then(|edts| mp4_child(edts, b"elst")) else {
            continue;
        };
        return mp4_edit_entries(elst, movie_timescale, media_timescale);
    }
    None
}

/// The payload of the first box of `kind` directly inside `data`.
fn mp4_child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    mp4_boxes(data)
        .find(|(found, _)| found == kind)
        .map(|(_, payload)| payload)
}

/// The boxes directly inside `data`, in order.
fn mp4_boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut offset = 0usize;
    std::iter::from_fn(move || {
        let header = data.get(offset..offset + 8)?;
        let size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = [header[4], header[5], header[6], header[7]];
        let (size, header_len) = match size {
            // A 64-bit size follows the type.
            1 => {
                let wide = data.get(offset + 8..offset + 16)?;
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(wide);
                (usize::try_from(u64::from_be_bytes(bytes)).ok()?, 16)
            }
            // The box extends to the end of the data.
            0 => (data.len() - offset, 8),
            _ => (size, 8),
        };
        if size < header_len {
            return None;
        }
        let end = offset.checked_add(size)?;
        if end > data.len() {
            return None;
        }
        let payload = &data[offset + header_len..end];
        offset = end;
        Some((kind, payload))
    })
}

/// The timescale of a movie or media header box.
fn mp4_timescale(payload: &[u8]) -> Option<u32> {
    let version = *payload.first()?;
    // The version decides the width of the creation and modification times
    // that precede the timescale.
    let offset = if version == 1 { 20 } else { 12 };
    let bytes = payload.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// The entries of an edit list, as the media time and the segment duration.
fn mp4_edit_entries(elst: &[u8], movie_timescale: u32, media_timescale: u32) -> Option<Mp4Edit> {
    let version = *elst.first()?;
    let count_bytes = elst.get(4..8)?;
    let count = u32::from_be_bytes([
        count_bytes[0],
        count_bytes[1],
        count_bytes[2],
        count_bytes[3],
    ]) as usize;
    let mut pos = 8usize;
    let mut segment_duration = 0u64;
    let mut media_time = 0i64;
    for index in 0..count {
        let (segment, time) = if version == 1 {
            let bytes = elst.get(pos..pos + 16)?;
            let segment = u64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            let time = i64::from_be_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]);
            pos += 16;
            (segment, time)
        } else {
            let bytes = elst.get(pos..pos + 8)?;
            let segment = u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
            let time = i64::from(i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]));
            pos += 8;
            (segment, time)
        };
        // The media rate (a 16.16 fixed-point number) follows the time.
        pos += 4;
        if index == 0 {
            media_time = time;
        }
        segment_duration = segment_duration.checked_add(segment)?;
    }
    Some(Mp4Edit {
        media_time,
        segment_duration,
        movie_timescale,
        media_timescale,
    })
}
