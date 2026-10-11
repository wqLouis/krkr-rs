//! Shared L3 resource resolution: text faces, default-face fallback and the
//! host image codec.
//!
//! [`ResourceResolver`] is the per-engine dispatch state (never a process
//! global) that owns the L2 [`TextEngine`], the requested-face cache, the
//! registered-face order, the host image codec and the pre-rendered-font map. The visual `Font` native,
//! `Layer.drawText`/`drawGlyph`, `ImageFunction.drawText`/`drawGlyph` and
//! `Layer.saveLayerImage` compose it. The `TextRenderBase` and `LayerExDraw`
//! plugin metrics keep their own shaping state but resolve faces through
//! [`resolve_face`].
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::rc::Rc;

use api::media::{DecodedImage, ImageInfo};
use engine::layer_ops::RectI;
use engine::text::{
    CoverageMask, FontId, TextEngine, TextStyle, blit_coverage_into, blur_coverage,
    compose_text_into, rasterize_text_coverage,
};
use engine::{BitmapState, StorageReadError};
use tjs::{Tjs2Engine, TjsValue, Value};

use crate::CompatCtx;

/// Default face requested when `Font.getList`/`Font.defaultFaceName` need to
/// establish a platform face (the reference `TVPDefaultFontFaceName`).
const DEFAULT_FACE: &str = "MS Gothic";

/// Host-supplied image decoder: the L1 `media::decode_image` call behind
/// `Bitmap.load`.
type DecodeFn = dyn Fn(&[u8], Option<&str>) -> Result<DecodedImage, String>;
/// Host-supplied image header prober.
type ProbeFn = dyn Fn(&[u8], Option<&str>) -> Result<ImageInfo, String>;
/// Host-supplied image encoder/saver.
type SaveFn = dyn Fn(&str, &str, u32, u32, &[u8]) -> Result<(), String>;

/// The per-engine L3 resource resolver.
#[derive(Default)]
pub(crate) struct ResourceResolver {
    /// Shared text shaping/layout engine.
    pub(crate) engine: TextEngine,
    /// Requested face string → registered face id.
    pub(crate) faces: HashMap<String, FontId>,
    /// Registered family names in registration order (`Font.getList`).
    registered: Vec<String>,
    /// First registered family name (`Font.defaultFaceName`).
    default_face: Option<String>,
    /// Host image decoder.
    decode: Option<Rc<DecodeFn>>,
    /// Host image header prober.
    probe: Option<Rc<ProbeFn>>,
    /// Host image encoder/saver.
    save: Option<Rc<SaveFn>>,
    /// Loaded pre-rendered fonts, keyed by the requested storage spelling.
    prerendered_storage: HashMap<String, Rc<PrerenderedFont>>,
    /// Font-value → pre-rendered font mappings, in registration order.
    prerendered: Vec<(PrerenderedKey, Rc<PrerenderedFont>)>,
}

/// Borrow the per-engine [`ResourceResolver`].
pub(crate) fn resource_state(ctx: &CompatCtx) -> std::cell::RefMut<'_, ResourceResolver> {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    unsafe { ctx.services() }.state::<ResourceResolver>()
}

impl ResourceResolver {
    /// Install (or replace) the host image codec closures.
    pub(crate) fn set_codec(
        &mut self,
        decode: impl Fn(&[u8], Option<&str>) -> Result<DecodedImage, String> + 'static,
        probe: impl Fn(&[u8], Option<&str>) -> Result<ImageInfo, String> + 'static,
        save: impl Fn(&str, &str, u32, u32, &[u8]) -> Result<(), String> + 'static,
    ) {
        self.decode = Some(Rc::new(decode));
        self.probe = Some(Rc::new(probe));
        self.save = Some(Rc::new(save));
    }

    /// Decode image bytes through the host decoder.
    pub(crate) fn decode(&self, bytes: &[u8], hint: Option<&str>) -> Result<DecodedImage, String> {
        let decoder = self
            .decode
            .clone()
            .ok_or_else(|| "no image decoder is installed".to_string())?;
        decoder(bytes, hint)
    }

    /// Probe image bytes through the host prober.
    pub(crate) fn probe(&self, bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, String> {
        let prober = self
            .probe
            .clone()
            .ok_or_else(|| "no image header prober is installed".to_string())?;
        prober(bytes, hint)
    }

    /// Encode and write a bitmap through the host saver.
    pub(crate) fn save_image(
        &self,
        name: &str,
        type_name: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), String> {
        let saver = self
            .save
            .clone()
            .ok_or_else(|| "no image encoder is installed".to_string())?;
        saver(name, type_name, width, height, rgba)
    }

    /// Maps the font value `key` to the pre-rendered font `storage`.
    ///
    /// The storage is read and validated now; a missing or malformed file is
    /// reported, matching `TVPMapPrerenderedFont`
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:127-167`) and
    /// `tTVPPrerenderedFont` (`reference/cpp/core/visual/PrerenderedFont.cpp:9-76`).
    pub(crate) fn map_prerendered(
        &mut self,
        ctx: &CompatCtx,
        key: PrerenderedKey,
        storage: &str,
    ) -> Result<(), String> {
        let font = self.load_prerendered(ctx, storage)?;
        if let Some(index) = self
            .prerendered
            .iter()
            .position(|(mapped, _)| *mapped == key)
        {
            self.prerendered[index].1 = font;
        } else {
            self.prerendered.push((key, font));
        }
        Ok(())
    }

    /// Drops the mapping for `key` (the reference `TVPUnmapPrerenderedFont`,
    /// `LayerBitmapImpl.cpp:169-185`).
    pub(crate) fn unmap_prerendered(&mut self, key: &PrerenderedKey) {
        self.prerendered.retain(|(k, _)| k != key);
    }

    /// The pre-rendered font mapped to `key`, if any.
    pub(crate) fn prerendered_for(&self, key: &PrerenderedKey) -> Option<Rc<PrerenderedFont>> {
        self.prerendered
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, font)| Rc::clone(font))
    }

    /// Loads (or reuses) the pre-rendered font object for `storage`.
    fn load_prerendered(
        &mut self,
        ctx: &CompatCtx,
        storage: &str,
    ) -> Result<Rc<PrerenderedFont>, String> {
        if let Some(font) = self.prerendered_storage.get(storage) {
            return Ok(Rc::clone(font));
        }
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        let Some(store) = services.storage() else {
            return Err(format!("Cannot find storage {storage}"));
        };
        // SAFETY: the host keeps the attached storage alive for the binding's
        // lifetime (the `attach_storage` contract).
        let store = unsafe { &*store };
        let bytes = match store.read(storage) {
            Ok(bytes) => bytes,
            Err(StorageReadError::NotFound(_)) => {
                return Err(format!("Cannot find storage {storage}"));
            }
            Err(error) => return Err(error.to_string()),
        };
        let font = Rc::new(PrerenderedFont::parse(bytes).map_err(|error| error.to_string())?);
        self.prerendered_storage
            .insert(storage.to_string(), Rc::clone(&font));
        Ok(font)
    }

    /// Resolve `face`, recording the registered family name for
    /// `Font.getList`/`Font.defaultFaceName`.
    pub(crate) fn resolve_face(&mut self, ctx: &CompatCtx, face: &str) -> Result<FontId, String> {
        let id = resolve_face(ctx, &mut self.engine, &mut self.faces, face)?;
        self.record(id);
        Ok(id)
    }

    /// Record the family name behind `id` in registration order.
    fn record(&mut self, id: FontId) {
        let Some(name) = self.engine.family_name(id).map(str::to_owned) else {
            return;
        };
        if !self.registered.iter().any(|entry| entry == &name) {
            self.registered.push(name.clone());
        }
        if self.default_face.is_none() {
            self.default_face = Some(name);
        }
    }

    /// The registered face family names, establishing the platform default on
    /// first use.
    pub(crate) fn face_names(&mut self, ctx: &CompatCtx) -> Vec<String> {
        if self.registered.is_empty() {
            let _ = self.resolve_face(ctx, DEFAULT_FACE);
        }
        self.registered.clone()
    }

    /// The default face name, or the empty string when no face is registered.
    pub(crate) fn default_face_name(&mut self, ctx: &CompatCtx) -> String {
        if self.default_face.is_none() {
            let _ = self.resolve_face(ctx, DEFAULT_FACE);
        }
        self.default_face.clone().unwrap_or_default()
    }
}

/// Resolve `face` to a face registered on `engine`.
///
/// The face may be a comma-separated candidate list with an optional leading
/// `@` (vertical writing). Each candidate is tried through the
/// `System.addFont` registry, the attached storage, then the host's installed
/// system fonts. When no candidate resolves, the host's Japanese-capable CJK
/// fallback is tried, then the engine's first registered face (the reference
/// `TVPCreateFontStream` default,
/// `reference/cpp/core/visual/FontImpl.cpp:159-168`); only an engine with no
/// registered face at all raises.
pub(crate) fn resolve_face(
    ctx: &CompatCtx,
    engine: &mut TextEngine,
    cache: &mut HashMap<String, FontId>,
    face: &str,
) -> Result<FontId, String> {
    if let Some(id) = cache.get(face).copied() {
        return Ok(id);
    }
    let candidates = face.strip_prefix('@').unwrap_or(face);
    for candidate in candidates.split(',') {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        if let Ok(id) = try_face(ctx, engine, candidate) {
            cache.insert(face.to_string(), id);
            return Ok(id);
        }
    }
    // No candidate is registered, in storage or an installed family: try the
    // host's Japanese-capable CJK fallback for the requested name
    // (`FontImpl.cpp:159-164`), then the engine's first registered face
    // (`TVPCreateFontStream`'s default).
    if let Ok(id) = engine.load_system_font(face) {
        cache.insert(face.to_string(), id);
        return Ok(id);
    }
    if let Some(id) = engine.default_font() {
        cache.insert(face.to_string(), id);
        return Ok(id);
    }
    Err(format!("no font is available for face '{face}'"))
}

/// Try one face candidate through the `System.addFont` registry, the attached
/// storage and the host's installed system fonts (exact family only).
fn try_face(ctx: &CompatCtx, engine: &mut TextEngine, face: &str) -> Result<FontId, String> {
    if let Some(id) = engine.font_id(face) {
        return Ok(id);
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let registered = unsafe { ctx.services() }
        .state::<crate::natives::plugins::add_font::registry::FontRegistry>()
        .lookup(face);
    if let Some((bytes, index, _)) = registered {
        return engine
            .add_font(&bytes, index)
            .map_err(|error| format!("face '{face}' is not a usable font: {error}"));
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    if let Some(storage) = unsafe { ctx.services() }.storage() {
        // SAFETY: the host keeps the attached storage alive for the binding's
        // lifetime (the `attach_storage` contract).
        let storage = unsafe { &*storage };
        match storage.read(face) {
            Ok(bytes) => {
                return engine
                    .add_font(&bytes, 0)
                    .map_err(|error| format!("face '{face}' is not a usable font: {error}"));
            }
            Err(StorageReadError::NotFound(_)) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    engine
        .load_system_font_exact(face)
        .map_err(|error| error.to_string())
}

/// Extract the text a `drawGlyph` glyph argument carries: a string, or an
/// object's `text`/`char`/`character` member.
pub(crate) fn glyph_text(host: &Tjs2Engine, value: &Value) -> Option<String> {
    match value.ty {
        tjs::VAL_STRING => Some(crate::native_util::value_as_string(value)),
        tjs::VAL_OBJECT if !value.object_handle().is_null() => {
            let object = host.retain_object_arg(value).ok()?;
            for name in ["text", "char", "character"] {
                if let Ok(TjsValue::String(text)) = host.member(object.raw_id(), name) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}

/// The script parameters shared by the text-draw natives.
pub(crate) struct TextDrawParams<'a> {
    /// The text to draw.
    pub(crate) text: &'a str,
    /// Pen x in destination pixels.
    pub(crate) x: i32,
    /// Pen y in destination pixels.
    pub(crate) y: i32,
    /// Straight-alpha text colour (the alpha is replaced by `opacity`).
    pub(crate) color: [u8; 4],
    /// Text opacity, `0..=255`.
    pub(crate) opacity: i32,
    /// Whether to antialias the coverage.
    pub(crate) antialias: bool,
    /// Shadow strength (the reference `shadowlevel`).
    pub(crate) shadow_level: i32,
    /// Straight-alpha shadow colour.
    pub(crate) shadow_color: [u8; 4],
    /// Shadow spread (the reference `shadowwidth`).
    pub(crate) shadow_width: i32,
    /// Shadow offset `(x, y)`.
    pub(crate) shadow_offset: (i32, i32),
}

/// Draw `params` into `bitmap`, resolving `face` through `resolver`.
///
/// `clip` is the layer/bitmap clip rectangle. Returns the union of the drawn
/// glyph rectangles, or `None` when nothing had ink.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_text_into_bitmap(
    resolver: &mut ResourceResolver,
    ctx: &CompatCtx,
    bitmap: &mut BitmapState,
    face: &str,
    size_px: f32,
    bold: bool,
    italic: bool,
    angle_tenths: f64,
    clip: RectI,
    params: &TextDrawParams<'_>,
) -> Result<Option<RectI>, String> {
    let font = resolver.resolve_face(ctx, face)?;
    let mut style = TextStyle::new(size_px);
    style.bold = bold;
    style.italic = italic;
    style.color = [params.color[0], params.color[1], params.color[2], 255];
    let key = PrerenderedKey {
        face: face.to_string(),
        height: size_px as i32,
        bold,
        italic,
        angle: angle_tenths.round() as i32,
    };
    if let Some(prerendered) = resolver.prerendered_for(&key) {
        return draw_prerendered_text(
            resolver,
            bitmap,
            font,
            &prerendered,
            size_px,
            &style,
            angle_tenths,
            clip,
            params,
        );
    }
    let layout = resolver
        .engine
        .layout(font, params.text, &style, None)
        .map_err(|error| format!("text layout failed: {error}"))?;
    let angle = (angle_tenths * std::f64::consts::PI / 1800.0) as f32;
    let opacity = params.opacity.clamp(0, 255) as u8;
    let mut ink: Option<RectI> = None;
    // A shadow exists only when `shadowlevel` is nonzero; `shadowwidth` is
    // the blur radius the shadow coverage is expanded by.
    if params.shadow_level != 0 {
        // Each character's own coverage is blurred with the cone kernel and
        // drawn once at the text opacity, before the main pass.
        let level = params.shadow_level.clamp(0, 255) as u8;
        let masks = rasterize_text_coverage(&resolver.engine, &layout, &style, params.antialias)
            .map_err(|error| format!("shadow rasterization failed: {error}"))?;
        let base = (
            (params.x + params.shadow_offset.0) as f32,
            (params.y + params.shadow_offset.1) as f32,
        );
        let (sin, cos) = angle.sin_cos();
        for mask in &masks {
            let blurred = blur_coverage(mask, level, params.shadow_width)
                .map_err(|error| format!("shadow blur failed: {error}"))?;
            let (rot_x, rot_y) = if angle == 0.0 {
                (blurred.x(), blurred.y())
            } else {
                (
                    blurred.x() * cos - blurred.y() * sin,
                    blurred.x() * sin + blurred.y() * cos,
                )
            };
            let dest_x = (base.0 + rot_x).round() as i32;
            let dest_y = (base.1 + rot_y).round() as i32;
            if let Some(rect) = blit_coverage_into(
                bitmap,
                clip,
                &blurred,
                dest_x,
                dest_y,
                params.shadow_color,
                opacity,
            ) {
                ink = Some(match ink {
                    None => rect,
                    Some(previous) => union(previous, rect),
                });
            }
        }
    }
    let origin = (params.x as f32, params.y as f32);
    if let Some(rect) = compose_text_into(
        &resolver.engine,
        &layout,
        &style,
        origin,
        angle,
        bitmap,
        clip,
        opacity,
        params.antialias,
    )
    .map_err(|error| format!("text composition failed: {error}"))?
    {
        ink = Some(match ink {
            None => rect,
            Some(previous) => union(previous, rect),
        });
    }
    Ok(ink)
}

/// The smallest rectangle covering both `a` and `b`.
fn union(a: RectI, b: RectI) -> RectI {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

/// A positioned pre-rendered or fallback glyph mask.
struct PrerenderedGlyph {
    mask: CoverageMask,
    x: i32,
    y: i32,
}

/// Draws a run whose font is mapped to a pre-rendered `.tft`.
///
/// Each character uses the `.tft` descriptor's coverage, origin and ink width;
/// a character the file does not carry falls back to the rasterizer. The pen
/// advances by the descriptor's `IncX`/`IncY`, and the glyph origin includes
/// the reference's ascent offset
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:715-728,1275-1433`).
#[allow(clippy::too_many_arguments)]
fn draw_prerendered_text(
    resolver: &ResourceResolver,
    bitmap: &mut BitmapState,
    font: FontId,
    prerendered: &PrerenderedFont,
    size_px: f32,
    style: &TextStyle,
    angle_tenths: f64,
    clip: RectI,
    params: &TextDrawParams<'_>,
) -> Result<Option<RectI>, String> {
    let ascent = f64::from(
        resolver
            .engine
            .font_metrics(font, size_px)
            .map_err(|error| format!("text metrics failed: {error}"))?
            .ascent,
    );
    let angle = angle_tenths * std::f64::consts::PI / 1800.0;
    let angle90 = angle + std::f64::consts::FRAC_PI_2;
    // The reference truncates the ascent projection to an integer
    // (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:722-728`).
    let ascent_ofs_x = (-angle90.cos() * ascent) as i32;
    let ascent_ofs_y = (angle90.sin() * ascent) as i32;
    let opacity = params.opacity.clamp(0, 255) as u8;

    let mut pen_x = params.x as f64;
    let mut pen_y = params.y as f64;
    let mut glyphs: Vec<PrerenderedGlyph> = Vec::new();
    for unit in params.text.encode_utf16() {
        if let Some(item) = prerendered.find_code_unit(unit) {
            if item.width != 0 && item.height != 0 {
                let coverage = prerendered
                    .retrieve(item)
                    .map_err(|error| error.to_string())?;
                let mask = CoverageMask::new(
                    0.0,
                    0.0,
                    u32::from(item.width),
                    u32::from(item.height),
                    coverage,
                )
                .map_err(|error| format!("pre-rendered glyph: {error}"))?;
                glyphs.push(PrerenderedGlyph {
                    mask,
                    x: pen_x.round() as i32 + i32::from(item.origin_x) + ascent_ofs_x,
                    y: pen_y.round() as i32 - i32::from(item.origin_y) + ascent_ofs_y,
                });
            }
            pen_x += f64::from(item.inc_x);
            if item.inc_y != 0 {
                let mut inc_y = item.inc_y;
                if angle_tenths < 1800.0 {
                    if inc_y > 0 {
                        inc_y = -inc_y;
                    }
                } else if inc_y < 0 {
                    inc_y = -inc_y;
                }
                pen_y += f64::from(inc_y);
            }
        } else {
            let ch = char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}');
            let glyph = resolver
                .engine
                .rasterize_glyph(font, ch, style)
                .map_err(|error| format!("text rasterization failed: {error}"))?;
            if glyph.width != 0 && glyph.height != 0 {
                let mask =
                    CoverageMask::new(0.0, 0.0, glyph.width, glyph.height, glyph.coverage.clone())
                        .map_err(|error| format!("glyph mask: {error}"))?;
                glyphs.push(PrerenderedGlyph {
                    mask,
                    x: pen_x.round() as i32 + glyph.bearing_x.round() as i32 + ascent_ofs_x,
                    y: pen_y.round() as i32 + ascent.round() as i32
                        - glyph.bearing_y.round() as i32,
                });
            }
            let advance = f64::from(glyph.advance);
            if angle_tenths == 0.0 {
                pen_x += advance;
            } else if angle_tenths == 2700.0 {
                pen_y += advance;
            } else {
                pen_x += angle.cos() * advance;
                pen_y += -angle.sin() * advance;
            }
        }
    }

    let mut ink: Option<RectI> = None;
    if params.shadow_level != 0 {
        let level = params.shadow_level.clamp(0, 255) as u8;
        for glyph in &glyphs {
            let blurred = blur_coverage(&glyph.mask, level, params.shadow_width)
                .map_err(|error| format!("shadow blur failed: {error}"))?;
            let x = glyph.x.saturating_add(params.shadow_offset.0);
            let y = glyph.y.saturating_add(params.shadow_offset.1);
            if let Some(rect) =
                blit_coverage_into(bitmap, clip, &blurred, x, y, params.shadow_color, opacity)
            {
                ink = Some(match ink {
                    None => rect,
                    Some(previous) => union(previous, rect),
                });
            }
        }
    }
    for glyph in &glyphs {
        if let Some(rect) = blit_coverage_into(
            bitmap,
            clip,
            &glyph.mask,
            glyph.x,
            glyph.y,
            style.color,
            opacity,
        ) {
            ink = Some(match ink {
                None => rect,
                Some(previous) => union(previous, rect),
            });
        }
    }
    Ok(ink)
}

// ---------------------------------------------------------------------------
// Pre-rendered fonts (`.tft`)
// ---------------------------------------------------------------------------

/// The file signature (21 bytes plus the trailing `\x1a`).
const PRERENDERED_SIGNATURE: &[u8] = b"TVP pre-rendered font\x1a";

/// Size of the fixed header preceding the index tables.
const PRERENDERED_HEADER_LEN: usize = 36;

/// Size of one packed `tTVPPrerenderedCharacterItem`.
const PRERENDERED_ITEM_LEN: usize = 20;

/// The font value a pre-rendered mapping is stored under.
///
/// The reference keys the map by `tTVPFont` equality — height, flags, angle
/// and face (`reference/cpp/core/visual/tvpfontstruc.h:28-32`). The compat
/// metric and draw seams carry face, height, bold, italic and angle, so the
/// key covers exactly those.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PrerenderedKey {
    /// Requested face string.
    pub(crate) face: String,
    /// Requested height in pixels.
    pub(crate) height: i32,
    /// Bold flag.
    pub(crate) bold: bool,
    /// Italic flag.
    pub(crate) italic: bool,
    /// Rotation in tenths of a degree.
    pub(crate) angle: i32,
}

/// Errors produced while mapping or decoding a pre-rendered font.
///
/// The message text matches the reference's `TVPPrerenderedFontMappingFailed`
/// sub-messages (`reference/cpp/core/base/impl/MsgImpl.h:58-59`,
/// `reference/cpp/core/base/MsgIntfInc.h:218-224`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum PrerenderedFontError {
    /// The signature did not match.
    #[error(
        "Prerendered font mapping failed : Signature not found or invalid pre-rendered font file."
    )]
    InvalidSignature,
    /// The file is not a 16-bit Unicode font.
    #[error("Prerendered font mapping failed : Not a 16-bit UNICODE font file.")]
    NotUnicode16,
    /// The header version is neither 0 nor 1.
    #[error("Prerendered font mapping failed : Invalid header version.")]
    InvalidVersion,
    /// The file is empty.
    #[error("Prerendered font mapping failed : File size is zero.")]
    ZeroSize,
    /// The file is truncated or the index points outside it.
    #[error("Prerendered font mapping failed : File read error.")]
    Read,
    /// A size computation overflowed.
    #[error("Prerendered font mapping failed : Memory allocation error.")]
    Allocation,
}

/// One packed character descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CharItem {
    /// Absolute offset of the compressed coverage inside the file.
    pub(crate) offset: u32,
    /// Coverage width in pixels.
    pub(crate) width: u16,
    /// Coverage height in pixels.
    pub(crate) height: u16,
    /// Horizontal origin offset.
    pub(crate) origin_x: i16,
    /// Vertical origin offset (negated when drawing).
    pub(crate) origin_y: i16,
    /// Horizontal pen advance.
    pub(crate) inc_x: i16,
    /// Vertical pen advance.
    pub(crate) inc_y: i16,
    /// Advance used by the text-width sum.
    pub(crate) inc: i16,
}

/// A parsed pre-rendered font.
pub(crate) struct PrerenderedFont {
    /// Data version (0 or 1).
    version: u8,
    /// Sorted UTF-16 code units, parallel to `items`.
    ch_index: Vec<u16>,
    /// Character descriptors, indexed like `ch_index`.
    items: Vec<CharItem>,
    /// The whole file, indexed by a descriptor's `offset`.
    image: Vec<u8>,
}

impl PrerenderedFont {
    /// Parses `bytes` as a `.tft`.
    ///
    /// # Errors
    ///
    /// Returns a [`PrerenderedFontError`] for a bad signature, wrong Unicode
    /// width or version, an empty file, or a truncated/overflowing index.
    pub(crate) fn parse(bytes: Vec<u8>) -> Result<Self, PrerenderedFontError> {
        if bytes.len() < PRERENDERED_HEADER_LEN {
            if bytes.is_empty() {
                return Err(PrerenderedFontError::ZeroSize);
            }
            return Err(PrerenderedFontError::InvalidSignature);
        }
        if !bytes.starts_with(PRERENDERED_SIGNATURE) {
            return Err(PrerenderedFontError::InvalidSignature);
        }
        if bytes[23] != 2 {
            return Err(PrerenderedFontError::NotUnicode16);
        }
        let version = bytes[22];
        if version != 0 && version != 1 {
            return Err(PrerenderedFontError::InvalidVersion);
        }

        let index_count = read_u32(&bytes, 24)? as usize;
        let ch_index_offset = read_u32(&bytes, 28)? as usize;
        let index_offset = read_u32(&bytes, 32)? as usize;

        let ch_index_len = index_count
            .checked_mul(2)
            .ok_or(PrerenderedFontError::Allocation)?;
        let index_len = index_count
            .checked_mul(PRERENDERED_ITEM_LEN)
            .ok_or(PrerenderedFontError::Allocation)?;
        let ch_index_end = ch_index_offset
            .checked_add(ch_index_len)
            .ok_or(PrerenderedFontError::Read)?;
        let index_end = index_offset
            .checked_add(index_len)
            .ok_or(PrerenderedFontError::Read)?;
        if ch_index_end > bytes.len() || index_end > bytes.len() {
            return Err(PrerenderedFontError::Read);
        }

        let mut ch_index = Vec::with_capacity(index_count);
        for i in 0..index_count {
            let at = ch_index_offset + i * 2;
            ch_index.push(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
        }

        let mut items = Vec::with_capacity(index_count);
        for i in 0..index_count {
            let at = index_offset + i * PRERENDERED_ITEM_LEN;
            items.push(parse_char_item(&bytes[at..at + PRERENDERED_ITEM_LEN])?);
        }

        Ok(Self {
            version,
            ch_index,
            items,
            image: bytes,
        })
    }

    /// Returns the descriptor for a UTF-16 code unit, or `None`.
    ///
    /// This is the reference's binary search over the sorted `ChIndex`
    /// (`reference/cpp/core/visual/PrerenderedFont.cpp:94-112`).
    pub(crate) fn find_code_unit(&self, ch: u16) -> Option<&CharItem> {
        let count = self.ch_index.len();
        if count == 0 {
            return None;
        }
        let mut s = 0usize;
        let mut e = count;
        loop {
            let d = e - s;
            if d <= 1 {
                return if self.ch_index[s] == ch {
                    self.items.get(s)
                } else {
                    None
                };
            }
            let m = s + (d >> 1);
            if self.ch_index[m] > ch {
                e = m;
            } else {
                s = m;
            }
        }
    }

    /// Decodes a descriptor's coverage into a tight `width * height` mask.
    ///
    /// # Errors
    ///
    /// Returns [`PrerenderedFontError::Read`] when the compressed data is
    /// truncated or expands past the descriptor's area.
    pub(crate) fn retrieve(&self, item: &CharItem) -> Result<Vec<u8>, PrerenderedFontError> {
        let width = usize::from(item.width);
        let height = usize::from(item.height);
        if width == 0 || height == 0 {
            return Ok(Vec::new());
        }
        let total = width
            .checked_mul(height)
            .ok_or(PrerenderedFontError::Allocation)?;
        let mut out: Vec<u8> = Vec::with_capacity(total);
        let mut ptr = item.offset as usize;
        while out.len() < total {
            if ptr >= self.image.len() {
                return Err(PrerenderedFontError::Read);
            }
            let byte = self.image[ptr];
            ptr += 1;
            let run_len = if self.version == 0 {
                if byte == 0x41 {
                    if out.is_empty() || ptr >= self.image.len() {
                        return Err(PrerenderedFontError::Read);
                    }
                    let len = self.image[ptr] as usize;
                    ptr += 1;
                    Some(len)
                } else {
                    out.push(byte);
                    None
                }
            } else if byte >= 0x41 {
                Some((byte - 0x40) as usize)
            } else {
                out.push(byte);
                None
            };
            if let Some(len) = run_len {
                if len == 0 {
                    continue;
                }
                let last = out.last().copied().ok_or(PrerenderedFontError::Read)?;
                if out.len() + len > total {
                    return Err(PrerenderedFontError::Read);
                }
                out.resize(out.len() + len, last);
            }
        }
        for value in &mut out {
            *value = (u32::from(*value) * 4).min(255) as u8;
        }
        Ok(out)
    }
}

/// Reads a little-endian `u32` at `offset`.
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, PrerenderedFontError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(PrerenderedFontError::Read)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Parses one packed character descriptor.
fn parse_char_item(bytes: &[u8]) -> Result<CharItem, PrerenderedFontError> {
    let u16_at = |offset: usize| -> Result<u16, PrerenderedFontError> {
        let slice = bytes
            .get(offset..offset + 2)
            .ok_or(PrerenderedFontError::Read)?;
        Ok(u16::from_le_bytes([slice[0], slice[1]]))
    };
    let i16_at =
        |offset: usize| -> Result<i16, PrerenderedFontError> { Ok(u16_at(offset)? as i16) };
    Ok(CharItem {
        offset: read_u32(bytes, 0)?,
        width: u16_at(4)?,
        height: u16_at(6)?,
        origin_x: i16_at(8)?,
        origin_y: i16_at(10)?,
        inc_x: i16_at(12)?,
        inc_y: i16_at(14)?,
        inc: i16_at(16)?,
    })
}

#[cfg(test)]
mod prerendered_tests {
    use super::*;

    /// Builds a version-1 `.tft` for `chars`, each a `2x2` coverage whose four
    /// literal bytes are `0x10, 0x20, 0x30, 0x3F` and whose descriptor `inc`
    /// is `inc`.
    fn build_tft(chars: &[u16], inc: i16) -> Vec<u8> {
        let header = PRERENDERED_HEADER_LEN;
        let ch_index_offset = header;
        let index_offset = ch_index_offset + chars.len() * 2;
        let data_offset = index_offset + chars.len() * PRERENDERED_ITEM_LEN;
        let mut bytes = vec![0u8; data_offset];
        bytes[..PRERENDERED_SIGNATURE.len()].copy_from_slice(PRERENDERED_SIGNATURE);
        bytes[22] = 1;
        bytes[23] = 2;
        bytes[24..28].copy_from_slice(&(chars.len() as u32).to_le_bytes());
        bytes[28..32].copy_from_slice(&(ch_index_offset as u32).to_le_bytes());
        bytes[32..36].copy_from_slice(&(index_offset as u32).to_le_bytes());
        for (i, ch) in chars.iter().enumerate() {
            bytes[ch_index_offset + i * 2..ch_index_offset + i * 2 + 2]
                .copy_from_slice(&ch.to_le_bytes());
        }
        for i in 0..chars.len() {
            let at = index_offset + i * PRERENDERED_ITEM_LEN;
            let offset = (data_offset + i * 4) as u32;
            bytes[at..at + 4].copy_from_slice(&offset.to_le_bytes());
            bytes[at + 4..at + 6].copy_from_slice(&2u16.to_le_bytes());
            bytes[at + 6..at + 8].copy_from_slice(&2u16.to_le_bytes());
            bytes[at + 12..at + 14].copy_from_slice(&3i16.to_le_bytes());
            bytes[at + 16..at + 18].copy_from_slice(&inc.to_le_bytes());
        }
        for _ in chars {
            bytes.extend_from_slice(&[0x10, 0x20, 0x30, 0x3F]);
        }
        bytes
    }

    /// Reference: the header, sorted `ChIndex` binary search and the version-1
    /// run-length decoder are `tTVPPrerenderedFont`
    /// (`reference/cpp/core/visual/PrerenderedFont.cpp:47-175`).
    #[test]
    fn parses_finds_and_retrieves_a_version_one_font() {
        let font = PrerenderedFont::parse(build_tft(&[0x41, 0x42], 7)).expect("parse");
        let item = font.find_code_unit(0x42).expect("B is indexed");
        assert_eq!(item.inc, 7);
        let coverage = font.retrieve(item).expect("retrieve");
        // `TVPUpscale65_255` multiplies each decoded byte by four, saturated:
        // 0x10→0x40, 0x20→0x80, 0x30→0xC0, 0x3F→0xFC
        // (`reference/cpp/core/visual/tvpgl.cpp:11392-11412`).
        assert_eq!(coverage, vec![0x40, 0x80, 0xC0, 0xFC]);
        assert!(font.find_code_unit(0x43).is_none());
    }

    /// Reference: the version-0 decoder repeats the previous byte after a
    /// `0x41` marker (`reference/cpp/core/visual/PrerenderedFont.cpp:130-143`).
    #[test]
    fn decodes_a_version_zero_run() {
        let mut bytes = build_tft(&[0x41], 7);
        bytes[22] = 0;
        bytes.truncate(PRERENDERED_HEADER_LEN + 2 + PRERENDERED_ITEM_LEN);
        bytes.extend_from_slice(&[0x10, 0x41, 0x03]);
        let font = PrerenderedFont::parse(bytes).expect("parse");
        let item = font.find_code_unit(0x41).expect("A is indexed");
        assert_eq!(
            font.retrieve(item).expect("retrieve"),
            vec![0x40, 0x40, 0x40, 0x40]
        );
    }

    /// Reference: a wrong signature, a non-16-bit flag and a bad version each
    /// raise `TVPPrerenderedFontMappingFailed`
    /// (`reference/cpp/core/visual/PrerenderedFont.cpp:22-58`).
    #[test]
    fn rejects_a_bad_header() {
        assert!(matches!(
            PrerenderedFont::parse(vec![0u8; 64]),
            Err(PrerenderedFontError::InvalidSignature)
        ));
        let mut bytes = build_tft(&[0x41], 7);
        bytes[23] = 1;
        assert!(matches!(
            PrerenderedFont::parse(bytes),
            Err(PrerenderedFontError::NotUnicode16)
        ));
        let mut bytes = build_tft(&[0x41], 7);
        bytes[22] = 2;
        assert!(matches!(
            PrerenderedFont::parse(bytes),
            Err(PrerenderedFontError::InvalidVersion)
        ));
    }
}
