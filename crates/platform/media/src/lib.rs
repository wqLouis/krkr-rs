//! krkr-rs bottom media engine.
//!
//! Stateless [`Media`] object, pure image [`decode_image`]/[`probe_image`]/
//! [`encode_image`] functions, and explicit [`AudioDecoderState`]/
//! [`VideoDecoderState`] values owned by the middle layer. Backends: FFmpeg
//! (Linux) and MediaCodec (Android) for audio/video, plus the platform-free
//! TLG5/TLG6 image decoder.
//!
//! # Stateless objects, explicit state
//!
//! [`Media`] holds no state and has no `&mut self` methods. Every evolving
//! value (the streaming decoder states) is a separate value created by this
//! crate and owned by the caller. There are no globals, no lazy caches and no
//! engine state; the decoder lookup tables are compile-time `const`s.
//!
//! # What is implemented
//!
//! * **Images** — the native TLG5/TLG6 decoder and `image`-crate formats,
//!   with header probing and PNG/BMP/JPEG encoding.
//! * **Audio** — symphonia whole-file/streaming decode (wav, pcm, ogg,
//!   vorbis, flac, mp3, mp4/aac) plus the Ogg Opus adapter, with header-only
//!   probing and an explicit caller-owned [`AudioDecoderState`]. The desktop
//!   build links FFmpeg as well, and [`decode_audio_ffmpeg`] decodes a whole
//!   track with it — the desktop build's first choice, ahead of the built-in
//!   decoder, which has no SBR synthesis and therefore reports an AAC track
//!   with the SBR extension instead of decoding it at the rate the
//!   container declares. The Android build links no FFmpeg; there the
//!   built-in decoder comes first and the platform's AAC decoder
//!   (`decode_audio_mediacodec`) is the fallback.
//! * **Video** — the `present_video`/`video_audio` surface over an explicit
//!   [`VideoDecoderState`]. On Linux the FFmpeg backend (`video-ffmpeg`)
//!   demuxes in memory, converts frames to tightly packed
//!   RGBA8 and decodes the audio track; on Android the `video-mediacodec`
//!   feature selects the NDK backend (`ndk` + a raw `AMediaExtractor` shim).
//!   Exactly one backend must be selected (see the `video` module).
//! * **YUV** — the pure `yuv420_to_rgba` conversion in [`yuv`].
//!
//! # Safety lint
//!
//! Outside Android there is no raw FFI in this crate, so `unsafe_code` is set
//! to `forbid`. Android needs one raw `AMediaExtractor` shim (the `ndk` crate
//! does not expose the extractor); `forbid` cannot be locally relaxed, so the
//! crate uses `deny` there and the shim module carries the single scoped
//! `#[allow(unsafe_code)]`.

#![cfg_attr(not(target_os = "android"), forbid(unsafe_code))]
#![cfg_attr(target_os = "android", deny(unsafe_code))]
#![warn(missing_docs)]

pub mod audio;
#[cfg(feature = "video-ffmpeg")]
mod audio_ffmpeg;
mod hint;
pub mod image_decode;
pub mod image_encode;
pub(crate) mod tlg;
mod video;
pub mod yuv;

pub use audio::{AudioDecoderState, decode_audio, open_audio, probe_audio, read_audio, seek_audio};
#[cfg(feature = "video-ffmpeg")]
pub use audio_ffmpeg::decode_audio_ffmpeg;
pub use image_decode::{decode_image, probe_image};
pub use image_encode::encode_image;
#[cfg(feature = "video-mediacodec")]
pub use video::decode_audio_mediacodec;
pub use video::{VideoDecoderState, open_video, present_video, video_audio};

use api::media::MediaCapabilities;

/// Stateless media backend object.
///
/// The object carries no configuration and no state. Every decode action
/// either is a free function or takes the evolving decoder state as an
/// explicit parameter, so two independent [`Media`] values can coexist in one
/// process and the backend remembers nothing between calls.
///
/// There is nothing to configure. In particular there is no size limit: a
/// decode that cannot be performed (malformed, overflowing, or unsupported
/// data) returns a typed [`api::error::MediaError`] rather than a substituted
/// value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Media;

impl Media {
    /// Creates a media backend.
    ///
    /// The value is a facade over the codecs the build links; creating two of
    /// them does not create two decoders.
    pub const fn new() -> Self {
        Self
    }
}

/// Reports which formats this backend can decode.
///
/// Stateless: the result depends only on how the crate was compiled, not on
/// per-object state — `m` selects nothing and is accepted only so the call
/// reads like every other media action. The middle layer uses the result to
/// choose a decode path without backend-specific `#[cfg]` branches.
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
