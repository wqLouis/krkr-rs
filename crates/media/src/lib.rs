//! krkr-rs bottom media engine.
//!
//! Stateless [`Media`] object, pure image [`decode_image`]/[`probe_image`]
//! functions, and explicit [`AudioDecoderState`]/[`VideoDecoderState`] values
//! owned by the middle layer. Backends: FFmpeg (Linux) and MediaCodec
//! (Android) for audio/video, plus the platform-free TLG5/TLG6 image decoder.
//! See `docs/migration/05-media-engine.md` and
//! `docs/migration/03-middle-layer-api.md` §4.
//!
//! # D9: stateless objects, explicit state
//!
//! [`Media`] holds only immutable configuration and has no `&mut self`
//! methods. Every evolving value (the streaming decoder states) is a separate
//! value created by this crate and owned by the caller. There are no globals,
//! no lazy caches and no engine state; the decoder lookup tables are
//! compile-time `const`s.
//!
//! # What is implemented
//!
//! * **Images** — the native TLG5/TLG6 decoder (ported from
//!   `tvp-visual/src/tlg.rs`) and `image`-crate formats, with header probing.
//! * **Audio** — symphonia whole-file/streaming decode (wav, pcm, ogg,
//!   vorbis, flac, mp3) plus the Ogg Opus adapter, with header-only probing
//!   and an explicit caller-owned [`AudioDecoderState`].
//! * **Video** — the `present_video`/`video_audio` surface over an explicit
//!   [`VideoDecoderState`]. On Linux the FFmpeg backend (D13
//!   `video-ffmpeg`) demuxes in memory, converts frames to tightly packed
//!   RGBA8 and decodes the audio track; on Android the `video-mediacodec`
//!   feature selects a typed [`api::error::MediaError::Unsupported`] stub. Exactly one
//!   backend must be selected (see [`video`]).
//! * **YUV** — the pure `yuv420_to_rgba` conversion ported from
//!   `tvp-natives/src/video/yuv.rs`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod audio;
mod hint;
pub mod image_decode;
pub(crate) mod tlg;
pub mod video;
pub mod yuv;

pub use audio::{AudioDecoderState, decode_audio, open_audio, probe_audio, read_audio, seek_audio};
pub use image_decode::{decode_image, probe_image};
pub use video::{VideoDecoderState, open_video, present_video, video_audio};
pub use yuv::{CropRect, Yuv420Layout, YuvSampling, micros_to_millis, yuv420_to_rgba};

use api::media::MediaCapabilities;

/// Immutable configuration for a [`Media`] object.
///
/// The media engine currently exposes no configurable options. In
/// particular there is no size limit to configure: a decode that cannot be
/// performed (malformed, overflowing, or unsupported data) returns a typed
/// [`api::error::MediaError`] rather than a substituted value. This type stays
/// `#[non_exhaustive]` so options can be added without an API break.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaConfig {}

impl MediaConfig {
    /// Creates the default (currently empty) configuration.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Stateless media backend object.
///
/// Created once and shared by reference (see the D9 notes in the crate docs).
/// It owns only immutable [`MediaConfig`]; every decode action either is a
/// free function or takes the evolving decoder state as an explicit
/// parameter, so two independent `Media` values can coexist in one process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Media {
    config: MediaConfig,
}

impl Media {
    /// Creates a media backend with the default configuration.
    pub fn new() -> Self {
        Self {
            config: MediaConfig::default(),
        }
    }

    /// Creates a media backend with an explicit configuration.
    pub fn with_config(config: MediaConfig) -> Self {
        Self { config }
    }

    /// Returns the immutable configuration this object was created with.
    pub fn config(&self) -> &MediaConfig {
        &self.config
    }
}

/// Creates a stateless [`Media`] backend (the factory spelling used by the
/// middle-layer API).
pub fn create_media(config: MediaConfig) -> Media {
    Media::with_config(config)
}

/// Builds the typed "not implemented in this build" error used by the
/// audio/video scaffolds.
///
/// This is the value the middle layer surfaces to scripts as the friendly
/// "no decoder in this build" failure. Only the MediaCodec stub needs it
/// today, so it is compiled in only for that backend.
#[cfg(feature = "video-mediacodec")]
pub(crate) fn unsupported(feature: &str) -> api::error::MediaError {
    api::error::MediaError::Unsupported {
        message: format!("{feature} is not implemented in this build"),
    }
}

/// Reports which formats this backend can decode.
///
/// Stateless: the result depends only on how the crate was compiled, not on
/// per-object state. The middle layer uses it to choose a decode path without
/// backend-specific `#[cfg]` branches.
pub fn capabilities(m: &Media) -> MediaCapabilities {
    let _ = m;
    MediaCapabilities {
        image_formats: image_decode::IMAGE_FORMATS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        audio_formats: audio::AUDIO_FORMATS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        video_formats: video::VIDEO_FORMATS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        streaming_audio: true,
        streaming_video: video::STREAMING_VIDEO,
    }
}
