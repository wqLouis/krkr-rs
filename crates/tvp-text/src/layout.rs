//! Text layout: line breaking, wrapping and alignment for CJK text.
//!
//! # Conventions
//!
//! - **Coordinate space**: y-down pixel space, origin at the top-left of the
//!   text block. A run occupies `[y, y + line_height)`, lines stack downward.
//! - **Baseline**: a line's baseline is at `run.y + ascent` where `ascent` is
//!   the font's ascent ([`GlyphAtlas::ascent`]); each glyph's ink top is
//!   `run.y + ascent − bearing_y`, exactly the reference
//!   `drect.top = y + ascent − bitmap_top` (`LayerBitmapImpl.cpp:913`,
//!   `FreeType.cpp:499`). Glyphs are **not** vertically centered in the line:
//!   that would shift CJK ink by several pixels at larger sizes.
//! - **Wrapping**: with `wrap: true`, a line breaks when the next glyph plus
//!   the inter-character `pitch` would exceed `max_width`; this is the
//!   reference `TextRender` condition `m_boxWidth < advance + x + pitch`
//!   (`reference/cpp/plugins/TextRender.cpp:953`). CJK breaks anywhere
//!   (ideographs need no space separators); a space at a break point is
//!   dropped instead of being emitted. With `wrap: false` the text runs on one
//!   line per `\n` even if it overflows.
//! - **Whitespace**: spaces and other whitespace contribute their advance (and
//!   `pitch`) to the line width but produce no placed glyph. Control
//!   characters are skipped entirely.
//! - **Line height**: defaults to the font's `ascent − descent` at the given
//!   pixel height; can be overridden via [`LayoutOptions::line_height`]. The
//!   reference `TextRender` plugin advances `y` by `ascent + lineSpacing`
//!   (`TextRender.cpp:853`); a caller can reproduce that by setting
//!   `line_height` to `ascent + spacing`.
//! - **Per-glyph advances** are rounded to whole pixels before summing, like
//!   the reference `FT_PosToInt` (`FreeType.cpp:488`, `FreeType.h:65`), so
//!   `run.width` agrees with [`crate::measure_width`] for fully-present text.
//! - **Kerning**: not applied — neither `GetTextExtent` nor `GetBitmap` in the
//!   reference calls `FT_Get_Kerning`.
//!
//! # Pre-rendered (`.tft`) layout
//!
//! [`layout_prerendered`] mirrors the vector layout but consumes a
//! [`PrerenderedFont`], using the reference `.tft` geometry: the ink left edge
//! is `pen_x + OriginX`, the ink top is `line_top + ascent − OriginY`, and the
//! pen advances by `CellIncX` (`LayerBitmapImpl.cpp:279`, `:1367`). The caller
//! supplies the `ascent` because the reference takes it from the mapped
//! outline face (`TVPGetCharacter`'s `AscentOfsY`, `LayerBitmapImpl.cpp:725`).
//! It must be the same [`GlyphAtlas::ascent`] the vector path uses for that
//! `(face, height, bold)`, so a `.tft` glyph and an outline glyph on one line
//! share a baseline; likewise `line_height` should be that atlas's
//! [`GlyphAtlas::line_height`]. When no outline face resolves to a rasterizer,
//! a mapped `.tft` is still self-contained: use the pixel height as a
//! deterministic fallback ascent/line height (the same fallback the visual
//! layer uses) rather than a made-up fraction of the em.
//!
//! # Not implemented (matching this reference revision)
//!
//! - **Vertical writing**: a leading `@` on a face name only selects the
//!   vertical face in the font database (`TVPFindFont`, `FontImpl.cpp:282`);
//!   the visual layer never switches to vertical metrics (`AscentOfsX/Y` are
//!   rotation offsets, not vertical layout). Both layout functions here are
//!   horizontal.
//! - **Ruby**: the `TextRender` plugin parses `[...]` but leaves ruby as a
//!   `TODO` (`reference/cpp/plugins/TextRender.cpp:744`), and the core `Font`
//!   has no ruby property. No ruby is synthesized here.

use crate::atlas::GlyphAtlas;
use crate::prerendered::{PrerenderedFont, PrerenderedGlyph};

/// Horizontal alignment of each visual line within `max_width`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// Glyphs start at x = 0.
    #[default]
    Left,
    /// The line is centered within `max_width`.
    Center,
    /// The line is right-aligned to `max_width`.
    Right,
}

/// Options controlling [`layout`].
#[derive(Debug, Clone, Copy, Default)]
pub struct LayoutOptions {
    /// Per-line horizontal alignment within `max_width`.
    pub align: Align,
    /// Wrap lines that exceed `max_width` (CJK breaks anywhere).
    pub wrap: bool,
    /// Override the line height in pixels; defaults to the font's
    /// `ascent − descent`.
    pub line_height: Option<f32>,
    /// Extra inter-character spacing in pixels, added after every glyph
    /// (the reference `TextRender` `pitch`, `TextRender.cpp:953`). Defaults
    /// to 0.
    pub pitch: f32,
}

/// One glyph placed on a line.
#[derive(Debug, Clone, Copy)]
pub struct PlacedGlyph {
    /// The character.
    pub ch: char,
    /// Left edge of the glyph's advance box, in px, relative to the text block
    /// origin (y-down; alignment offsets already applied).
    pub x: f32,
    /// Top of the glyph's ink quad, in px, relative to the text block origin.
    pub y: f32,
    /// Atlas position of the ink quad (top-left corner).
    pub uv: (u32, u32),
    /// Ink quad size in pixels.
    pub size: (u32, u32),
    /// Horizontal advance of this glyph, in px.
    pub advance: f32,
}

/// One visual line of text.
#[derive(Debug, Clone)]
pub struct GlyphRun {
    /// The placed glyphs of this line (whitespace is not placed).
    pub chars: Vec<PlacedGlyph>,
    /// Top of the line, in px (y-down).
    pub y: f32,
    /// Total advance width of the line, in px (includes whitespace advances
    /// and `pitch`).
    pub width: f32,
    /// Height of this line in px.
    pub line_height: f32,
}

/// Result of a layout: the runs plus the total block height.
#[derive(Debug, Clone, Default)]
pub struct TextLayout {
    /// Visual lines, top to bottom.
    pub runs: Vec<GlyphRun>,
    /// Total height of the text block, in px (`line_height` × line count).
    pub total_height: f32,
}

/// Layout `text` into lines for the given `atlas` (which fixes the font and
/// pixel height).
///
/// `font_height` must equal `atlas.pixel_height()`; it is passed separately so
/// callers that only carry a `FontState.height` can pass it through directly
/// (the mismatch is caught by `debug_assert`).
///
/// `max_width` is the width each visual line is laid out and aligned within.
/// Returns the placed glyphs plus `total_height` for layer sizing.
pub fn layout(
    text: &str,
    max_width: f32,
    font_height: f32,
    atlas: &mut GlyphAtlas,
    options: &LayoutOptions,
) -> TextLayout {
    debug_assert_eq!(
        font_height as u32,
        atlas.pixel_height(),
        "font_height must equal the atlas pixel height"
    );
    let max_width = max_width.max(0.0);
    let line_height = options.line_height.unwrap_or_else(|| atlas.line_height());
    // Baseline offset from the top of a line; the reference uses the face's
    // ascent (`GetAscentHeight`), independent of the chosen line height.
    let ascent = atlas.ascent();
    let pitch = options.pitch.max(0.0);

    let mut runs: Vec<GlyphRun> = Vec::new();
    // Y of the top of the next visual line (y-down).
    let mut y = 0.0_f32;

    for source_line in split_lines(text) {
        if source_line.is_empty() {
            // Intentional blank line (e.g. "\n\n"): preserve the vertical slot.
            runs.push(GlyphRun {
                chars: Vec::new(),
                y,
                width: 0.0,
                line_height,
            });
            y += line_height;
            continue;
        }
        if source_line.chars().all(char::is_whitespace) {
            // Whitespace-only source lines produce no visual line at all.
            continue;
        }

        // Current visual line being built.
        let mut placed: Vec<PlacedGlyph> = Vec::new();
        let mut width = 0.0_f32;

        for ch in source_line.chars() {
            if ch.is_control() {
                continue;
            }
            let slot = atlas.rasterize_char(ch);
            if ch.is_whitespace() {
                // Spaces advance the pen but draw nothing.
                width += slot.advance + pitch;
                continue;
            }
            if options.wrap && !placed.is_empty() && width + slot.advance + pitch > max_width {
                // Wrap: flush the current visual line, start a new one.
                push_run(
                    &mut runs,
                    &mut placed,
                    width,
                    y,
                    line_height,
                    max_width,
                    options.align,
                );
                y += line_height;
                width = 0.0;
            }
            placed.push(PlacedGlyph {
                ch,
                x: width,
                // Ink top from the baseline, matching the reference
                // `drect.top = y + ascent - bitmap_top`.
                y: y + ascent - slot.bearing_y as f32,
                uv: (slot.u, slot.v),
                size: (slot.w, slot.h),
                advance: slot.advance,
            });
            width += slot.advance + pitch;
        }

        push_run(
            &mut runs,
            &mut placed,
            width,
            y,
            line_height,
            max_width,
            options.align,
        );
        y += line_height;
    }

    TextLayout {
        runs,
        total_height: y,
    }
}

/// Split `text` into source lines on `\n`, dropping the phantom empty line
/// produced by a trailing `\n`.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Apply per-line alignment and push the run (skipping empty ones).
fn push_run(
    runs: &mut Vec<GlyphRun>,
    placed: &mut Vec<PlacedGlyph>,
    width: f32,
    y: f32,
    line_height: f32,
    max_width: f32,
    align: Align,
) {
    if placed.is_empty() {
        return;
    }
    let offset = match align {
        Align::Left => 0.0,
        Align::Center => ((max_width - width) / 2.0).max(0.0),
        Align::Right => (max_width - width).max(0.0),
    };
    if offset != 0.0 {
        for glyph in placed.iter_mut() {
            glyph.x += offset;
        }
    }
    runs.push(GlyphRun {
        chars: std::mem::take(placed),
        y,
        width,
        line_height,
    });
}

// ---------------------------------------------------------------------------
// Pre-rendered (`.tft`) layout
// ---------------------------------------------------------------------------

/// One `.tft` glyph placed on a line, in the reference draw coordinate space
/// (integers, y-down, origin at the text block's top-left).
#[derive(Debug, Clone, Copy)]
pub struct PlacedPrerenderedGlyph {
    /// The character.
    pub ch: char,
    /// Left edge of the coverage bitmap: `pen_x + OriginX` (reference
    /// `drect.left`, `LayerBitmapImpl.cpp:912`).
    pub x: i32,
    /// Top edge of the coverage bitmap: `line_top + ascent − OriginY`
    /// (reference `drect.top`, `LayerBitmapImpl.cpp:913`).
    pub y: i32,
    /// The glyph's raw metrics and bitmap location.
    pub glyph: PrerenderedGlyph,
}

/// One visual line of `.tft` text.
#[derive(Debug, Clone)]
pub struct PrerenderedGlyphRun {
    /// Placed glyphs (whitespace and newlines are not placed).
    pub chars: Vec<PlacedPrerenderedGlyph>,
    /// Top of the line, in px (y-down).
    pub y: i32,
    /// Total advance width of the line, in px (sum of `CellIncX` + `pitch`).
    pub width: i32,
    /// Height of this line in px.
    pub line_height: i32,
}

/// Result of a `.tft` layout: the runs plus the total block height.
#[derive(Debug, Clone, Default)]
pub struct PrerenderedTextLayout {
    /// Visual lines, top to bottom.
    pub runs: Vec<PrerenderedGlyphRun>,
    /// Total height of the text block, in px (`line_height` × line count).
    pub total_height: i32,
}

/// Options controlling [`layout_prerendered`].
#[derive(Debug, Clone, Copy)]
pub struct PrerenderedLayoutOptions {
    /// Per-line horizontal alignment within `max_width`.
    pub align: Align,
    /// Wrap lines whose advance would exceed `max_width`.
    pub wrap: bool,
    /// Extra inter-character spacing in pixels (reference `pitch`).
    pub pitch: i32,
}

impl Default for PrerenderedLayoutOptions {
    fn default() -> Self {
        Self {
            align: Align::Left,
            wrap: false,
            pitch: 0,
        }
    }
}

/// Layout `text` with a `.tft` pre-rendered font.
///
/// `ascent` is the distance from the top of a line to its baseline — in the
/// reference this comes from the mapped outline face
/// (`TVPGetCharacter`'s `AscentOfsY`, `LayerBitmapImpl.cpp:725`), not from the
/// `.tft` itself. `line_height` is the vertical step between lines.
///
/// The pen advances by [`PrerenderedGlyph::advance`] (`CellIncX`, the reference
/// draw advance) and glyphs are placed with their true `OriginX`/`OriginY`
/// bearings. `Invalid`/control characters are skipped; `'\n'` starts a new
/// line. Characters absent from the `.tft` are skipped here (the reference
/// falls back to the outline rasterizer per character; callers that need a
/// faithful fallback should test [`PrerenderedFont::find`] first).
///
/// The returned `width` is the pen advance in whole pixels; the reference
/// `Font.getTextWidth` for a mapped `.tft` instead sums the `Inc` field
/// ([`PrerenderedFont::measure_width_with`]).
pub fn layout_prerendered(
    font: &PrerenderedFont,
    text: &str,
    max_width: i32,
    ascent: i32,
    line_height: i32,
    options: &PrerenderedLayoutOptions,
) -> PrerenderedTextLayout {
    let max_width = max_width.max(0);
    let line_height = line_height.max(1);
    let pitch = options.pitch.max(0);

    let mut runs: Vec<PrerenderedGlyphRun> = Vec::new();
    let mut y = 0_i32;

    for source_line in split_lines(text) {
        if source_line.is_empty() {
            runs.push(PrerenderedGlyphRun {
                chars: Vec::new(),
                y,
                width: 0,
                line_height,
            });
            y += line_height;
            continue;
        }
        if source_line.chars().all(char::is_whitespace) {
            continue;
        }

        let mut placed: Vec<PlacedPrerenderedGlyph> = Vec::new();
        let mut width = 0_i32;

        for ch in source_line.chars() {
            if ch.is_control() {
                continue;
            }
            let Some(glyph) = font.find(ch) else {
                continue;
            };
            if options.wrap && !placed.is_empty() && width + glyph.advance() + pitch > max_width {
                push_prerendered_run(
                    &mut runs,
                    &mut placed,
                    width,
                    y,
                    line_height,
                    max_width,
                    options.align,
                );
                y += line_height;
                width = 0;
            }
            if glyph.width == 0 || glyph.height == 0 {
                // Whitespace / zero-ink: advance the pen but draw nothing
                // (reference `DrawTextMultiple` skips `BlackBoxX == 0` but
                // still does `x += CellIncX`).
                width += glyph.advance() + pitch;
                continue;
            }
            placed.push(PlacedPrerenderedGlyph {
                ch,
                x: glyph.left(width),
                y: glyph.top(y, ascent),
                glyph,
            });
            width += glyph.advance() + pitch;
        }

        push_prerendered_run(
            &mut runs,
            &mut placed,
            width,
            y,
            line_height,
            max_width,
            options.align,
        );
        y += line_height;
    }

    PrerenderedTextLayout {
        runs,
        total_height: y,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_prerendered_run(
    runs: &mut Vec<PrerenderedGlyphRun>,
    placed: &mut Vec<PlacedPrerenderedGlyph>,
    width: i32,
    y: i32,
    line_height: i32,
    max_width: i32,
    align: Align,
) {
    if placed.is_empty() {
        return;
    }
    let offset = match align {
        Align::Left => 0,
        Align::Center => ((max_width - width) / 2).max(0),
        Align::Right => (max_width - width).max(0),
    };
    if offset != 0 {
        for glyph in placed.iter_mut() {
            glyph.x += offset;
        }
    }
    runs.push(PrerenderedGlyphRun {
        chars: std::mem::take(placed),
        y,
        width,
        line_height,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prerendered::PrerenderedFont;

    const MAGIC: &[u8; 22] = b"TVP pre-rendered font\x1a";
    const HEADER_LEN: usize = 36;
    const ITEM_LEN: usize = 20;

    /// Build a version-1 `.tft` from `(ch, w, h, origin_x, origin_y, inc_x,
    /// inc)` records. Coverage bytes are arbitrary non-zero patterns.
    #[allow(clippy::type_complexity)]
    fn build_tft(glyphs: &[(char, u16, u16, i16, i16, i16, i16)]) -> Vec<u8> {
        // The `.tft` character index must be sorted for the binary search.
        let mut glyphs = glyphs.to_vec();
        glyphs.sort_by_key(|g| g.0 as u32);
        let glyphs = &glyphs[..];
        let count = glyphs.len();
        let mut bitmaps = Vec::new();
        let mut offsets = Vec::new();
        for &(_, w, h, ..) in glyphs {
            offsets.push((HEADER_LEN + bitmaps.len()) as u32);
            for i in 0..(w as usize * h as usize) {
                bitmaps.push((i % 16) as u8);
            }
        }
        let ch_index = HEADER_LEN + bitmaps.len();
        let index = ch_index + count * 2;
        let mut data = vec![0u8; index + count * ITEM_LEN];
        data[..22].copy_from_slice(MAGIC);
        data[22] = 1;
        data[23] = 2;
        data[24..28].copy_from_slice(&(count as u32).to_le_bytes());
        data[28..32].copy_from_slice(&(ch_index as u32).to_le_bytes());
        data[32..36].copy_from_slice(&(index as u32).to_le_bytes());
        data[HEADER_LEN..HEADER_LEN + bitmaps.len()].copy_from_slice(&bitmaps);
        for (i, &(ch, w, h, ox, oy, icx, inc)) in glyphs.iter().enumerate() {
            data[ch_index + i * 2..ch_index + i * 2 + 2]
                .copy_from_slice(&(ch as u16).to_le_bytes());
            let item = &mut data[index + i * ITEM_LEN..index + (i + 1) * ITEM_LEN];
            item[0..4].copy_from_slice(&offsets[i].to_le_bytes());
            item[4..6].copy_from_slice(&w.to_le_bytes());
            item[6..8].copy_from_slice(&h.to_le_bytes());
            item[8..10].copy_from_slice(&ox.to_le_bytes());
            item[10..12].copy_from_slice(&oy.to_le_bytes());
            item[12..14].copy_from_slice(&icx.to_le_bytes());
            item[16..18].copy_from_slice(&inc.to_le_bytes());
        }
        data
    }

    #[test]
    fn places_glyphs_with_origin_and_advance() {
        // A: origin (1,4), w=2, h=3, advance 5. B: origin (0,3), advance 7.
        let data = build_tft(&[('A', 2, 3, 1, 4, 5, 5), ('B', 2, 3, 0, 3, 7, 7)]);
        let font = PrerenderedFont::from_bytes(data).unwrap();
        let laid = layout_prerendered(
            &font,
            "AB",
            1000,
            10,
            12,
            &PrerenderedLayoutOptions::default(),
        );
        assert_eq!(laid.runs.len(), 1);
        let run = &laid.runs[0];
        assert_eq!(run.chars.len(), 2);
        // left = pen_x + OriginX, top = line_top + ascent - OriginY
        assert_eq!((run.chars[0].x, run.chars[0].y), (1, 10 - 4));
        assert_eq!((run.chars[1].x, run.chars[1].y), (5, 10 - 3));
        assert_eq!(run.width, 5 + 7);
        assert_eq!(laid.total_height, 12);
    }

    #[test]
    fn zero_ink_glyph_advances_but_is_not_placed() {
        let data = build_tft(&[
            ('A', 2, 2, 0, 2, 5, 5),
            (' ', 0, 0, 0, 0, 4, 4),
            ('B', 2, 2, 0, 2, 5, 5),
        ]);
        let font = PrerenderedFont::from_bytes(data).unwrap();
        let laid = layout_prerendered(
            &font,
            "A B",
            1000,
            8,
            10,
            &PrerenderedLayoutOptions::default(),
        );
        let run = &laid.runs[0];
        let chars: String = run.chars.iter().map(|g| g.ch).collect();
        assert_eq!(chars, "AB");
        assert_eq!(run.width, 5 + 4 + 5);
        assert_eq!(run.chars[1].x, 5 + 4);
    }

    #[test]
    fn wraps_and_aligns() {
        let data = build_tft(&[
            ('A', 1, 1, 0, 1, 5, 5),
            ('B', 1, 1, 0, 1, 5, 5),
            ('C', 1, 1, 0, 1, 5, 5),
        ]);
        let font = PrerenderedFont::from_bytes(data).unwrap();
        // Two 5-px glyphs fit in 10 px; the third wraps.
        let laid = layout_prerendered(
            &font,
            "ABC",
            10,
            6,
            8,
            &PrerenderedLayoutOptions {
                wrap: true,
                ..Default::default()
            },
        );
        assert_eq!(laid.runs.len(), 2);
        assert_eq!(laid.runs[0].chars.len(), 2);
        assert_eq!(laid.runs[1].chars.len(), 1);
        assert_eq!(laid.runs[1].chars[0].x, 0);
        assert_eq!(laid.runs[1].y, 8);
        assert_eq!(laid.total_height, 16);

        // Center a 5 px line inside 21 px: offset (21-5)/2 = 8, plus origin 0.
        let laid = layout_prerendered(
            &font,
            "A",
            21,
            6,
            8,
            &PrerenderedLayoutOptions {
                align: Align::Center,
                ..Default::default()
            },
        );
        assert_eq!(laid.runs[0].chars[0].x, 8);
    }

    #[test]
    fn newline_starts_a_new_line() {
        let data = build_tft(&[('A', 1, 1, 2, 1, 5, 5)]);
        let font = PrerenderedFont::from_bytes(data).unwrap();
        let laid = layout_prerendered(
            &font,
            "A\nA",
            1000,
            7,
            9,
            &PrerenderedLayoutOptions::default(),
        );
        assert_eq!(laid.runs.len(), 2);
        assert_eq!(laid.runs[0].chars[0].y, 7 - 1);
        assert_eq!(laid.runs[1].chars[0].y, 9 + 7 - 1);
    }
}
