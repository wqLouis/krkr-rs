//! Platform-independent YUV → RGBA conversion and timestamp rescaling for the
//! Android MediaCodec movie backend.
//!
//! This module is compiled on **every** target, not just Android, on purpose:
//! the MediaCodec decoder cannot run on a development machine, so the only
//! way its pixel and timestamp math gets real coverage is to keep it free of
//! Android types and unit-test it on the host. The Android-only layer parses
//! the codec's output format and feeds these functions.
//!
//! # What the tests here cover
//!
//! * a solid-colour frame converts to the expected uniform RGBA,
//! * odd widths (chroma rounding),
//! * a row stride larger than the visible width,
//! * a crop rectangle smaller than the coded frame,
//! * both `COLOR_FormatYUV420Flexible` layouts (planar/I420 and NV12),
//! * microsecond → millisecond rescaling.
//!
//! The conversion is BT.601 limited-range (video range), which is what H.264
//! decoders emit: `Y=16` is black, `Y=235` is white, chroma is centred on
//! `128`.

/// Chroma layout of a `COLOR_FormatYUV420Flexible` output buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvSampling {
    /// I420: a Y plane, then a full U plane, then a full V plane.
    Planar,
    /// NV12: a Y plane, then one interleaved U/V plane (`u, v, u, v, ...`).
    SemiPlanar,
}

/// An exclusive rectangle inside the coded frame, in pixels.
///
/// `right`/`bottom` are exclusive (width is `right - left`), matching the
/// NDK's own `AImageCropRect` convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropRect {
    /// Left edge in pixels.
    pub left: usize,
    /// Top edge in pixels.
    pub top: usize,
    /// Right edge in pixels (exclusive).
    pub right: usize,
    /// Bottom edge in pixels (exclusive).
    pub bottom: usize,
}

impl CropRect {
    /// A rectangle covering the whole `width × height` frame.
    pub fn full(width: usize, height: usize) -> Self {
        Self {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }

    /// Visible width in pixels.
    pub fn width(self) -> usize {
        self.right.saturating_sub(self.left)
    }

    /// Visible height in pixels.
    pub fn height(self) -> usize {
        self.bottom.saturating_sub(self.top)
    }
}

/// Plane geometry of one MediaCodec YUV420 byte buffer.
///
/// MediaCodec pads each plane: `y_stride` is the number of bytes per Y row
/// (which can exceed the visible width) and `y_rows` is the allocated Y plane
/// height (`slice-height`, which can exceed the visible height). The chroma
/// plane geometry is derived from those, because MediaCodec derives it the
/// same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Yuv420Layout {
    /// Bytes per row of the Y plane.
    pub y_stride: usize,
    /// Number of rows in the Y plane (`slice-height`).
    pub y_rows: usize,
    /// Bytes per chroma row (half the Y stride for planar, the Y stride for
    /// interleaved NV12).
    pub uv_stride: usize,
    /// Number of rows in each chroma plane.
    pub uv_rows: usize,
    /// Chroma layout.
    pub sampling: YuvSampling,
    /// Visible region inside the coded frame.
    pub crop: CropRect,
}

impl Yuv420Layout {
    /// Build a layout from the values reported by
    /// `AMediaCodec_getOutputFormat`, deriving the chroma geometry.
    pub fn new(y_stride: usize, y_rows: usize, sampling: YuvSampling, crop: CropRect) -> Self {
        let y_stride = y_stride.max(1);
        let y_rows = y_rows.max(1);
        let (uv_stride, uv_rows) = match sampling {
            YuvSampling::Planar => (y_stride.div_ceil(2), y_rows.div_ceil(2)),
            YuvSampling::SemiPlanar => (y_stride, y_rows.div_ceil(2)),
        };
        Self {
            y_stride,
            y_rows,
            uv_stride: uv_stride.max(1),
            uv_rows: uv_rows.max(1),
            sampling,
            crop,
        }
    }

    /// Visible width in pixels.
    pub fn width(&self) -> usize {
        self.crop.width()
    }

    /// Visible height in pixels.
    pub fn height(&self) -> usize {
        self.crop.height()
    }
}

/// Convert a tightly-typed view of a YUV420 buffer to tightly packed RGBA8.
///
/// The returned vector always has exactly `width * height * 4` bytes (the
/// visible crop size). Reads outside `yuv` yield zero rather than panicking,
/// so a short or malformed buffer cannot abort the process.
pub fn yuv420_to_rgba(yuv: &[u8], layout: &Yuv420Layout) -> Vec<u8> {
    let width = layout.width();
    let height = layout.height();
    let mut rgba = vec![0u8; width.saturating_mul(height).saturating_mul(4)];

    let y_size = layout.y_stride.saturating_mul(layout.y_rows);
    let u_base = y_size;
    let v_base = y_size.saturating_add(layout.uv_stride.saturating_mul(layout.uv_rows));

    for row in 0..height {
        let sy = layout.crop.top + row;
        let y_row = sy.saturating_mul(layout.y_stride);
        let cy = sy / 2;
        for col in 0..width {
            let sx = layout.crop.left + col;
            let luma = get(yuv, y_row + sx);
            let cx = sx / 2;
            let (u, v) = match layout.sampling {
                YuvSampling::Planar => (
                    get(yuv, u_base + cy * layout.uv_stride + cx),
                    get(yuv, v_base + cy * layout.uv_stride + cx),
                ),
                YuvSampling::SemiPlanar => {
                    let index = u_base + cy * layout.uv_stride + cx * 2;
                    (get(yuv, index), get(yuv, index + 1))
                }
            };
            let (r, g, b) = yuv_to_rgb(luma, u, v);
            let out = (row * width + col) * 4;
            rgba[out] = r;
            rgba[out + 1] = g;
            rgba[out + 2] = b;
            rgba[out + 3] = 255;
        }
    }
    rgba
}

/// Rescale a MediaCodec presentation timestamp (microseconds) to milliseconds.
///
/// MediaCodec reports sample times in microseconds, so this is the exact
/// equivalent of the FFmpeg backend's time-base rescale for an `AV_TIME_BASE`
/// time base. Truncating (rather than rounding) matches FFmpeg's integer
/// rescale.
pub fn micros_to_millis(micros: i64) -> i64 {
    micros / 1000
}

/// Bounds-checked byte read: out-of-range reads become zero.
fn get(buffer: &[u8], index: usize) -> u8 {
    buffer.get(index).copied().unwrap_or(0)
}

/// BT.601 limited-range YUV → RGB.
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    let c = i32::from(y) - 16;
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let r = (298 * c + 409 * e + 128) >> 8;
    let g = (298 * c - 100 * d - 208 * e + 128) >> 8;
    let b = (298 * c + 516 * d + 128) >> 8;
    (clamp(r), clamp(g), clamp(b))
}

fn clamp(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}
