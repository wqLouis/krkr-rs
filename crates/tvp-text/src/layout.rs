//! Text layout: line breaking, wrapping and alignment for CJK text.
//!
//! # Conventions
//!
//! - **Coordinate space**: y-down pixel space, origin at the top-left of the
//!   text block. A run occupies `[y, y + line_height)`, lines stack downward.
//! - **Baseline**: glyphs are *vertically centered* within their line
//!   (`y + (line_height − ink_height) / 2`), the convention CJK text uses
//!   (glyphs are designed to sit in a centered em box; there is no
//!   Latin-style baseline snapping). The font's baseline of a line sits at
//!   `run.y + ascent_px` for consumers that need it.
//! - **Wrapping**: with `wrap: true`, a line breaks whenever the next glyph
//!   would exceed `max_width`. CJK breaks anywhere (ideographs need no
//!   space separators); a space at a break point is dropped instead of being
//!   emitted. With `wrap: false` the text runs on one line per `\n` even if it
//!   overflows.
//! - **Whitespace**: spaces and other whitespace contribute their advance to
//!   the line width but produce no placed glyph (nothing to draw). Control
//!   characters are skipped entirely.
//! - **Line height**: defaults to the font's `ascent − descent` at the given
//!   pixel height; can be overridden via [`LayoutOptions::line_height`].
//! - **Kerning**: not applied — CJK fonts define no kerning pairs, which is
//!   the target use case. `ab_glyph` exposes `kern()` if Latin pair kerning is
//!   ever needed.

use crate::atlas::GlyphAtlas;

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
    /// Total advance width of the line, in px (includes whitespace advances).
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
            let slot = atlas.rasterize_char(ch);
            if ch.is_whitespace() {
                // Spaces advance the pen but draw nothing.
                width += slot.advance;
                continue;
            }
            if options.wrap && !placed.is_empty() && width + slot.advance > max_width {
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
            let ink_h = slot.h as f32;
            placed.push(PlacedGlyph {
                ch,
                x: width,
                y: y + (line_height - ink_h) / 2.0,
                uv: (slot.u, slot.v),
                size: (slot.w, slot.h),
                advance: slot.advance,
            });
            width += slot.advance;
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
