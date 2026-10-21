//! FFmpeg audio decode (Linux/desktop) — the desktop build's first choice
//! for a track, ahead of the built-in decoder.
//!
//! The built-in decoder is symphonia, which has no SBR synthesis: an AAC
//! track with the SBR extension (HE-AAC) is decoded at the core's rate while
//! the container declares the extension's rate, so the track would play at the
//! wrong speed. FFmpeg's AAC decoder reconstructs the SBR band, so this module
//! decodes such a track at the rate the container declares.
//!
//! The whole track is materialized: the caller plays the returned buffer.
//! The track decoding itself is shared with the FFmpeg video backend, which
//! decodes a movie's audio track the same way.
//!
//! # The container's priming
//!
//! An AAC track is encoded with a delay: the first frames hold the encoder's
//! warm-up, not the recording, and the container says how many samples they
//! are — the track's `initial_padding`, which the demuxer hands to the
//! decoder as the skip-samples side data of the first packet. The decoder
//! removes them, so the track this module returns starts at the recording
//! and ends at the recording's end; without that the voice would start a
//! frame late and run long.
//!
//! # The channel layout
//!
//! The track's channel count is the one the container declares. A track whose
//! `AudioSpecificConfig` declares a mono core stays mono even when its
//! bitstream carries the parametric-stereo extension: the extension is only
//! reconstructed for a track the container declares as stereo.

use std::io::Cursor;
use std::sync::Arc;

use api::error::MediaError;
use api::media::{AudioSpec, DecodedAudio};
use ffmpeg::ChannelLayout;
use ffmpeg::Error as FfmpegError;
use ffmpeg::Packet;
use ffmpeg::format::Sample;
use ffmpeg::format::context::Input;
use ffmpeg::format::context::StreamIo;
use ffmpeg::format::sample;
use ffmpeg::media::Type as MediaType;
use ffmpeg::software::resampling;
use ffmpeg::util::frame::audio::Audio as AudioFrame;
use ffmpeg_next as ffmpeg;

use crate::Media;
use crate::hint::{display_name, extension_of};

/// Decodes a whole audio track with FFmpeg into interleaved `f32` samples.
///
/// The result carries the decoded [`AudioSpec`]; the samples are interleaved
/// per channel (`interleaved[frame * channels + channel]`). The whole track
/// is materialized, so the caller owns the samples and no decoding happens
/// during playback.
///
/// `hint` is the storage name or extension that located the bytes;
/// it only steers the container detection and is never trusted over the
/// content. A track that cannot be decoded returns a typed [`MediaError`] —
/// no silence is substituted.
pub fn decode_audio_ffmpeg(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<Arc<DecodedAudio>, MediaError> {
    let _ = m;
    let name = display_name(hint, "<audio>");
    let mut input = open_input(&bytes, hint, name)?;
    let stream_index = input
        .streams()
        .best(MediaType::Audio)
        .map(|stream| stream.index())
        .ok_or_else(|| MediaError::Unsupported {
            message: format!("{name}: no audio stream"),
        })?;
    let track = decode_audio_track(&mut input, stream_index, name)?;
    Ok(Arc::new(track))
}

/// Opens an in-memory FFmpeg demuxer over `bytes`.
///
/// The extension hint is passed to FFmpeg for format detection only; the
/// bytes always decide the container. The `Arc<[u8]>` is shared into the
/// `StreamIo`, so no whole-buffer copy is made.
pub(crate) fn open_input(
    bytes: &Arc<[u8]>,
    hint: Option<&str>,
    name: &str,
) -> Result<Input, MediaError> {
    ffmpeg::init().map_err(|error| MediaError::Io {
        message: format!("{name}: ffmpeg init failed: {error}"),
    })?;
    let probe_name = hint
        .and_then(extension_of)
        .map(|ext| format!("input.{ext}"));
    let io = StreamIo::from_read_seek(Cursor::new(Arc::clone(bytes))).map_err(|error| {
        MediaError::Io {
            message: format!("{name}: {error}"),
        }
    })?;
    ffmpeg::format::input_from_stream(io, probe_name.as_deref(), None).map_err(|error| {
        MediaError::Decode {
            format: String::new(),
            message: format!("{name}: open demuxer: {error}"),
        }
    })
}

/// Decodes every packet of the audio track `stream_index` into interleaved
/// `f32`, resampling to the decoder's native rate/layout.
pub(crate) fn decode_audio_track(
    input: &mut Input,
    stream_index: usize,
    name: &str,
) -> Result<DecodedAudio, MediaError> {
    let parameters = input
        .stream(stream_index)
        .map(|stream| stream.parameters())
        .ok_or_else(|| MediaError::InvalidState {
            message: format!("{name}: audio stream vanished after probing"),
        })?;
    let context =
        ffmpeg::codec::context::Context::from_parameters(parameters).map_err(|error| {
            MediaError::Unsupported {
                message: format!("{name}: {error}"),
            }
        })?;
    let mut decoder = context
        .decoder()
        .audio()
        .map_err(|error| MediaError::Unsupported {
            message: format!("{name}: no audio decoder: {error}"),
        })?;
    // The decoder knows the stream's channel layout even when a decoded frame
    // does not repeat it.
    let decoder_layout = decoder.channel_layout();

    let mut samples: Vec<f32> = Vec::new();
    let mut spec = AudioSpec::default();
    let mut resampler: Option<resampling::Context> = None;
    // The input parameters the converter was built from (the sample format,
    // the channel layout mask and the rate), so a frame whose format, layout
    // or rate differs gets its own converter.
    let mut resampler_input: Option<(Sample, u64, u32)> = None;
    // The layout the track is converted to, known once the first frame has
    // been decoded.
    let mut track_layout: Option<ChannelLayout> = None;
    let mut demux_eof = false;

    loop {
        let mut frame = AudioFrame::empty();
        match decoder.receive_frame(&mut frame) {
            Ok(()) => {
                // The converter and the frame must describe the same audio:
                // the converter is built from the frame's own parameters, and
                // the frame carries the layout and the rate the converter
                // expects. A frame that does not repeat the stream's layout
                // or rate takes them from the decoder.
                let layout = canonical_layout(resolve_layout(
                    frame.channel_layout(),
                    decoder_layout,
                    name,
                )?);
                let rate = if frame.rate() > 0 {
                    frame.rate()
                } else {
                    decoder.rate()
                };
                frame.set_channel_layout(layout);
                frame.set_rate(rate);
                // The L2 mixer renders mono and stereo only, so a track with
                // more channels is downmixed to stereo.
                let out_layout = if layout.channels() > 2 {
                    ChannelLayout::STEREO
                } else {
                    layout
                };
                let input = (frame.format(), layout.bits(), rate);
                if resampler_input != Some(input) {
                    let converter = resampling::Context::get(
                        frame.format(),
                        layout,
                        rate,
                        Sample::F32(sample::Type::Packed),
                        out_layout,
                        rate,
                    )
                    .map_err(|error| MediaError::Decode {
                        format: String::new(),
                        message: format!("{name}: audio resampler: {error}"),
                    })?;
                    spec.sample_rate = rate;
                    spec.channels = channel_count(name, out_layout.channels())?;
                    track_layout = Some(out_layout);
                    resampler = Some(converter);
                    resampler_input = Some(input);
                }

                let converted = match resampler.as_mut() {
                    Some(converter) => resample_frame(converter, &frame, name)?,
                    None => {
                        return Err(MediaError::Decode {
                            format: String::new(),
                            message: format!("{name}: the audio resampler is missing"),
                        });
                    }
                };
                samples.extend_from_slice(&converted);
                continue;
            }
            Err(FfmpegError::Eof) => break,
            Err(FfmpegError::Other { errno }) if errno == ffmpeg::error::EAGAIN => {}
            Err(error) => {
                return Err(MediaError::Decode {
                    format: String::new(),
                    message: format!("{name}: audio decode: {error}"),
                });
            }
        }

        if demux_eof {
            break;
        }

        let mut packet = Packet::empty();
        match packet.read(input) {
            Ok(()) => {
                if packet.stream() == stream_index {
                    // The packet is handed to the decoder as it came: the
                    // demuxer's skip-samples side data, which carries the
                    // container's priming, is what makes the decoder drop
                    // the encoder's delay at the start of the track.
                    decoder
                        .send_packet(&packet)
                        .map_err(|error| MediaError::Decode {
                            format: String::new(),
                            message: format!("{name}: audio packet: {error}"),
                        })?;
                }
            }
            Err(FfmpegError::Eof) => {
                decoder.send_eof().map_err(|error| MediaError::Decode {
                    format: String::new(),
                    message: format!("{name}: audio decoder flush: {error}"),
                })?;
                demux_eof = true;
            }
            Err(error) => {
                return Err(MediaError::Io {
                    message: format!("{name}: {error}"),
                });
            }
        }
    }

    // The resampler may still hold samples of the track; flush them so the
    // tail of the track is not dropped.
    if let Some(converter) = resampler.as_mut()
        && let Some(delay) = converter.delay()
        && let Some(layout) = track_layout
    {
        let tail = usize::try_from(delay.output).map_err(|_| MediaError::Decode {
            format: String::new(),
            message: format!("{name}: the audio tail length is out of range"),
        })?;
        let mut output = AudioFrame::new(Sample::F32(sample::Type::Packed), tail, layout);
        converter
            .flush(&mut output)
            .map_err(|error| MediaError::Decode {
                format: String::new(),
                message: format!("{name}: audio flush: {error}"),
            })?;
        samples.extend_from_slice(audio_packed_f32(&output, name)?);
    }

    if samples.is_empty() {
        return Err(MediaError::Decode {
            format: String::new(),
            message: format!("{name}: no audio data decoded"),
        });
    }

    spec.total_frames = if spec.channels > 0 {
        Some(samples.len() as u64 / u64::from(spec.channels))
    } else {
        None
    };
    Ok(DecodedAudio::new(spec, samples))
}

/// Gives an unnamed layout its canonical name.
///
/// FFmpeg reports a PCM AU track as `channels: 1, mask: 0`; the resampler
/// compares the named layout and rejects that frame as an input change even
/// though the converter was built from the same mask. Naming mono/stereo
/// keeps the frame and the converter on the same layout.
fn canonical_layout(layout: ChannelLayout) -> ChannelLayout {
    if layout.bits() != 0 {
        return layout;
    }
    match layout.channels() {
        1 => ChannelLayout::MONO,
        2 => ChannelLayout::STEREO,
        _ => layout,
    }
}

/// The channel layout a decoded audio frame is converted from.
///
/// The frame's own layout is authoritative. A frame that carries none is
/// converted with the decoder's layout: the decoder knows the stream's
/// layout from the container even when the frame does not repeat it. A
/// track whose frame and decoder both carry no layout has an unknown
/// channel count, and the only sound layout that could be invented for it
/// would either drop half of a stereo track or read a mono track as
/// interleaved stereo, so it is reported instead.
pub(crate) fn resolve_layout(
    frame_layout: ChannelLayout,
    decoder_layout: ChannelLayout,
    name: &str,
) -> Result<ChannelLayout, MediaError> {
    if frame_layout.channels() > 0 {
        return Ok(frame_layout);
    }
    if decoder_layout.channels() > 0 {
        return Ok(decoder_layout);
    }
    Err(MediaError::InvalidData {
        message: format!("{name}: the audio track carries no channel layout"),
    })
}

/// Converts a decoder channel count to the `AudioSpec::channels` field.
///
/// The count is kept in a 32-bit field, so only a negative count cannot be
/// represented; it is reported rather than clamped.
pub(crate) fn channel_count(name: &str, channels: i32) -> Result<u32, MediaError> {
    u32::try_from(channels).map_err(|_| MediaError::InvalidData {
        message: format!("{name}: audio channel count {channels} is negative"),
    })
}

/// Runs the resampler over one decoded frame and copies the packed `f32`
/// samples out.
fn resample_frame(
    converter: &mut resampling::Context,
    frame: &AudioFrame,
    name: &str,
) -> Result<Vec<f32>, MediaError> {
    let mut output = AudioFrame::empty();
    converter
        .run(frame, &mut output)
        .map_err(|error| MediaError::Decode {
            format: String::new(),
            message: format!("{name}: audio resample: {error}"),
        })?;
    Ok(audio_packed_f32(&output, name)?.to_vec())
}

/// Borrows the single packed `f32` plane of an audio frame.
///
/// A frame with no plane carries no samples: that is a decode failure, not an
/// empty frame.
fn audio_packed_f32<'a>(frame: &'a AudioFrame, name: &str) -> Result<&'a [f32], MediaError> {
    if frame.planes() == 0 {
        return Err(MediaError::Decode {
            format: String::new(),
            message: format!("{name}: the converted audio frame carries no samples"),
        });
    }
    Ok(frame.plane::<f32>(0))
}
