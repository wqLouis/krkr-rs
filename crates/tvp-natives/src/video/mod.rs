//! Movie decoding for [`VideoOverlay`](crate::video_overlay).
//!
//! # With the `ffmpeg` feature (default)
//!
//! [`MovieDecoder`] is backed by the system FFmpeg libraries (`libavformat` +
//! `libavcodec` + `libswscale` + `libswresample`). The game's movies are
//! ordinary **ISO-BMFF / MP4** files (despite their `.mpg` names): H.264
//! video + AAC audio. This opens the media from an in-memory byte buffer (the
//! XP3 entry read through the engine storage) with a custom `AVIOContext` —
//! no temp files — reports the real container metadata, decodes video frames
//! to tightly-packed RGBA8 on demand and decodes the whole audio track to
//! interleaved `f32` PCM.
//!
//! # Without the feature (e.g. Android)
//!
//! When the crate is built with `--no-default-features` (or a dependent
//! disables the feature) there is no FFmpeg to link. [`init`] is a no-op and
//! every [`MovieDecoder`] constructor/method returns a descriptive `Err`
//! instead of silently succeeding, so a game that tries to play a movie gets
//! a real error it can report. The public types ([`MovieMetadata`],
//! [`RgbaFrame`], [`DecodedAudioPcm`]) are identical in both configurations.
//!
//! # Shape (both configurations)
//!
//! * `MovieDecoder::open` demuxes the buffer, builds the video/audio
//!   decoders and reads [`MovieMetadata`] (`originalWidth`, `originalHeight`,
//!   `fps`, `totalFrame`, `totalTime`, stream counts and the audio sample
//!   format).
//! * `MovieDecoder::present_at` seeks (when the request moves backwards) and
//!   decodes forward until the frame covering the requested millisecond is
//!   available, returning it as an [`RgbaFrame`].
//! * `MovieDecoder::audio_pcm` returns the whole-track [`DecodedAudioPcm`]
//!   (decoded once), ready to hand to the engine sound mixer.
//!
//! Everything is confined to the VM thread: decoder contexts are not shared.

#[cfg(feature = "ffmpeg")]
mod ffmpeg;
#[cfg(not(feature = "ffmpeg"))]
mod unsupported;

#[cfg(feature = "ffmpeg")]
pub use ffmpeg::MovieDecoder;
#[cfg(not(feature = "ffmpeg"))]
pub use unsupported::MovieDecoder;

#[cfg(feature = "ffmpeg")]
use std::sync::Once;

/// Real container metadata exposed through the `VideoOverlay` properties.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieMetadata {
    /// Coded video width (not display aspect ratio).
    pub width: u32,
    /// Coded video height.
    pub height: u32,
    /// Frames per second (prefers the container's average frame rate).
    pub fps: f64,
    /// Total number of video frames (container value, else derived from the
    /// duration and `fps`).
    pub total_frames: i64,
    /// Video duration in milliseconds.
    pub total_time_ms: i64,
    /// Number of video streams in the container.
    pub video_streams: i64,
    /// Number of audio streams in the container.
    pub audio_streams: i64,
    /// Audio sample rate (0 when there is no audio).
    pub audio_sample_rate: u32,
    /// Audio channel count (0 when there is no audio).
    pub audio_channels: u16,
}

/// One decoded video frame, converted to tightly packed RGBA8.
#[derive(Debug, Clone, PartialEq)]
pub struct RgbaFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Presentation timestamp in milliseconds.
    pub pts_ms: i64,
    /// `width * height * 4` RGBA bytes, no row padding.
    pub data: Vec<u8>,
}

impl RgbaFrame {
    /// A 64-bit FNV-1a hash of the pixel bytes (diagnostics / tests).
    pub fn checksum(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in &self.data {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Whether the frame is entirely transparent black (i.e. no picture).
    pub fn is_blank(&self) -> bool {
        self.data.iter().all(|&byte| byte == 0)
    }
}

/// Whole-track decoded audio: interleaved `f32` samples in `[-1, 1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudioPcm {
    /// Samples per second.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Interleaved samples (`samples[frame * channels + channel]`).
    pub samples: Vec<f32>,
}

impl DecodedAudioPcm {
    /// Number of interleaved sample values.
    pub fn sample_values(&self) -> usize {
        self.samples.len()
    }

    /// Number of sample frames (one value per channel).
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    /// Duration in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate.max(1))
    }
}

#[cfg(feature = "ffmpeg")]
static FFMPEG_INIT: Once = Once::new();

/// Initialize the movie decoder once per process. Safe to call from any
/// thread.
///
/// With the `ffmpeg` feature this initializes the FFmpeg libraries and
/// reports their error; without it there is nothing to initialize.
#[cfg(feature = "ffmpeg")]
pub fn init() -> Result<(), String> {
    let mut result = Ok(());
    FFMPEG_INIT.call_once(|| {
        if let Err(e) = ffmpeg_next::init() {
            result = Err(format!("ffmpeg init: {e}"));
        }
    });
    result
}

/// Initialize the movie decoder once per process.
///
/// This build has no `ffmpeg` feature, so there is nothing to initialize and
/// this always returns `Ok(())`.
#[cfg(not(feature = "ffmpeg"))]
pub fn init() -> Result<(), String> {
    Ok(())
}
