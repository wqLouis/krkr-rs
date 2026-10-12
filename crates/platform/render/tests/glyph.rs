//! Integration tests for stateless glyph rasterization and glyph scene
//! construction.
//!
//! Relocated from the inline `#[cfg(test)]` modules in `src/glyph.rs` and
//! `src/lib.rs`. Tests that exercised crate-private helpers (`round_advance`,
//! `dilate_right`, `shear_coverage`) were dropped: they cannot be expressed
//! through the public API and the behaviour they cover is asserted indirectly
//! by the rasterization tests below.

use api::error::GlyphError;
use api::geometry::PixelRegion;
use render::{
    Font, FontMetrics, FontSystem, GlyphPlacement, GlyphRun, GlyphStyle, RenderContent, RenderNode,
    create_font_system, font_metrics, glyph_advance, load_font, rasterize_glyph,
};

/// System fonts tried by the rasterization tests. A missing font skips the
/// test rather than failing, so the suite stays portable.
const TEST_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Loads the first available system test font, if any.
fn load_test_font(fs: &FontSystem) -> Option<Font> {
    TEST_FONT_PATHS.iter().find_map(|path| {
        let bytes = std::fs::read(path).ok()?;
        load_font(fs, &bytes, 0).ok()
    })
}

#[test]
fn load_font_rejects_garbage() {
    let fs = create_font_system();
    assert!(matches!(
        load_font(&fs, b"", 0),
        Err(GlyphError::InvalidFont { .. })
    ));
    assert!(matches!(
        load_font(&fs, b"definitely not a font", 0),
        Err(GlyphError::InvalidFont { .. })
    ));
}

#[test]
fn rasterizes_a_latin_glyph_consistently() {
    let fs = create_font_system();
    let Some(font) = load_test_font(&fs) else {
        return;
    };
    let style = GlyphStyle::default();
    let bitmap = rasterize_glyph(&fs, &font, 'A', 32.0, style).expect("rasterize a Latin glyph");
    assert!(bitmap.is_consistent(), "coverage must match dimensions");
    assert!(!bitmap.is_empty(), "'A' must have ink");
    assert!(bitmap.advance > 0.0);
    assert!(bitmap.coverage.iter().any(|&coverage| coverage > 0));
    assert_eq!(bitmap.advance, glyph_advance(&fs, &font, 'A', 32.0, style));
}

#[test]
fn bold_widens_and_bumps_advance() {
    let fs = create_font_system();
    let Some(font) = load_test_font(&fs) else {
        return;
    };
    let regular = rasterize_glyph(
        &fs,
        &font,
        'A',
        32.0,
        GlyphStyle {
            bold: false,
            italic: false,
        },
    )
    .expect("regular glyph");
    let bold = rasterize_glyph(
        &fs,
        &font,
        'A',
        32.0,
        GlyphStyle {
            bold: true,
            italic: false,
        },
    )
    .expect("bold glyph");
    assert_eq!(bold.width, regular.width + 1);
    assert_eq!(bold.advance, regular.advance + 1.0);
}

#[test]
fn italic_shear_does_not_narrow() {
    let fs = create_font_system();
    let Some(font) = load_test_font(&fs) else {
        return;
    };
    let regular = rasterize_glyph(
        &fs,
        &font,
        'A',
        32.0,
        GlyphStyle {
            bold: false,
            italic: false,
        },
    )
    .expect("regular glyph");
    let italic = rasterize_glyph(
        &fs,
        &font,
        'A',
        32.0,
        GlyphStyle {
            bold: false,
            italic: true,
        },
    )
    .expect("italic glyph");
    assert!(italic.width >= regular.width);
    // Italic must not change the advance.
    assert_eq!(italic.advance, regular.advance);
}

#[test]
fn metrics_are_positive_at_a_positive_size() {
    let fs = create_font_system();
    let Some(font) = load_test_font(&fs) else {
        return;
    };
    let metrics = font_metrics(&fs, &font, 32.0);
    assert!(metrics.ascent > 0.0);
    assert!(metrics.descent >= 0.0);
    assert!(metrics.line_height() > 0.0);
    assert!(metrics.units_per_em >= 1.0);
    assert_eq!(font_metrics(&fs, &font, 0.0), FontMetrics::default());
}

#[test]
fn control_characters_and_bad_sizes_are_handled() {
    let fs = create_font_system();
    let Some(font) = load_test_font(&fs) else {
        return;
    };
    let control = rasterize_glyph(&fs, &font, '\n', 32.0, GlyphStyle::default())
        .expect("control is not an error");
    assert!(control.is_empty());
    assert_eq!(control.advance, 0.0);
    assert_eq!(
        glyph_advance(&fs, &font, '\n', 32.0, GlyphStyle::default()),
        0.0
    );

    assert!(matches!(
        rasterize_glyph(&fs, &font, 'A', 0.0, GlyphStyle::default()),
        Err(GlyphError::Rasterize { .. })
    ));
    assert_eq!(
        glyph_advance(&fs, &font, 'A', -1.0, GlyphStyle::default()),
        0.0
    );
}

#[test]
fn a_mapped_glyph_without_an_outline_keeps_its_advance() {
    let fs = create_font_system();
    let Some(font) = load_test_font(&fs) else {
        return;
    };
    // A space is mapped by the face but has no outline; it must draw nothing
    // and still advance the pen, not fail rasterization.
    let space = rasterize_glyph(&fs, &font, ' ', 32.0, GlyphStyle::default())
        .expect("a space is not an error");
    assert!(space.is_empty(), "a space has no ink");
    assert_eq!(
        space.advance,
        glyph_advance(&fs, &font, ' ', 32.0, GlyphStyle::default())
    );
    assert!(space.advance > 0.0, "a space always advances");
}

#[test]
fn glyph_scene_construction() {
    let run = GlyphRun {
        atlas: None,
        size_px: 16.0,
        color: [1.0, 1.0, 1.0, 1.0],
        glyphs: vec![
            GlyphPlacement::new('A', 0.0, 0.0, 9.0, PixelRegion::new(0, 0, 8, 12)),
            GlyphPlacement::new('B', 9.0, 0.0, 9.0, PixelRegion::new(8, 0, 8, 12)),
        ],
    };
    assert_eq!(run.len(), 2);
    assert!(!run.is_empty());

    let node = RenderNode {
        content: RenderContent::Glyphs(run),
        ..RenderNode::default()
    };
    assert!(matches!(node.content, RenderContent::Glyphs(_)));
}
