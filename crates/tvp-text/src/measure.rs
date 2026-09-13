//! Text measurement: pixel metrics of a string, matching the reference
//! `tFreeTypeFace::GetGlyphSizeFromCharcode` / `Font.getTextWidth` contract.
//!
//! # Reference behavior
//!
//! KiriKiri2/TVP measures a string by summing a per-character extent
//! (`tTVPNativeBaseBitmap::GetTextSize`, `LayerBitmapImpl.cpp:1435`, and
//! `tTJSNI_Font::GetTextWidthDirect`, `LayerIntf.cpp:11702`):
//!
//! * Each character's advance comes from
//!   `tFreeTypeFace::GetGlyphSizeFromCharcode` (`FreeType.cpp:626`), which
//!   reports `FT_PosToInt(glyph->metrics.horiAdvance)` — the glyph's
//!   *unhinted-by-drawing* advance, rounded to a whole pixel.
//! * `FT_PosToInt(x)` is `((x + 32) >> 6)` on the 26.6 fixed-point value
//!   (`FreeType.h:65`), i.e. round-half-up.
//! * When the font has no glyph for the character (`FT_Get_Char_Index` returns
//!   0), the extent falls back to `w = h = Face->GetHeight()`
//!   (`FreeTypeFontRasterizer.cpp:147`), i.e. the pixel height.
//! * The loop walks the whole C string up to its terminating NUL. There is no
//!   newline special case: a `'\n'` is just a character (usually absent from
//!   the font, so it measures as the pixel height).
//! * Bold/italic/underline/strikeout do **not** change the measured advance;
//!   the reference computes the size metric before `FT_GlyphSlot_Embolden`.
//!
//! `getTextHeight` is independent of the string: both
//! `tTJSNI_Font::GetTextHeight` (`LayerIntf.cpp:11724`) and
//! `tTVPNativeBaseBitmap::GetTextHeight` (`LayerBitmapImpl.cpp:1479`) return
//! `abs(Font.Height)`.

use ab_glyph::{Font, GlyphId, ScaleFont};

use crate::font::{FontFace, px_scale_for_height, round_advance};

/// Measure the pixel width of `text` at `pixel_height`, as the reference
/// `Font.getTextWidth` would.
///
/// The whole string is measured up to the first NUL (like C's
/// `while(*buf)`); there is no `'\n'` truncation. Each character contributes
/// the rounded glyph advance, or `pixel_height` when the face has no glyph for
/// it (the reference `GetTextExtent` failure fallback). The result is an
/// integer; per-character rounding happens before the sum, exactly as in the
/// reference.
pub fn measure_width(text: &str, font: &FontFace, pixel_height: f32) -> u32 {
    let scale = px_scale_for_height(font.font(), pixel_height);
    let scaled = font.font().as_scaled(scale);
    let height = round_advance(pixel_height).max(0);
    let mut width = 0_i32;
    for ch in text.chars() {
        if ch == '\0' {
            // The reference iterates `while(*buf)`, so a NUL terminates it.
            break;
        }
        let id = scaled.glyph_id(ch);
        if id == GlyphId(0) {
            width += height;
        } else {
            width += round_advance(scaled.h_advance(id));
        }
    }
    width.max(0) as u32
}

/// The reference `Font.getTextHeight`: `abs(Font.Height)` in pixels.
///
/// This is independent of the measured string, matching
/// `tTVPNativeBaseBitmap::GetTextHeight` (`LayerBitmapImpl.cpp:1479`) and
/// `tTJSNI_Font::GetTextHeight` (`LayerIntf.cpp:11724`).
pub fn measure_height(pixel_height: i32) -> u32 {
    pixel_height.unsigned_abs()
}

/// The four rotated extents the reference exposes as `getEscWidthX`,
/// `getEscWidthY`, `getEscHeightX` and `getEscHeightY`
/// (`LayerBitmapImpl.cpp:1485-1502`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EscapedExtents {
    /// `cos(angle) * width`.
    pub width_x: f64,
    /// `sin(angle) * -width`.
    pub width_y: f64,
    /// `sin(angle) * height`.
    pub height_x: f64,
    /// `cos(angle) * height`.
    pub height_y: f64,
}

/// Rotate a `(width, height)` extent by `angle_tenths` (the reference
/// `Font.Angle`, in tenths of a degree; the radian angle is
/// `angle_tenths * π / 1800`).
pub fn escaped_extents_of(width: u32, height: u32, angle_tenths: i32) -> EscapedExtents {
    let rad = f64::from(angle_tenths) * std::f64::consts::PI / 1800.0;
    let w = f64::from(width);
    let h = f64::from(height);
    EscapedExtents {
        width_x: rad.cos() * w,
        width_y: rad.sin() * -w,
        height_x: rad.sin() * h,
        height_y: rad.cos() * h,
    }
}

/// `Font.getEscWidthX/Y` and `getEscHeightX/Y` for a face at `pixel_height`.
pub fn escaped_extents(
    text: &str,
    font: &FontFace,
    pixel_height: i32,
    angle_tenths: i32,
) -> EscapedExtents {
    escaped_extents_of(
        measure_width(text, font, pixel_height as f32),
        measure_height(pixel_height),
        angle_tenths,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GlyphAtlas;

    /// Best-effort Japanese face; tests skip when no CJK font is installed.
    fn jp_face() -> Option<FontFace> {
        FontFace::discover_system_jp()
    }

    #[test]
    fn height_is_absolute_font_height() {
        assert_eq!(measure_height(12), 12);
        assert_eq!(measure_height(-12), 12);
        assert_eq!(measure_height(0), 0);
    }

    #[test]
    fn escaped_extents_match_the_trigonometry() {
        // At 0deg the width/height pass through; at 90deg (900 tenths) the
        // width maps onto -Y and the height onto +X, like the reference.
        let e = escaped_extents_of(100, 20, 0);
        assert!((e.width_x - 100.0).abs() < 1e-9);
        assert!(e.width_y.abs() < 1e-9);
        assert!(e.height_x.abs() < 1e-9);
        assert!((e.height_y - 20.0).abs() < 1e-9);

        let e = escaped_extents_of(100, 20, 900);
        assert!(e.width_x.abs() < 1e-9);
        assert!((e.width_y + 100.0).abs() < 1e-9);
        assert!((e.height_x - 20.0).abs() < 1e-9);
        assert!(e.height_y.abs() < 1e-9);
    }

    #[test]
    fn width_matches_the_rounded_atlas_advances() {
        // For characters the face has, `measure_width` must equal the sum of
        // the per-glyph advances the atlas reports (both rounded with
        // `FT_PosToInt`), because the reference rounds per character before
        // summing (`FreeType.cpp:626`).
        let Some(face) = jp_face() else {
            eprintln!("skipping: no system CJK font");
            return;
        };
        let text = "日本語abc0";
        let height = 24.0_f32;
        let measured = measure_width(text, &face, height);
        let mut atlas = GlyphAtlas::with_default_width(std::sync::Arc::new(face), height as u32);
        let expected: u32 = text
            .chars()
            .map(|c| atlas.rasterize_char(c).advance as u32)
            .sum();
        assert_eq!(measured, expected);
    }

    #[test]
    fn missing_glyph_measures_as_pixel_height() {
        // Reference `GetTextExtent` failure fallback: `w = h = Face->GetHeight()`
        // (`FreeTypeFontRasterizer.cpp:147`). U+E000 is private-use and absent
        // from the CJK face.
        let Some(face) = jp_face() else {
            eprintln!("skipping: no system CJK font");
            return;
        };
        assert_eq!(measure_width("\u{E000}", &face, 32.0), 32);
        assert_eq!(measure_width("a\u{E000}b", &face, 32.0), {
            measure_width("a", &face, 32.0) + 32 + measure_width("b", &face, 32.0)
        });
    }

    #[test]
    fn newline_is_measured_like_any_other_character() {
        // The reference walks the C string to its NUL, with no newline special
        // case (`LayerBitmapImpl.cpp:1444`). A `\n` (normally absent from the
        // font) therefore contributes the pixel height.
        let Some(face) = jp_face() else {
            eprintln!("skipping: no system CJK font");
            return;
        };
        assert_eq!(measure_width("\n", &face, 32.0), 32);
        assert_eq!(
            measure_width("A\nB", &face, 32.0),
            measure_width("A", &face, 32.0) + 32 + measure_width("B", &face, 32.0)
        );
    }

    #[test]
    fn nul_terminates_measurement() {
        let Some(face) = jp_face() else {
            eprintln!("skipping: no system CJK font");
            return;
        };
        assert_eq!(
            measure_width("A\0B", &face, 24.0),
            measure_width("A", &face, 24.0)
        );
    }

    #[test]
    fn additive_and_monotonic() {
        let Some(face) = jp_face() else {
            eprintln!("skipping: no system CJK font");
            return;
        };
        let mut prev = 0;
        for end in 1..="日本語テスト".chars().count() {
            let prefix: String = "日本語テスト".chars().take(end).collect();
            let w = measure_width(&prefix, &face, 30.0);
            assert!(w >= prev, "width shrank at {prefix:?}: {w} < {prev}");
            prev = w;
        }
        assert_eq!(
            measure_width("あい", &face, 30.0),
            measure_width("あ", &face, 30.0) + measure_width("い", &face, 30.0)
        );
    }
}
