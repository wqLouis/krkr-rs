//! Video decode: the stateless `present_video`/`video_audio` surface.
//!
//! This module defines the final `Media` video API — the pure
//! `present_video`/`video_audio` actions and the bottom-defined
//! [`VideoDecoderState`] value owned by the middle layer. Two compile-time
//! backends implement it:
//!
//! * [`video-ffmpeg`](https://ffmpeg.org) (`video-ffmpeg`, the default on
//!   Linux/desktop) — the maintained `ffmpeg-next` binding linked
//!   dynamically through pkg-config. It demuxes from an in-memory
//!   `StreamIo`, converts frames to tightly packed RGBA8 with `swscale`, and
//!   decodes the audio track to interleaved `f32` with `swresample`.
//! * MediaCodec (`video-mediacodec`, selected by the Android frontend) — the
//!   NDK `AMediaExtractor`/`AMediaCodec` backend over raw `extern "C"` FFI
//!   (`libmediandk`). It links no FFmpeg and needs no JNI.
//!
//! Exactly one backend must be selected; selecting none or both is a
//! `compile_error!`. Consumers choose explicitly, so an Android build never
//! links `libav*`:
//!
//! ```toml
//! # Android frontend
//! media = { path = "../platform/media", default-features = false, features = ["video-mediacodec"] }
//! ```
//!
//! # Stateless object, explicit state
//!
//! [`Media`] stores nothing here. All demuxer/codec/frame/audio state lives
//! in the [`VideoDecoderState`] value created by [`open_video`] and owned by
//! the caller, who passes it back on every call. There are no globals, no
//! lazy caches and no backend registry; dropping the state releases every
//! underlying decoder/format context (RAII).
//!
//! # The backend seam
//!
//! The backend module is private. [`open_video`], [`present_video`] and
//! [`video_audio`] are the only path into it, and they forward to the
//! backend's `open`/`present`/`audio`; those three stay inside the crate.
//! Only the state type and, on the MediaCodec build, the standalone audio
//! path are re-exported, because the crate root publishes them.
//!
//! # The picture the clock has reached
//!
//! [`present_video`] returns the picture whose timestamp the clock has
//! reached: the latest picture at or before the requested time. The stream is
//! read forward until a picture after the requested time turns up, and the
//! picture before it is the one presented.
//!
//! A request behind the picture on screen is served from that picture when
//! the difference is smaller than `SEEK_TOLERANCE_MS`; a request further
//! behind it reads the stream from the requested time again. A stream that
//! cannot be read from an earlier time — a raw elementary stream has no
//! index to seek in — keeps the picture on screen and is read forward only.

use std::sync::Arc;

use api::error::MediaError;
use api::media::{DecodedAudio, RgbaFrame, VideoSpec};

use crate::Media;

#[cfg(all(feature = "video-ffmpeg", feature = "video-mediacodec"))]
compile_error!(
    "media: `video-ffmpeg` and `video-mediacodec` are mutually exclusive; select exactly one"
);

#[cfg(not(any(feature = "video-ffmpeg", feature = "video-mediacodec")))]
compile_error!(
    "media: no video backend selected; enable exactly one of `video-ffmpeg` (Linux/desktop) \
     or `video-mediacodec` (Android), e.g. \
     `media = {{ default-features = false, features = [\"video-mediacodec\"] }}`"
);

#[cfg(feature = "video-ffmpeg")]
#[path = "ffmpeg.rs"]
mod backend;

#[cfg(feature = "video-mediacodec")]
#[path = "mediacodec.rs"]
mod backend;

pub use backend::VideoDecoderState;

/// Decodes a whole audio track with the platform's audio decoder.
///
/// The MediaCodec backend decodes an MP4/AAC track (including an HE-AAC
/// track, whose SBR band the platform decoder reconstructs) with the
/// platform's `audio/mp4a-latm` decoder. The FFmpeg backend has no
/// counterpart: its track decoding belongs to the video backend.
///
/// # Errors
///
/// [`MediaError::Unsupported`] when the bytes hold no track the platform
/// decoder accepts, and [`MediaError::Decode`] when the track is malformed.
#[cfg(feature = "video-mediacodec")]
pub use backend::decode_audio as decode_audio_mediacodec;

/// How far a backwards request may lag the picture on screen before the
/// decoder reads the stream from the requested time instead of reusing it.
#[cfg(any(target_os = "android", feature = "video-ffmpeg"))]
pub(crate) const SEEK_TOLERANCE_MS: u64 = 500;

/// Video container/codec names advertised by [`crate::capabilities`].
///
/// The FFmpeg backend reports the demuxers/decoders it is built with. The
/// MediaCodec backend reports the baseline set the Android media framework
/// demuxes/decodes; the exact codecs available are device-dependent, and an
/// unavailable one surfaces as a typed [`MediaError::Unsupported`] from
/// [`open_video`] rather than a silent fallback.
#[cfg(feature = "video-ffmpeg")]
pub(crate) const VIDEO_FORMATS: &[&str] = &[
    "mp4", "mov", "matroska", "webm", "avi", "mpegts", "h264", "hevc", "vp8", "vp9", "av1",
    "mpeg4", "aac",
];

/// See [`VIDEO_FORMATS`].
#[cfg(feature = "video-mediacodec")]
pub(crate) const VIDEO_FORMATS: &[&str] = &[
    "mp4", "mov", "matroska", "webm", "mpegts", "3gp", "h264", "hevc", "vp8", "vp9", "av1",
    "mpeg4", "aac", "mp3", "opus", "vorbis", "flac",
];

/// Whether incremental video decode is available in this build.
#[cfg(feature = "video-ffmpeg")]
pub(crate) const STREAMING_VIDEO: bool = true;

/// See [`STREAMING_VIDEO`].
#[cfg(feature = "video-mediacodec")]
pub(crate) const STREAMING_VIDEO: bool = true;

/// Opens a video stream and reads its [spec](VideoSpec).
///
/// On success returns the stream's [`VideoSpec`] — including its audio track
/// parameters (`sample_rate`/`channels`, both `0` when there is none) — plus
/// a fresh [`VideoDecoderState`] for [`present_video`]/[`video_audio`].
/// `bytes` is shared by `Arc` so the caller can keep the compressed buffer
/// alive; the state retains it for seeking.
///
/// # Errors
///
/// [`MediaError::Unsupported`] when the build has no decoder for the
/// container, [`MediaError::Decode`] when the container is malformed, and
/// [`MediaError::NotFound`] when the stream holds no video track.
pub fn open_video(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(VideoSpec, VideoDecoderState), MediaError> {
    backend::open(m, bytes, hint)
}

/// Presents the picture whose timestamp the clock has reached at `at_ms`.
///
/// The picture is the latest one at or before the requested time. A request
/// within `SEEK_TOLERANCE_MS` behind the picture on screen is served from
/// that picture; a request further back reads the stream from the requested
/// time again, and a stream that cannot be read from an earlier time keeps
/// the picture on screen. Any other request reads the stream forward.
/// Returns a tightly packed RGBA8 [`RgbaFrame`], shared with the decoder so
/// that handing the frame on copies no pixels.
///
/// # Errors
///
/// [`MediaError::Decode`] when the frame cannot be decoded,
/// [`MediaError::InvalidState`] when the state does not belong to this
/// backend, and [`MediaError::Unsupported`] when the build has no decoder for
/// the stream.
pub fn present_video(
    m: &Media,
    state: &mut VideoDecoderState,
    at_ms: u64,
) -> Result<Arc<RgbaFrame>, MediaError> {
    backend::present(m, state, at_ms)
}

/// Returns the video's decoded audio track as interleaved `f32` PCM.
///
/// The audio track is decoded when [`open_video`] opens the stream; this call
/// hands out the shared buffer, so a caller that plays it does not copy the
/// whole track. When the stream has no audio track it returns a typed
/// [`MediaError::Unsupported`].
///
/// # Errors
///
/// [`MediaError::Unsupported`] when the stream carries no audio track, and
/// [`MediaError::Decode`] when the track cannot be decoded.
pub fn video_audio(m: &Media, state: &VideoDecoderState) -> Result<Arc<DecodedAudio>, MediaError> {
    backend::audio(m, state)
}
