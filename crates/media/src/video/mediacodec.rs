//! Android MediaCodec video backend — typed `Unsupported` stub.
//!
//! The Android frontend selects this feature (`default-features = false,
//! features = ["video-mediacodec"]`) so the build never links FFmpeg. Until
//! the `AMediaExtractor` + `AMediaCodec` implementation lands, every entry
//! point returns a typed [`MediaError::Unsupported`]; nothing is silently
//! substituted.

use std::sync::Arc;

use api::error::MediaError;
use api::media::{DecodedAudio, RgbaFrame, VideoSpec};

use crate::Media;

/// Bottom-defined state for an incremental video decode.
///
/// The MediaCodec backend is not implemented in this build, so this value is
/// uninhabited from outside the crate and carries no state. The type exists so
/// the public API is identical to the FFmpeg build.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoDecoderState {}

/// Opens a video stream. Stub: always fails with [`MediaError::Unsupported`].
pub fn open(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(VideoSpec, VideoDecoderState), MediaError> {
    let _ = (m, bytes, hint);
    Err(crate::unsupported("video decode (mediacodec)"))
}

/// Presents a frame. Stub: always fails with [`MediaError::Unsupported`].
pub fn present(
    m: &Media,
    state: &mut VideoDecoderState,
    at_ms: u64,
) -> Result<RgbaFrame, MediaError> {
    let _ = (m, state, at_ms);
    Err(crate::unsupported("video decode (mediacodec)"))
}

/// Returns the decoded audio track. Stub: always fails with
/// [`MediaError::Unsupported`].
pub fn audio(m: &Media, state: &VideoDecoderState) -> Result<DecodedAudio, MediaError> {
    let _ = (m, state);
    Err(crate::unsupported("video audio (mediacodec)"))
}
