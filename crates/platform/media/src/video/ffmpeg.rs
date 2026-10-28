//! FFmpeg video backend (Linux/desktop) — linked dynamically through
//! pkg-config via `ffmpeg-next`.
//!
//! All state lives in `VideoDecoderState`: the in-memory demuxer, the video
//! decoder, the SwScale RGBA converter, the current frame and the decoded
//! audio track. `Media` stores nothing and there are no globals or caches.

use std::fmt;
use std::sync::Arc;

use api::error::MediaError;
use api::media::{DecodedAudio, RgbaFrame, VideoSpec};
use ffmpeg::Error as FfmpegError;
use ffmpeg::Packet;
use ffmpeg::Rational;
use ffmpeg::format::Pixel;
use ffmpeg::format::context::Input;
use ffmpeg::media::Type as MediaType;
use ffmpeg::software::scaling;
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg_next as ffmpeg;

use super::SEEK_TOLERANCE_MS;
use crate::Media;
use crate::audio_ffmpeg::{decode_audio_track, open_input};
use crate::hint::display_name;

/// Number of bytes per RGBA8 pixel.
const RGBA_BYTES_PER_PIXEL: usize = 4;

/// The length of a picture of a stream without a frame rate, in
/// milliseconds: the twenty-five pictures per second the movie player
/// assumes.
const DEFAULT_FRAME_MS: u64 = 40;

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
    /// Video stream time base, which converts frame timestamps to ms.
    time_base: Rational,
    /// Whether the demuxer can read the stream from an earlier time. A
    /// stream without an index cannot, and it is read forward only.
    seekable: bool,
    /// The clock the pictures without a timestamp are stamped with.
    picture_clock_ms: u64,
    /// SwScale context, built lazily from the first decoded frame so a
    /// mid-stream format change can rebuild it.
    scaler: Option<scaling::Context>,
    /// Scratch RGBA output frame reused across `present_video` calls.
    rgba: VideoFrame,
    /// The picture most recently returned by `present_video`, shared.
    current: Option<Arc<RgbaFrame>>,
    /// The first picture after the last request, shared.
    pending: Option<Arc<RgbaFrame>>,
    /// The fully decoded audio track (if the container has one), shared with
    /// whoever plays it.
    audio: Option<Arc<DecodedAudio>>,
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

/// Builds the typed decode error for this stream.
fn decode_error(name: &str, error: FfmpegError) -> MediaError {
    MediaError::Decode {
        format: String::new(),
        message: format!("{name}: {error}"),
    }
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
pub(crate) fn open(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(VideoSpec, VideoDecoderState), MediaError> {
    let _ = m;
    let name = display_name(hint, "<video>").to_string();
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
        seekable: true,
        picture_clock_ms: 0,
        scaler: None,
        rgba: VideoFrame::empty(),
        current: None,
        pending: None,
        audio: audio.map(Arc::new),
        eof: false,
    };
    Ok((spec, state))
}

/// Presents the picture the clock has reached at `at_ms`.
///
/// The picture is the latest one at or before the requested time: the
/// timestamp the clock has reached. The stream is read forward until a
/// picture after the requested time is found, and the picture before it is
/// the one presented.
pub(crate) fn present(
    m: &Media,
    state: &mut VideoDecoderState,
    at_ms: u64,
) -> Result<Arc<RgbaFrame>, MediaError> {
    let _ = m;

    // A request behind the picture on screen is served by that picture when
    // the difference is negligible; a request further back reads the stream
    // from the requested time again.
    if let Some(current) = &state.current
        && at_ms < current.pts_ms
    {
        let gap = current.pts_ms - at_ms;
        let current = Arc::clone(current);
        if gap <= SEEK_TOLERANCE_MS || !state.seekable {
            return Ok(current);
        }
        if let Err(error) = seek_to(state, at_ms) {
            // The stream cannot be read from an earlier time: the picture on
            // screen is kept and the refusal is reported.
            log::warn!("{error}");
            state.seekable = false;
            return Ok(current);
        }
    }

    // The picture after the last request may have become due.
    if let Some(pending) = state.pending.take() {
        if pending.pts_ms <= at_ms {
            state.current = Some(Arc::clone(&pending));
        } else {
            state.pending = Some(pending);
        }
    }

    // The stream is read forward until a picture after the requested time
    // turns up: the picture before it is the one the clock has reached.
    while state.pending.is_none() {
        let Some(frame) = decode_next(state)? else {
            break;
        };
        if frame.pts_ms <= at_ms {
            state.current = Some(frame);
        } else {
            state.pending = Some(frame);
        }
    }

    if let Some(current) = &state.current {
        return Ok(Arc::clone(current));
    }
    // The clock is before every picture of the stream: the first picture of
    // the stream is the one presented.
    match &state.pending {
        Some(first) => Ok(Arc::clone(first)),
        None => Err(MediaError::EndOfStream),
    }
}

/// Returns the decoded audio track, if the container has one.
///
/// The track is shared: the caller plays the same buffer the decoder
/// produced, so opening a movie does not copy the whole track.
pub(crate) fn audio(m: &Media, state: &VideoDecoderState) -> Result<Arc<DecodedAudio>, MediaError> {
    let _ = m;
    state.audio.clone().ok_or_else(|| MediaError::Unsupported {
        message: format!("{}: no audio track", state.name),
    })
}

/// Seeks the demuxer to `at_ms` and flushes the video decoder.
fn seek_to(state: &mut VideoDecoderState, at_ms: u64) -> Result<(), MediaError> {
    // `avformat_seek_file` with stream index -1 expects AV_TIME_BASE units
    // (microseconds).
    let timestamp = at_ms
        .checked_mul(1000)
        .and_then(|micros| i64::try_from(micros).ok())
        .ok_or_else(|| MediaError::InvalidData {
            message: format!("{}: seek target {at_ms} ms is out of range", state.name),
        })?;
    state
        .input
        .seek(timestamp, ..)
        .map_err(|error| MediaError::Io {
            message: format!("{}: seek failed: {error}", state.name),
        })?;
    state.decoder.flush();
    state.current = None;
    state.pending = None;
    state.eof = false;
    state.picture_clock_ms = at_ms;
    Ok(())
}

/// Decodes the next video frame, feeding packets until the decoder produces
/// one. Returns `None` at end of stream.
fn decode_next(state: &mut VideoDecoderState) -> Result<Option<Arc<RgbaFrame>>, MediaError> {
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
                state
                    .decoder
                    .send_eof()
                    .map_err(|error| decode_error(&state.name, error))?;
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
) -> Result<Arc<RgbaFrame>, MediaError> {
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

    let pts_ms = picture_timestamp_ms(
        &mut state.picture_clock_ms,
        state.time_base,
        state.spec.fps,
        frame,
        &state.name,
    )?;
    Ok(Arc::new(RgbaFrame::new(width, height, pts_ms, rgba)))
}

/// The picture's timestamp in milliseconds, and the clock it leaves behind.
///
/// A picture that carries a timestamp keeps it and moves the clock to it; a
/// picture without one takes the clock's value. The clock then moves on by
/// the picture's duration, so a stream that carries no timestamp at all is
/// stamped picture by picture.
fn picture_timestamp_ms(
    clock_ms: &mut u64,
    time_base: Rational,
    fps: f32,
    frame: &VideoFrame,
    name: &str,
) -> Result<u64, MediaError> {
    let duration_ms = picture_duration_ms(time_base, fps, frame, name)?;
    let pts_ms = match frame_timestamp_ms(time_base, frame, name)? {
        Some(pts_ms) => {
            *clock_ms = pts_ms;
            pts_ms
        }
        None => *clock_ms,
    };
    *clock_ms = pts_ms.saturating_add(duration_ms);
    Ok(pts_ms)
}

/// Converts a frame timestamp in stream time-base units to milliseconds.
///
/// A frame without a usable timestamp has none: the picture clock stamps it.
fn frame_timestamp_ms(
    time_base: Rational,
    frame: &VideoFrame,
    name: &str,
) -> Result<Option<u64>, MediaError> {
    let Some(timestamp) = frame.timestamp().or_else(|| frame.pts()) else {
        return Ok(None);
    };
    let numerator = i128::from(time_base.numerator());
    let denominator = i128::from(time_base.denominator());
    if timestamp < 0 || numerator <= 0 || denominator <= 0 {
        return Ok(None);
    }
    let ms = i128::from(timestamp) * numerator * 1000 / denominator;
    u64::try_from(ms)
        .map(Some)
        .map_err(|_| MediaError::InvalidData {
            message: format!("{name}: frame timestamp {ms} ms is out of range"),
        })
}

/// The picture's length in milliseconds: the frame's own length, or the
/// nominal length of a picture of the stream.
fn picture_duration_ms(
    time_base: Rational,
    fps: f32,
    frame: &VideoFrame,
    name: &str,
) -> Result<u64, MediaError> {
    let duration = frame.packet().duration;
    if duration > 0 {
        let numerator = i128::from(time_base.numerator());
        let denominator = i128::from(time_base.denominator());
        if numerator > 0 && denominator > 0 {
            let ms = i128::from(duration) * numerator * 1000 / denominator;
            let ms = u64::try_from(ms).map_err(|_| MediaError::InvalidData {
                message: format!("{name}: picture length {ms} ms is out of range"),
            })?;
            if ms > 0 {
                return Ok(ms);
            }
        }
    }
    Ok(nominal_frame_ms(fps))
}

/// The nominal length of a picture of the stream, in milliseconds.
///
/// The stream's own frame rate gives it; a stream without one falls back to
/// the twenty-five pictures per second the movie player assumes.
fn nominal_frame_ms(fps: f32) -> u64 {
    if fps >= 1.0 {
        let rate = f64::from(fps).round();
        if (1.0..=1000.0).contains(&rate) {
            let ms = (1000.0 / rate).round();
            if (1.0..=1000.0).contains(&ms) {
                return ms as u64;
            }
        }
    }
    DEFAULT_FRAME_MS
}

#[cfg(test)]
mod tests {
    use ffmpeg_next::ChannelLayout;

    use super::{Rational, VideoFrame, frame_timestamp_ms, picture_timestamp_ms};
    use crate::audio_ffmpeg::{channel_count, resolve_layout};

    /// A non-negative channel count is stored as reported; the reference keeps
    /// it in a 32-bit `tTVPWaveFormat::Channels`
    /// (`reference/cpp/core/sound/WaveIntf.h:47`).
    #[test]
    fn channel_count_accepts_a_non_negative_count() {
        assert_eq!(channel_count("<video>", 2).ok(), Some(2));
        assert_eq!(
            channel_count("<video>", i32::MAX).ok(),
            Some(i32::MAX as u32)
        );
    }

    /// A negative decoder count cannot be represented in the reference's
    /// unsigned 32-bit field (`reference/cpp/core/sound/WaveIntf.h:47`), so it
    /// is reported rather than clamped.
    #[test]
    fn channel_count_rejects_a_negative_count() {
        assert!(channel_count("<video>", -1).is_err());
    }

    /// A frame that carries no channel layout is converted with the decoder's
    /// layout, so a stereo track stays stereo instead of being played as one
    /// channel.
    ///
    /// The decoder's channel count is derived from the very layout that is
    /// missing, so it cannot stand in for the layout; the decoder's own
    /// layout is the stream's, and the reference keeps the file's channel
    /// count (`reference/cpp/core/movie/ffmpeg/AEFactory.cpp:136-138`).
    #[test]
    fn an_empty_frame_layout_falls_back_to_the_decoder_layout() {
        let layout = resolve_layout(
            ChannelLayout::default(0),
            ChannelLayout::default(2),
            "<video>",
        )
        .expect("layout");
        assert_eq!(layout.channels(), 2, "a stereo track must stay stereo");
    }

    /// A frame that carries its own layout is converted with it.
    #[test]
    fn a_frame_layout_wins_over_the_decoder_layout() {
        let layout = resolve_layout(
            ChannelLayout::default(1),
            ChannelLayout::default(2),
            "<video>",
        )
        .expect("layout");
        assert_eq!(layout.channels(), 1);
    }

    /// A track whose frame and decoder both carry no layout has an unknown
    /// channel count; it is reported instead of being played as one channel.
    #[test]
    fn a_track_with_no_layout_at_all_is_reported() {
        assert!(
            resolve_layout(
                ChannelLayout::default(0),
                ChannelLayout::default(0),
                "<video>"
            )
            .is_err()
        );
    }

    /// A frame PTS is rescaled from the stream time base to milliseconds.
    #[test]
    fn a_frame_timestamp_is_rescaled_to_milliseconds() {
        let mut frame = VideoFrame::empty();
        frame.set_pts(Some(15));
        assert_eq!(
            frame_timestamp_ms(Rational::new(1, 30), &frame, "<video>"),
            Ok(Some(500))
        );
    }

    /// A frame with no usable PTS carries no timestamp: the picture clock
    /// stamps it.
    ///
    /// The reference substitutes the clock's value for a picture without a
    /// timestamp
    /// (`reference/cpp/core/movie/ffmpeg/VideoPlayerVideo.cpp:539-547`).
    #[test]
    fn a_frame_without_a_timestamp_carries_none() {
        let frame = VideoFrame::empty();
        assert_eq!(
            frame_timestamp_ms(Rational::new(1, 30), &frame, "<video>"),
            Ok(None)
        );
    }

    /// A picture without a timestamp takes the clock's value, and the clock
    /// moves on by the picture's length.
    ///
    /// The reference guesses the next picture's timestamp from the picture's
    /// own length
    /// (`reference/cpp/core/movie/ffmpeg/VideoPlayerVideo.cpp:600-601`).
    #[test]
    fn a_picture_without_a_timestamp_takes_the_clock() {
        let mut clock = 0u64;
        let frame = VideoFrame::empty();
        let pts = picture_timestamp_ms(&mut clock, Rational::new(1, 30), 10.0, &frame, "<video>")
            .expect("timestamp");
        assert_eq!(pts, 0, "the first picture takes the clock's value");
        assert_eq!(
            clock, 100,
            "the clock moves on by the nominal picture length"
        );
    }

    /// A picture with a timestamp keeps it, and the clock moves on from it.
    #[test]
    fn a_picture_with_a_timestamp_keeps_it() {
        let mut clock = 0u64;
        let mut frame = VideoFrame::empty();
        frame.set_pts(Some(30));
        let pts = picture_timestamp_ms(&mut clock, Rational::new(1, 30), 10.0, &frame, "<video>")
            .expect("timestamp");
        assert_eq!(pts, 1000, "the picture keeps its own timestamp");
        assert_eq!(clock, 1100, "the clock moves on from the picture");
    }
}
