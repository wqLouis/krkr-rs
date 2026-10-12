//! FFmpeg video backend (Linux/desktop) — linked dynamically through
//! pkg-config via `ffmpeg-next`.
//!
//! All state lives in `VideoDecoderState`: the in-memory demuxer, the video
//! decoder, the SwScale RGBA converter, the current frame and the decoded
//! audio track. `Media` stores nothing and there are no globals or caches.

use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use api::error::MediaError;
use api::media::{AudioSpec, DecodedAudio, RgbaFrame, VideoSpec};
use ffmpeg::ChannelLayout;
use ffmpeg::Error as FfmpegError;
use ffmpeg::Packet;
use ffmpeg::Rational;
use ffmpeg::format::Pixel;
use ffmpeg::format::Sample;
use ffmpeg::format::context::Input;
use ffmpeg::format::context::StreamIo;
use ffmpeg::format::sample;
use ffmpeg::media::Type as MediaType;
use ffmpeg::software::resampling;
use ffmpeg::software::scaling;
use ffmpeg::util::frame::audio::Audio as AudioFrame;
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg_next as ffmpeg;

use super::SEEK_TOLERANCE_MS;
use crate::Media;
use crate::hint::extension_of;

/// Number of bytes per RGBA8 pixel.
const RGBA_BYTES_PER_PIXEL: usize = 4;

/// Bottom-defined state for an incremental video decode.
///
/// Holds the demuxer/codec context, the SwScale converter, the current frame
/// and the decoded audio track. The middle layer owns this value and passes
/// it back on every call; `Media` never stores it. It is opaque on purpose,
/// so the backend implementation can change without an API break.
///
/// Dropping the state releases the FFmpeg contexts (RAII). It is deliberately
/// neither `Clone` nor `PartialEq`: it owns an open codec context.
#[non_exhaustive]
pub struct VideoDecoderState {
    /// Display name used in error messages (the caller's hint or `<video>`).
    name: String,
    /// The probed stream spec (video dimensions/fps plus audio parameters).
    spec: VideoSpec,
    /// The in-memory demuxer. Owns the compressed bytes through its
    /// `StreamIo`, and is reopened on `present` only if a seek is refused.
    input: Input,
    /// The opened video decoder.
    decoder: ffmpeg::codec::decoder::Video,
    /// Demuxer stream index of the video track.
    video_stream_index: usize,
    /// Video stream time base, used to convert frame timestamps to ms.
    time_base: Rational,
    /// SwScale context, built lazily from the first decoded frame so a
    /// mid-stream format change can rebuild it.
    scaler: Option<scaling::Context>,
    /// Scratch RGBA output frame reused across `present_video` calls.
    rgba: VideoFrame,
    /// The frame most recently returned by `present_video`.
    current: Option<RgbaFrame>,
    /// The fully decoded audio track (if the container has one).
    audio: Option<DecodedAudio>,
    /// Set once the demuxer has hit EOF and the decoder has been flushed.
    eof: bool,
}

impl fmt::Debug for VideoDecoderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoDecoderState")
            .field("spec", &self.spec)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

/// Returns the hint used in error messages.
fn display_name(hint: Option<&str>) -> &str {
    hint.filter(|name| !name.is_empty()).unwrap_or("<video>")
}

/// Builds the typed decode error for this stream.
fn decode_error(name: &str, error: FfmpegError) -> MediaError {
    MediaError::Decode {
        format: String::new(),
        message: format!("{name}: {error}"),
    }
}

/// Opens an in-memory FFmpeg demuxer over `bytes`.
///
/// The extension hint is passed to FFmpeg for format detection only; the
/// bytes always decide the container. The `Arc<[u8]>` is shared into the
/// `StreamIo`, so no whole-buffer copy is made.
fn open_input(bytes: &Arc<[u8]>, hint: Option<&str>, name: &str) -> Result<Input, MediaError> {
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

/// Counts the container's streams of `kind`.
fn count_streams(input: &Input, kind: MediaType) -> u32 {
    input
        .streams()
        .filter(|stream| stream.parameters().medium() == kind)
        .count() as u32
}

/// The 0-based position of `index` among the container's `kind` streams, or
/// `-1` when it is not one of them.
fn stream_index_of(input: &Input, kind: MediaType, index: usize) -> i32 {
    input
        .streams()
        .filter(|stream| stream.parameters().medium() == kind)
        .position(|stream| stream.index() == index)
        .map_or(-1, |position| position as i32)
}

/// Converts a stream/frame rational pair into a nominal fps.
fn fps_from(avg: Rational, frame_rate: Option<Rational>) -> f32 {
    for rate in [Some(avg), frame_rate].into_iter().flatten() {
        if rate.numerator() > 0 && rate.denominator() > 0 {
            return rate.numerator() as f32 / rate.denominator() as f32;
        }
    }
    0.0
}

/// Opens a video stream and probes its [`VideoSpec`].
pub fn open(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(VideoSpec, VideoDecoderState), MediaError> {
    let _ = m;
    let name = display_name(hint).to_string();
    let mut input = open_input(&bytes, hint, &name)?;

    let (video_stream_index, time_base, avg_frame_rate, total_frames_hint, duration_us) = {
        let stream =
            input
                .streams()
                .best(MediaType::Video)
                .ok_or_else(|| MediaError::Unsupported {
                    message: format!("{name}: no video stream"),
                })?;
        (
            stream.index(),
            stream.time_base(),
            stream.avg_frame_rate(),
            stream.frames(),
            input.duration(),
        )
    };

    let video_streams = count_streams(&input, MediaType::Video);
    let video_stream = stream_index_of(&input, MediaType::Video, video_stream_index);

    let parameters = input
        .stream(video_stream_index)
        .map(|stream| stream.parameters())
        .ok_or_else(|| MediaError::InvalidState {
            message: format!("{name}: video stream vanished after probing"),
        })?;
    let context =
        ffmpeg::codec::context::Context::from_parameters(parameters).map_err(|error| {
            MediaError::Unsupported {
                message: format!("{name}: {error}"),
            }
        })?;
    let decoder = context
        .decoder()
        .video()
        .map_err(|error| MediaError::Unsupported {
            message: format!("{name}: no video decoder: {error}"),
        })?;

    let width = decoder.width();
    let height = decoder.height();
    if width == 0 || height == 0 {
        return Err(MediaError::InvalidData {
            message: format!("{name}: video dimensions are not known"),
        });
    }
    let fps = fps_from(avg_frame_rate, decoder.frame_rate());

    let audio_stream_index = input
        .streams()
        .best(MediaType::Audio)
        .map(|stream| stream.index());
    let audio_streams = count_streams(&input, MediaType::Audio);
    let audio_stream =
        audio_stream_index.map_or(-1, |index| stream_index_of(&input, MediaType::Audio, index));
    let audio = match audio_stream_index {
        Some(index) => Some(decode_audio_track(&mut input, index, &name)?),
        None => None,
    };
    let (sample_rate, channels) = audio
        .as_ref()
        .map(|track| (track.spec.sample_rate, track.spec.channels))
        .unwrap_or((0, 0));

    // The audio pass consumed the demuxer; restart it so video can decode from
    // the first packet. Reopening is cheaper and less fragile than rewinding a
    // custom-IO context that has already latched EOF.
    if audio.is_some() {
        input = open_input(&bytes, hint, &name)?;
    }

    let duration_ms = u64::try_from(duration_us / 1000).ok().filter(|ms| *ms > 0);
    let total_frames = if total_frames_hint > 0 {
        Some(total_frames_hint as u64)
    } else {
        match (duration_ms, fps) {
            (Some(ms), fps) if fps > 0.0 => {
                Some(((ms as f64 / 1000.0) * f64::from(fps)).round() as u64)
            }
            _ => None,
        }
    };

    let spec = VideoSpec {
        width,
        height,
        fps,
        total_frames,
        sample_rate,
        channels,
        audio_streams,
        video_streams,
        audio_stream,
        video_stream,
    };

    let state = VideoDecoderState {
        name,
        spec,
        input,
        decoder,
        video_stream_index,
        time_base,
        scaler: None,
        rgba: VideoFrame::empty(),
        current: None,
        audio,
        eof: false,
    };
    Ok((spec, state))
}

/// Decodes every packet of the audio track `stream_index` into interleaved
/// `f32`, resampling to the decoder's native rate/layout.
fn decode_audio_track(
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

    let mut samples: Vec<f32> = Vec::new();
    let mut spec = AudioSpec::default();
    let mut resampler: Option<resampling::Context> = None;
    let mut demux_eof = false;

    loop {
        let mut frame = AudioFrame::empty();
        match decoder.receive_frame(&mut frame) {
            Ok(()) => {
                if resampler.is_none() {
                    let layout = audio_layout(&frame);
                    let rate = frame.rate();
                    // The reference's movie audio buffer converts to the
                    // mixer's stereo layout
                    // (`reference/cpp/core/movie/ffmpeg/AEFactory.cpp:150-175`);
                    // the L2 mixer renders mono/stereo only, so a
                    // multi-channel track is downmixed here.
                    let out_layout = if layout.channels() > 2 {
                        ChannelLayout::STEREO
                    } else {
                        layout
                    };
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
                    spec.channels = u16::try_from(out_layout.channels().max(0)).unwrap_or(0);
                    resampler = Some(converter);
                }

                let converted =
                    resample_frame(resampler.as_mut().expect("resampler"), &frame, name)?;
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
                    decoder
                        .send_packet(&packet)
                        .map_err(|error| MediaError::Decode {
                            format: String::new(),
                            message: format!("{name}: audio packet: {error}"),
                        })?;
                }
            }
            Err(FfmpegError::Eof) => {
                let _ = decoder.send_eof();
                demux_eof = true;
            }
            Err(error) => {
                return Err(MediaError::Io {
                    message: format!("{name}: {error}"),
                });
            }
        }
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

/// Returns the channel layout of a decoded frame, falling back to a default
/// layout when the decoder did not set one.
fn audio_layout(frame: &AudioFrame) -> ChannelLayout {
    let layout = frame.channel_layout();
    if layout.channels() > 0 {
        layout
    } else {
        ChannelLayout::default(i32::from(frame.channels().max(1)))
    }
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
    Ok(audio_packed_f32(&output).to_vec())
}

/// Borrows the single packed `f32` plane of an audio frame.
fn audio_packed_f32(frame: &AudioFrame) -> &[f32] {
    if frame.planes() == 0 {
        &[]
    } else {
        frame.plane::<f32>(0)
    }
}

/// Presents the frame at (or after) `at_ms`.
pub fn present(
    m: &Media,
    state: &mut VideoDecoderState,
    at_ms: u64,
) -> Result<RgbaFrame, MediaError> {
    let _ = m;

    // A request just behind the current frame is served by that frame rather
    // than seeking backwards for a negligible time difference.
    if let Some(current) = &state.current
        && at_ms <= current.pts_ms
        && at_ms.saturating_add(SEEK_TOLERANCE_MS) >= current.pts_ms
    {
        return Ok(current.clone());
    }

    let need_seek = match &state.current {
        Some(current) => at_ms.saturating_add(SEEK_TOLERANCE_MS) < current.pts_ms,
        None => true,
    };
    if need_seek {
        seek_to(state, at_ms)?;
    }

    loop {
        match decode_next(state)? {
            Some(frame) => {
                if frame.pts_ms >= at_ms {
                    state.current = Some(frame.clone());
                    return Ok(frame);
                }
                state.current = Some(frame);
            }
            None => {
                return state.current.clone().ok_or(MediaError::EndOfStream);
            }
        }
    }
}

/// Returns the decoded audio track, if the container has one.
pub fn audio(m: &Media, state: &VideoDecoderState) -> Result<DecodedAudio, MediaError> {
    let _ = m;
    state.audio.clone().ok_or_else(|| MediaError::Unsupported {
        message: format!("{}: no audio track", state.name),
    })
}

/// Seeks the demuxer to `at_ms` and flushes the video decoder.
fn seek_to(state: &mut VideoDecoderState, at_ms: u64) -> Result<(), MediaError> {
    // `avformat_seek_file` with stream index -1 expects AV_TIME_BASE units
    // (microseconds).
    let timestamp = i64::try_from(at_ms.saturating_mul(1000)).unwrap_or(i64::MAX);
    state
        .input
        .seek(timestamp, ..)
        .map_err(|error| MediaError::Io {
            message: format!("{}: seek failed: {error}", state.name),
        })?;
    state.decoder.flush();
    state.current = None;
    state.eof = false;
    Ok(())
}

/// Decodes the next video frame, feeding packets until the decoder produces
/// one. Returns `None` at end of stream.
fn decode_next(state: &mut VideoDecoderState) -> Result<Option<RgbaFrame>, MediaError> {
    loop {
        let mut frame = VideoFrame::empty();
        match state.decoder.receive_frame(&mut frame) {
            Ok(()) => return convert_frame(state, &frame).map(Some),
            Err(FfmpegError::Eof) => return Ok(None),
            Err(FfmpegError::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                if state.eof {
                    return Ok(None);
                }
            }
            Err(error) => return Err(decode_error(&state.name, error)),
        }

        let mut packet = Packet::empty();
        match packet.read(&mut state.input) {
            Ok(()) => {
                if packet.stream() == state.video_stream_index {
                    state
                        .decoder
                        .send_packet(&packet)
                        .map_err(|error| decode_error(&state.name, error))?;
                }
            }
            Err(FfmpegError::Eof) => {
                let _ = state.decoder.send_eof();
                state.eof = true;
            }
            Err(error) => {
                return Err(MediaError::Io {
                    message: format!("{}: {error}", state.name),
                });
            }
        }
    }
}

/// Converts a decoded frame to tightly packed RGBA8, rebuilding the scaler if
/// the frame format or dimensions changed.
fn convert_frame(
    state: &mut VideoDecoderState,
    frame: &VideoFrame,
) -> Result<RgbaFrame, MediaError> {
    let width = frame.width();
    let height = frame.height();
    if width == 0 || height == 0 {
        return Err(MediaError::InvalidData {
            message: format!("{}: decoded frame has no dimensions", state.name),
        });
    }

    let rebuild = match &state.scaler {
        Some(scaler) => {
            scaler.input().format != frame.format()
                || scaler.input().width != width
                || scaler.input().height != height
        }
        None => true,
    };
    if rebuild {
        state.scaler = Some(
            scaling::Context::get(
                frame.format(),
                width,
                height,
                Pixel::RGBA,
                width,
                height,
                scaling::Flags::BILINEAR,
            )
            .map_err(|error| MediaError::Decode {
                format: String::new(),
                message: format!("{}: scaler: {error}", state.name),
            })?,
        );
    }

    {
        let scaler = state.scaler.as_mut().expect("scaler was just built");
        scaler
            .run(frame, &mut state.rgba)
            .map_err(|error| MediaError::Decode {
                format: String::new(),
                message: format!("{}: scale to RGBA: {error}", state.name),
            })?;
    }

    let stride = state.rgba.stride(0);
    let row_bytes = width as usize * RGBA_BYTES_PER_PIXEL;
    let data = state.rgba.data(0);
    let mut rgba = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = row * stride;
        let end = start + row_bytes;
        if end > data.len() {
            return Err(MediaError::Decode {
                format: String::new(),
                message: format!("{}: RGBA frame is smaller than expected", state.name),
            });
        }
        rgba.extend_from_slice(&data[start..end]);
    }

    let pts_ms = frame_pts_ms(state.time_base, frame);
    Ok(RgbaFrame::new(width, height, pts_ms, rgba))
}

/// Converts a frame timestamp in stream time-base units to milliseconds.
fn frame_pts_ms(time_base: Rational, frame: &VideoFrame) -> u64 {
    let Some(timestamp) = frame.timestamp().or_else(|| frame.pts()) else {
        return 0;
    };
    let numerator = i128::from(time_base.numerator());
    let denominator = i128::from(time_base.denominator());
    if timestamp <= 0 || numerator <= 0 || denominator <= 0 {
        return 0;
    }
    let ms = i128::from(timestamp) * numerator * 1000 / denominator;
    u64::try_from(ms).unwrap_or(0)
}
