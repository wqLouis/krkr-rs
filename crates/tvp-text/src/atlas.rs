//! Glyph atlas: rasterizes glyphs for a face at a fixed pixel height into a
//! packed RGBA grid.
//!
//! # Packing strategy
//!
//! A fixed-cell grid: every glyph occupies a square cell whose side is derived
//! from the font's vertical metrics (`ascent − descent` at the requested pixel
//! height) plus a small padding. Cells are laid out left-to-right, top-to-
//! bottom; the atlas grows **only by appending rows** (never by widening), so
//! previously returned `(u, v, w, h)` slots stay valid forever — there is no
//! re-packing and no UV invalidation.
//!
//! # Glyph data
//!
//! Each glyph is stored as straight white RGBA (`255, 255, 255, alpha`) with
//! coverage-derived alpha — the standard representation for tinted text:
//! downstream layers multiply by the text color. Kerning is not applied (CJK
//! fonts set no kerning pairs; this is a horizontal text layout), and glyphs
//! are cached in a plain growing `HashMap` keyed by `char` — no eviction, since
//! KAG text sets are small and bounded in practice.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use ab_glyph::{Font, PxScale, ScaleFont};

use crate::font::{FontFace, glyph_id_with_fallback, px_scale_for_height, round_advance};

/// Padding, in pixels, kept around every glyph inside its cell. Absorbs the
/// +1 px that integerized bounds can exceed the font's declared metrics by,
/// and keeps antialiased edge pixels inside the cell.
const PAD: u32 = 2;

/// A single rasterized glyph: the position and size of its ink quad inside the
/// atlas RGBA buffer, plus the horizontal advance and baseline bearing used by
/// layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphSlot {
    /// Atlas x of the ink quad's top-left corner, in pixels.
    pub u: u32,
    /// Atlas y of the ink quad's top-left corner, in pixels.
    pub v: u32,
    /// Ink quad width in pixels (0 for whitespace / zero-ink glyphs).
    pub w: u32,
    /// Ink quad height in pixels (0 for whitespace / zero-ink glyphs).
    pub h: u32,
    /// Horizontal advance in pixels at this atlas's pixel height.
    pub advance: f32,
    /// Distance from the baseline up to the top of the ink quad, in pixels
    /// (FreeType `bitmap_top`). 0 for whitespace / zero-ink glyphs. Layout
    /// places the ink top at `line_top + ascent - bearing_y`, matching the
    /// reference `drect.top = y + baseline - bitmap_top`
    /// (`LayerBitmapImpl.cpp:913`, `FreeType.cpp:499`).
    pub bearing_y: i32,
}

/// Rasterized glyph cache for one face at one pixel height.
pub struct GlyphAtlas {
    font: Arc<FontFace>,
    /// Requested pixel height (KAG font height, i.e. the em size in px).
    pixel_height: u32,
    /// Whether glyphs are synthetically emboldened (KAG `Font.bold`).
    bold: bool,
    /// The `ab_glyph` scale corresponding to `pixel_height` (see
    /// [`px_scale_for_height`] for why this is not `pixel_height` itself).
    scale: PxScale,
    /// Side length of one grid cell, in pixels.
    cell: u32,
    /// Number of cells per row.
    cols: u32,
    /// Number of rows currently allocated.
    rows: u32,
    /// Atlas width in pixels (fixed at construction).
    width: u32,
    /// Atlas height in pixels (grows as rows are appended).
    height: u32,
    /// RGBA buffer, `width * height * 4` bytes.
    rgba: Vec<u8>,
    /// Glyph slots by character.
    slots: HashMap<char, GlyphSlot>,
    /// Index of the next free cell.
    next_cell: u32,
}

impl GlyphAtlas {
    /// Create an atlas for `font` at `pixel_height`, with a default width of
    /// 512 px.
    pub fn with_default_width(font: impl Into<Arc<FontFace>>, pixel_height: u32) -> Self {
        Self::new(font, pixel_height, 512)
    }

    /// Create a bold atlas for `font` at `pixel_height`, with a default width
    /// of 512 px (see [`GlyphAtlas::new_bold`]).
    pub fn with_default_width_bold(
        font: impl Into<Arc<FontFace>>,
        pixel_height: u32,
        bold: bool,
    ) -> Self {
        Self::new_bold(font, pixel_height, 512, bold)
    }

    /// Create an atlas for `font` at `pixel_height`.
    ///
    /// `atlas_width` is the fixed atlas width in pixels; the height grows on
    /// demand. `pixel_height` must be at least 1.
    pub fn new(font: impl Into<Arc<FontFace>>, pixel_height: u32, atlas_width: u32) -> Self {
        Self::new_bold(font, pixel_height, atlas_width, false)
    }

    /// Create an atlas with optional synthetic emboldening.
    ///
    /// `bold` applies a 1 px horizontal coverage dilation at rasterization
    /// time (ab_glyph has no `FT_GlyphSlot_Embolden` equivalent), and widens
    /// the advance by the same amount so emboldened glyphs do not collide.
    pub fn new_bold(
        font: impl Into<Arc<FontFace>>,
        pixel_height: u32,
        atlas_width: u32,
        bold: bool,
    ) -> Self {
        let font = font.into();
        assert!(
            pixel_height >= 1,
            "pixel_height must be >= 1, got {pixel_height}"
        );
        let scale = px_scale_for_height(font.font(), pixel_height as f32);
        let scaled = font.font().as_scaled(scale);
        // ascent − descent at this scale = the full vertical extent; the ink
        // of any glyph fits inside it by definition of the font's metrics.
        let line_height = scaled.ascent() - scaled.descent();
        // One extra padding column when bold: the 1 px dilation must stay
        // inside the cell (PAD already covers the right edge, but keeping the
        // reasoning explicit here documents why bold never clips).
        let bold_pad = u32::from(bold);
        let cell = (line_height.ceil() as u32).max(pixel_height) + 2 * PAD + bold_pad;
        let width = atlas_width.max(cell);
        let cols = (width / cell).max(1);
        let height = cell;
        let rgba = vec![0; width as usize * height as usize * 4];
        Self {
            font,
            pixel_height,
            bold,
            scale,
            cell,
            cols,
            rows: 1,
            width,
            height,
            rgba,
            slots: HashMap::new(),
            next_cell: 0,
        }
    }

    /// Rasterize `c` (once) and return its slot.
    ///
    /// Missing glyphs fall back to the font's U+FFFD replacement glyph, or to
    /// the classic `.notdef` glyph (glyph 0) when the font has no replacement.
    /// Control characters produce a zero slot (no ink, zero advance) without
    /// touching the atlas.
    pub fn rasterize_char(&mut self, c: char) -> GlyphSlot {
        if c.is_control() {
            return GlyphSlot {
                u: 0,
                v: 0,
                w: 0,
                h: 0,
                advance: 0.0,
                bearing_y: 0,
            };
        }
        if let Some(&slot) = self.slots.get(&c) {
            return slot;
        }
        let slot = self.rasterize_new(c);
        self.slots.insert(c, slot);
        slot
    }

    fn rasterize_new(&mut self, c: char) -> GlyphSlot {
        let scaled = self.font.font().as_scaled(self.scale);
        let id = glyph_id_with_fallback(c, &scaled);
        // The reference rounds the draw advance to a whole pixel too: it copies
        // `FTFace->glyph->advance.x` and then `FT_PosToInt`s it
        // (`FreeType.cpp:488`, `:626`).
        let mut advance = round_advance(scaled.h_advance(id)) as f32;
        let glyph = id.with_scale_and_position(self.scale, ab_glyph::point(0.0, 0.0));
        let (u, v, w, h, bearing_y) = match scaled.outline_glyph(glyph) {
            Some(outline) => {
                let bounds = outline.px_bounds();
                let (mut w, h) = (bounds.width() as u32, bounds.height() as u32);
                // `px_bounds` is relative to the pen at the baseline (y-down),
                // so the ink top is at `min.y` (negative above the baseline).
                // FreeType's `bitmap_top` is the positive distance the other
                // way; this is what the reference stores as the glyph's
                // vertical bearing.
                let bearing_y = (-bounds.min.y).round() as i32;
                if w == 0 || h == 0 {
                    // No ink (e.g. space): still cache the advance and bearing.
                    (0, 0, 0, 0, bearing_y)
                } else {
                    // Rasterize into a scratch cell so bold can dilate the
                    // coverage before it lands in the packed atlas.
                    let cell = self.cell as usize;
                    let mut cov = vec![0u8; cell * cell];
                    let base = PAD as usize;
                    outline.draw(|dx, dy, coverage| {
                        let x = base + dx as usize;
                        let y = base + dy as usize;
                        if x < cell && y < cell {
                            let alpha = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
                            let slot = &mut cov[y * cell + x];
                            *slot = (*slot).max(alpha);
                        }
                    });
                    if self.bold {
                        embolden_coverage(&mut cov, cell);
                        // The right edge grew by one pixel.
                        w += 1;
                        advance += 1.0;
                    }
                    let index = self.alloc_cell();
                    let (col, row) = (index % self.cols, index / self.cols);
                    let base_x = col * self.cell + PAD;
                    let base_y = row * self.cell + PAD;
                    for yy in 0..cell {
                        for xx in 0..cell {
                            let alpha = cov[yy * cell + xx];
                            if alpha == 0 {
                                continue;
                            }
                            let ax = base_x + xx as u32;
                            let ay = base_y + yy as u32;
                            if ax >= self.width || ay >= self.height {
                                continue;
                            }
                            let i = ((ay * self.width + ax) * 4) as usize;
                            self.rgba[i..i + 4].copy_from_slice(&[255, 255, 255, alpha]);
                        }
                    }
                    (base_x, base_y, w, h, bearing_y)
                }
            }
            None => (0, 0, 0, 0, 0),
        };
        GlyphSlot {
            u,
            v,
            w,
            h,
            advance,
            bearing_y,
        }
    }

    /// Allocate the next free cell, growing the atlas by one row if needed.
    fn alloc_cell(&mut self) -> u32 {
        let index = self.next_cell;
        self.next_cell += 1;
        let needed_rows = index / self.cols + 1;
        while self.rows < needed_rows {
            let row_len = self.width as usize * self.cell as usize * 4;
            self.rgba.resize(self.rgba.len() + row_len, 0);
            self.rows += 1;
            self.height += self.cell;
        }
        index
    }

    /// The RGBA pixel buffer (`width * height * 4` bytes).
    pub fn atlas_rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// The atlas size `(width, height)` in pixels.
    pub fn atlas_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The pixel height this atlas was rasterized at.
    pub fn pixel_height(&self) -> u32 {
        self.pixel_height
    }

    /// The face this atlas rasterizes.
    pub fn font(&self) -> &FontFace {
        &self.font
    }

    /// Line height (`ascent − descent`) in pixels at this atlas's pixel
    /// height.
    pub fn line_height(&self) -> f32 {
        let scaled = self.font.font().as_scaled(self.scale);
        scaled.ascent() - scaled.descent()
    }

    /// Ascent (`baseline` above the top of the em box) in pixels at this
    /// atlas's pixel height. Used for underline/strikeout placement.
    pub fn ascent(&self) -> f32 {
        self.font.font().as_scaled(self.scale).ascent()
    }

    /// Whether glyphs in this atlas are synthetically emboldened.
    pub fn bold(&self) -> bool {
        self.bold
    }

    /// The cached slot for `c`, if it has been rasterized.
    pub fn slot(&self, c: char) -> Option<GlyphSlot> {
        self.slots.get(&c).copied()
    }

    /// Number of distinct glyphs currently rasterized.
    pub fn glyph_count(&self) -> usize {
        self.slots.len()
    }
}

/// Horizontally dilate a rasterized glyph's coverage by one pixel to the
/// right, producing a synthetic bold. `cov` is a `cell × cell` buffer; the
/// caller reserves an extra padding column for the growth.
fn embolden_coverage(cov: &mut [u8], cell: usize) {
    for y in 0..cell {
        let row = y * cell;
        for x in (1..cell).rev() {
            let prev = cov[row + x - 1];
            if prev > cov[row + x] {
                cov[row + x] = prev;
            }
        }
    }
}

/// Cache key: `(face id, pixel height, bold)`. Keying by height means multiple
/// concurrent text sizes (e.g. 12 px and 24 px) each keep their own atlas and
/// never thrash one another; keying by `bold` keeps the emboldened raster
/// separate from the regular one.
type AtlasKey = (u64, u32, bool);

/// Process-global glyph atlases, shared across `drawText` calls so a glyph is
/// rasterized once per `(face, height)` for the lifetime of the process.
///
/// Each atlas sits behind its own `Mutex` so that building/locking an atlas
/// for one height does not block unrelated heights; the short outer lock only
/// looks up the `Arc`.
static ATLAS_CACHE: LazyLock<Mutex<HashMap<AtlasKey, Arc<Mutex<GlyphAtlas>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Run `f` with the process-wide cached atlas for `(face, pixel_height)`,
/// creating and caching it on first use.
///
/// The atlas grows across calls (rows are only appended and existing glyph
/// slots stay valid; see the module docs), so repeated `drawText` calls reuse
/// already-rasterized glyphs instead of re-parsing the font and re-rasterizing
/// every character. Callers must keep the scene lock out of `f` if `f` can
/// rasterize new glyphs.
pub fn with_cached_atlas<R>(
    face: Arc<FontFace>,
    pixel_height: u32,
    f: impl FnOnce(&mut GlyphAtlas) -> R,
) -> R {
    with_cached_atlas_styled(face, pixel_height, false, f)
}

/// Like [`with_cached_atlas`], but selects the synthetic-bold atlas when
/// `bold` is set. Bold and regular atlases are cached independently.
pub fn with_cached_atlas_styled<R>(
    face: Arc<FontFace>,
    pixel_height: u32,
    bold: bool,
    f: impl FnOnce(&mut GlyphAtlas) -> R,
) -> R {
    let key = (face.id(), pixel_height, bold);
    let atlas = {
        let mut cache = ATLAS_CACHE
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        cache
            .entry(key)
            .or_insert_with(|| {
                Arc::new(Mutex::new(GlyphAtlas::with_default_width_bold(
                    face,
                    pixel_height,
                    bold,
                )))
            })
            .clone()
    };
    let mut guard = atlas.lock().unwrap_or_else(|poison| poison.into_inner());
    f(&mut guard)
}
