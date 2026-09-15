//! Stub movie decoder for builds without the `ffmpeg` feature.
//!
//! This submodule provides the exact same public API as the FFmpeg-backed
//! [`MovieDecoder`](super::ffmpeg::MovieDecoder), but every constructor and
//! method fails with a descriptive error. It exists so a target whose NDK
//! provides no FFmpeg (Android / MediaCodec) can still compile the crate and
//! so a game that tries to play a movie gets a real error to report instead
//! of silently receiving empty frames.

use super::{DecodedAudioPcm, MovieMetadata, RgbaFrame};

/// The error returned by every fallible operation in this configuration.
const UNAVAILABLE: &str = "video decoding is unavailable: this build has no MPEG decoder (built without the `ffmpeg` feature)";

/// Metadata reported when no movie can ever be opened.
static EMPTY_METADATA: MovieMetadata = MovieMetadata {
    width: 0,
    height: 0,
    fps: 0.0,
    total_frames: 0,
    total_time_ms: 0,
    video_streams: 0,
    audio_streams: 0,
    audio_sample_rate: 0,
    audio_channels: 0,
};

/// API-compatible stand-in for the FFmpeg-backed decoder.
///
/// [`MovieDecoder::open`] never succeeds, so no instance can be observed by
/// callers; the remaining methods exist only so the public API (and therefore
/// every caller) is byte-for-byte identical in both configurations.
pub struct MovieDecoder;

impl MovieDecoder {
    /// Always fails: this build has no movie decoder.
    pub fn open(_bytes: Vec<u8>) -> Result<Self, String> {
        Err(UNAVAILABLE.to_string())
    }

    /// Always reports empty metadata (no movie can be open).
    pub fn metadata(&self) -> &MovieMetadata {
        &EMPTY_METADATA
    }

    /// Always `None` (no movie can be open).
    pub fn current_frame(&self) -> Option<&RgbaFrame> {
        None
    }

    /// Always `None`: the audio track is never decoded in this build.
    pub fn cached_audio(&self) -> Option<&DecodedAudioPcm> {
        None
    }

    /// Always fails: this build has no movie decoder.
    pub fn audio_pcm(&mut self) -> Result<&DecodedAudioPcm, String> {
        Err(UNAVAILABLE.to_string())
    }

    /// Always fails: this build has no movie decoder.
    pub fn present_at(&mut self, _target_ms: i64) -> Result<Option<RgbaFrame>, String> {
        Err(UNAVAILABLE.to_string())
    }
}
