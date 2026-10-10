//! Integration tests for [`api::glyph`].

use api::glyph::*;

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
        ..FontMetrics::default()
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
