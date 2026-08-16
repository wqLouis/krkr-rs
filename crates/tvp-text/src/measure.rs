//! Text measurement: pixel width of a string for hit-testing and centering.

use ab_glyph::{Font, ScaleFont};

use crate::font::{FontFace, glyph_id_with_fallback, px_scale_for_height};

/// Measure the pixel width of `text` (its first line) at `pixel_height`.
///
/// Width is the sum of the glyph advances, matching what [`layout`] places.
/// Measuring stops at the first `\n`; `\r` and other control characters
/// contribute nothing. Missing glyphs measure with the fallback glyph's
/// advance (see [`GlyphAtlas::rasterize_char`]).
///
/// [`layout`]: crate::layout
/// [`GlyphAtlas::rasterize_char`]: crate::GlyphAtlas::rasterize_char
pub fn measure_width(text: &str, font: &FontFace, pixel_height: f32) -> u32 {
    let scale = px_scale_for_height(font.font(), pixel_height);
    let scaled = font.font().as_scaled(scale);
    let mut width = 0.0_f32;
    for ch in text.chars() {
        if ch == '\n' {
            break;
        }
        if ch.is_control() {
            continue;
        }
        width += scaled.h_advance(glyph_id_with_fallback(ch, &scaled));
    }
    width.ceil() as u32
}
