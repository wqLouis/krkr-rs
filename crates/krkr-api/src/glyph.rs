//! Glyph and font DTOs.
//!
//! Glyph rasterization itself lives in the bottom `FontSystem`; these are
//! the plain values it returns and the style/format descriptions it accepts.
//! The middle layer owns the atlas and layout built from them.

/// Requested glyph style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GlyphStyle {
    /// Request a bold face/synthesis.
    pub bold: bool,
    /// Request an italic/oblique face/synthesis.
    pub italic: bool,
}

/// A single rasterized glyph as 8-bit coverage.
///
/// `coverage` is `width * height` bytes, row-major, where `0` is fully
/// transparent and `255` fully covered. `bearing_x`/`bearing_y` position the
/// bitmap relative to the pen, and `advance` is the pen movement after the
/// glyph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlyphBitmap {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// `width * height` coverage bytes, row-major.
    pub coverage: Vec<u8>,
    /// Horizontal bearing from the pen origin to the bitmap's left edge.
    pub bearing_x: f32,
    /// Vertical bearing from the baseline to the bitmap's top edge.
    pub bearing_y: f32,
    /// Horizontal pen advance.
    pub advance: f32,
}

impl GlyphBitmap {
    /// Creates a rasterized glyph from its metrics and coverage.
    pub fn new(
        width: u32,
        height: u32,
        coverage: Vec<u8>,
        bearing_x: f32,
        bearing_y: f32,
        advance: f32,
    ) -> Self {
        Self {
            width,
            height,
            coverage,
            bearing_x,
            bearing_y,
            advance,
        }
    }

    /// Returns the number of coverage bytes expected for this bitmap.
    pub const fn pixel_count(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }

    /// Returns `true` if the glyph has no positive extent.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Returns `true` if `coverage` holds exactly `width * height` bytes.
    pub fn is_consistent(&self) -> bool {
        self.coverage.len() as u64 == self.pixel_count()
    }
}

/// Scaled metrics for a font at a given pixel size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FontMetrics {
    /// Distance above the baseline, in pixels.
    pub ascent: f32,
    /// Distance below the baseline, in pixels (typically positive).
    pub descent: f32,
    /// Recommended extra line spacing, in pixels.
    pub line_gap: f32,
    /// Font design units per em.
    pub units_per_em: f32,
}

impl FontMetrics {
    /// Returns the recommended baseline-to-baseline line height.
    pub fn line_height(&self) -> f32 {
        self.ascent + self.descent + self.line_gap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_bitmap_consistency() {
        let glyph = GlyphBitmap::new(3, 2, vec![0; 6], 0.5, 1.5, 4.0);
        assert_eq!(glyph.pixel_count(), 6);
        assert!(glyph.is_consistent());
        assert!(!glyph.is_empty());
        assert!(!GlyphBitmap::new(3, 2, vec![0; 5], 0.0, 0.0, 0.0).is_consistent());
        assert!(GlyphBitmap::default().is_empty());
    }

    #[test]
    fn font_metrics_line_height() {
        let metrics = FontMetrics {
            ascent: 12.0,
            descent: 4.0,
            line_gap: 2.0,
            units_per_em: 1000.0,
        };
        assert_eq!(metrics.line_height(), 18.0);
    }

    #[test]
    fn style_default_is_regular() {
        assert_eq!(
            GlyphStyle::default(),
            GlyphStyle {
                bold: false,
                italic: false
            }
        );
    }
}
