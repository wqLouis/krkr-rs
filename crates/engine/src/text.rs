//! L2 text layout, glyph-atlas composition and GPU projection.
//!
//! The module delegates the hard parts:
//!
//! * **Layout/shaping/bidi/line-breaking/font-fallback** go to
//!   [`cosmic_text`], a pure-Rust text stack (harfrust shaping, `fontdb`
//!   discovery/fallback, `unicode-bidi`, `unicode-linebreak`).
//! * **Rasterization** is the L1 [`render`] `FontSystem` /
//!   [`render::rasterize_glyph`] / [`render::glyph_advance`] /
//!   [`render::font_metrics`] contract. L1 owns the font and the coverage
//!   bitmap; L2 owns the *atlas* that packs those bitmaps.
//! * **GPU projection** is the L1 quad pipeline: [`project_text`] turns
//!   positioned glyphs into per-glyph [`RenderContent::Texture`] quads against
//!   an L2-owned [`GlyphAtlas`].
//!
//! Script-visible text is drawn by compositing per-character coverage into the
//! layer's [`BitmapState`] ([`rasterize_text_coverage`] +
//! [`compose_text_into`]), so `getPixel` and every other readback observe the
//! same pixels the reference produces. The [`GlyphAtlas`] path rasterizes the
//! same coverage once, packs it into a texture, and [`project_text`] emits
//! textured quads for the L1 pipeline; it needs a live [`RenderPipeline`].
//!
//! ## State
//!
//! Everything here is explicit caller-owned state: [`TextEngine`] holds the
//! shaping system and the registered faces, [`GlyphAtlas`] holds the atlas
//! texture, its CPU mirror and the glyph cache, and [`TextCoverageCache`]
//! holds the rasterized character masks and their blurred shadows. There are
//! no globals, no statics and no hidden caches.

use std::path::PathBuf;
use std::sync::Arc;

use api::geometry::{PixelRegion, RectF};
use api::glyph::{GlyphBackend, GlyphBitmap, GlyphStyle};
use render::{
    Font, FontMetrics, FontSystem, RenderContent, RenderNode, RenderNodeId, RenderPipeline,
    RenderScene, RenderState, Texture, TextureDesc, TextureFormat,
};

use crate::BitmapState;
use crate::hasher::{Map, map};
use crate::layer_ops::{RectI, intersect_rect};

use cosmic_text::fontdb;
use cosmic_text::{Attrs, Buffer, Family, FontSystem as CosmicFontSystem, Metrics, Shaping};

/// Maximum number of entries held by each L2 text memo.
///
/// It bounds the rasterized character masks ([`TextCoverageCache`]), the
/// shaped-layout memo, the per-character advance memo and the
/// missing-family memo ([`TextEngine`]) and the glyph atlas' entry map
/// ([`GlyphAtlas`]).
pub const MAX_TEXT_CACHE_ENTRIES: usize = 1300;

/// Japanese/Chinese/Korean-capable fallback families, tried in order when a
/// requested family is not installed on the host.
///
/// The families are reached by name when the host's system fonts carry them.
const CJK_FALLBACK_FAMILIES: &[&str] = &[
    "Noto Sans CJK JP",
    "Noto Sans CJK SC",
    "Noto Sans CJK TC",
    "Noto Sans CJK KR",
    "Droid Sans Fallback",
];

/// Well-known Noto CJK collection paths, tried when the family query misses.
const KNOWN_JP_PATHS: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/local/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Errors produced by text layout, atlas composition and projection.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TextError {
    /// A [`FontId`] was used that was not registered on this engine.
    #[error("font {0} is not registered")]
    UnknownFont(usize),
    /// The requested pixel size is zero, negative or not finite.
    #[error("invalid text size {0}")]
    InvalidSize(f32),
    /// The font data is not a usable face at the requested collection index.
    #[error("invalid font data: {message}")]
    InvalidFont {
        /// Human-readable description of the failure.
        message: String,
    },
    /// No installed system font provides the requested family and no
    /// Japanese-capable CJK fallback is available either.
    #[error("no system font provides family '{family}'")]
    FontNotFound {
        /// Requested family name.
        family: String,
    },
    /// The glyph atlas has no free cell for the requested bitmap.
    ///
    /// This is never silently worked around: the caller must grow or recreate
    /// the atlas.
    #[error("glyph atlas ({width}x{height}) has no room for the glyph")]
    AtlasFull {
        /// Atlas width in pixels.
        width: u32,
        /// Atlas height in pixels.
        height: u32,
    },
    /// A coverage mask's byte count does not match its dimensions.
    #[error("coverage mask {width}x{height} has {len} bytes")]
    InvalidCoverage {
        /// Declared mask width.
        width: u32,
        /// Declared mask height.
        height: u32,
        /// Number of coverage bytes supplied.
        len: usize,
    },
    /// A coverage convolution would produce a mask too large to allocate.
    #[error("blurring coverage by {blur_width} pixels overflows the mask size")]
    MaskTooLarge {
        /// Requested blur width in pixels.
        blur_width: i32,
    },
    /// The L1 glyph rasterizer failed.
    #[error(transparent)]
    Glyph(#[from] api::error::GlyphError),
    /// An L1 render action (texture creation/upload) failed.
    #[error(transparent)]
    Render(#[from] api::error::RenderError),
}

/// A registered font face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct FontId(usize);

impl FontId {
    /// Returns the registration index of this face.
    pub const fn index(self) -> usize {
        self.0
    }
}

/// A registered font face: the L1 rasterizer face plus the shaping id and
/// family name `cosmic-text` uses to select it.
struct Face {
    /// L1 parsed face used for rasterization.
    render: Font,
    /// Family name used in [`Attrs`] so shaping picks this face.
    family: String,
}

/// Style of a run of text.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct TextStyle {
    /// Em size in pixels.
    pub size_px: f32,
    /// Synthetic bold (applied by the L1 rasterizer).
    pub bold: bool,
    /// Synthetic italic/oblique (applied by the L1 rasterizer).
    pub italic: bool,
    /// Straight-alpha RGBA text colour.
    ///
    /// This is **not** part of the glyph-atlas cache key: the atlas stores
    /// colour-independent coverage and the colour is applied as a per-node
    /// tint, so recolouring reuses cached glyphs.
    pub color: [u8; 4],
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size_px: 16.0,
            bold: false,
            italic: false,
            color: [0, 0, 0, 255],
        }
    }
}

impl TextStyle {
    /// Creates a style at `size_px` with default colour and no synthesis.
    pub const fn new(size_px: f32) -> Self {
        Self {
            size_px,
            ..Self::DEFAULT
        }
    }

    const DEFAULT: Self = Self {
        size_px: 16.0,
        bold: false,
        italic: false,
        color: [0, 0, 0, 255],
    };

    /// Returns the L1 rasterizer style for this text style.
    const fn glyph_style(self) -> GlyphStyle {
        GlyphStyle {
            bold: self.bold,
            italic: self.italic,
        }
    }
}

/// One glyph after shaping and layout, in the text's local coordinate space.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct LaidOutGlyph {
    /// Unicode code point of the glyph's cluster.
    pub codepoint: char,
    /// Face the glyph was shaped with.
    pub font: FontId,
    /// Zero-based visual line index.
    pub line: usize,
    /// Pen x offset from the text origin, in pixels.
    pub x: f32,
    /// Baseline y offset from the text origin, in pixels (y grows down).
    ///
    /// A line's baseline is the face's ascent in whole pixels, so a glyph's
    /// ink starts `bearing_y` pixels above it.
    pub baseline_y: f32,
    /// Shaped advance of the glyph, in pixels.
    pub advance: f32,
}

/// The result of laying out one string.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct TextLayout {
    /// Positioned glyphs in visual order.
    pub glyphs: Vec<LaidOutGlyph>,
    /// Maximum line width in pixels.
    pub width: f32,
    /// Total height in pixels (`line_height * line_count`).
    pub height: f32,
    /// Baseline-to-baseline line height used for the layout.
    pub line_height: f32,
}

impl TextLayout {
    /// Returns `true` if the layout contains no glyphs.
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    /// Returns the number of positioned glyphs.
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }
}

/// An 8-bit single-channel coverage mask with the position of its top-left
/// corner in a text's local coordinate space.
///
/// `coverage` holds exactly `width * height` row-major bytes, where `0` is
/// fully transparent and `255` fully covered. The constructor enforces that
/// invariant, so a mask is always internally consistent. The coverage buffer is
/// reference-counted so placing the same mask at a different pen position is a
/// cheap handle copy, not a pixel copy.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CoverageMask {
    x: f32,
    y: f32,
    width: u32,
    height: u32,
    coverage: Arc<Vec<u8>>,
}

impl CoverageMask {
    /// Creates a coverage mask from a positioned buffer.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::InvalidCoverage`] when `coverage.len()` is not
    /// exactly `width * height`.
    pub fn new(
        x: f32,
        y: f32,
        width: u32,
        height: u32,
        coverage: Vec<u8>,
    ) -> Result<Self, TextError> {
        if coverage.len() as u64 != u64::from(width) * u64::from(height) {
            return Err(TextError::InvalidCoverage {
                width,
                height,
                len: coverage.len(),
            });
        }
        Ok(Self {
            x,
            y,
            width,
            height,
            coverage: Arc::new(coverage),
        })
    }

    /// The mask's left edge in text-local pixels.
    pub const fn x(&self) -> f32 {
        self.x
    }

    /// The mask's top edge in text-local pixels.
    pub const fn y(&self) -> f32 {
        self.y
    }

    /// The mask width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The mask height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The row-major `width * height` coverage bytes.
    pub fn coverage(&self) -> &[u8] {
        self.coverage.as_slice()
    }

    /// Returns `true` when the mask has no pixels.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Cache key for a shaped [`TextLayout`].
///
/// It covers every input that affects shaping: the face, the text, the size,
/// the synthetic style and the wrap width. The text colour is deliberately
/// absent because colour does not move glyphs, so recolouring a label reuses
/// its layout.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LayoutKey {
    /// Registered face index.
    font: FontId,
    /// Source text.
    text: String,
    /// `size_px.to_bits()` (so the key stays `Eq`).
    size_bits: u32,
    /// Synthetic bold.
    bold: bool,
    /// Synthetic italic.
    italic: bool,
    /// `wrap_width.to_bits()` when wrapping takes effect.
    wrap_bits: Option<u32>,
}

/// Cache key for one measured per-character advance.
///
/// It covers every input the rasterizer's advance depends on: the face, the
/// character, the exact pixel size and the synthetic style. The text colour
/// is absent because colour does not move a glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct AdvanceKey {
    /// Registered face index.
    font: FontId,
    /// Code point.
    codepoint: char,
    /// `size_px.to_bits()` (so the key stays `Eq`).
    size_bits: u32,
    /// Synthetic bold.
    bold: bool,
    /// Synthetic italic.
    italic: bool,
}

/// L2 text service: shaping/layout system plus registered faces.
///
/// Created once per engine and owned by it. It keeps no globals; the glyph
/// atlas and render state are separate explicit values.
#[non_exhaustive]
pub struct TextEngine {
    /// L1 stateless rasterizer service.
    render_system: FontSystem,
    /// `cosmic-text` shaping/layout system (stateless service).
    cosmic: CosmicFontSystem,
    /// Registered faces, indexed by [`FontId`].
    faces: Vec<Face>,
    /// Lowercased family name → face id, for name-based lookup.
    by_name: Map<String, FontId>,
    /// Lazily built host font database used by [`TextEngine::load_system_font`].
    /// It is kept separate from the shaping database so that only explicitly
    /// resolved faces participate in layout.
    system_fonts: Option<fontdb::Database>,
    /// System-font directories the host supplied through
    /// [`TextEngine::set_system_font_dirs`]. They are scanned, in addition to
    /// `fontdb`'s default locations, when the system database is first built.
    system_font_dirs: Vec<PathBuf>,
    /// Whether the "resolved to the engine default face" fallback has been
    /// logged. The record is emitted once per engine, not once per name.
    default_fallback_logged: bool,
    /// Memoized shaped layouts, keyed by shaping inputs only. Bounded at the
    /// reference limit with least-recently-used eviction.
    layout_cache: LruCache<LayoutKey, TextLayout>,
    /// Memoized per-character advances, keyed by face, character, size and
    /// synthetic style. Bounded at [`MAX_TEXT_CACHE_ENTRIES`] with
    /// least-recently-used eviction.
    advance_cache: LruCache<AdvanceKey, f32>,
    /// Families the host's installed fonts do not provide, memoized so that a
    /// repeated exact lookup does not scan the font database again. Bounded at
    /// [`MAX_TEXT_CACHE_ENTRIES`] with least-recently-used eviction.
    missing_system_families: LruCache<String, ()>,
}

impl TextEngine {
    /// Creates an empty text engine with no registered faces.
    ///
    /// The shaping database starts empty: only faces added with
    /// [`TextEngine::add_font`] or resolved with
    /// [`TextEngine::load_system_font`] are available, which keeps rendering
    /// deterministic and free of unrelated host font state.
    pub fn new() -> Self {
        Self {
            render_system: render::create_font_system(),
            cosmic: CosmicFontSystem::new_with_locale_and_db(
                String::new(),
                cosmic_text::fontdb::Database::new(),
            ),
            faces: Vec::new(),
            by_name: map(),
            system_fonts: None,
            system_font_dirs: Vec::new(),
            default_fallback_logged: false,
            layout_cache: LruCache::new(MAX_TEXT_CACHE_ENTRIES),
            advance_cache: LruCache::new(MAX_TEXT_CACHE_ENTRIES),
            missing_system_families: LruCache::new(MAX_TEXT_CACHE_ENTRIES),
        }
    }

    /// Selects the host's system-font directories.
    ///
    /// The directories are scanned lazily, the first time a face is resolved
    /// from the host, together with `fontdb`'s default system locations. A
    /// database built before this call is discarded so the new list takes
    /// effect; call it before any face has been resolved to avoid the rescan.
    ///
    /// The missing-family memo is dropped with the database it describes: a
    /// family the previous directories did not provide may be in the new ones.
    pub fn set_system_font_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.system_font_dirs = dirs;
        self.system_fonts = None;
        self.missing_system_families.clear();
    }

    /// Registers a font face from `bytes` (a TTF/OTF, or a TTC/OTC with
    /// `index`) and returns its id.
    ///
    /// The same bytes are loaded into both the L1 rasterizer and the
    /// `cosmic-text` shaping database so layout and rasterization agree on the
    /// face.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::Glyph`] when the L1 rasterizer rejects the data
    /// and [`TextError::InvalidFont`] when `index` is not a face in the
    /// collection.
    pub fn add_font(&mut self, bytes: &[u8], index: u32) -> Result<FontId, TextError> {
        let face = render::load_font(&self.render_system, bytes, index)?;
        let source = cosmic_text::fontdb::Source::Binary(Arc::new(bytes.to_vec()));
        let ids = self.cosmic.db_mut().load_font_source(source);
        let cosmic = *ids
            .get(index as usize)
            .ok_or_else(|| TextError::InvalidFont {
                message: format!("font collection has no face at index {index}"),
            })?;
        let family = self
            .cosmic
            .db()
            .face(cosmic)
            .and_then(|info| info.families.first())
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        let id = FontId(self.faces.len());
        if !family.is_empty() {
            self.by_name.entry(family.to_lowercase()).or_insert(id);
        }
        self.faces.push(Face {
            render: face,
            family,
        });
        Ok(id)
    }

    /// Looks up a face registered under `family` (case-insensitive).
    pub fn font_id(&self, family: &str) -> Option<FontId> {
        self.by_name.get(&family.to_lowercase()).copied()
    }

    /// Resolves `family` against the host's installed fonts, registers the
    /// matched face and returns its id.
    ///
    /// The family is matched case-insensitively, including the localized
    /// family names in the font's name table. When nothing matches, the
    /// Japanese/CJK fallback families are tried (`Noto Sans CJK JP` and the
    /// other `Noto Sans CJK` regions, then `Droid Sans Fallback`), then the
    /// well-known Noto CJK collection paths. Only the resolved face is
    /// loaded into the shaping database, so layout never picks up an
    /// unrelated system font.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::FontNotFound`] when neither `family` nor the CJK
    /// fallback is installed, and propagates [`TextError::Glyph`] /
    /// [`TextError::InvalidFont`] when the matched data cannot be registered.
    pub fn load_system_font(&mut self, family: &str) -> Result<FontId, TextError> {
        if let Some(id) = self.font_id(family) {
            return Ok(id);
        }
        let (bytes, index) = self.system_face_bytes(family, true)?;
        self.finish_system_face(family, &bytes, index)
    }

    /// Resolves `family` against the host's installed fonts without the CJK
    /// fallback, registers the matched face and returns its id.
    ///
    /// This is the exact-family query a comma-separated candidate list needs:
    /// a candidate that is not installed must not silently resolve to the
    /// Japanese fallback before the next candidate is tried.
    ///
    /// A family the host does not provide is memoized, so a repeated query
    /// reports the same [`TextError::FontNotFound`] without scanning the host
    /// font database again. The memo holds misses only: a family the host does
    /// provide resolves on the first query, and a face registered afterwards
    /// is found by [`TextEngine::font_id`] before the memo is consulted. The
    /// memo is dropped when [`TextEngine::set_system_font_dirs`] rebuilds the
    /// font database.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::FontNotFound`] when no installed face carries
    /// `family`, and propagates the registration errors of
    /// [`TextEngine::add_font`].
    pub fn load_system_font_exact(&mut self, family: &str) -> Result<FontId, TextError> {
        let key = family.to_lowercase();
        // A face registered on this engine resolves by name, so a family added
        // after a failed lookup is not shadowed by the memoized miss.
        if let Some(id) = self.font_id(family) {
            self.missing_system_families.remove(&key);
            return Ok(id);
        }
        if self.missing_system_families.get(&key).is_some() {
            return Err(TextError::FontNotFound {
                family: family.to_string(),
            });
        }
        let (bytes, index) = match self.system_face_bytes(family, false) {
            Ok(found) => found,
            Err(error) => {
                if matches!(error, TextError::FontNotFound { .. }) {
                    self.missing_system_families.insert(key, ());
                }
                return Err(error);
            }
        };
        self.missing_system_families.remove(&key);
        self.finish_system_face(family, &bytes, index)
    }

    /// Reads the bytes and collection index of the system face matching
    /// `family`. With `fallback`, a missing family falls back through
    /// [`CJK_FALLBACK_FAMILIES`] and the well-known Noto CJK paths.
    fn system_face_bytes(
        &mut self,
        family: &str,
        fallback: bool,
    ) -> Result<(Vec<u8>, u32), TextError> {
        if self.system_fonts.is_none() {
            self.system_fonts = Some(load_system_database(&self.system_font_dirs));
        }
        let system = self
            .system_fonts
            .as_mut()
            .ok_or_else(|| TextError::FontNotFound {
                family: family.to_string(),
            })?;
        let matched = if fallback {
            let named = find_family(system, family).or_else(|| {
                CJK_FALLBACK_FAMILIES
                    .iter()
                    .find_map(|candidate| find_family(system, candidate))
            });
            named.or_else(|| load_known_jp_path(system))
        } else {
            find_family(system, family)
        };
        matched
            .and_then(|id| system.with_face_data(id, |data, index| (data.to_vec(), index)))
            .ok_or_else(|| TextError::FontNotFound {
                family: family.to_string(),
            })
    }

    /// Registers a system face already read from the font database and
    /// aliases `family` to it.
    fn finish_system_face(
        &mut self,
        family: &str,
        bytes: &[u8],
        index: u32,
    ) -> Result<FontId, TextError> {
        let id = self.add_font(bytes, index)?;
        self.by_name.entry(family.to_lowercase()).or_insert(id);
        Ok(id)
    }

    /// Returns the L1 face for `id`, if registered.
    pub fn face(&self, id: FontId) -> Option<&Font> {
        self.faces.get(id.0).map(|face| &face.render)
    }

    /// Returns the family name registered for `id`, if the face has one.
    pub fn family_name(&self, id: FontId) -> Option<&str> {
        self.faces
            .get(id.0)
            .map(|face| face.family.as_str())
            .filter(|family| !family.is_empty())
    }

    /// Returns the first registered face, the fallback for a face that cannot
    /// be resolved, and notes that `family` used it.
    ///
    /// The first such fallback is logged once per engine rather than once per
    /// name.
    pub fn default_font_for(&mut self, family: &str) -> Option<FontId> {
        if self.faces.is_empty() {
            return None;
        }
        if !self.default_fallback_logged {
            self.default_fallback_logged = true;
            log::info!("system fonts: face '{family}' resolved to the engine default face");
        }
        Some(FontId(0))
    }

    /// Returns the L1 metrics of `id` at `size_px`.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::UnknownFont`] when `id` is not registered.
    pub fn font_metrics(&self, id: FontId, size_px: f32) -> Result<FontMetrics, TextError> {
        let face = self.faces.get(id.0).ok_or(TextError::UnknownFont(id.0))?;
        Ok(render::font_metrics(
            &self.render_system,
            &face.render,
            size_px,
        ))
    }

    /// Lays out `text` for face `font` using `cosmic-text` shaping, bidi and
    /// line breaking, and returns the positioned glyphs.
    ///
    /// `wrap_width` sets the layout width; `None` lays the text out on a
    /// single line. Bold/italic are applied later by the L1 rasterizer, so
    /// shaping always uses the registered face's normal weight so the shaped
    /// positions are stable.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::UnknownFont`] or [`TextError::InvalidSize`].
    pub fn layout(
        &mut self,
        font: FontId,
        text: &str,
        style: &TextStyle,
        wrap_width: Option<f32>,
    ) -> Result<TextLayout, TextError> {
        validate_size(style.size_px)?;

        let wrap = wrap_width.filter(|w| w.is_finite() && *w > 0.0);
        let key = LayoutKey {
            font,
            text: text.to_owned(),
            size_bits: style.size_px.to_bits(),
            bold: style.bold,
            italic: style.italic,
            wrap_bits: wrap.map(f32::to_bits),
        };
        if let Some(cached) = self.layout_cache.get(&key) {
            return Ok(cached.clone());
        }

        let layout = {
            // Split the borrow so `cosmic` can be borrowed mutably while
            // `faces` (and the immutable `render_system`) stay borrowed.
            let Self {
                render_system,
                cosmic,
                faces,
                ..
            } = &mut *self;
            let face = faces.get(font.0).ok_or(TextError::UnknownFont(font.0))?;

            let metrics = render::font_metrics(render_system, &face.render, style.size_px);
            let measured = metrics.line_height();
            let line_height = if measured.is_finite() && measured > 0.0 {
                measured
            } else {
                style.size_px * 1.2
            };
            // A line's baseline sits at the face's ascent in whole pixels:
            // the ascender scaled to the pixel size and truncated, the way
            // the outline rasterizer computes it. The line box is not
            // vertically centred around the baseline: a glyph's ink top is
            // `baseline - bitmap_top`, so a centred or fractional baseline
            // moves every glyph off its row.
            let baseline = metrics.ascent.floor();

            let attributes = Attrs::new()
                .family(Family::Name(&face.family))
                .weight(cosmic_text::Weight::NORMAL)
                .style(cosmic_text::Style::Normal);

            let mut buffer = Buffer::new(cosmic, Metrics::new(style.size_px, line_height));
            buffer.set_size(wrap, None);
            buffer.set_text(text, &attributes, Shaping::Advanced, None);

            let mut glyphs = Vec::new();
            let mut max_line = 0usize;
            let mut line_width: f32 = 0.0;
            {
                let mut borrowed = buffer.borrow_with(cosmic);
                for run in borrowed.layout_runs() {
                    max_line = max_line.max(run.line_i);
                    line_width = line_width.max(run.line_w);
                    let baseline_y = run.line_top + baseline;
                    for glyph in run.glyphs {
                        let cluster = &run.text[glyph.start..glyph.end];
                        let codepoint = cluster.chars().next().unwrap_or('\u{FFFD}');
                        glyphs.push(LaidOutGlyph {
                            codepoint,
                            font,
                            line: run.line_i,
                            x: glyph.x + glyph.font_size * glyph.x_offset,
                            baseline_y,
                            advance: glyph.w,
                        });
                    }
                }
            }

            TextLayout {
                glyphs,
                width: line_width,
                height: (max_line + 1) as f32 * line_height,
                line_height,
            }
        };

        self.layout_cache.insert(key, layout.clone());
        Ok(layout)
    }

    /// Returns the number of shaped layouts currently memoized.
    ///
    /// Used by tests and diagnostics; the value never affects rendering.
    pub fn cached_layouts(&self) -> usize {
        self.layout_cache.len()
    }

    /// Rasterizes one glyph of `font` for `style`, returning its 8-bit
    /// coverage and pen bearings.
    ///
    /// This is the CPU half of the text pipeline without the atlas: it exists
    /// for consumers that composite glyph coverage directly into a bitmap
    /// (the compat `Layer.drawString` path) instead of uploading a texture.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::UnknownFont`] or [`TextError::InvalidSize`] for an
    /// unregistered face or a bad size, and [`TextError::Glyph`] when the L1
    /// rasterizer rejects the glyph.
    pub fn rasterize_glyph(
        &self,
        font: FontId,
        codepoint: char,
        style: &TextStyle,
    ) -> Result<GlyphBitmap, TextError> {
        validate_size(style.size_px)?;
        let face = self
            .faces
            .get(font.0)
            .ok_or(TextError::UnknownFont(font.0))?;
        Ok(render::rasterize_glyph(
            &self.render_system,
            &face.render,
            codepoint,
            style.size_px,
            style.glyph_style(),
        )?)
    }

    /// Returns the rasterizer's pen advance for one character.
    ///
    /// This is the per-character extent a pre-rendered-font width sums in for
    /// a character the `.tft` does not carry. The synthetic style does not
    /// change it: the bold widens the drawn coverage, not the measured
    /// extent.
    ///
    /// The advance is memoized per face, character, size and synthetic
    /// style, so a repeated measurement of the same character reuses the
    /// value the rasterizer returned instead of scaling the face again. The
    /// memoized value is the rasterizer's own result, so it is identical to
    /// the value a fresh measurement returns.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::UnknownFont`] when `font` is not registered.
    pub fn glyph_advance(
        &mut self,
        font: FontId,
        codepoint: char,
        style: &TextStyle,
    ) -> Result<f32, TextError> {
        let key = AdvanceKey {
            font,
            codepoint,
            size_bits: style.size_px.to_bits(),
            bold: style.bold,
            italic: style.italic,
        };
        if let Some(cached) = self.advance_cache.get(&key) {
            return Ok(*cached);
        }
        let face = self
            .faces
            .get(font.0)
            .ok_or(TextError::UnknownFont(font.0))?;
        let advance = render::glyph_advance(
            &self.render_system,
            &face.render,
            codepoint,
            style.size_px,
            style.glyph_style(),
        );
        self.advance_cache.insert(key, advance);
        Ok(advance)
    }

    /// The number of memoized per-character advances.
    #[cfg(test)]
    fn cached_advances(&self) -> usize {
        self.advance_cache.len()
    }

    /// Returns the bounding rectangle of the rasterized `text` in the text's
    /// local coordinate space (origin at the pen start), or `None` when the
    /// run has no ink.
    ///
    /// This is the union of each shaped glyph's bitmap box, the quantity
    /// `Font.getGlyphDrawRect` reports.
    ///
    /// # Errors
    ///
    /// Propagates the layout and rasterization errors of
    /// [`TextEngine::layout`] / [`TextEngine::rasterize_glyph`].
    pub fn glyph_draw_rect(
        &mut self,
        font: FontId,
        text: &str,
        style: &TextStyle,
        wrap_width: Option<f32>,
    ) -> Result<Option<RectI>, TextError> {
        let layout = self.layout(font, text, style, wrap_width)?;
        let mut ink: Option<RectI> = None;
        for glyph in &layout.glyphs {
            let mask = self.rasterize_glyph(glyph.font, glyph.codepoint, style)?;
            if mask.is_empty() {
                continue;
            }
            let left = (glyph.x + mask.bearing_x).round() as i32;
            let top = (glyph.baseline_y - mask.bearing_y).round() as i32;
            let rect = (
                left,
                top,
                left.saturating_add(mask.width as i32),
                top.saturating_add(mask.height as i32),
            );
            ink = Some(match ink {
                None => rect,
                Some(previous) => union_rect(previous, rect),
            });
        }
        Ok(ink)
    }
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds the host font database used by [`TextEngine::load_system_font`].
///
/// `fontdb`'s own scan knows the Windows, macOS and Unix font directories but
/// excludes Android, so `dirs` (the host's system-font directories, supplied
/// through [`TextEngine::set_system_font_dirs`]) is scanned in addition. The
/// resulting face count and directory count are logged at `info`, once per
/// engine, so a missing font source is visible in the log rather than only in a
/// later failure.
fn load_system_database(dirs: &[PathBuf]) -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let directories = load_font_directories(&mut db, dirs);
    let faces = db.len();
    if faces == 0 {
        log::info!(
            "system fonts: no fonts found in any of {directories} directories; \
             text has no face to fall back to"
        );
    } else {
        log::info!("system fonts: {faces} faces from {directories} directories");
    }
    db
}

/// Scans the font files in every existing directory of `dirs` into `db` and
/// returns how many directories existed.
///
/// A directory that is not present is skipped: the platform list names
/// locations a device may not mount, and an absent directory is an absent
/// source rather than an error.
fn load_font_directories(db: &mut fontdb::Database, dirs: &[PathBuf]) -> usize {
    let mut directories = 0;
    for dir in dirs {
        if dir.is_dir() {
            db.load_fonts_dir(dir);
            directories += 1;
        }
    }
    directories
}

/// Finds the face whose family name equals `family` case-insensitively.
///
/// The exact-name CSS query runs first so the best weight/style match for the
/// family is chosen; the scan then covers case differences and the localized
/// family names the query's exact comparison misses.
fn find_family(db: &fontdb::Database, family: &str) -> Option<fontdb::ID> {
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    };
    if let Some(id) = db.query(&query) {
        return Some(id);
    }
    let wanted = family.to_lowercase();
    db.faces().find_map(|face| {
        face.families
            .iter()
            .any(|(name, _)| name.to_lowercase() == wanted)
            .then_some(face.id)
    })
}

/// Loads the well-known Noto CJK collections and returns the first face whose
/// family name contains `JP` (the matching face inside a shared `.ttc`).
fn load_known_jp_path(db: &mut fontdb::Database) -> Option<fontdb::ID> {
    for path in KNOWN_JP_PATHS {
        if !std::path::Path::new(path).is_file() {
            continue;
        }
        let ids = db.load_font_source(fontdb::Source::File(std::path::PathBuf::from(*path)));
        let matched = ids.into_iter().find(|id| {
            db.face(*id)
                .is_some_and(|face| face.families.iter().any(|(name, _)| name.contains("JP")))
        });
        if let Some(id) = matched {
            return Some(id);
        }
    }
    None
}

/// Cache key for one rasterized glyph in the atlas.
///
/// The atlas stores colour-independent straight-alpha coverage, so the text
/// colour is **not** part of the key: a colour change reuses the cached
/// coverage and applies the colour as a per-node tint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    /// Registered face index.
    font: usize,
    /// Code point.
    codepoint: char,
    /// `size_px.to_bits()` (so the key stays `Eq`).
    size_bits: u32,
    /// Synthetic bold.
    bold: bool,
    /// Synthetic italic.
    italic: bool,
}

/// One glyph packed into the atlas.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct AtlasGlyph {
    /// Sub-rectangle of the atlas holding the glyph's RGBA pixels.
    pub region: PixelRegion,
    /// Horizontal bearing from the pen to the bitmap's left edge.
    pub bearing_x: f32,
    /// Vertical bearing from the baseline to the bitmap's top edge.
    pub bearing_y: f32,
    /// Shaped advance after the glyph, in pixels.
    pub advance: f32,
    /// Em size the glyph was rasterized at.
    pub size_px: f32,
}

impl AtlasGlyph {
    /// Returns `true` when the glyph has no ink (e.g. a space).
    pub const fn is_empty(&self) -> bool {
        self.region.width == 0 || self.region.height == 0
    }
}

/// A glyph atlas: an L2-owned RGBA texture plus its CPU mirror and cache.
///
/// Glyphs are packed left-to-right, top-to-bottom, one pixel apart to avoid
/// bilinear bleeding. Rasterizing a glyph writes its sub-rectangle into the CPU
/// mirror and records the region as pending; [`GlyphAtlas::flush`] uploads
/// every pending region as one [`render::upload_textures`] batch. The CPU
/// mirror is kept coherent with the GPU so the same pixels can be composited
/// for CPU readback and re-uploaded after a render-state recreation.
///
/// The entry map is bounded at [`MAX_TEXT_CACHE_ENTRIES`] and evicts the least
/// recently used glyph, the same bound and policy as [`TextCoverageCache`] and
/// the shaped-layout memo. Eviction drops the map entry only: the shelf packer
/// never reclaims the cell, so [`GlyphAtlas::clear`] is the way to reset the
/// pixel space between frames.
#[non_exhaustive]
pub struct GlyphAtlas {
    /// Atlas texture handle passed to the quad pipeline.
    texture: Texture,
    /// Atlas width in pixels.
    width: u32,
    /// Atlas height in pixels.
    height: u32,
    /// CPU mirror, `width * height * 4` straight-alpha RGBA bytes.
    pixels: Vec<u8>,
    /// Next free x in the current shelf.
    cursor_x: u32,
    /// Current shelf top.
    cursor_y: u32,
    /// Tallest glyph in the current shelf.
    row_height: u32,
    /// Rasterized glyphs, keyed by face/codepoint/size/style and evicted least
    /// recently used at the reference limit.
    entries: LruCache<GlyphKey, AtlasGlyph>,
    /// Regions written to the CPU mirror but not yet copied to the GPU.
    pending: Vec<PendingGlyphUpload>,
}

/// One rasterized glyph region waiting for its GPU copy.
struct PendingGlyphUpload {
    /// Sub-rectangle of the atlas holding the pixels.
    region: PixelRegion,
    /// Tightly packed straight-alpha RGBA pixels for `region`.
    pixels: Vec<u8>,
}

impl GlyphAtlas {
    /// Creates a square `size × size` RGBA atlas texture.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::Render`] when the backend rejects the descriptor.
    pub fn new(pipeline: &RenderPipeline, size: u32) -> Result<Self, TextError> {
        let size = size.max(1);
        let texture = render::create_texture(
            pipeline,
            TextureDesc::new(size, size).with_format(TextureFormat::Rgba8Srgb),
        )?;
        let len = (size as usize) * (size as usize) * 4;
        Ok(Self {
            texture,
            width: size,
            height: size,
            pixels: vec![0; len],
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
            entries: LruCache::new(MAX_TEXT_CACHE_ENTRIES),
            pending: Vec::new(),
        })
    }

    /// Drops every cached glyph and resets the shelf packer, zeroing the CPU
    /// mirror.
    ///
    /// A glyph handle returned before the call is no longer valid. Pixel space
    /// is only reclaimed here because the shelf packer never reuses an
    /// individual cell; the owner calls this between frames once the frame's
    /// projected glyphs have been consumed.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.pixels.fill(0);
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.row_height = 0;
        self.pending.clear();
    }

    /// Returns the atlas texture handle.
    pub fn texture(&self) -> &Texture {
        &self.texture
    }

    /// Returns the atlas dimensions in pixels.
    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Returns the CPU mirror, `width * height * 4` straight-alpha RGBA bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Returns the number of cached glyphs.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no glyph has been cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of rasterized regions waiting for
    /// [`GlyphAtlas::flush`].
    ///
    /// This is a diagnostic: a caller can check that a run of text queued its
    /// glyphs for one batch instead of uploading each glyph separately.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Rasterizes `codepoint` (if needed) and packs it into the atlas.
    ///
    /// The glyph is cached by face, code point, size and style (never colour),
    /// so a repeated request does not re-rasterize or re-upload and a colour
    /// change reuses the cached coverage. The new region is written to the CPU
    /// mirror and queued for [`GlyphAtlas::flush`]; it is not copied to the GPU
    /// until that flush.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::UnknownFont`], [`TextError::Glyph`] or
    /// [`TextError::AtlasFull`].
    pub fn glyph(
        &mut self,
        engine: &TextEngine,
        font: FontId,
        codepoint: char,
        style: &TextStyle,
    ) -> Result<AtlasGlyph, TextError> {
        validate_size(style.size_px)?;
        let key = GlyphKey {
            font: font.0,
            codepoint,
            size_bits: style.size_px.to_bits(),
            bold: style.bold,
            italic: style.italic,
        };
        if let Some(entry) = self.entries.get(&key) {
            return Ok(*entry);
        }

        let face = engine
            .faces
            .get(font.0)
            .ok_or(TextError::UnknownFont(font.0))?;
        let bitmap = render::rasterize_glyph(
            &engine.render_system,
            &face.render,
            codepoint,
            style.size_px,
            style.glyph_style(),
        )?;

        let entry = if bitmap.is_empty() {
            AtlasGlyph {
                region: PixelRegion::ZERO,
                bearing_x: bitmap.bearing_x,
                bearing_y: bitmap.bearing_y,
                advance: bitmap.advance,
                size_px: style.size_px,
            }
        } else {
            let (x, y) =
                self.allocate(bitmap.width, bitmap.height)
                    .ok_or(TextError::AtlasFull {
                        width: self.width,
                        height: self.height,
                    })?;
            let rgba = coverage_rgba(&bitmap);
            self.blit(x, y, bitmap.width, bitmap.height, &rgba);
            let region = PixelRegion::new(x, y, bitmap.width, bitmap.height);
            self.pending.push(PendingGlyphUpload {
                region,
                pixels: rgba,
            });
            AtlasGlyph {
                region,
                bearing_x: bitmap.bearing_x,
                bearing_y: bitmap.bearing_y,
                advance: bitmap.advance,
                size_px: style.size_px,
            }
        };

        self.entries.insert(key, entry);
        Ok(entry)
    }

    /// Copies every region rasterized since the last flush to the atlas texture
    /// as one [`render::upload_textures`] batch.
    ///
    /// On success the pending list is cleared; on failure it is kept so the
    /// caller can retry. [`GlyphAtlas::glyph`] updates the CPU mirror
    /// regardless, so CPU readback stays coherent whether or not the GPU copy
    /// has happened.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::Render`] when the backend rejects the batch.
    pub fn flush(
        &mut self,
        pipeline: &RenderPipeline,
        state: &mut RenderState,
    ) -> Result<(), TextError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let uploads: Vec<render::TextureUpload<'_>> = self
            .pending
            .iter()
            .map(|pending| render::TextureUpload {
                texture: &self.texture,
                region: pending.region,
                pixels: &pending.pixels,
            })
            .collect();
        render::upload_textures(pipeline, state, &uploads)?;
        drop(uploads);
        self.pending.clear();
        Ok(())
    }

    /// Rasterizes every glyph of `layout` and returns them positioned for
    /// projection or CPU composition.
    ///
    /// Every new glyph in the layout is uploaded by a single
    /// [`GlyphAtlas::flush`] at the end, so a line of N new glyphs costs one
    /// GPU batch rather than N.
    ///
    /// # Errors
    ///
    /// Propagates the same errors as [`GlyphAtlas::glyph`] and
    /// [`GlyphAtlas::flush`].
    pub fn rasterize_layout(
        &mut self,
        pipeline: &RenderPipeline,
        state: &mut RenderState,
        engine: &TextEngine,
        layout: &TextLayout,
        style: &TextStyle,
        opacity: f32,
    ) -> Result<Vec<PositionedGlyph>, TextError> {
        let mut positioned = Vec::with_capacity(layout.glyphs.len());
        for glyph in &layout.glyphs {
            let entry = self.glyph(engine, glyph.font, glyph.codepoint, style)?;
            positioned.push(PositionedGlyph {
                glyph: entry,
                x: glyph.x,
                baseline_y: glyph.baseline_y,
                opacity,
                color: style.color,
            });
        }
        self.flush(pipeline, state)?;
        Ok(positioned)
    }

    /// Finds a free `width × height` cell, advancing a shelf packer.
    fn allocate(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if width == 0 || height == 0 || width > self.width || height > self.height {
            return None;
        }
        if self.cursor_x.saturating_add(width) > self.width {
            self.cursor_x = 0;
            self.cursor_y = self
                .cursor_y
                .saturating_add(self.row_height)
                .saturating_add(1);
            self.row_height = 0;
        }
        if self.cursor_y.saturating_add(height) > self.height {
            return None;
        }
        let position = (self.cursor_x, self.cursor_y);
        self.cursor_x = self.cursor_x.saturating_add(width).saturating_add(1);
        self.row_height = self.row_height.max(height);
        Some(position)
    }

    /// Copies a tightly packed RGBA rectangle into the CPU mirror.
    fn blit(&mut self, x: u32, y: u32, width: u32, height: u32, rgba: &[u8]) {
        if width == 0 || height == 0 {
            return;
        }
        let stride = self.width as usize * 4;
        let row = width as usize * 4;
        let start = y as usize * stride + x as usize * 4;
        for (r, src) in rgba.chunks_exact(row).take(height as usize).enumerate() {
            let destination = start + r * stride;
            if let Some(dst) = self.pixels.get_mut(destination..destination + row) {
                dst.copy_from_slice(src);
            }
        }
    }
}

/// A glyph resolved against an atlas and placed at a pen position.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct PositionedGlyph {
    /// Atlas entry for the glyph.
    pub glyph: AtlasGlyph,
    /// Pen x offset in the target coordinate space.
    pub x: f32,
    /// Baseline y offset in the target coordinate space (y grows down).
    pub baseline_y: f32,
    /// Composed opacity in `[0, 1]`.
    pub opacity: f32,
    /// Straight-alpha text colour applied as a tint to the coverage.
    pub color: [u8; 4],
}

impl PositionedGlyph {
    /// Create a positioned glyph at `(x, baseline_y)` with the given tint and
    /// opacity.
    pub fn new(glyph: AtlasGlyph, x: f32, baseline_y: f32, opacity: f32, color: [u8; 4]) -> Self {
        Self {
            glyph,
            x,
            baseline_y,
            opacity,
            color,
        }
    }

    /// Returns the destination rectangle for the glyph's bitmap, or `None`
    /// for a zero-size glyph.
    pub fn destination(&self) -> Option<RectF> {
        if self.glyph.is_empty() {
            return None;
        }
        Some(RectF::new(
            self.x + self.glyph.bearing_x,
            self.baseline_y - self.glyph.bearing_y,
            self.glyph.region.width as f32,
            self.glyph.region.height as f32,
        ))
    }
}

/// Projects positioned glyphs into `scene` as textured quads under `parent`.
///
/// Each glyph becomes a [`RenderContent::Texture`] node that samples its cell
/// of the shared [`GlyphAtlas`] texture and is drawn by the existing quad
/// pipeline. Sibling `z` order follows the glyph order, so overlapping glyphs
/// (e.g. combining marks) composite deterministically. Returns the new node
/// ids.
pub fn project_text(
    scene: &mut RenderScene,
    parent: RenderNodeId,
    atlas: &GlyphAtlas,
    glyphs: &[PositionedGlyph],
    start_z: i32,
) -> Vec<RenderNodeId> {
    let mut ids = Vec::new();
    let mut z = start_z;
    for positioned in glyphs {
        let Some(dst) = positioned.destination() else {
            continue;
        };
        let node = RenderNode {
            parent: Some(parent),
            content: RenderContent::Texture(atlas.texture.clone()),
            dst,
            src: positioned.glyph.region,
            opacity: clamp01(positioned.opacity),
            color: rgba_tint(positioned.color),
            z,
            ..RenderNode::default()
        };
        let id = scene.push(node);
        if let Some(parent_node) = scene.node_mut(parent) {
            parent_node.children.push(id);
        }
        ids.push(id);
        z = z.saturating_add(1);
    }
    ids
}

/// Composites positioned glyphs into a fresh `width × height` RGBA buffer.
///
/// This is the buffer-based companion of [`project_text`]: it reads the same
/// [`GlyphAtlas`] coverage, tints it with each glyph's colour and applies
/// straight-alpha source-over with the glyph's opacity. It is not the
/// production text path — the emulator composites coverage straight into the
/// layer bitmap with [`compose_text_into`] — but it is the reference for a
/// test that compares the projected atlas pixels with a CPU composite. The
/// returned buffer starts fully transparent.
pub fn compose_text_cpu(
    atlas: &GlyphAtlas,
    glyphs: &[PositionedGlyph],
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut out = vec![0u8; width as usize * height as usize * 4];
    let (atlas_w, atlas_h) = (atlas.width, atlas.height);
    for positioned in glyphs {
        let region = positioned.glyph.region;
        if region.is_empty() || width == 0 || height == 0 {
            continue;
        }
        let dest_x = (positioned.x + positioned.glyph.bearing_x).round() as i64;
        let dest_y = (positioned.baseline_y - positioned.glyph.bearing_y).round() as i64;
        let opacity = clamp01(positioned.opacity);
        for row in 0..region.height as i64 {
            let dy = dest_y + row;
            if dy < 0 || dy >= height as i64 {
                continue;
            }
            for column in 0..region.width as i64 {
                let dx = dest_x + column;
                if dx < 0 || dx >= width as i64 {
                    continue;
                }
                let sx = region.x as i64 + column;
                let sy = region.y as i64 + row;
                if sx < 0 || sx >= atlas_w as i64 || sy < 0 || sy >= atlas_h as i64 {
                    continue;
                }
                let source = ((sy as usize) * atlas_w as usize + sx as usize) * 4;
                let destination = ((dy as usize) * width as usize + dx as usize) * 4;
                if let (Some(src), Some(dst)) = (
                    atlas.pixels.get(source..source + 4),
                    out.get_mut(destination..destination + 4),
                ) {
                    blend_tinted_coverage(dst, src[3], positioned.color, opacity);
                }
            }
        }
    }
    out
}

/// Cache key for one rasterized character mask in [`TextCoverageCache`].
///
/// The key covers the face, the character, the size, the synthetic style,
/// antialiasing and the blur parameters. The sharp character and its blurred
/// shadow are separate entries distinguished by `blur`. Rotation is absent
/// because the rasterizer produces an unrotated bitmap and the draw only
/// offsets it; colour is absent because it is a draw argument, not cached
/// data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CoverageKey {
    /// Registered face index.
    font: usize,
    /// Code point.
    codepoint: char,
    /// `size_px.to_bits()` (so the key stays `Eq`).
    size_bits: u32,
    /// Synthetic bold.
    bold: bool,
    /// Synthetic italic.
    italic: bool,
    /// Coverage antialiasing.
    antialias: bool,
    /// `None` for the sharp character; `Some((blur_level, blur_width))` for
    /// the blurred shadow (the reference `Blured` plus its blur parameters).
    blur: Option<(u8, i32)>,
}

impl CoverageKey {
    /// The key of the sharp, unblurred character.
    fn sharp(font: FontId, codepoint: char, style: &TextStyle, antialias: bool) -> Self {
        Self {
            font: font.0,
            codepoint,
            size_bits: style.size_px.to_bits(),
            bold: style.bold,
            italic: style.italic,
            antialias,
            blur: None,
        }
    }

    /// The key of the blurred shadow of the same character.
    fn blurred(
        font: FontId,
        codepoint: char,
        style: &TextStyle,
        antialias: bool,
        blur_level: u8,
        blur_width: i32,
    ) -> Self {
        Self {
            blur: Some((blur_level, blur_width)),
            ..Self::sharp(font, codepoint, style, antialias)
        }
    }
}

/// A fixed-capacity least-recently-used map.
///
/// A hit moves to the front of the recency order; an insert at the limit
/// evicts the entry at the back.
struct LruCache<K, V> {
    /// Live and reusable slots; a live key owns exactly one slot.
    slots: Vec<Slot<K, V>>,
    /// Indices of evicted slots available for reuse.
    free: Vec<usize>,
    /// Live key → slot index.
    index: Map<K, usize>,
    /// Most-recently-used slot.
    head: Option<usize>,
    /// Least-recently-used slot, the next eviction victim.
    tail: Option<usize>,
    /// Maximum number of live entries; must be positive.
    capacity: usize,
}

/// One slot of an [`LruCache`] recency list.
struct Slot<K, V> {
    /// Key while the slot is live.
    key: Option<K>,
    /// Value while the slot is live.
    value: Option<V>,
    /// More-recently-used neighbour.
    prev: Option<usize>,
    /// Less-recently-used neighbour.
    next: Option<usize>,
}

impl<K, V> Slot<K, V> {
    /// An unoccupied slot.
    fn empty() -> Self {
        Self {
            key: None,
            value: None,
            prev: None,
            next: None,
        }
    }
}

impl<K: Eq + std::hash::Hash + Clone, V> LruCache<K, V> {
    /// Creates an empty cache holding at most `capacity` entries.
    fn new(capacity: usize) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            index: map(),
            head: None,
            tail: None,
            capacity,
        }
    }

    /// The number of live entries.
    fn len(&self) -> usize {
        self.index.len()
    }

    /// Returns `true` when the cache holds no entry.
    fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Drops every entry.
    fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
        self.index.clear();
        self.head = None;
        self.tail = None;
    }

    /// Returns the value for `key`, marking it the most recently used.
    fn get(&mut self, key: &K) -> Option<&V> {
        let slot = *self.index.get(key)?;
        self.unlink(slot);
        self.link_front(slot);
        self.slots[slot].value.as_ref()
    }

    /// Inserts `value` under `key`, marking it the most recently used and
    /// evicting the least recently used entry when the cache is full.
    fn insert(&mut self, key: K, value: V) {
        if let Some(&slot) = self.index.get(&key) {
            self.slots[slot].value = Some(value);
            self.unlink(slot);
            self.link_front(slot);
            return;
        }
        if self.index.len() >= self.capacity {
            self.evict_lru();
        }
        let slot = match self.free.pop() {
            Some(slot) => slot,
            None => {
                self.slots.push(Slot::empty());
                self.slots.len() - 1
            }
        };
        self.slots[slot].key = Some(key.clone());
        self.slots[slot].value = Some(value);
        self.slots[slot].prev = None;
        self.slots[slot].next = None;
        self.index.insert(key, slot);
        self.link_front(slot);
    }

    /// Removes the entry for `key`, dropping its value, and reports whether
    /// an entry was present.
    fn remove(&mut self, key: &K) -> bool {
        let Some(&slot) = self.index.get(key) else {
            return false;
        };
        self.slots[slot].value = None;
        self.slots[slot].key = None;
        self.index.remove(key);
        self.unlink(slot);
        self.free.push(slot);
        true
    }

    /// Removes the least recently used entry, if any.
    fn evict_lru(&mut self) {
        let Some(slot) = self.tail else {
            return;
        };
        self.unlink(slot);
        if let Some(key) = self.slots[slot].key.take() {
            self.index.remove(&key);
        }
        self.slots[slot].value = None;
        self.free.push(slot);
    }

    /// Detaches `slot` from the recency list.
    fn unlink(&mut self, slot: usize) {
        let (prev, next) = {
            let entry = &self.slots[slot];
            (entry.prev, entry.next)
        };
        match prev {
            Some(prev) => self.slots[prev].next = next,
            None => self.head = next,
        }
        match next {
            Some(next) => self.slots[next].prev = prev,
            None => self.tail = prev,
        }
        self.slots[slot].prev = None;
        self.slots[slot].next = None;
    }

    /// Inserts `slot` at the front of the recency list.
    fn link_front(&mut self, slot: usize) {
        let head = self.head;
        self.slots[slot].prev = None;
        self.slots[slot].next = head;
        if let Some(head) = head {
            self.slots[head].prev = Some(slot);
        }
        self.head = Some(slot);
        if self.tail.is_none() {
            self.tail = Some(slot);
        }
    }
}

/// L2-owned cache of rasterized character masks and their blurred shadows.
///
/// A character is looked up by [`CoverageKey`] and, on a miss, rasterized and
/// (when the font is blurred) its shadow derived from that coverage before
/// inserting, so both the sharp character and its shadow are reused by a
/// later draw. Eviction is least-recently-used. The type is explicit
/// caller-owned state; there is no global cache.
pub struct TextCoverageCache {
    /// Cached masks, evicted least-recently-used at the reference limit.
    entries: LruCache<CoverageKey, CoverageMask>,
    /// Number of masks produced by rasterization; a diagnostic for tests.
    rasterizations: u64,
}

impl TextCoverageCache {
    /// Creates an empty cache.
    pub fn new() -> Self {
        Self {
            entries: LruCache::new(MAX_TEXT_CACHE_ENTRIES),
            rasterizations: 0,
        }
    }

    /// The number of cached masks.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when the cache holds no mask.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of glyphs rasterized through this cache.
    ///
    /// Used by tests and diagnostics; the value never affects rendering.
    pub fn rasterizations(&self) -> u64 {
        self.rasterizations
    }

    /// Drops every cached mask.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Looks `key` up, marking it the most recently used.
    fn get(&mut self, key: &CoverageKey) -> Option<&CoverageMask> {
        self.entries.get(key)
    }

    /// Inserts `mask` under `key`, evicting the least recently used entry at
    /// the reference limit.
    fn insert(&mut self, key: CoverageKey, mask: CoverageMask) {
        self.entries.insert(key, mask);
    }
}

impl Default for TextCoverageCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Places a pen-relative (cached) mask at `glyph`'s pen position.
///
/// The cached mask stores only the glyph's bearings, so the same coverage is
/// reused at any pen position; this adds the layout's `x`/`baseline_y`. The
/// coverage buffer is shared, not copied.
fn position_mask(mask: &CoverageMask, glyph: &LaidOutGlyph) -> CoverageMask {
    CoverageMask {
        x: glyph.x + mask.x,
        y: glyph.baseline_y + mask.y,
        width: mask.width,
        height: mask.height,
        coverage: Arc::clone(&mask.coverage),
    }
}

/// Rasterizes one glyph into a cached pen-relative sharp mask, rasterizing it
/// only on a miss.
///
/// `None` means the glyph has no ink (a space or a mapped glyph without an
/// outline); the empty result is cached so a repeated draw does not rasterize
/// it again.
fn sharp_coverage(
    cache: &mut TextCoverageCache,
    engine: &TextEngine,
    glyph: &LaidOutGlyph,
    style: &TextStyle,
    antialias: bool,
) -> Result<Option<CoverageMask>, TextError> {
    let key = CoverageKey::sharp(glyph.font, glyph.codepoint, style, antialias);
    if let Some(cached) = cache.get(&key) {
        if cached.is_empty() {
            return Ok(None);
        }
        return Ok(Some(cached.clone()));
    }
    let bitmap = engine.rasterize_glyph(glyph.font, glyph.codepoint, style)?;
    cache.rasterizations += 1;
    if bitmap.is_empty() || !bitmap.is_consistent() {
        cache.insert(key, CoverageMask::new(0.0, 0.0, 0, 0, Vec::new())?);
        return Ok(None);
    }
    let mut coverage = bitmap.coverage;
    if !antialias {
        for value in &mut coverage {
            *value = if *value >= 128 { 255 } else { 0 };
        }
    }
    // The mask is relative to the pen, not the layout, so it is reusable at
    // any position.
    let mask = CoverageMask::new(
        bitmap.bearing_x,
        -bitmap.bearing_y,
        bitmap.width,
        bitmap.height,
        coverage,
    )?;
    cache.insert(key, mask.clone());
    Ok(Some(mask))
}

/// Rasterizes every glyph of `layout` into its own 8-bit coverage mask,
/// positioned in the text's local coordinate space, reusing `cache`.
///
/// Each glyph keeps its own mask (rather than one mask for the whole run) so
/// a caller can blur or transform one character without bleeding into its
/// neighbours.
///
/// When `antialias` is false the glyph coverage is thresholded to fully
/// transparent or fully opaque, matching the aliased coverage the CPU
/// compositor blends.
///
/// # Errors
///
/// Propagates [`TextEngine::rasterize_glyph`]'s errors.
pub fn rasterize_text_coverage(
    cache: &mut TextCoverageCache,
    engine: &TextEngine,
    layout: &TextLayout,
    style: &TextStyle,
    antialias: bool,
) -> Result<Vec<CoverageMask>, TextError> {
    let mut masks = Vec::with_capacity(layout.glyphs.len());
    for glyph in &layout.glyphs {
        if let Some(mask) = sharp_coverage(cache, engine, glyph, style, antialias)? {
            masks.push(position_mask(&mask, glyph));
        }
    }
    // A run whose non-whitespace characters all rasterize to no ink means the
    // resolved face cannot render them; report a typed error instead of
    // compositing nothing.
    if masks.is_empty()
        && let Some(glyph) = layout
            .glyphs
            .iter()
            .find(|glyph| requires_ink(glyph.codepoint))
    {
        return Err(TextError::Glyph(api::error::GlyphError::MissingGlyph {
            codepoint: glyph.codepoint as u32,
        }));
    }
    Ok(masks)
}

/// Blurs every glyph of `layout` with the character-blur kernel, returning the
/// cached shadow masks positioned in the text's local coordinate space.
///
/// The blurred mask is derived from the cached sharp coverage, so a shadow
/// never rasterizes a glyph a second time.
///
/// # Errors
///
/// Propagates [`rasterize_text_coverage`]'s errors and
/// [`TextError::MaskTooLarge`].
pub fn blurred_text_coverage(
    cache: &mut TextCoverageCache,
    engine: &TextEngine,
    layout: &TextLayout,
    style: &TextStyle,
    antialias: bool,
    blur_level: u8,
    blur_width: i32,
) -> Result<Vec<CoverageMask>, TextError> {
    let mut masks = Vec::with_capacity(layout.glyphs.len());
    for glyph in &layout.glyphs {
        let key = CoverageKey::blurred(
            glyph.font,
            glyph.codepoint,
            style,
            antialias,
            blur_level,
            blur_width,
        );
        if let Some(cached) = cache.get(&key) {
            masks.push(position_mask(cached, glyph));
            continue;
        }
        let Some(sharp) = sharp_coverage(cache, engine, glyph, style, antialias)? else {
            continue;
        };
        let blurred = blur_coverage(&sharp, blur_level, blur_width)?;
        cache.insert(key, blurred.clone());
        masks.push(position_mask(&blurred, glyph));
    }
    Ok(masks)
}

/// The drawable form one layout takes under the selected glyph backend.
///
/// The two forms are deliberately different values: the `cpu` form carries
/// per-character coverage the caller composites into a bitmap, and the `gpu`
/// form carries pen-positioned atlas glyphs the caller projects into a
/// [`RenderScene`]. Both are built from the same [`TextLayout`], so the glyph
/// positions, advances and metrics agree; only the rasterization and draw path
/// differ.
#[derive(Clone, Debug, PartialEq)]
pub enum RasterizedRun {
    /// Coverage masks in text-local space, for [`compose_text_into`].
    Coverage(Vec<CoverageMask>),
    /// Pen-positioned glyphs backed by the caller's [`GlyphAtlas`], for
    /// [`project_text`] or [`compose_text_cpu`].
    Atlas(Vec<PositionedGlyph>),
}

/// The services and caches one [`rasterize_run`] pass draws from.
pub struct TextRasterServices<'a> {
    /// Coverage cache the CPU backend reuses.
    pub cache: &'a mut TextCoverageCache,
    /// Glyph atlas the GPU backend fills.
    pub atlas: &'a mut GlyphAtlas,
    /// Render pipeline that uploads the atlas texture.
    pub pipeline: &'a RenderPipeline,
    /// Render state the atlas upload updates.
    pub state: &'a mut RenderState,
    /// Shaping engine the glyphs are rasterized from.
    pub engine: &'a TextEngine,
}

/// Rasterizes `layout` through the selected glyph `backend`.
///
/// `Cpu` returns the coverage masks [`compose_text_into`] composites into a
/// bitmap, reusing [`TextRasterServices::cache`]. `Gpu` fills
/// [`TextRasterServices::atlas`] and returns pen-positioned glyphs for
/// [`project_text`]; it needs a live [`TextRasterServices::pipeline`] to
/// upload the atlas texture.
///
/// The shaped [`TextLayout`] is the single source of glyph positions and
/// advances, so choosing `Gpu` changes only which raster coverage the draw
/// consumes, never a script-observable metric. The one visual difference is
/// antialiasing: `Cpu` thresholds the coverage to on/off when `antialias` is
/// false, while the atlas cache keys coverage without it, so `Gpu` always
/// rasterizes antialiased coverage (the readback bitmap the `Cpu` composite
/// produces is unaffected).
///
/// # Errors
///
/// Propagates [`rasterize_text_coverage`] / [`GlyphAtlas::rasterize_layout`]'s
/// errors.
pub fn rasterize_run(
    backend: GlyphBackend,
    services: TextRasterServices<'_>,
    layout: &TextLayout,
    style: &TextStyle,
    antialias: bool,
    opacity: f32,
) -> Result<RasterizedRun, TextError> {
    let TextRasterServices {
        cache,
        atlas,
        pipeline,
        state,
        engine,
    } = services;
    match backend {
        GlyphBackend::Cpu => Ok(RasterizedRun::Coverage(rasterize_text_coverage(
            cache, engine, layout, style, antialias,
        )?)),
        GlyphBackend::Gpu => Ok(RasterizedRun::Atlas(
            atlas.rasterize_layout(pipeline, state, engine, layout, style, opacity)?,
        )),
    }
}

/// Whether a codepoint is expected to leave ink.
///
/// Whitespace and control characters legitimately rasterize to nothing; any
/// other character with no coverage means the resolved face cannot render it.
fn requires_ink(codepoint: char) -> bool {
    !codepoint.is_whitespace() && !codepoint.is_control()
}

/// Blurs a positioned 8-bit coverage mask with the reference character-blur
/// kernel.
///
/// `blur_width == 0` keeps the mask's size and scales every byte by
/// `blur_level / 256`; `blur_level == 255 && blur_width == 0` is the sharp
/// no-op. A nonzero width expands the mask by `abs(blur_width)` transparent
/// pixels on every side and convolves the source with the cone kernel.
///
/// # Errors
///
/// Returns [`TextError::MaskTooLarge`] when the expanded mask cannot be
/// allocated.
pub fn blur_coverage(
    mask: &CoverageMask,
    blur_level: u8,
    blur_width: i32,
) -> Result<CoverageMask, TextError> {
    if blur_level == 255 && blur_width == 0 {
        return Ok(mask.clone());
    }
    if blur_width == 0 {
        let coverage = mask
            .coverage
            .iter()
            .map(|&src| mul_coverage(src, blur_level))
            .collect();
        return CoverageMask::new(mask.x, mask.y, mask.width, mask.height, coverage);
    }

    let bw = blur_width.unsigned_abs();
    let pad = bw
        .checked_mul(2)
        .ok_or(TextError::MaskTooLarge { blur_width })?;
    let width = mask
        .width
        .checked_add(pad)
        .ok_or(TextError::MaskTooLarge { blur_width })?;
    let height = mask
        .height
        .checked_add(pad)
        .ok_or(TextError::MaskTooLarge { blur_width })?;
    let len = (width as usize)
        .checked_mul(height as usize)
        .ok_or(TextError::MaskTooLarge { blur_width })?;
    let mut dest = Vec::new();
    dest.try_reserve_exact(len)
        .map_err(|_| TextError::MaskTooLarge { blur_width })?;
    dest.resize(len, 0);

    // The integer cone radius and normalisation.
    let bw_i = i64::from(bw);
    let mut lvsum_total: i64 = 0;
    for oy in -bw_i..=bw_i {
        for ox in -bw_i..=bw_i {
            let distance = i64::from(fast_int_hypot(ox as i32, oy as i32));
            if distance <= bw_i {
                lvsum_total += bw_i - distance + 1;
            }
        }
    }
    let lvsum = if lvsum_total != 0 {
        (1i64 << 18) / lvsum_total
    } else {
        1i64 << 18
    };

    let source_width = mask.width as usize;
    for oy in -bw_i..=bw_i {
        for ox in -bw_i..=bw_i {
            let distance = i64::from(fast_int_hypot(ox as i32, oy as i32));
            if distance > bw_i {
                continue;
            }
            let weight = ((bw_i - distance + 1) * lvsum * i64::from(blur_level)) >> 8;
            if weight == 0 {
                continue;
            }
            for sy in 0..mask.height as usize {
                let row = (sy as i64 + bw_i + oy) * i64::from(width);
                for sx in 0..source_width {
                    let index = (row + sx as i64 + bw_i + ox) as usize;
                    let add = (i64::from(mask.coverage[sy * source_width + sx]) * weight) >> 18;
                    if add == 0 {
                        continue;
                    }
                    let value = i64::from(dest[index]) + add;
                    dest[index] = value.min(255) as u8;
                }
            }
        }
    }

    CoverageMask::new(
        mask.x - blur_width as f32,
        mask.y - blur_width as f32,
        width,
        height,
        dest,
    )
}

/// Scales one coverage byte by `level / 256`, saturating at `255`.
fn mul_coverage(src: u8, level: u8) -> u8 {
    ((u32::from(src) * u32::from(level)) >> 8).min(255) as u8
}

/// The integer hypotenuse approximation, reproduced bit for bit.
fn fast_int_hypot(lx: i32, ly: i32) -> u32 {
    let lx = lx.unsigned_abs();
    let ly = ly.unsigned_abs();
    let (len1, len2) = if lx >= ly { (lx, ly) } else { (ly, lx) };
    let t = len2 + (len2 >> 1);
    len1 - (len1 >> 5) - (len1 >> 7) + (t >> 2) + (t >> 6)
}

/// Composites a positioned coverage mask into `bitmap`, source-over, tinted
/// with `color` and scaled by `opacity` (`0..=255`).
///
/// Returns the clipped destination rectangle that received ink, or `None`
/// when the mask falls entirely outside `clip` or `bitmap`.
pub fn blit_coverage_into(
    bitmap: &mut BitmapState,
    clip: RectI,
    mask: &CoverageMask,
    dest_x: i32,
    dest_y: i32,
    color: [u8; 4],
    opacity: u8,
) -> Option<RectI> {
    if mask.is_empty() {
        return None;
    }
    let rect = (
        dest_x,
        dest_y,
        dest_x.saturating_add(mask.width as i32),
        dest_y.saturating_add(mask.height as i32),
    );
    let drawn = clip_glyph_rect(bitmap, clip, rect)?;
    let opacity = f32::from(opacity) / 255.0;
    for row in 0..mask.height as i32 {
        for column in 0..mask.width as i32 {
            let index = row as usize * mask.width as usize + column as usize;
            let coverage = mask.coverage[index];
            if coverage == 0 {
                continue;
            }
            let x = dest_x + column;
            let y = dest_y + row;
            if x < drawn.0 || y < drawn.1 || x >= drawn.2 || y >= drawn.3 {
                continue;
            }
            if let Some(offset) = bitmap.pixel_offset(x as u32, y as u32)
                && let Some(pixel) = bitmap.rgba.get_mut(offset..offset + 4)
            {
                blend_tinted_coverage(pixel, coverage, color, opacity);
            }
        }
    }
    bitmap.mark_dirty();
    Some(drawn)
}

/// Composites a run of already-rasterized glyph masks directly into `bitmap`,
/// source-over.
///
/// Unlike [`compose_text_cpu`], this needs no [`GlyphAtlas`] or render
/// pipeline: it blends the tinted coverage into the existing bitmap in place.
/// That is the CPU path a script-visible `Layer.drawText`/`ImageFunction.drawText`
/// needs, since those natives run without a renderer. The masks are the sharp
/// coverage [`rasterize_text_coverage`] already produced, so the main pass does
/// not rasterize the glyphs a second time.
///
/// `origin` is the pen start in bitmap coordinates. `angle` rotates each
/// glyph's rasterized box around `origin`, in radians. `clip` is a half-open
/// pixel rectangle intersected with the bitmap bounds. `opacity` is `0..=255`.
///
/// Returns the union of the drawn glyph rectangles, or `None` when nothing had
/// ink.
///
/// # Errors
///
/// Returns [`TextError::InvalidSize`] for a bad text size.
pub fn compose_text_into(
    masks: &[CoverageMask],
    style: &TextStyle,
    origin: (f32, f32),
    angle: f32,
    bitmap: &mut BitmapState,
    clip: RectI,
    opacity: u8,
) -> Result<Option<RectI>, TextError> {
    validate_size(style.size_px)?;
    let (sin, cos) = angle.sin_cos();
    let mut ink: Option<RectI> = None;
    for mask in masks {
        let (rot_x, rot_y) = if angle == 0.0 {
            (mask.x, mask.y)
        } else {
            (mask.x * cos - mask.y * sin, mask.x * sin + mask.y * cos)
        };
        let dest_x = (origin.0 + rot_x).round() as i32;
        let dest_y = (origin.1 + rot_y).round() as i32;
        if let Some(drawn) =
            blit_coverage_into(bitmap, clip, mask, dest_x, dest_y, style.color, opacity)
        {
            ink = Some(match ink {
                None => drawn,
                Some(previous) => union_rect(previous, drawn),
            });
        }
    }
    Ok(ink)
}

/// Intersects `rect` with `clip` and the bitmap's own bounds.
fn clip_glyph_rect(bitmap: &BitmapState, clip: RectI, rect: RectI) -> Option<RectI> {
    let bounds = (
        0,
        0,
        bitmap.width.min(i32::MAX as u32) as i32,
        bitmap.height.min(i32::MAX as u32) as i32,
    );
    let clipped = intersect_rect(rect, clip)?;
    intersect_rect(clipped, bounds)
}

/// The smallest rectangle covering both `a` and `b`.
fn union_rect(a: RectI, b: RectI) -> RectI {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

/// Returns [`TextError::InvalidSize`] unless `size_px` is finite and positive.
fn validate_size(size_px: f32) -> Result<(), TextError> {
    if size_px.is_finite() && size_px > 0.0 {
        Ok(())
    } else {
        Err(TextError::InvalidSize(size_px))
    }
}

/// Expands 8-bit coverage into straight-alpha white RGBA.
///
/// The atlas keeps only coverage (`alpha = coverage`, RGB white) so one cached
/// cell can be tinted with any text colour at draw time without re-rasterizing.
fn coverage_rgba(bitmap: &GlyphBitmap) -> Vec<u8> {
    let mut out = vec![0u8; (bitmap.width as usize) * (bitmap.height as usize) * 4];
    for (pixel, &coverage) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(bitmap.coverage.iter())
    {
        pixel[0] = 255;
        pixel[1] = 255;
        pixel[2] = 255;
        pixel[3] = coverage;
    }
    out
}

/// Converts a straight-alpha `[u8; 4]` colour into a `[f32; 4]` node tint.
fn rgba_tint(color: [u8; 4]) -> [f32; 4] {
    [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        color[3] as f32 / 255.0,
    ]
}

/// Blends one coverage byte tinted by `color` into `dst`, scaled by `opacity`.
///
/// The atlas stores straight-alpha coverage (white RGB, coverage in alpha), so
/// the tinted source pixel is `[r, g, b, coverage × a / 255]`. This reproduces
/// exactly the pixels the previous colour-baked atlas produced, keeping the CPU
/// `getPixel` baseline byte-for-byte stable.
fn blend_tinted_coverage(dst: &mut [u8], coverage: u8, color: [u8; 4], opacity: f32) {
    let alpha = ((coverage as u16 * color[3] as u16) / 255) as u8;
    let src = [color[0], color[1], color[2], alpha];
    blend_source_over(dst, &src, opacity);
}

/// Source-over blends one straight-alpha RGBA pixel `src` into `dst`, scaled
/// by `opacity`.
fn blend_source_over(dst: &mut [u8], src: &[u8], opacity: f32) {
    if dst.len() < 4 || src.len() < 4 {
        return;
    }
    let source_alpha = src[3] as f32 / 255.0 * opacity;
    if source_alpha <= 0.0 {
        return;
    }
    let dest_alpha = dst[3] as f32 / 255.0;
    let out_alpha = source_alpha + dest_alpha * (1.0 - source_alpha);
    if out_alpha <= 0.0 {
        dst[..4].fill(0);
        return;
    }
    for channel in 0..3 {
        let source = src[channel] as f32 / 255.0;
        let destination = dst[channel] as f32 / 255.0;
        let out =
            (source * source_alpha + destination * dest_alpha * (1.0 - source_alpha)) / out_alpha;
        dst[channel] = (out.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    dst[3] = (out_alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
}

/// Clamps an opacity to `[0, 1]`, mapping NaN to `0`.
fn clamp01(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// System fonts tried by the tests; a missing font skips rather than
    /// fails.
    const TEST_FONT_PATHS: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    ];

    /// Reads the first available test font, if any.
    fn load_test_font() -> Option<Vec<u8>> {
        TEST_FONT_PATHS
            .iter()
            .find_map(|path| std::fs::read(path).ok())
    }

    /// Reference: `tTVPNativeBaseBitmap::GetTextSize` sums the rasterizer's
    /// per-character extent, one UTF-16 unit at a time
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1435-1473`). A
    /// character's advance therefore depends only on the face, the character,
    /// the size and the synthetic style, and a memoized advance must be the
    /// rasterizer's own value.
    #[test]
    fn a_memoized_advance_is_the_rasterizer_value() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let font = engine.add_font(&bytes, 0).expect("register font");
        let style = TextStyle::new(24.0);

        let first = engine
            .glyph_advance(font, 'A', &style)
            .expect("measure the advance");
        let second = engine
            .glyph_advance(font, 'A', &style)
            .expect("measure the advance");
        assert_eq!(
            first.to_bits(),
            second.to_bits(),
            "a repeated measurement must return the same advance"
        );
        assert_eq!(
            engine.cached_advances(),
            1,
            "a repeated measurement must not add a cache entry"
        );

        // A second engine measures the same face bytes from scratch, so the
        // memoized value must equal the freshly measured one.
        let mut other = TextEngine::new();
        let other_font = other.add_font(&bytes, 0).expect("register font");
        let fresh = other
            .glyph_advance(other_font, 'A', &style)
            .expect("measure the advance");
        assert_eq!(
            first.to_bits(),
            fresh.to_bits(),
            "the memoized advance must equal a fresh measurement"
        );
    }

    /// Reference: the rasterizer's extent depends on the face, the character,
    /// the pixel size and the synthetic bold/italic
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1435-1473`), so
    /// each of those must select its own memo entry.
    #[test]
    fn the_advance_memo_is_keyed_by_character_size_and_style() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let font = engine.add_font(&bytes, 0).expect("register font");
        let plain = TextStyle::new(24.0);
        let mut bold = plain;
        bold.bold = true;

        engine
            .glyph_advance(font, 'A', &plain)
            .expect("measure the advance");
        assert_eq!(engine.cached_advances(), 1);

        engine
            .glyph_advance(font, 'B', &plain)
            .expect("measure the advance");
        assert_eq!(
            engine.cached_advances(),
            2,
            "another character must select its own entry"
        );

        engine
            .glyph_advance(font, 'A', &TextStyle::new(26.0))
            .expect("measure the advance");
        assert_eq!(
            engine.cached_advances(),
            3,
            "another size must select its own entry"
        );

        engine
            .glyph_advance(font, 'A', &bold)
            .expect("measure the advance");
        assert_eq!(
            engine.cached_advances(),
            4,
            "the synthetic style must select its own entry"
        );
    }

    /// Reference: `TVPInternalEnumFonts` keys the enumerated font map by every
    /// localized `TT_NAME_ID_FONT_FAMILY` entry (Japanese, Chinese, Korean,
    /// English), not only the English family name
    /// (`reference/cpp/core/visual/FontImpl.cpp:82-124`) and `TVPFindFont`
    /// matches the requested name against that map
    /// (`FontImpl.cpp:277-285`). The port's family lookup must therefore find
    /// a face by any of its family names; the match is case-insensitive.
    #[test]
    fn find_family_matches_localized_names_case_insensitively() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut db = fontdb::Database::new();
        db.load_font_data(bytes);
        // Pick a family spelling that actually has case to fold, so the test
        // exercises the case-insensitive scan rather than the exact query.
        let Some(family) = db.faces().find_map(|face| {
            face.families
                .iter()
                .map(|(name, _)| name.clone())
                .find(|name| name.chars().any(|c| c.is_ascii_uppercase()))
        }) else {
            return;
        };
        let lowercased = family.to_lowercase();

        let check = |lookup: &str| {
            let id = find_family(&db, lookup).expect("family must match");
            let face = db.face(id).expect("matched face exists");
            assert!(
                face.families
                    .iter()
                    .any(|(name, _)| name.to_lowercase() == lowercased),
                "the matched face does not carry the requested family"
            );
        };
        check(&family);
        check(&lowercased);
    }

    /// Reference: `TVPFindFont` returns the matching face from the enumerated
    /// font map, and `TVPCreateFontStream` falls back to the default face when
    /// the requested name is absent (`reference/cpp/core/visual/FontImpl.cpp:
    /// 159-164,277-285`). `load_system_font` therefore resolves an unknown
    /// family to the Japanese-capable fallback when the host has one, and
    /// reports [`TextError::FontNotFound`] when it has none.
    #[test]
    fn load_system_font_resolves_unknown_family_or_reports_absence() {
        let mut engine = TextEngine::new();
        match engine.load_system_font("this-family-is-not-installed") {
            Ok(id) => {
                let layout = engine
                    .layout(id, "A", &TextStyle::new(24.0), None)
                    .expect("the fallback face must shape");
                assert!(!layout.is_empty());
            }
            Err(TextError::FontNotFound { .. }) => {}
            Err(error) => panic!("unexpected error: {error}"),
        }
    }

    /// Copies the first available test font into a fresh temporary directory
    /// and returns the directory plus the family name `fontdb` reports for
    /// it. `None` when the host has no usable test font.
    fn explicit_font_dir_fixture() -> Option<(PathBuf, String)> {
        let path = TEST_FONT_PATHS
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())?;
        let mut probe = fontdb::Database::new();
        let ids = probe.load_font_source(fontdb::Source::File(path.clone()));
        let family = ids.into_iter().find_map(|id| {
            probe
                .face(id)
                .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
        })?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("krkr-text-fonts-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::copy(&path, dir.join(path.file_name()?)).ok()?;
        Some((dir, family))
    }

    /// Reference: `TVPEnumFontsProc` enumerates a directory's fonts into the
    /// name→path map and `TVPFindFont` matches a requested name against that
    /// map (`reference/cpp/core/visual/FontImpl.cpp:100-168,277-285`). A
    /// directory supplied to the text engine must therefore make its families
    /// resolvable even when the platform's default scan does not reach it.
    #[test]
    fn an_explicit_directory_supplies_a_resolvable_family() {
        let Some((dir, family)) = explicit_font_dir_fixture() else {
            return;
        };
        let mut db = fontdb::Database::new();
        load_font_directories(&mut db, std::slice::from_ref(&dir));
        assert!(
            find_family(&db, &family).is_some(),
            "the explicit directory must supply {family:?}"
        );

        let mut engine = TextEngine::new();
        engine.set_system_font_dirs(vec![dir.clone()]);
        let id = engine
            .load_system_font_exact(&family)
            .expect("a supplied directory must resolve its family");
        assert!(
            engine.layout(id, "A", &TextStyle::new(24.0), None).is_ok(),
            "the resolved face must shape"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reference: `TVPFindFont`/`TVPCreateFontStream` do not fabricate a face
    /// for a name that was never enumerated, and only the fallback entry
    /// point substitutes the default (`reference/cpp/core/visual/FontImpl.cpp:
    /// 159-168,277-285`). The exact query must therefore raise the typed
    /// error rather than silently picking another face.
    #[test]
    fn an_absent_exact_family_reports_a_typed_error() {
        let mut engine = TextEngine::new();
        let error = engine
            .load_system_font_exact("this-family-is-not-installed")
            .expect_err("an absent exact family must not resolve");
        assert!(matches!(error, TextError::FontNotFound { .. }));
    }

    /// Reference: `TVPCreateFontStream` looks the requested name up in the
    /// enumerated font map and falls back to the default face when the map
    /// does not hold it (`reference/cpp/core/visual/FontImpl.cpp:159-168`),
    /// and `TVPFindFont` matches the map by name
    /// (`reference/cpp/core/visual/FontImpl.cpp:277-289`). A name the map
    /// does not hold therefore reports the same absence every time it is
    /// asked for, and the memo that answers the repeat must report the very
    /// same error.
    #[test]
    fn an_absent_exact_family_reports_the_same_error_twice() {
        let mut engine = TextEngine::new();
        let family = "this-family-is-not-installed";
        let first = engine
            .load_system_font_exact(family)
            .expect_err("an absent exact family must not resolve");
        let second = engine
            .load_system_font_exact(family)
            .expect_err("an absent exact family must not resolve");
        assert!(matches!(first, TextError::FontNotFound { .. }));
        assert!(matches!(second, TextError::FontNotFound { .. }));
        assert_eq!(
            first.to_string(),
            second.to_string(),
            "the memoized miss must report the same error as the first lookup"
        );
    }

    /// Reference: the font map is keyed by family name, so a face registered
    /// under a name is found by that name
    /// (`reference/cpp/core/visual/FontImpl.cpp:277-289`); the host's
    /// installed fonts are consulted only for a name the map does not hold.
    /// A face registered after a failed lookup must therefore resolve, and
    /// the memoized miss must not shadow it.
    #[test]
    fn a_registered_face_is_not_shadowed_by_a_memoized_miss() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let id = engine.add_font(&bytes, 0).expect("register the face");
        let family = engine.family_name(id).expect("family name").to_string();
        // The memo records the family as missing, as a failed exact lookup
        // does.
        engine
            .missing_system_families
            .insert(family.to_lowercase(), ());
        let resolved = engine
            .load_system_font_exact(&family)
            .expect("the registered family must resolve");
        assert_eq!(resolved, id, "the registered face must answer the lookup");
        assert!(
            engine
                .missing_system_families
                .get(&family.to_lowercase())
                .is_none(),
            "the successful lookup must drop the memoized miss"
        );
    }

    /// A game may ship a font whose family name starts with `*`. The name
    /// lookup answers it, so the host's installed fonts are never consulted
    /// for it.
    ///
    /// Reference: the font map is keyed by family name
    /// (`reference/cpp/core/visual/FontImpl.cpp:96-131,277-289`), so a
    /// registered family resolves whatever its name looks like.
    #[test]
    fn a_registered_pseudo_family_resolves() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let id = engine.add_font(&bytes, 0).expect("register the face");
        engine.by_name.insert("*pseudo".to_string(), id);
        let resolved = engine
            .load_system_font_exact("*pseudo")
            .expect("the registered pseudo family must resolve");
        assert_eq!(resolved, id);
    }

    /// The memo describes one font database. Selecting the host's font
    /// directories rebuilds that database, so the memoized misses of the
    /// previous one are dropped with it.
    #[test]
    fn a_memoized_miss_is_dropped_with_the_font_database() {
        let mut engine = TextEngine::new();
        let family = "this-family-is-not-installed";
        assert!(matches!(
            engine.load_system_font_exact(family),
            Err(TextError::FontNotFound { .. })
        ));
        assert!(
            engine
                .missing_system_families
                .get(&family.to_lowercase())
                .is_some(),
            "the miss must be memoized"
        );
        engine.set_system_font_dirs(Vec::new());
        assert!(
            engine
                .missing_system_families
                .get(&family.to_lowercase())
                .is_none(),
            "the rebuilt font database must not keep the previous misses"
        );
    }

    /// A directory that is not mounted is an absent source, not an error.
    #[test]
    fn a_missing_directory_is_skipped() {
        let mut db = fontdb::Database::new();
        load_font_directories(&mut db, &[PathBuf::from("/nonexistent-krkr-font-dir")]);
        assert_eq!(db.len(), 0);
    }

    /// Reference: `FreeTypeFontRasterizer::GetBitmap` falls back to the face's
    /// default and first glyph and raises `TVPFontRasterizeError` when none of
    /// them can be rendered
    /// (`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:185-194`). A run
    /// whose non-whitespace characters all rasterize to no ink must therefore
    /// report a typed error rather than composite nothing.
    #[test]
    fn a_blank_run_for_a_non_whitespace_character_is_a_typed_error() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let Ok(id) = engine.add_font(&bytes, 0) else {
            return;
        };
        let style = TextStyle::new(24.0);
        // Pick a codepoint the face maps to a glyph with no outline, so the
        // rasterizer returns an empty mask for a character that should ink.
        let chosen = ['\u{2800}', '\u{200B}', '\u{2060}', '\u{FEFF}', '\u{FFFC}']
            .into_iter()
            .find(|ch| {
                requires_ink(*ch)
                    && engine
                        .rasterize_glyph(id, *ch, &style)
                        .is_ok_and(|bitmap| bitmap.is_empty())
            });
        let Some(codepoint) = chosen else {
            return;
        };
        let layout = TextLayout {
            glyphs: vec![LaidOutGlyph {
                codepoint,
                font: id,
                line: 0,
                x: 0.0,
                baseline_y: 0.0,
                advance: 0.0,
            }],
            width: 0.0,
            height: 0.0,
            line_height: 0.0,
        };
        let mut cache = TextCoverageCache::new();
        let error = rasterize_text_coverage(&mut cache, &engine, &layout, &style, true)
            .expect_err("an inked-less non-whitespace run must not render blank");
        assert!(matches!(
            error,
            TextError::Glyph(api::error::GlyphError::MissingGlyph { .. })
        ));
    }

    /// Reference: `TVPGetCharacter` looks a character up in `tTVPFontCache`
    /// keyed by `tTVPFontAndCharacterData` (face, character, `Antialiased`,
    /// `BlurLevel`, `BlurWidth`, `Blured`, `Hinting`;
    /// `reference/cpp/core/visual/CharacterData.h:89-115`) and, on a miss,
    /// rasterizes it and, when `font.Blured` is set, applies
    /// `tTVPCharacterData::Blur` before inserting
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:222-232,244-320`).
    /// A repeated draw therefore reuses the cached mask instead of outlining
    /// the glyph again, and the shadow is derived from that cached coverage.
    #[test]
    fn coverage_cache_reuses_a_rasterized_character() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let Ok(font) = engine.add_font(&bytes, 0) else {
            return;
        };
        let style = TextStyle::new(24.0);
        let layout = engine
            .layout(font, "A", &style, None)
            .expect("layout of a single Latin glyph");
        let mut cache = TextCoverageCache::new();

        let first = rasterize_text_coverage(&mut cache, &engine, &layout, &style, true)
            .expect("first rasterization");
        let after_first = cache.rasterizations();
        assert_eq!(after_first, 1, "the first draw rasterizes the glyph");

        let second = rasterize_text_coverage(&mut cache, &engine, &layout, &style, true)
            .expect("second rasterization");
        assert_eq!(
            cache.rasterizations(),
            after_first,
            "a repeated draw must reuse the cached coverage"
        );
        assert_eq!(first, second, "the cached mask must be identical");

        // The shadow is blurred from the cached sharp coverage, so it does
        // not rasterize the glyph a second time.
        let blurred = blurred_text_coverage(&mut cache, &engine, &layout, &style, true, 255, 1)
            .expect("shadow rasterization");
        assert_eq!(
            cache.rasterizations(),
            after_first,
            "the shadow must reuse the cached sharp coverage"
        );
        assert!(!blurred.is_empty(), "the shadow has ink");
        assert_ne!(blurred, first, "the blurred mask is expanded");

        // The cached mask is pen-relative, so the same character at two pen
        // positions reuses one rasterization at each correct position.
        let pair = engine
            .layout(font, "AA", &style, None)
            .expect("layout of two Latin glyphs");
        let both = rasterize_text_coverage(&mut cache, &engine, &pair, &style, true)
            .expect("reuse across positions");
        assert_eq!(
            cache.rasterizations(),
            after_first,
            "the second position must reuse the cached coverage"
        );
        assert_eq!(both.len(), 2);
        assert_ne!(both[0].x(), both[1].x(), "each glyph keeps its own pen x");
        assert_eq!(both[0].coverage(), both[1].coverage());
    }

    /// Rasterizes `text` for `font` through `cache` and returns the layout
    /// plus the positioned sharp masks.
    fn cached_run(
        engine: &mut TextEngine,
        cache: &mut TextCoverageCache,
        font: FontId,
        text: &str,
        style: &TextStyle,
    ) -> (TextLayout, Vec<CoverageMask>) {
        let layout = engine.layout(font, text, style, None).expect("layout");
        let masks =
            rasterize_text_coverage(cache, engine, &layout, style, true).expect("rasterize");
        (layout, masks)
    }

    /// The uncached oracle: rasterizes each glyph directly from the L1
    /// rasterizer, bypassing [`TextCoverageCache`], and pairs the pen-relative
    /// mask with its glyph.
    fn uncached_pen_masks(
        engine: &TextEngine,
        layout: &TextLayout,
        style: &TextStyle,
        antialias: bool,
    ) -> Vec<(CoverageMask, LaidOutGlyph)> {
        let mut masks = Vec::new();
        for glyph in &layout.glyphs {
            let bitmap = engine
                .rasterize_glyph(glyph.font, glyph.codepoint, style)
                .expect("rasterize");
            if bitmap.is_empty() {
                continue;
            }
            let mut coverage = bitmap.coverage;
            if !antialias {
                for value in &mut coverage {
                    *value = if *value >= 128 { 255 } else { 0 };
                }
            }
            let mask = CoverageMask::new(
                bitmap.bearing_x,
                -bitmap.bearing_y,
                bitmap.width,
                bitmap.height,
                coverage,
            )
            .expect("consistent coverage");
            masks.push((mask, *glyph));
        }
        masks
    }

    /// Positions pen-relative masks at their glyphs' pen positions.
    fn position_all(pen: &[(CoverageMask, LaidOutGlyph)]) -> Vec<CoverageMask> {
        pen.iter()
            .map(|(mask, glyph)| position_mask(mask, glyph))
            .collect()
    }

    /// Blits positioned masks at their own coordinates.
    fn blit_run(
        bitmap: &mut BitmapState,
        clip: RectI,
        masks: &[CoverageMask],
        color: [u8; 4],
        opacity: u8,
    ) {
        for mask in masks {
            blit_coverage_into(
                bitmap,
                clip,
                mask,
                mask.x().round() as i32,
                mask.y().round() as i32,
                color,
                opacity,
            );
        }
    }

    /// Reference: `tTVPFontAndCharacterData` keys the character cache on the
    /// face (`tTVPFont`: height, flags, angle, face), the character,
    /// `Antialiased` and the blur fields
    /// (`reference/cpp/core/visual/CharacterData.h:89-115`), and
    /// `TVPGetCharacter` rasterizes on a miss
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:244-320`). A
    /// different size or style is therefore a distinct entry and rasterizes
    /// separately, while the same size and style reuses the cached mask.
    #[test]
    fn coverage_cache_distinguishes_size_and_style() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let Ok(font) = engine.add_font(&bytes, 0) else {
            return;
        };
        let mut cache = TextCoverageCache::new();
        let base = TextStyle::new(24.0);
        let mut bold = TextStyle::new(24.0);
        bold.bold = true;
        let large = TextStyle::new(32.0);

        let _ = cached_run(&mut engine, &mut cache, font, "A", &base);
        assert_eq!(cache.rasterizations(), 1);
        let _ = cached_run(&mut engine, &mut cache, font, "A", &base);
        assert_eq!(
            cache.rasterizations(),
            1,
            "same size and style reuses the mask"
        );
        let _ = cached_run(&mut engine, &mut cache, font, "A", &bold);
        assert_eq!(cache.rasterizations(), 2, "bold is a distinct cache entry");
        let _ = cached_run(&mut engine, &mut cache, font, "A", &large);
        assert_eq!(
            cache.rasterizations(),
            3,
            "a different size is a distinct entry"
        );
    }

    /// Reference: `tTVPFontAndCharacterData` carries no colour
    /// (`reference/cpp/core/visual/CharacterData.h:89-96`); colour is a draw
    /// argument (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:907-949`),
    /// so recolouring reuses the cached mask.
    #[test]
    fn coverage_cache_reuses_a_mask_across_colours() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let Ok(font) = engine.add_font(&bytes, 0) else {
            return;
        };
        let mut cache = TextCoverageCache::new();
        let mut red = TextStyle::new(24.0);
        red.color = [255, 0, 0, 255];
        let mut blue = TextStyle::new(24.0);
        blue.color = [0, 0, 255, 255];

        let (_, red_masks) = cached_run(&mut engine, &mut cache, font, "A", &red);
        assert_eq!(cache.rasterizations(), 1);
        let (_, blue_masks) = cached_run(&mut engine, &mut cache, font, "A", &blue);
        assert_eq!(
            cache.rasterizations(),
            1,
            "colour is not part of the mask key"
        );
        assert_eq!(red_masks, blue_masks, "the coverage is colour-independent");
    }

    /// Reference: the character cache is bounded by `TVP_CH_MAX_CACHE_COUNT`
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:108`) and evicts
    /// the least recently used entry, not the oldest inserted: a hit runs
    /// `FindAndTouchWithHash`, which moves the entry to the front, and
    /// `ChopLast` removes from the tail
    /// (`reference/cpp/core/tjs2/tjsHashSearch.h:358-364,424-433,593-599`). A
    /// request after eviction is still rasterized and cached correctly.
    #[test]
    fn coverage_cache_evicts_the_least_recently_used() {
        let style = TextStyle::new(16.0);
        let synthetic = |index: usize| {
            let codepoint =
                char::from_u32(u32::try_from(index).expect("small index")).expect("codepoint");
            CoverageKey::sharp(FontId(usize::MAX), codepoint, &style, true)
        };
        let mask = || CoverageMask::new(0.0, 0.0, 1, 1, vec![128]).expect("1x1 mask");

        let mut cache = TextCoverageCache::new();
        for index in 0..MAX_TEXT_CACHE_ENTRIES {
            cache.insert(synthetic(index), mask());
        }
        assert_eq!(cache.len(), MAX_TEXT_CACHE_ENTRIES);

        // Touch the oldest entry so the least recently used one is the second
        // inserted, which distinguishes LRU from FIFO.
        let touched = synthetic(0);
        assert!(cache.get(&touched).is_some());
        cache.insert(synthetic(MAX_TEXT_CACHE_ENTRIES), mask());
        assert_eq!(
            cache.len(),
            MAX_TEXT_CACHE_ENTRIES,
            "the reference limit is a hard bound"
        );
        assert!(
            cache.get(&touched).is_some(),
            "the touched entry survives eviction"
        );
        assert!(
            cache.get(&synthetic(1)).is_none(),
            "the least recently used entry is evicted"
        );

        // A genuinely new request after eviction rasterizes once and is then
        // reused; the glyph is still correct.
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let Ok(font) = engine.add_font(&bytes, 0) else {
            return;
        };
        let before = cache.rasterizations();
        let (layout, first) = cached_run(&mut engine, &mut cache, font, "A", &style);
        assert_eq!(
            cache.rasterizations(),
            before + 1,
            "the newly requested glyph is rasterized once"
        );
        assert_eq!(
            first,
            position_all(&uncached_pen_masks(&engine, &layout, &style, true)),
            "the glyph after eviction matches a fresh rasterization"
        );
        let (_, second) = cached_run(&mut engine, &mut cache, font, "A", &style);
        assert_eq!(
            cache.rasterizations(),
            before + 1,
            "the newly cached glyph is reused"
        );
        assert_eq!(first, second);
        assert!(!first.is_empty(), "the glyph still has ink after eviction");
    }

    /// Reference: `TVPGetCharacter` returns the same rasterized coverage
    /// whether it comes from the cache or a fresh rasterization
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:244-320`), so
    /// caching must not change a single output pixel. This compares the cached
    /// path against a freshly rasterized (uncached) composition of the same
    /// run, including its blurred shadow.
    #[test]
    fn cached_and_uncached_composition_are_pixel_identical() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let Ok(font) = engine.add_font(&bytes, 0) else {
            return;
        };
        let mut style = TextStyle::new(24.0);
        style.color = [30, 60, 90, 255];
        let shadow = [200, 10, 10, 255];
        let clip = (0, 0, 256, 64);

        let mut cache = TextCoverageCache::new();
        let (layout, sharp) = cached_run(&mut engine, &mut cache, font, "Ag", &style);
        let shadowed = blurred_text_coverage(&mut cache, &engine, &layout, &style, true, 200, 2)
            .expect("shadow");

        let pen = uncached_pen_masks(&engine, &layout, &style, true);
        assert_eq!(
            sharp,
            position_all(&pen),
            "cached sharp masks match a fresh rasterization"
        );
        let shadow_uncached: Vec<CoverageMask> = pen
            .iter()
            .map(|(mask, glyph)| position_mask(&blur_coverage(mask, 200, 2).expect("blur"), glyph))
            .collect();
        assert_eq!(
            shadowed, shadow_uncached,
            "cached shadows match a fresh blur"
        );

        let mut cached_bitmap = BitmapState::new(256, 64, vec![0; 256 * 64 * 4]);
        blit_run(&mut cached_bitmap, clip, &shadowed, shadow, 220);
        compose_text_into(
            &sharp,
            &style,
            (0.0, 0.0),
            0.0,
            &mut cached_bitmap,
            clip,
            255,
        )
        .expect("compose cached");

        let mut uncached_bitmap = BitmapState::new(256, 64, vec![0; 256 * 64 * 4]);
        blit_run(&mut uncached_bitmap, clip, &shadow_uncached, shadow, 220);
        compose_text_into(
            &position_all(&pen),
            &style,
            (0.0, 0.0),
            0.0,
            &mut uncached_bitmap,
            clip,
            255,
        )
        .expect("compose uncached");

        assert_eq!(
            cached_bitmap.rgba, uncached_bitmap.rgba,
            "caching must not change a pixel"
        );
    }

    /// Reference: the `tTVPFontCache` limit is `TVP_CH_MAX_CACHE_COUNT`
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:108`) and eviction
    /// is least-recently-used: `FindAndTouchWithHash` promotes a hit and
    /// `ChopLast` removes the tail
    /// (`reference/cpp/core/tjs2/tjsHashSearch.h:358-364,424-433,593-599`).
    /// The shaped-layout memo shares that bound, stays at it by evicting one
    /// entry per insert, and keeps a re-read layout while dropping the least
    /// recently used one.
    #[test]
    fn layout_memo_evicts_the_least_recently_used_at_the_reference_bound() {
        let Some(bytes) = load_test_font() else {
            return;
        };
        let mut engine = TextEngine::new();
        let Ok(font) = engine.add_font(&bytes, 0) else {
            return;
        };
        let style = TextStyle::new(16.0);
        let wrap: Option<f32> = None;

        let key_for = |text: &str| LayoutKey {
            font,
            text: text.to_owned(),
            size_bits: style.size_px.to_bits(),
            bold: style.bold,
            italic: style.italic,
            wrap_bits: wrap.map(f32::to_bits),
        };

        let mut texts = Vec::with_capacity(MAX_TEXT_CACHE_ENTRIES + 1);
        for index in 0..MAX_TEXT_CACHE_ENTRIES {
            let text = index.to_string();
            engine
                .layout(font, &text, &style, wrap)
                .expect("layout of a distinct label");
            texts.push(text);
        }
        // Re-read the first layout so it becomes the most recently used.
        engine
            .layout(font, &texts[0], &style, wrap)
            .expect("re-read a layout");
        let text = MAX_TEXT_CACHE_ENTRIES.to_string();
        engine
            .layout(font, &text, &style, wrap)
            .expect("layout past the reference bound");
        texts.push(text);

        assert_eq!(
            engine.cached_layouts(),
            MAX_TEXT_CACHE_ENTRIES,
            "the memo must stay at the reference bound, not shrink to zero"
        );
        assert!(
            engine.layout_cache.get(&key_for(&texts[0])).is_some(),
            "the re-read layout must survive eviction"
        );
        assert!(
            engine.layout_cache.get(&key_for(&texts[1])).is_none(),
            "the least recently used layout must be evicted first"
        );
        assert!(
            engine
                .layout_cache
                .get(&key_for(texts.last().expect("inserted a layout")))
                .is_some(),
            "the most recent layout must survive eviction"
        );
    }
}
