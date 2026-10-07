//! Video decode scaffolds.
//!
//! This module defines the final `Media` video surface — the pure
//! `present_video`/`video_audio` actions and the bottom-defined
//! [`VideoDecoderState`] value owned by the middle layer — but **no codec is
//! wired up yet**. The FFmpeg (Linux) and MediaCodec (Android) backends
//! arrive in a later task. Every action returns [`MediaError::Unsupported`]
//! so the middle layer can be compiled and type-checked against the real
//! signatures today.
//!
//! Nothing here stores state: [`VideoDecoderState`] is created by
//! [`open_video`] and returned to the caller, who owns and passes it back.
//! `Media` itself remains immutable.

use std::sync::Arc;

use krkr_api::error::MediaError;
use krkr_api::media::{DecodedAudio, RgbaFrame, VideoSpec};

use crate::Media;

/// Bottom-defined state for an incremental video decode.
///
/// Holds the demuxer/codec context, the current frame and the audio track.
/// The middle layer owns this value and passes it back on every call; `Media`
/// never stores it. It is opaque on purpose so the later FFmpeg/MediaCodec
/// implementation can change its layout without an API break.
///
/// The scaffold is uninhabited from outside the crate (external code cannot
/// construct it because it is `#[non_exhaustive]` and has no fields).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoDecoderState {}

/// Opens a video stream and reads its [spec](VideoSpec).
///
/// On success returns the stream's [`VideoSpec`] plus a fresh
/// [`VideoDecoderState`] for [`present_video`]/[`video_audio`]. `bytes` is
/// shared by `Arc` so the caller can keep the compressed buffer alive.
///
/// Scaffold: always returns [`MediaError::Unsupported`].
pub fn open_video(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(VideoSpec, VideoDecoderState), MediaError> {
    let _ = (m, bytes, hint);
    Err(crate::unsupported("video decode"))
}

/// Presents the frame at (or after) `at_ms`.
///
/// The backward-seek contract is the FFmpeg backend's current behaviour:
/// seek when moving backwards past a tolerance, otherwise decode forward.
/// Returns a tightly packed RGBA8 [`RgbaFrame`].
///
/// Scaffold: always returns [`MediaError::Unsupported`].
pub fn present_video(
    m: &Media,
    state: &mut VideoDecoderState,
    at_ms: u64,
) -> Result<RgbaFrame, MediaError> {
    let _ = (m, state, at_ms);
    Err(crate::unsupported("video decode"))
}

/// Returns the video's decoded audio track as interleaved `f32` PCM.
///
/// Scaffold: always returns [`MediaError::Unsupported`].
pub fn video_audio(m: &Media, state: &VideoDecoderState) -> Result<DecodedAudio, MediaError> {
    let _ = (m, state);
    Err(crate::unsupported("video audio"))
}
