//! Media decode DTOs.
//!
//! These values cross the boundary between the middle layer and the bottom
//! `Media` engine. They carry only decoded/descriptive data; the decoder
//! context itself is a separate, bottom-defined state value owned by the
//! middle layer.

/// Description of an encoded image without decoding it in full.
///
/// Returned by an image probe (used by `loadHeader`-style calls).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageInfo {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Whether the source has an alpha channel.
    pub has_alpha: bool,
    /// Detected format name (e.g. `"png"`, `"tlg5"`), when known.
    pub format: Option<String>,
}

/// A fully decoded image as tightly packed, row-major RGBA8.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodedImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes of RGBA8, row-major.
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    /// Creates a decoded image from its dimensions and pixel buffer.
    ///
    /// The caller is responsible for `rgba.len() == width * height * 4`;
    /// use [`DecodedImage::is_consistent`] to check it.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            rgba,
        }
    }

    /// Returns the number of pixels.
    pub const fn pixel_count(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }

    /// Returns `true` if `rgba` holds exactly `width * height * 4` bytes.
    pub fn is_consistent(&self) -> bool {
        self.rgba.len() as u64 == self.pixel_count() * 4
    }
}

/// Format of a decoded audio stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AudioSpec {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u16,
    /// Total number of frames, when known (e.g. for a whole file).
    pub total_frames: Option<u64>,
}

impl AudioSpec {
    /// Returns the duration in milliseconds, when the total frame count and
    /// sample rate are known and non-zero.
    pub fn duration_ms(&self) -> Option<u64> {
        if self.sample_rate == 0 {
            return None;
        }
        let frames = self.total_frames?;
        Some(frames.saturating_mul(1000) / u64::from(self.sample_rate))
    }
}

/// A fully decoded audio buffer as interleaved `f32` samples.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodedAudio {
    /// Format of the decoded samples.
    pub spec: AudioSpec,
    /// Interleaved samples; length is `frames * channels`.
    pub interleaved: Vec<f32>,
}

impl DecodedAudio {
    /// Creates a decoded buffer from its spec and samples.
    pub fn new(spec: AudioSpec, interleaved: Vec<f32>) -> Self {
        Self { spec, interleaved }
    }

    /// Returns the number of complete frames in the buffer.
    ///
    /// Returns `0` when the channel count is zero.
    pub fn frame_count(&self) -> u64 {
        if self.spec.channels == 0 {
            return 0;
        }
        self.interleaved.len() as u64 / u64::from(self.spec.channels)
    }
}

/// A single decoded video frame as tightly packed, row-major RGBA8.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RgbaFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Presentation timestamp in milliseconds.
    pub pts_ms: u64,
    /// `width * height * 4` bytes of RGBA8, row-major.
    pub rgba: Vec<u8>,
}

impl RgbaFrame {
    /// Creates a video frame from its dimensions, timestamp and pixels.
    pub fn new(width: u32, height: u32, pts_ms: u64, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pts_ms,
            rgba,
        }
    }

    /// Returns the number of pixels.
    pub const fn pixel_count(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }

    /// Returns `true` if `rgba` holds exactly `width * height * 4` bytes.
    pub fn is_consistent(&self) -> bool {
        self.rgba.len() as u64 == self.pixel_count() * 4
    }
}

/// Format of a decoded video stream, including its audio track.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VideoSpec {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Nominal frames per second.
    pub fps: f32,
    /// Total number of frames, when known.
    pub total_frames: Option<u64>,
    /// Audio sample rate in Hz (0 when there is no audio track).
    pub sample_rate: u32,
    /// Audio channel count (0 when there is no audio track).
    pub channels: u16,
    /// Number of audio streams in the container.
    pub audio_streams: u32,
    /// Number of video streams in the container.
    pub video_streams: u32,
    /// Index of the selected audio stream among the container's audio streams,
    /// or `-1` when none is selected.
    pub audio_stream: i32,
    /// Index of the selected video stream among the container's video streams,
    /// or `-1` when none is selected.
    pub video_stream: i32,
}

impl VideoSpec {
    /// Returns `true` if the stream carries an audio track.
    pub const fn has_audio(&self) -> bool {
        self.channels > 0 && self.sample_rate > 0
    }
}

/// What the active media backend can decode.
///
/// A stateless capability report so the middle layer can choose a path
/// without compiling against backend-specific `#[cfg]` branches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaCapabilities {
    /// Supported image format names.
    pub image_formats: Vec<String>,
    /// Supported audio format/codec names.
    pub audio_formats: Vec<String>,
    /// Supported video format/codec names.
    pub video_formats: Vec<String>,
    /// Whether streaming (incremental) audio decode is available.
    pub streaming_audio: bool,
    /// Whether streaming (incremental) video decode is available.
    pub streaming_video: bool,
}

impl MediaCapabilities {
    /// Returns a capability report with no supported formats.
    pub fn none() -> Self {
        Self::default()
    }

    /// Returns `true` when nothing at all can be decoded.
    pub fn is_empty(&self) -> bool {
        self.image_formats.is_empty()
            && self.audio_formats.is_empty()
            && self.video_formats.is_empty()
    }
}
