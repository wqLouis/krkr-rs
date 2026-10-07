//! L2 text layout, glyph-atlas composition and GPU projection.
//!
//! This module is the middle-layer (L2) text seam. It deliberately does the
//! minimum itself and delegates the hard parts:
//!
//! * **Layout/shaping/bidi/line-breaking/font-fallback** are delegated to
//!   [`cosmic_text`], a pure-Rust text stack (harfrust shaping, `fontdb`
//!   discovery/fallback, `unicode-bidi`, `unicode-linebreak`). We do **not**
//!   reimplement a shaping engine.
//! * **Rasterization** is the L1 [`render`] `FontSystem` /
//!   [`render::rasterize_glyph`] / [`render::glyph_advance`] /
//!   [`render::font_metrics`] contract. L1 owns the font and the coverage
//!   bitmap; L2 owns the *atlas* that packs those bitmaps.
//! * **GPU drawing** is the existing L1 quad pipeline: laid-out glyphs are
//!   projected into a [`RenderScene`] as per-glyph [`RenderContent::Texture`]
//!   quads. We do **not** add a second text draw path and we do **not**
//!   depend on a renderer backend.
//!
//! ## Why `cosmic-text`
//!
//! `cosmic-text` is the only candidate that provides a *complete*,
//! renderer-agnostic text front end in pure Rust while leaving rasterization
//! to us:
//!
//! * `rustybuzz`/`harfrust` alone is a shaper: no bidi, no line breaking, no
//!   font discovery/fallback, no line metrics — all of which this module would
//!   have to rebuild.
//! * `swash` bundles shaping *and* rasterization; using it would duplicate
//!   (and bypass) the L1 rasterizer we already have and still leave bidi/line
//!   breaking unimplemented.
//! * `glyphon` is a `wgpu` text integration. It would pull `wgpu` into L2,
//!   violating the architecture's "no render backend in the middle layer"
//!   invariant, while our backend is `ash`/Vulkan.
//! * `vello` is a GPU 2D *renderer*, not a layout engine; it would replace the
//!   quad pipeline rather than feed it.
//!
//! `cosmic-text` is used only for its layout half, so disabling its `swash`
//! and `fontconfig` default features keeps its dependency surface small and
//! free of system-library linkage; fonts are loaded explicitly by the caller.
//!
//! ## CPU and GPU coexistence (D8 `getPixel`)
//!
//! The same rasterized coverage feeds two consumers:
//!
//! * the **GPU path** ([`project_text`]) uploads a glyph atlas once and emits
//!   textured quads, drawn by the existing pipeline; and
//! * the **CPU path** ([`compose_text_cpu`]) composites the same atlas pixels
//!   into an RGBA buffer, which is what a script-visible `getPixel`/readback
//!   must query.
//!
//! Both read the identical [`GlyphAtlas`] pixels, so the D8 correctness
//! baseline and the accelerated display agree. The CPU path is not a fallback
//! for a missing GPU: it is the authoritative, deterministic compositor; the
//! GPU path is the visual presentation of the same data.
//!
//! ## State (D9)
//!
//! Everything here is explicit caller-owned state: [`TextEngine`] holds the
//! shaping system and the registered faces, and [`GlyphAtlas`] holds the
//! atlas texture, its CPU mirror and the glyph cache. There are no globals,
//! no statics and no hidden caches.

use std::collections::HashMap;
use std::sync::Arc;

use api::geometry::{PixelRegion, RectF};
use api::glyph::{GlyphBitmap, GlyphStyle};
use render::{
    Font, FontMetrics, FontSystem, RenderContent, RenderNode, RenderNodeId, RenderPipeline,
    RenderScene, RenderState, Texture, TextureDesc, TextureFormat,
};

use cosmic_text::{Attrs, Buffer, Family, FontSystem as CosmicFontSystem, Metrics, Shaping};

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
    /// The glyph atlas has no free cell for the requested bitmap.
    ///
    /// This is never silently worked around: the caller must grow or recreate
    /// the atlas (D11).
    #[error("glyph atlas ({width}x{height}) has no room for the glyph")]
    AtlasFull {
        /// Atlas width in pixels.
        width: u32,
        /// Atlas height in pixels.
        height: u32,
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
pub struct TextStyle {
    /// Em size in pixels.
    pub size_px: f32,
    /// Synthetic bold (applied by the L1 rasterizer).
    pub bold: bool,
    /// Synthetic italic/oblique (applied by the L1 rasterizer).
    pub italic: bool,
    /// Straight-alpha RGBA text colour, baked into the atlas.
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
    pub baseline_y: f32,
    /// Shaped advance of the glyph, in pixels.
    pub advance: f32,
}

/// The result of laying out one string.
#[derive(Clone, Debug, Default, PartialEq)]
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

/// L2 text service: shaping/layout system plus registered faces.
///
/// Created once per engine and owned by it. It keeps no globals; the glyph
/// atlas and render state are separate explicit values.
pub struct TextEngine {
    /// L1 stateless rasterizer service.
    render_system: FontSystem,
    /// `cosmic-text` shaping/layout system (stateless service).
    cosmic: CosmicFontSystem,
    /// Registered faces, indexed by [`FontId`].
    faces: Vec<Face>,
    /// Lowercased family name → face id, for name-based lookup.
    by_name: HashMap<String, FontId>,
}

impl TextEngine {
    /// Creates an empty text engine with no registered faces.
    ///
    /// System font discovery is intentionally disabled: only faces added with
    /// [`TextEngine::add_font`] are available, which keeps rendering
    /// deterministic and free of host-specific font state (D11).
    pub fn new() -> Self {
        Self {
            render_system: render::create_font_system(),
            cosmic: CosmicFontSystem::new_with_locale_and_db(
                String::new(),
                cosmic_text::fontdb::Database::new(),
            ),
            faces: Vec::new(),
            by_name: HashMap::new(),
        }
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

    /// Returns the L1 face for `id`, if registered.
    pub fn face(&self, id: FontId) -> Option<&Font> {
        self.faces.get(id.0).map(|face| &face.render)
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
        if !(style.size_px.is_finite() && style.size_px > 0.0) {
            return Err(TextError::InvalidSize(style.size_px));
        }

        // Split the borrow so `cosmic` can be borrowed mutably while `faces`
        // (and the immutable `render_system`) stay borrowed.
        let Self {
            render_system,
            cosmic,
            faces,
            ..
        } = self;
        let face = faces.get(font.0).ok_or(TextError::UnknownFont(font.0))?;

        let metrics = render::font_metrics(render_system, &face.render, style.size_px);
        let measured = metrics.line_height();
        let line_height = if measured.is_finite() && measured > 0.0 {
            measured
        } else {
            style.size_px * 1.2
        };

        let attributes = Attrs::new()
            .family(Family::Name(&face.family))
            .weight(cosmic_text::Weight::NORMAL)
            .style(cosmic_text::Style::Normal);

        let mut buffer = Buffer::new(cosmic, Metrics::new(style.size_px, line_height));
        let width = wrap_width.filter(|w| w.is_finite() && *w > 0.0);
        buffer.set_size(width, None);
        buffer.set_text(text, &attributes, Shaping::Advanced, None);

        let mut glyphs = Vec::new();
        let mut max_line = 0usize;
        let mut line_width: f32 = 0.0;
        {
            let mut borrowed = buffer.borrow_with(cosmic);
            for run in borrowed.layout_runs() {
                max_line = max_line.max(run.line_i);
                line_width = line_width.max(run.line_w);
                let baseline_y = run.line_y;
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

        Ok(TextLayout {
            glyphs,
            width: line_width,
            height: (max_line + 1) as f32 * line_height,
            line_height,
        })
    }
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache key for one rasterized glyph in the atlas.
///
/// The colour is part of the key because the atlas stores straight-alpha RGBA
/// with the colour **baked in**: the existing quad pipeline tints textured
/// quads white (`[1, 1, 1, opacity]`), so per-run colour lives in the atlas,
/// not the draw call. Different colours therefore use different atlas cells.
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
    /// Baked straight-alpha RGBA colour.
    color: [u8; 4],
}

/// One glyph packed into the atlas.
#[derive(Clone, Copy, Debug, PartialEq)]
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
/// bilinear bleeding. Adding a new glyph uploads only its sub-rectangle with
/// [`render::upload_texture`]; the CPU mirror is kept so the same pixels can be
/// composited for D8 readback and re-uploaded after a render-state recreation.
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
    /// Rasterized glyphs, keyed by face/codepoint/size/style/colour.
    entries: HashMap<GlyphKey, AtlasGlyph>,
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
            entries: HashMap::new(),
        })
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

    /// Rasterizes `codepoint` (if needed) and packs it into the atlas.
    ///
    /// The glyph is cached by face, code point, size, style and colour, so a
    /// repeated request does not re-rasterize or re-upload.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::UnknownFont`], [`TextError::Glyph`],
    /// [`TextError::AtlasFull`] or [`TextError::Render`].
    pub fn glyph(
        &mut self,
        pipeline: &RenderPipeline,
        state: &mut RenderState,
        engine: &TextEngine,
        font: FontId,
        codepoint: char,
        style: &TextStyle,
    ) -> Result<AtlasGlyph, TextError> {
        if !(style.size_px.is_finite() && style.size_px > 0.0) {
            return Err(TextError::InvalidSize(style.size_px));
        }
        let key = GlyphKey {
            font: font.0,
            codepoint,
            size_bits: style.size_px.to_bits(),
            bold: style.bold,
            italic: style.italic,
            color: style.color,
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
            let rgba = bake_rgba(&bitmap, style.color);
            self.blit(x, y, bitmap.width, bitmap.height, &rgba);
            let region = PixelRegion::new(x, y, bitmap.width, bitmap.height);
            render::upload_texture(pipeline, state, &self.texture, region, &rgba)?;
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

    /// Rasterizes every glyph of `layout` and returns them positioned for
    /// projection or CPU composition.
    ///
    /// # Errors
    ///
    /// Propagates the same errors as [`GlyphAtlas::glyph`].
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
            let entry = self.glyph(pipeline, state, engine, glyph.font, glyph.codepoint, style)?;
            positioned.push(PositionedGlyph {
                glyph: entry,
                x: glyph.x,
                baseline_y: glyph.baseline_y,
                opacity,
            });
        }
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
        let stride = self.width as usize * 4;
        let row = width as usize * 4;
        for r in 0..height as usize {
            let source = r * row;
            let destination = (y as usize + r) * stride + x as usize * 4;
            if let (Some(src), Some(dst)) = (
                rgba.get(source..source + row),
                self.pixels.get_mut(destination..destination + row),
            ) {
                dst.copy_from_slice(src);
            }
        }
    }
}

/// A glyph resolved against an atlas and placed at a pen position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionedGlyph {
    /// Atlas entry for the glyph.
    pub glyph: AtlasGlyph,
    /// Pen x offset in the target coordinate space.
    pub x: f32,
    /// Baseline y offset in the target coordinate space (y grows down).
    pub baseline_y: f32,
    /// Composed opacity in `[0, 1]`.
    pub opacity: f32,
}

impl PositionedGlyph {
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
/// This is the CPU composition path used by the D8 correctness baseline
/// (`getPixel`/readback): it reads the same [`GlyphAtlas`] pixels the GPU
/// path uploads and applies straight-alpha source-over with each glyph's
/// opacity. The returned buffer starts fully transparent.
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
                    blend_source_over(dst, src, opacity);
                }
            }
        }
    }
    out
}

/// Bakes coverage into straight-alpha RGBA with the text colour.
fn bake_rgba(bitmap: &GlyphBitmap, color: [u8; 4]) -> Vec<u8> {
    let mut out = vec![0u8; (bitmap.width as usize) * (bitmap.height as usize) * 4];
    for (index, &coverage) in bitmap.coverage.iter().enumerate() {
        let alpha = ((coverage as u16 * color[3] as u16) / 255) as u8;
        out[index * 4] = color[0];
        out[index * 4 + 1] = color[1];
        out[index * 4 + 2] = color[2];
        out[index * 4 + 3] = alpha;
    }
    out
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
