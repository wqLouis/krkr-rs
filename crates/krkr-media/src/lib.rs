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
//! * **YUV** — the pure `yuv420_to_rgba` conversion ported from
//!   `tvp-natives/src/video/yuv.rs`.
//!
//! # What is scaffolded
//!
//! * **Video** — [`open_video`], [`present_video`] and [`video_audio`]
//!   return [`MediaError::Unsupported`].
//!
//! The signatures and state types are final; a later task fills in the
//! FFmpeg/MediaCodec video backends without changing the API.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod audio;
pub mod image_decode;
pub mod tlg;
pub mod video;
pub mod yuv;

pub use audio::{AudioDecoderState, decode_audio, open_audio, probe_audio, read_audio, seek_audio};
pub use image_decode::{decode_image, probe_image};
pub use video::{VideoDecoderState, open_video, present_video, video_audio};
pub use yuv::{CropRect, Yuv420Layout, YuvSampling, micros_to_millis, yuv420_to_rgba};

use krkr_api::error::MediaError;
use krkr_api::media::MediaCapabilities;

/// Immutable configuration for a [`Media`] object.
///
/// This is plain, copyable configuration — not evolving state. It is stored
/// by value in [`Media`] and never mutated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaConfig {
    /// Reserved: when `true`, image probing/decoding may accept images whose
    /// declared dimensions exceed the built-in safety cap. The current
    /// decoders ignore this flag and always enforce the cap.
    pub allow_large_images: bool,
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
/// "no decoder in this build" failure.
pub(crate) fn unsupported(feature: &str) -> MediaError {
    MediaError::Unsupported {
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
        video_formats: Vec::new(),
        streaming_audio: true,
        streaming_video: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_advertise_the_image_and_audio_backends() {
        let media = Media::new();
        let caps = capabilities(&media);
        assert!(caps.image_formats.iter().any(|name| name == "png"));
        assert!(caps.image_formats.iter().any(|name| name == "tlg6"));
        assert!(caps.audio_formats.iter().any(|name| name == "wav"));
        assert!(caps.audio_formats.iter().any(|name| name == "opus"));
        assert!(caps.video_formats.is_empty());
        assert!(caps.streaming_audio);
        assert!(!caps.streaming_video);
        assert!(!caps.is_empty());
    }

    #[test]
    fn config_is_preserved_and_media_is_stateless() {
        let config = MediaConfig {
            allow_large_images: true,
        };
        let media = Media::with_config(config.clone());
        assert_eq!(media.config(), &config);
        // Two independent objects do not alias.
        let other = create_media(MediaConfig::default());
        assert_ne!(media, other);
        assert_eq!(media, media.clone());
    }

    #[test]
    fn video_scaffold_reports_unsupported() {
        let media = Media::new();
        assert!(matches!(
            open_video(&media, std::sync::Arc::from(&[][..]), None),
            Err(MediaError::Unsupported { .. })
        ));
    }
}
