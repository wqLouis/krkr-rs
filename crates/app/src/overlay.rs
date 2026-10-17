//! Debug overlay: frame rate and resident memory drawn as a render-only node.
//!
//! The overlay is **not** part of the engine [`Scene`](engine::scene::Scene) and
//! owns no `Bitmap` a script can reach. It builds a small RGBA panel in memory,
//! uploads it as a texture and appends one [`RenderNode`] to the projected
//! [`RenderScene`]. Nothing above the render pipeline can observe it, so the
//! overlay cannot change what a script sees.
//!
//! The panel refreshes a few times a second through [`Overlay::tick`], never on
//! every frame: it increments a frame counter, and once the refresh interval
//! has elapsed it averages the frames over that interval into the displayed
//! rate and re-uploads the panel.

use api::error::RenderError;
use api::geometry::{PixelRegion, RectF};
use render::{
    RenderContent, RenderNode, RenderPipeline, RenderState, Texture, TextureDesc, create_texture,
    upload_texture,
};

/// Panel width in pixels.
///
/// It is wide enough for the longest line [`format_lines`] can produce: a
/// 20-digit `u64` byte count reduced to mebibytes is 14 digits, plus the
/// `RSS ` prefix and `M` suffix, well under the panel's character capacity.
const PANEL_WIDTH: u32 = 256;

/// Panel height in pixels: two text lines plus padding.
const PANEL_HEIGHT: u32 = 40;

/// Integer scale applied to every font pixel.
const CELL_SCALE: i32 = 2;

/// Font glyph width in pixels.
const GLYPH_WIDTH: i32 = 5;

/// Font glyph height in pixels.
const GLYPH_HEIGHT: i32 = 7;

/// Vertical gap between lines, in font pixels (scaled by [`CELL_SCALE`]).
const LINE_GAP: i32 = 2;

/// Left edge of the text inside the panel.
const TEXT_LEFT: i32 = 4;

/// Top edge of the first line inside the panel.
const TEXT_TOP: i32 = 4;

/// Translucent black panel background.
const PANEL_COLOR: [u8; 4] = [0, 0, 0, 176];

/// Opaque white text.
const TEXT_COLOR: [u8; 4] = [255, 255, 255, 255];

/// Milliseconds between panel refreshes (four times a second).
const REFRESH_INTERVAL_MS: f64 = 250.0;

/// Sibling sort key that keeps the overlay above every projected layer.
const OVERLAY_Z: i32 = i32::MAX;

/// Errors produced while building or refreshing the debug overlay.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub(crate) enum OverlayError {
    /// The panel text contains a character the built-in font does not cover.
    #[error("the debug overlay font has no glyph for {0:?}")]
    MissingGlyph(char),
    /// An L1 render action (texture creation or upload) failed.
    #[error(transparent)]
    Render(#[from] RenderError),
}

/// The debug overlay's texture and refresh bookkeeping.
pub(crate) struct Overlay {
    /// Uploaded RGBA panel.
    texture: Texture,
    /// Frames stepped since the last panel refresh.
    frames: u32,
    /// Host elapsed time of the last refresh, or `None` before the first one.
    last_refresh_ms: Option<f64>,
}

impl Overlay {
    /// Creates the overlay's panel texture.
    ///
    /// The pixels are uploaded by the first [`Overlay::tick`], so construction
    /// only allocates the backend resource.
    ///
    /// # Errors
    ///
    /// Returns [`OverlayError::Render`] when the texture cannot be created.
    pub(crate) fn new(pipeline: &RenderPipeline) -> Result<Self, OverlayError> {
        let texture = create_texture(pipeline, TextureDesc::new(PANEL_WIDTH, PANEL_HEIGHT))?;
        Ok(Self {
            texture,
            frames: 0,
            last_refresh_ms: None,
        })
    }

    /// Advances the overlay by one frame and re-uploads the panel when the
    /// refresh interval has elapsed.
    ///
    /// `now_ms` is the host's elapsed time; `rss` is the resident set size in
    /// bytes, or `None` when the host does not expose one (the memory line is
    /// then omitted).
    ///
    /// # Errors
    ///
    /// Returns [`OverlayError::MissingGlyph`] for a character the font does not
    /// cover, and [`OverlayError::Render`] when the upload fails.
    pub(crate) fn tick(
        &mut self,
        pipeline: &RenderPipeline,
        state: &mut RenderState,
        now_ms: f64,
        rss: Option<u64>,
    ) -> Result<(), OverlayError> {
        self.frames = self.frames.saturating_add(1);
        let due = match self.last_refresh_ms {
            Some(last) => now_ms - last >= REFRESH_INTERVAL_MS,
            None => true,
        };
        if !due {
            return Ok(());
        }

        let elapsed_ms = match self.last_refresh_ms {
            Some(last) => now_ms - last,
            None => now_ms,
        };
        let fps = if elapsed_ms > 0.0 {
            f64::from(self.frames) * 1000.0 / elapsed_ms
        } else {
            0.0
        };

        let mut panel = Panel::filled(PANEL_COLOR);
        let mut y = TEXT_TOP;
        for line in format_lines(fps, rss) {
            draw_text(&mut panel, TEXT_LEFT, y, &line)?;
            y += (GLYPH_HEIGHT + LINE_GAP) * CELL_SCALE;
        }
        upload_texture(
            pipeline,
            state,
            &self.texture,
            PixelRegion::new(0, 0, PANEL_WIDTH, PANEL_HEIGHT),
            panel.rgba(),
        )?;

        self.frames = 0;
        self.last_refresh_ms = Some(now_ms);
        Ok(())
    }

    /// Builds the overlay's render node.
    ///
    /// The node is positioned at the top-left of the framebuffer at the
    /// panel's own pixel size. The caller sets the node's parent and adds it
    /// to the scene.
    pub(crate) fn node(&self) -> RenderNode {
        RenderNode {
            content: RenderContent::Texture(self.texture.clone()),
            dst: RectF::new(0.0, 0.0, PANEL_WIDTH as f32, PANEL_HEIGHT as f32),
            z: OVERLAY_Z,
            ..RenderNode::default()
        }
    }
}

/// One RGBA panel being composed.
struct Panel {
    /// Tightly packed row-major RGBA8 pixels.
    rgba: Vec<u8>,
}

impl Panel {
    /// Creates a `PANEL_WIDTH`x`PANEL_HEIGHT` panel filled with `color`.
    fn filled(color: [u8; 4]) -> Self {
        let mut rgba = vec![0u8; PANEL_WIDTH as usize * PANEL_HEIGHT as usize * 4];
        for texel in rgba.chunks_exact_mut(4) {
            texel.copy_from_slice(&color);
        }
        Self { rgba }
    }

    /// Writes `color` at `(x, y)` when the point is inside the panel.
    fn put(&mut self, x: i32, y: i32, color: [u8; 4]) {
        if x < 0 || y < 0 || x >= PANEL_WIDTH as i32 || y >= PANEL_HEIGHT as i32 {
            return;
        }
        let offset = (y as usize * PANEL_WIDTH as usize + x as usize) * 4;
        self.rgba[offset..offset + 4].copy_from_slice(&color);
    }

    /// The panel's tightly packed pixels.
    fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// Formats the overlay's lines for `fps` and an optional `rss` byte count.
///
/// The memory line shows whole mebibytes; it is omitted when `rss` is `None`
/// rather than displaying a fabricated value.
fn format_lines(fps: f64, rss: Option<u64>) -> Vec<String> {
    let mut lines = vec![format!("FPS {fps:.1}")];
    if let Some(bytes) = rss {
        lines.push(format!("RSS {}M", bytes / (1024 * 1024)));
    }
    lines
}

/// Composites `text` at `(x, y)` into `panel` at [`CELL_SCALE`].
///
/// # Errors
///
/// Returns [`OverlayError::MissingGlyph`] when `text` contains a character the
/// built-in font does not cover.
fn draw_text(panel: &mut Panel, x: i32, y: i32, text: &str) -> Result<(), OverlayError> {
    let mut pen_x = x;
    for ch in text.chars() {
        let glyph = glyph_for(ch).ok_or(OverlayError::MissingGlyph(ch))?;
        draw_glyph(panel, pen_x, y, glyph);
        pen_x += (GLYPH_WIDTH + 1) * CELL_SCALE;
    }
    Ok(())
}

/// Composites one glyph at `(x, y)`, scaled by [`CELL_SCALE`].
fn draw_glyph(panel: &mut Panel, x: i32, y: i32, glyph: &Glyph) {
    for (row, bits) in glyph.rows.iter().enumerate() {
        for (column, &cell) in bits.as_bytes().iter().enumerate() {
            if cell != b'#' {
                continue;
            }
            for dy in 0..CELL_SCALE {
                for dx in 0..CELL_SCALE {
                    panel.put(
                        x + column as i32 * CELL_SCALE + dx,
                        y + row as i32 * CELL_SCALE + dy,
                        TEXT_COLOR,
                    );
                }
            }
        }
    }
}

/// Returns the glyph for `ch`, or `None` when the font does not cover it.
fn glyph_for(ch: char) -> Option<&'static Glyph> {
    FONT.iter().find(|glyph| glyph.ch == ch)
}

/// One 5x7 glyph drawn with `#` for ink and any other byte for background.
struct Glyph {
    /// The character this glyph represents.
    ch: char,
    /// Seven rows of five columns, top to bottom.
    rows: [&'static str; 7],
}

/// The built-in font.
///
/// It covers exactly the characters [`format_lines`] can emit: the digits, the
/// decimal point, the space, and the letters in `FPS` and `RSS`.
const FONT: &[Glyph] = &[
    Glyph {
        ch: ' ',
        rows: [
            "     ", "     ", "     ", "     ", "     ", "     ", "     ",
        ],
    },
    Glyph {
        ch: '0',
        rows: [
            ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '1',
        rows: [
            "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ],
    },
    Glyph {
        ch: '2',
        rows: [
            ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
        ],
    },
    Glyph {
        ch: '3',
        rows: [
            "#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '4',
        rows: [
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ],
    },
    Glyph {
        ch: '5',
        rows: [
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '6',
        rows: [
            "..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '7',
        rows: [
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ],
    },
    Glyph {
        ch: '8',
        rows: [
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ],
    },
    Glyph {
        ch: '9',
        rows: [
            ".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##..",
        ],
    },
    Glyph {
        ch: '.',
        rows: [
            "     ", "     ", "     ", "     ", "     ", ".##..", ".##..",
        ],
    },
    Glyph {
        ch: 'F',
        rows: [
            "#####", "#....", "#....", "####.", "#....", "#....", "#....",
        ],
    },
    Glyph {
        ch: 'P',
        rows: [
            "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
        ],
    },
    Glyph {
        ch: 'S',
        rows: [
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ],
    },
    Glyph {
        ch: 'R',
        rows: [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
    },
    Glyph {
        ch: 'M',
        rows: [
            "#...#", "##.##", "#.#.#", "#...#", "#...#", "#...#", "#...#",
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_fps_and_memory_lines() {
        assert_eq!(format_lines(60.0, None), vec!["FPS 60.0"]);
        assert_eq!(
            format_lines(59.5, Some(256 * 1024 * 1024)),
            vec!["FPS 59.5", "RSS 256M"]
        );
        // Whole mebibytes truncate rather than round.
        assert_eq!(
            format_lines(1.0, Some(1024 * 1024 + 500)),
            vec!["FPS 1.0", "RSS 1M"]
        );
    }

    /// Every character the formatter can emit has a glyph, so the panel never
    /// hits the missing-glyph error in normal operation.
    #[test]
    fn the_font_covers_the_formatter_alphabet() {
        for line in format_lines(12345.6, Some(u64::MAX)) {
            for ch in line.chars() {
                assert!(glyph_for(ch).is_some(), "no glyph for {ch:?} in {line:?}");
            }
        }
    }

    #[test]
    fn unknown_glyph_is_an_error() {
        let mut panel = Panel::filled([0, 0, 0, 0]);
        assert!(matches!(
            draw_text(&mut panel, 0, 0, "@"),
            Err(OverlayError::MissingGlyph('@'))
        ));
    }

    #[test]
    fn drawing_ink_marks_pixels_inside_the_panel() {
        let mut panel = Panel::filled([0, 0, 0, 0]);
        draw_text(&mut panel, 4, 4, "1").expect("draw");
        let inked = panel
            .rgba()
            .chunks_exact(4)
            .filter(|pixel| pixel.iter().any(|&byte| byte != 0))
            .count();
        assert!(inked > 0, "the glyph drew no ink");
        // The top-left panel corner is outside the drawn glyph.
        assert_eq!(&panel.rgba()[0..4], &[0, 0, 0, 0]);
    }
}
