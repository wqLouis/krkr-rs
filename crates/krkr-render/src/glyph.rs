//! Stateless glyph rasterization: the L1 half of the font contract in
//! `docs/migration/03-middle-layer-api.md` §3.
//!
//! [`FontSystem`] is the stateless service object, [`Font`] is immutable
//! parsed face data, and [`rasterize_glyph`], [`glyph_advance`] and
//! [`font_metrics`] are pure functions over them. The glyph atlas, layout and
//! every cache are middle-layer state and deliberately absent here.
//!
//! The rasterization core is adapted from `crates/tvp-text` (read-only
//! reference): [`px_scale_for_height`] preserves the reference height-to-scale
//! convention, and the rasterizer reproduces the atlas' missing-glyph fallback,
//! synthetic bold (1 px right dilation + advance bump) and the synthetic
//! oblique slant the middle layer previously applied at paint time. Unlike the
//! old atlas, this API returns per-glyph coverage positioned by its bearings
//! instead of packing pixels into a shared RGBA cell.

use std::fmt;

use ab_glyph::{Font as _, FontVec, GlyphId, PxScale, ScaleFont};

use krkr_api::error::GlyphError;
use krkr_api::glyph::{FontMetrics, GlyphBitmap, GlyphStyle};

/// Synthetic oblique slant applied when [`GlyphStyle::italic`] is set.
///
/// `0.25` is the conventional ~14° oblique the middle layer previously applied
/// at paint time (`crates/tvp-visual/src/natives/layer.rs`).
const ITALIC_SLANT: f32 = 0.25;

/// Stateless font service.
///
/// Created by [`create_font_system`]. It owns no mutable state and may be
/// shared by reference; the [`FontSystem`] argument is threaded through every
/// function so a future rasterizer backend can carry immutable configuration
/// here without changing the API.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontSystem;

/// Creates the stateless font service.
pub fn create_font_system() -> FontSystem {
    FontSystem
}

/// An immutable parsed font face.
///
/// The source bytes are owned by the contained `ab_glyph` face, so a `Font`
/// can be moved and shared across threads. [`Font::collection_index`] records
/// which face was selected from a TTC/OTC collection (always `0` for a plain
/// TrueType/OpenType font).
pub struct Font {
    face: FontVec,
    collection_index: u32,
}

impl Font {
    /// Returns the index of this face inside its source collection.
    ///
    /// This is `0` for a plain TTF/OTF and the selected face index for a
    /// TTC/OTC collection.
    pub fn collection_index(&self) -> u32 {
        self.collection_index
    }

    /// Borrows the underlying `ab_glyph` face.
    fn face(&self) -> &FontVec {
        &self.face
    }
}

impl fmt::Debug for Font {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Font")
            .field("collection_index", &self.collection_index)
            .finish_non_exhaustive()
    }
}

/// Parses a face from `bytes`, selecting `index` inside a TTC/OTC collection
/// (`0` for a plain font).
///
/// The bytes are copied into the returned [`Font`], which owns them.
///
/// # Errors
///
/// Returns [`GlyphError::InvalidFont`] when `bytes` is not a valid font or
/// `index` is out of range.
pub fn load_font(fs: &FontSystem, bytes: &[u8], index: u32) -> Result<Font, GlyphError> {
    let _ = fs;
    let face = FontVec::try_from_vec_and_index(bytes.to_vec(), index).map_err(|error| {
        GlyphError::InvalidFont {
            message: error.to_string(),
        }
    })?;
    Ok(Font {
        face,
        collection_index: index,
    })
}

/// Rasterizes `glyph` at `size_px` into an 8-bit coverage bitmap.
///
/// `size_px` is the *em size*: [`px_scale_for_height`] rescales it so a
/// full-width CJK glyph is exactly `size_px` pixels wide, matching the
/// reference measurement contract.
///
/// # Behavior
///
/// * Control characters produce an empty bitmap with zero advance, as in the
///   reference atlas.
/// * A character the face does not map falls back to U+FFFD and then to
///   `.notdef` (glyph 0); [`GlyphError::MissingGlyph`] is returned only when
///   the face cannot even outline `.notdef`.
/// * [`GlyphStyle::bold`] applies a 1 px rightward coverage dilation and adds
///   1 px to the advance.
/// * [`GlyphStyle::italic`] applies a synthetic oblique shear about the
///   glyph's vertical center without changing the advance.
///
/// # Errors
///
/// Returns [`GlyphError::Rasterize`] for a non-positive/non-finite `size_px`
/// or an un-outlineable glyph, and [`GlyphError::MissingGlyph`] for a truly
/// missing glyph.
pub fn rasterize_glyph(
    fs: &FontSystem,
    font: &Font,
    glyph: char,
    size_px: f32,
    style: GlyphStyle,
) -> Result<GlyphBitmap, GlyphError> {
    let _ = fs;
    if size_px <= 0.0 || !size_px.is_finite() {
        return Err(GlyphError::Rasterize {
            message: format!("invalid glyph size {size_px}"),
        });
    }
    if glyph.is_control() {
        return Ok(GlyphBitmap::default());
    }

    let scale = px_scale_for_height(font.face(), size_px);
    let scaled = font.face().as_scaled(scale);
    let id = glyph_id_with_fallback(glyph, &scaled);
    let mut advance = round_advance(scaled.h_advance(id)) as f32;

    let positioned = id.with_scale_and_position(scale, ab_glyph::point(0.0, 0.0));
    let Some(outline) = scaled.outline_glyph(positioned) else {
        return Err(if id == GlyphId(0) {
            GlyphError::MissingGlyph {
                codepoint: glyph as u32,
            }
        } else {
            GlyphError::Rasterize {
                message: format!("the face has no outline for U+{:04X}", glyph as u32),
            }
        });
    };

    let bounds = outline.px_bounds();
    let mut width = bounds.width() as u32;
    let height = bounds.height() as u32;
    let mut bearing_x = bounds.min.x;
    let bearing_y = (-bounds.min.y).round();

    if width == 0 || height == 0 {
        return Ok(GlyphBitmap::new(
            0,
            0,
            Vec::new(),
            bearing_x,
            bearing_y,
            advance,
        ));
    }

    let w = width as usize;
    let h = height as usize;
    let mut coverage = vec![0u8; w * h];
    outline.draw(|dx, dy, value| {
        let index = dy as usize * w + dx as usize;
        if let Some(slot) = coverage.get_mut(index) {
            let alpha = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            *slot = (*slot).max(alpha);
        }
    });

    if style.bold {
        coverage = dilate_right(&coverage, w, h);
        width += 1;
        advance += 1.0;
    }
    if style.italic {
        let (sheared, sheared_width, bearing_delta) = shear_coverage(&coverage, width, height);
        coverage = sheared;
        width = sheared_width;
        bearing_x += bearing_delta;
    }

    debug_assert_eq!(coverage.len(), width as usize * height as usize);
    Ok(GlyphBitmap::new(
        width, height, coverage, bearing_x, bearing_y, advance,
    ))
}

/// Returns the horizontal pen advance for `glyph`.
///
/// The result is the glyph's rounded advance plus the 1 px synthetic-bold bump,
/// i.e. exactly the `advance` field [`rasterize_glyph`] reports for the same
/// arguments. Control characters advance by `0`. A non-positive or non-finite
/// `size_px` also returns `0`.
pub fn glyph_advance(
    fs: &FontSystem,
    font: &Font,
    glyph: char,
    size_px: f32,
    style: GlyphStyle,
) -> f32 {
    let _ = fs;
    if size_px <= 0.0 || !size_px.is_finite() || glyph.is_control() {
        return 0.0;
    }
    let scale = px_scale_for_height(font.face(), size_px);
    let scaled = font.face().as_scaled(scale);
    let id = glyph_id_with_fallback(glyph, &scaled);
    let mut advance = round_advance(scaled.h_advance(id)) as f32;
    if style.bold {
        advance += 1.0;
    }
    advance
}

/// Returns the scaled metrics of `font` at `size_px`.
///
/// `descent` is reported as a positive distance below the baseline, matching
/// [`FontMetrics`]; `units_per_em` falls back to `1000` when the face does not
/// expose it. A non-positive or non-finite `size_px` yields
/// [`FontMetrics::default`].
pub fn font_metrics(fs: &FontSystem, font: &Font, size_px: f32) -> FontMetrics {
    let _ = fs;
    if size_px <= 0.0 || !size_px.is_finite() {
        return FontMetrics::default();
    }
    let scale = px_scale_for_height(font.face(), size_px);
    let scaled = font.face().as_scaled(scale);
    FontMetrics {
        ascent: scaled.ascent(),
        descent: -scaled.descent(),
        line_gap: scaled.line_gap(),
        units_per_em: font.face().units_per_em().unwrap_or(1000.0),
    }
}

/// Converts an em size in pixels into an `ab_glyph` [`PxScale`].
///
/// `ab_glyph` treats `PxScale.y` as the font's line height (`ascent −
/// descent`), whereas the reference treats a font height as the em size.
/// Rescaling by `height / units_per_em` makes a full-width CJK glyph exactly
/// `size_px` pixels wide. This is a direct adaptation of `tvp-text`'s
/// `px_scale_for_height`.
fn px_scale_for_height(font: &FontVec, size_px: f32) -> PxScale {
    debug_assert!(size_px > 0.0, "pixel size must be positive");
    let units_per_em = font.units_per_em().unwrap_or(1000.0);
    let extent = font.height_unscaled();
    let extent = if extent > 0.0 { extent } else { units_per_em };
    PxScale::from(size_px * extent / units_per_em)
}

/// Rounds a pixel-space advance the way the reference `FT_PosToInt` does:
/// round-half-up for positive values (`((x + 32) >> 6)` on 26.6 fixed point).
fn round_advance(advance: f32) -> i32 {
    (advance + 0.5).floor() as i32
}

/// Resolves `c` to a glyph id, preferring the face's U+FFFD replacement glyph
/// and then `.notdef` (glyph 0) exactly as the reference atlas does. Shared by
/// rasterization and advance lookup so both agree.
fn glyph_id_with_fallback(c: char, scaled: &ab_glyph::PxScaleFont<&FontVec>) -> GlyphId {
    let id = scaled.glyph_id(c);
    if id == GlyphId(0) && c != '\0' {
        let replacement = scaled.glyph_id('\u{FFFD}');
        if replacement != GlyphId(0) {
            return replacement;
        }
    }
    id
}

/// Dilates `coverage` one pixel to the right (synthetic bold).
///
/// Returns a buffer one column wider; a callback reading `value` from the
/// original slice means the shift never cascades beyond one pixel.
fn dilate_right(coverage: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; (width + 1) * height];
    for y in 0..height {
        let src = y * width;
        let dst = y * (width + 1);
        for x in 0..width {
            let value = coverage[src + x];
            out[dst + x] = out[dst + x].max(value);
            out[dst + x + 1] = out[dst + x + 1].max(value);
        }
    }
    out
}

/// Shears `coverage` horizontally about its vertical center (synthetic
/// italic).
///
/// Returns the sheared buffer, its width, and the shift to add to the bitmap's
/// left bearing. Rows are rounded to whole pixels, so no extra antialiasing is
/// introduced; the returned bearing is negative because the shear extends the
/// glyph to the left more than the right.
fn shear_coverage(coverage: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, f32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return (coverage.to_vec(), width, 0.0);
    }
    let shifts: Vec<i32> = (0..h)
        .map(|y| (ITALIC_SLANT * (h as f32 / 2.0 - y as f32)).round() as i32)
        .collect();
    let min_shift = shifts.iter().copied().min().unwrap_or(0);
    let max_shift = shifts.iter().copied().max().unwrap_or(0);
    let new_w = w + (max_shift - min_shift) as usize;
    let mut out = vec![0u8; new_w * h];
    for (y, shift) in shifts.iter().enumerate() {
        let offset = (*shift - min_shift) as usize;
        let src = y * w;
        let dst = y * new_w + offset;
        for x in 0..w {
            let value = coverage[src + x];
            let slot = &mut out[dst + x];
            *slot = (*slot).max(value);
        }
    }
    (out, new_w as u32, min_shift as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn round_advance_matches_ft_pos_to_int() {
        assert_eq!(round_advance(0.0), 0);
        assert_eq!(round_advance(3.4), 3);
        assert_eq!(round_advance(3.5), 4);
        assert_eq!(round_advance(10.0), 10);
    }

    #[test]
    fn dilate_right_adds_exactly_one_column() {
        let coverage = [0u8, 10, 0, 20, 0, 30];
        let out = dilate_right(&coverage, 3, 2);
        assert_eq!(out.len(), 4 * 2);
        assert_eq!(&out[0..4], &[0, 10, 10, 0]);
        assert_eq!(&out[4..8], &[20, 20, 30, 30]);
    }

    #[test]
    fn shear_extends_left_and_keeps_rows() {
        let coverage = vec![1u8; 2 * 4];
        let (out, width, bearing_delta) = shear_coverage(&coverage, 2, 4);
        assert_eq!(width, 3);
        assert_eq!(out.len(), 3 * 4);
        // Top row shifts right; the others stay put.
        assert_eq!(&out[0..3], &[0, 1, 1]);
        assert_eq!(&out[3..6], &[1, 1, 0]);
        assert!(bearing_delta <= 0.0);
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
        let bitmap =
            rasterize_glyph(&fs, &font, 'A', 32.0, style).expect("rasterize a Latin glyph");
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
}
