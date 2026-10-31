//! Stateless glyph rasterization: the L1 half of the font contract.
//!
//! [`FontSystem`] is the stateless service object, [`Font`] is immutable
//! parsed face data, and [`rasterize_glyph`], [`glyph_advance`] and
//! [`font_metrics`] are pure functions over them. The glyph atlas, layout and
//! every cache are middle-layer state and deliberately absent here.
//!
//! The rasterization core: [`px_scale_for_height`] keeps the height-to-scale
//! convention, and the rasterizer provides the missing-glyph fallback, the
//! synthetic bold (a 1 px right dilation of the drawn coverage plus the same
//! pixel in the drawn and the measured advance) and the synthetic oblique
//! slant. This API returns per-glyph coverage positioned by its bearings
//! instead of packing pixels into a shared RGBA cell.

use std::fmt;

use ab_glyph::{Font as _, FontVec, GlyphId, PxScale, ScaleFont};

use api::error::GlyphError;
use api::glyph::{FontMetrics, GlyphBitmap, GlyphStyle};

/// Synthetic oblique slant applied when [`GlyphStyle::italic`] is set.
///
/// `0.25` is the conventional ~14° oblique the middle layer applies at paint
/// time.
const ITALIC_SLANT: f32 = 0.25;

/// The pixel the synthetic bold adds to a glyph.
///
/// The coverage is dilated one pixel to the right and the advance grows by the
/// same pixel, so the drawn pen advance and the measured advance agree. A face
/// that emboldens a glyph for real grows the advance by about a twenty-fourth of
/// the em; one pixel is this backend's stand-in for that amount, and
/// `docs/code/platform/render.md` records the gap.
const BOLD_ADVANCE: f32 = 1.0;

/// Stateless font service.
///
/// Created by [`create_font_system`]. It owns no mutable state and may be
/// shared between callers; the [`FontSystem`] argument is threaded through every
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
    os2: Option<Os2Metrics>,
}

/// The OS/2 metrics the outline-text-metric interface reads, in design units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Os2Metrics {
    /// `usWinAscent`.
    win_ascent: u16,
    /// `usWinDescent`, taken as its magnitude the way the platform does.
    win_descent: u16,
    /// `sTypoAscender`.
    typo_ascent: i16,
    /// `sTypoDescender`.
    typo_descent: i16,
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
    let parsed =
        ttf_parser::Face::parse(bytes, index).map_err(|error| GlyphError::InvalidFont {
            message: error.to_string(),
        })?;
    let os2 = parsed.tables().os2.map(|table| Os2Metrics {
        win_ascent: table.windows_ascender() as u16,
        win_descent: table.windows_descender().unsigned_abs(),
        typo_ascent: table.typographic_ascender(),
        typo_descent: table.typographic_descender(),
    });
    let face = FontVec::try_from_vec_and_index(bytes.to_vec(), index).map_err(|error| {
        GlyphError::InvalidFont {
            message: error.to_string(),
        }
    })?;
    Ok(Font {
        face,
        collection_index: index,
        os2,
    })
}

/// Rasterizes `glyph` at `size_px` into an 8-bit coverage bitmap.
///
/// `size_px` is the *em size*: [`px_scale_for_height`] rescales it so a
/// full-width CJK glyph is exactly `size_px` pixels wide.
///
/// # Behavior
///
/// * Control characters produce an empty bitmap with zero advance.
/// * A character the face does not map falls back to U+FFFD and then to
///   `.notdef` (glyph 0); [`GlyphError::MissingGlyph`] is returned only when
///   the face cannot even outline `.notdef`.
/// * A glyph the face maps but does not outline (a space, a combining mark)
///   produces an empty bitmap carrying that glyph's advance.
/// * [`GlyphStyle::bold`] applies a 1 px rightward coverage dilation and adds
///   [`BOLD_ADVANCE`] to the drawn advance; [`glyph_advance`] reports the same
///   advance, so a layout that measures and a layout that draws agree.
/// * [`GlyphStyle::italic`] applies a synthetic oblique shear about the
///   glyph's vertical center without changing the advance.
///
/// # Errors
///
/// Returns [`GlyphError::Rasterize`] for a non-positive/non-finite `size_px`,
/// and [`GlyphError::MissingGlyph`] for a truly missing glyph.
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
    // The bold pixel is part of the advance whatever the glyph outlines, so the
    // no-outline and empty-bitmap returns below report it too.
    if style.bold {
        advance += BOLD_ADVANCE;
    }

    let positioned = id.with_scale_and_position(scale, ab_glyph::point(0.0, 0.0));
    let Some(outline) = scaled.outline_glyph(positioned) else {
        if id == GlyphId(0) {
            return Err(GlyphError::MissingGlyph {
                codepoint: glyph as u32,
            });
        }
        // The face maps this glyph but it has no outline; keep its advance so
        // the pen still moves.
        return Ok(GlyphBitmap::new(0, 0, Vec::new(), 0.0, 0.0, advance));
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
/// The result is the glyph's rounded advance plus [`BOLD_ADVANCE`] when the
/// style is bold, i.e. exactly the `advance` [`rasterize_glyph`] reports for the
/// same arguments: a measurement and a drawing of the same run advance alike.
/// Control characters advance by `0`. A non-positive or non-finite `size_px`
/// also returns `0`.
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
        advance += BOLD_ADVANCE;
    }
    advance
}

/// Returns the scaled metrics of `font` at `size_px`.
///
/// `descent` is reported as a positive distance below the baseline, matching
/// [`FontMetrics`]; `units_per_em` falls back to `1000` when the face does not
/// expose it. The outline metrics come from the face's OS/2 table and stay
/// zero for a face without one. A non-positive or non-finite `size_px` yields
/// [`FontMetrics::default`].
pub fn font_metrics(fs: &FontSystem, font: &Font, size_px: f32) -> FontMetrics {
    let _ = fs;
    if size_px <= 0.0 || !size_px.is_finite() {
        return FontMetrics::default();
    }
    let scale = px_scale_for_height(font.face(), size_px);
    let scaled = font.face().as_scaled(scale);
    let units_per_em = font.face().units_per_em().unwrap_or(1000.0);
    let mut metrics = FontMetrics {
        ascent: scaled.ascent(),
        descent: -scaled.descent(),
        line_gap: scaled.line_gap(),
        units_per_em,
        ..FontMetrics::default()
    };
    if let Some(os2) = font.os2 {
        let px_per_unit = size_px / units_per_em;
        metrics.otm_ascent = f32::from(os2.typo_ascent) * px_per_unit;
        metrics.otm_descent = f32::from(os2.typo_descent) * px_per_unit;
        if os2.win_ascent == 0 && os2.win_descent == 0 {
            metrics.win_ascent = metrics.ascent;
            metrics.win_descent = metrics.descent;
        } else {
            metrics.win_ascent = f32::from(os2.win_ascent) * px_per_unit;
            metrics.win_descent = f32::from(os2.win_descent) * px_per_unit;
        }
    }
    metrics
}

/// Converts an em size in pixels into an `ab_glyph` [`PxScale`].
///
/// `ab_glyph` treats `PxScale.y` as the font's line height (`ascent −
/// descent`), whereas a font height here is the em size. Rescaling by
/// `height / units_per_em` makes a full-width CJK glyph exactly `size_px`
/// pixels wide.
fn px_scale_for_height(font: &FontVec, size_px: f32) -> PxScale {
    // `rasterize_glyph`, `glyph_advance` and `font_metrics` all reject a
    // non-positive or non-finite `size_px` before they call this, so the scale
    // factor is positive here.
    debug_assert!(size_px > 0.0, "pixel size must be positive");
    let units_per_em = font.units_per_em().unwrap_or(1000.0);
    let extent = font.height_unscaled();
    let extent = if extent > 0.0 { extent } else { units_per_em };
    PxScale::from(size_px * extent / units_per_em)
}

/// Rounds a pixel-space advance: round-half-up for positive values
/// (`((x + 32) >> 6)` on 26.6 fixed point).
fn round_advance(advance: f32) -> i32 {
    (advance + 0.5).floor() as i32
}

/// Resolves `c` to a glyph id, preferring the face's U+FFFD replacement glyph
/// and then `.notdef` (glyph 0). Shared by rasterization and advance lookup so
/// both agree.
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
