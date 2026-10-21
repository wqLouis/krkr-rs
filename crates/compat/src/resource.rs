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

use std::cell::RefCell;
use std::rc::Rc;

use crate::collections::Map;
use crate::engine_api::layer_ops::RectI;
use crate::engine_api::{BitmapState, Location, Storage, StorageReadError};
use crate::engine_api::{
    CoverageMask, FontId, TextCoverageCache, TextEngine, TextStyle, blit_coverage_into,
    blur_coverage, blurred_text_coverage, compose_text_into, rasterize_text_coverage,
};
use api::media::{DecodedImage, ImageInfo};
use tjs::{Tjs2Engine, TjsValue, Value};

use crate::CompatCtx;
use crate::natives::plugins::add_font::registry::FontRegistry;
use crate::storage::strip_file_media;

/// Default face requested when `Font.getList`/`Font.defaultFaceName` need to
/// establish a platform face (the reference `TVPDefaultFontFaceName`).
const DEFAULT_FACE: &str = "MS Gothic";

/// The name the default-face fallback is resolved under.
///
/// A fixed name keeps the requested family free, so a face registered later
/// for that family (`System.addFont`, `GdiPlus.addPrivateFont`) is not shadowed
/// by the fallback's own name-map entry. The reference resolves an unknown name
/// to its default face without recording the requested name in its font map
/// (`reference/cpp/core/visual/FontImpl.cpp:159-168`).
const FALLBACK_FACE: &str = "__krkr_default_face__";

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
    /// Rasterized character masks and their blurred shadows, keyed like the
    /// reference `tTVPFontCache`.
    pub(crate) coverage_cache: TextCoverageCache,
    /// Requested face string → registered face id.
    pub(crate) faces: Map<String, FontId>,
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
    /// Loaded pre-rendered fonts, keyed by the resolved placed path.
    prerendered_storage: Map<String, Rc<PrerenderedFont>>,
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
        // The media prefix is L3 syntax; the host writer takes the bare name,
        // exactly as `read_image_from_storage` does on the decode path.
        saver(strip_file_media(name), type_name, width, height, rgba)
    }

    /// Maps the font value `key` to the pre-rendered font `storage`.
    ///
    /// The storage is resolved and its header validated now; the glyph body is
    /// read on first use. The reference reads and holds the whole file but
    /// validates only the header here too, so deferring the body (and the
    /// index-bounds check the reference leaves unchecked) is not
    /// script-observable (`reference/cpp/core/visual/PrerenderedFont.cpp:9-76`).
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

    /// Drops the mapping for `key` and releases the parsed font once no other
    /// mapping uses it. Loaded fonts are shared by storage, so the storage
    /// entry is dropped only when the last mapping goes.
    pub(crate) fn unmap_prerendered(&mut self, key: &PrerenderedKey) {
        let Some(index) = self.prerendered.iter().position(|(k, _)| k == key) else {
            return;
        };
        let (_, font) = self.prerendered.remove(index);
        if self
            .prerendered
            .iter()
            .any(|(_, other)| Rc::ptr_eq(other, &font))
        {
            return;
        }
        self.prerendered_storage
            .retain(|_, stored| !Rc::ptr_eq(stored, &font));
    }

    /// The pre-rendered font mapped to `key`, if any.
    pub(crate) fn prerendered_for(&self, key: &PrerenderedKey) -> Option<Rc<PrerenderedFont>> {
        self.prerendered
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, font)| Rc::clone(font))
    }

    /// Loads (or reuses) the pre-rendered font for `storage`, shared by
    /// resolved placed path (`TVPPrerenderedFonts`,
    /// `reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:129-147`).
    fn load_prerendered(
        &mut self,
        ctx: &CompatCtx,
        storage: &str,
    ) -> Result<Rc<PrerenderedFont>, String> {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        let Some(store) = services.storage() else {
            return Err(format!("Cannot find storage {storage}"));
        };
        // SAFETY: the host keeps the attached storage alive for the binding's
        // lifetime (the `attach_storage` contract).
        let store = unsafe { &*store };
        // The media prefix is L3 syntax; the L2 namespace resolves the bare name.
        let lookup = strip_file_media(storage);
        let Some(location) = store.find(lookup).map_err(|error| error.to_string())? else {
            return Err(format!("Cannot find storage {storage}"));
        };
        let placed_path = placed_path_key(&location);
        if let Some(font) = self.prerendered_storage.get(&placed_path) {
            return Ok(Rc::clone(font));
        }
        let font = Rc::new(PrerenderedFont::map(store, storage)?);
        self.prerendered_storage
            .insert(placed_path, Rc::clone(&font));
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
/// system fonts.
///
/// When no candidate resolves, the last family registered by `System.addFont`
/// is tried. Then the host's default face is tried
/// (the reference `TVPCreateFontStream` default,
/// `reference/cpp/core/visual/FontImpl.cpp:159-168`), then the engine's first
/// registered face; only an engine with no registered face at all raises.
///
/// Only a resolution found through a candidate is cached. A default-face
/// fallback is resolved under [`FALLBACK_FACE`] and is never cached under
/// `face`, so a face registered later for `face` (`System.addFont`,
/// `GdiPlus.addPrivateFont`) is found on the next lookup instead of being
/// shadowed by a stale fallback entry.
pub(crate) fn resolve_face(
    ctx: &CompatCtx,
    engine: &mut TextEngine,
    cache: &mut Map<String, FontId>,
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
    // The default face is the last face added to the registered font map, so
    // it is tried before the host's default family.
    let registered_default = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let registry = unsafe { ctx.services() }.state::<FontRegistry>();
        registry.last_family()
    };
    if let Some(family) = registered_default
        && let Ok(id) = try_face(ctx, engine, &family)
    {
        return Ok(id);
    }
    // No registered game face: fall back to the host's default face. Loading
    // it under a fixed name keeps `face` free for a later registration.
    if let Ok(id) = engine.load_system_font(FALLBACK_FACE) {
        return Ok(id);
    }
    if let Some(id) = engine.default_font_for(face) {
        return Ok(id);
    }
    Err(format!("no font is available for face '{face}'"))
}

/// Try one face candidate through the `System.addFont` registry, the attached
/// storage and the host's installed system fonts (exact family only).
///
/// A face spelled as a family name is never tried as a storage path: the
/// reference resolves a face only through its registered font names
/// (`TVPFindFont`, `FontImpl.cpp:277-291`), so probing the storage for a bare
/// family name misses on every text draw (a full storage search each time).
/// A file-shaped face (`somefont.ttf`) is still tried, which is how a script
/// loads a font file named directly as its face.
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
    if is_font_file_name(face) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let registered_storage = unsafe { ctx.services() }.storage();
        if let Some(storage) = registered_storage {
            // SAFETY: the host keeps the attached storage alive for the
            // binding's lifetime (the `attach_storage` contract).
            let storage = unsafe { &*storage };
            // The media prefix is L3 syntax; the L2 namespace resolves the bare name.
            match storage.read(strip_file_media(face)) {
                Ok(bytes) => {
                    return engine
                        .add_font(&bytes, 0)
                        .map_err(|error| format!("face '{face}' is not a usable font: {error}"));
                }
                Err(StorageReadError::NotFound(_)) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    engine
        .load_system_font_exact(face)
        .map_err(|error| error.to_string())
}

/// Whether `face` is spelled like a font file rather than a family name.
///
/// Only a file-shaped name is worth trying as a storage path; the font
/// extensions are the OpenType/TrueType families the reference enumerates
/// (`FontImpl.cpp:166-183`).
fn is_font_file_name(face: &str) -> bool {
    let face = strip_file_media(face);
    let extension = face
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    matches!(
        extension.as_deref(),
        Some("ttf" | "ttc" | "otf" | "otc" | "tte" | "dfont")
    )
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

/// A resolved face and the style it is drawn with.
pub(crate) struct FaceSpec<'a> {
    pub(crate) face: &'a str,
    pub(crate) size_px: f32,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) angle_tenths: f64,
}

/// Draw `params` into `bitmap`, resolving `face` through `resolver`.
///
/// `clip` is the layer/bitmap clip rectangle. Returns the union of the drawn
/// glyph rectangles, or `None` when nothing had ink.
pub(crate) fn draw_text_into_bitmap(
    resolver: &mut ResourceResolver,
    ctx: &CompatCtx,
    bitmap: &mut BitmapState,
    face: FaceSpec<'_>,
    clip: RectI,
    params: &TextDrawParams<'_>,
) -> Result<Option<RectI>, String> {
    let FaceSpec {
        face,
        size_px,
        bold,
        italic,
        angle_tenths,
    } = face;
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
        let body = prerendered.ensure_body(ctx)?;
        return draw_prerendered_text(
            resolver,
            bitmap,
            PrerenderedRun {
                font,
                body: &body,
                size_px,
                style: &style,
                angle_tenths,
            },
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
    // Rasterize each character once and reuse the coverage for both the
    // shadow and the main pass (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:244-320`).
    let masks = rasterize_text_coverage(
        &mut resolver.coverage_cache,
        &resolver.engine,
        &layout,
        &style,
        params.antialias,
    )
    .map_err(|error| format!("text rasterization failed: {error}"))?;
    // A shadow exists only when `shadowlevel` is nonzero; `shadowwidth` is
    // the blur radius the shadow coverage is expanded by.
    if params.shadow_level != 0 {
        // Each character's own coverage is blurred with the cone kernel and
        // drawn once at the text opacity, before the main pass.
        // `shadowlevel == 255 && shadowwidth == 0` is the reference's
        // "normal shadow", which reuses the unblurred character data
        // (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1168-1178`).
        let shadow_masks = if params.shadow_level == 255 && params.shadow_width == 0 {
            masks.clone()
        } else {
            let level = params.shadow_level.clamp(0, 255) as u8;
            blurred_text_coverage(
                &mut resolver.coverage_cache,
                &resolver.engine,
                &layout,
                &style,
                params.antialias,
                level,
                params.shadow_width,
            )
            .map_err(|error| format!("shadow blur failed: {error}"))?
        };
        let base = (
            (params.x + params.shadow_offset.0) as f32,
            (params.y + params.shadow_offset.1) as f32,
        );
        let (sin, cos) = angle.sin_cos();
        for mask in &shadow_masks {
            let (rot_x, rot_y) = if angle == 0.0 {
                (mask.x(), mask.y())
            } else {
                (
                    mask.x() * cos - mask.y() * sin,
                    mask.x() * sin + mask.y() * cos,
                )
            };
            let dest_x = (base.0 + rot_x).round() as i32;
            let dest_y = (base.1 + rot_y).round() as i32;
            if let Some(rect) = blit_coverage_into(
                bitmap,
                clip,
                mask,
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
    if let Some(rect) = compose_text_into(&masks, &style, origin, angle, bitmap, clip, opacity)
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

/// Inputs for drawing one run through a pre-rendered `.tft` font.
struct PrerenderedRun<'a> {
    /// Resolved font id for the mapped font.
    font: FontId,
    /// Parsed `.tft` body the glyphs are read from.
    body: &'a PrerenderedBody,
    /// Font size in pixels.
    size_px: f32,
    /// Text style (bold, italic, colour).
    style: &'a TextStyle,
    /// Rotation in tenths of a degree.
    angle_tenths: f64,
}

/// Draw a run whose font is mapped to a pre-rendered `.tft`.
///
/// Each character uses the `.tft` descriptor's coverage, origin and ink
/// width; a character the file does not carry falls back to the rasterizer.
/// The pen advances by the descriptor's `IncX`/`IncY`, and the glyph origin
/// includes the ascent offset.
fn draw_prerendered_text(
    resolver: &ResourceResolver,
    bitmap: &mut BitmapState,
    run: PrerenderedRun<'_>,
    clip: RectI,
    params: &TextDrawParams<'_>,
) -> Result<Option<RectI>, String> {
    let PrerenderedRun {
        font,
        body,
        size_px,
        style,
        angle_tenths,
    } = run;
    // The ascent is the face's ascender scaled to the pixel size and
    // truncated to whole pixels; the ascent offset is that integer projected
    // onto the rotation axes.
    let ascent = resolver
        .engine
        .font_metrics(font, size_px)
        .map_err(|error| format!("text metrics failed: {error}"))?
        .ascent
        .floor() as i32;
    let angle = angle_tenths * std::f64::consts::PI / 1800.0;
    let angle90 = angle + std::f64::consts::FRAC_PI_2;
    // The ascent projection is truncated to an integer.
    let ascent_ofs_x = (-angle90.cos() * f64::from(ascent)) as i32;
    let ascent_ofs_y = (angle90.sin() * f64::from(ascent)) as i32;
    let opacity = params.opacity.clamp(0, 255) as u8;

    let mut pen_x = params.x as f64;
    let mut pen_y = params.y as f64;
    let mut glyphs: Vec<PrerenderedGlyph> = Vec::new();
    for unit in params.text.encode_utf16() {
        if let Some(item) = body.find_code_unit(unit) {
            if item.width != 0 && item.height != 0 {
                let coverage = body.retrieve(&item).map_err(|error| error.to_string())?;
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
                    y: pen_y.round() as i32 + ascent - glyph.bearing_y.round() as i32,
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

/// The fixed header of a `.tft`, validated when the font is mapped.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PrerenderedHeader {
    /// Data version (0 or 1).
    version: u8,
    /// Number of entries in the sorted character index.
    index_count: usize,
    /// Offset of the sorted UTF-16 code units inside the file.
    ch_index_offset: usize,
    /// Offset of the character descriptors inside the file.
    index_offset: usize,
}

/// Validates the fixed header from its first [`PRERENDERED_HEADER_LEN`] bytes.
///
/// # Errors
///
/// The same [`PrerenderedFontError`] the reference raises at map time
/// (`reference/cpp/core/visual/PrerenderedFont.cpp:40-59`). A truncated,
/// non-empty file is `InvalidSignature`: the reference `memcmp`s 22 bytes of a
/// shorter allocation and then indexes `Image[23]` and the offset words out of
/// bounds, so we raise a typed error rather than copy that defect.
pub(crate) fn validate_header(prefix: &[u8]) -> Result<PrerenderedHeader, PrerenderedFontError> {
    if prefix.len() < PRERENDERED_HEADER_LEN {
        if prefix.is_empty() {
            return Err(PrerenderedFontError::ZeroSize);
        }
        return Err(PrerenderedFontError::InvalidSignature);
    }
    if !prefix.starts_with(PRERENDERED_SIGNATURE) {
        return Err(PrerenderedFontError::InvalidSignature);
    }
    if prefix[23] != 2 {
        return Err(PrerenderedFontError::NotUnicode16);
    }
    let version = prefix[22];
    if version != 0 && version != 1 {
        return Err(PrerenderedFontError::InvalidVersion);
    }
    Ok(PrerenderedHeader {
        version,
        index_count: read_u32(prefix, 24)? as usize,
        ch_index_offset: read_u32(prefix, 28)? as usize,
        index_offset: read_u32(prefix, 32)? as usize,
    })
}

/// A parsed pre-rendered font body.
///
/// The character index and glyph data stay in the file image; a descriptor is
/// read from it on demand instead of being copied into a parallel table.
pub(crate) struct PrerenderedBody {
    /// Data version (0 or 1).
    version: u8,
    /// Number of entries in the sorted character index.
    index_count: usize,
    /// Offset of the sorted UTF-16 code units inside `image`.
    ch_index_offset: usize,
    /// Offset of the character descriptors inside `image`.
    index_offset: usize,
    /// The whole file, indexed by a descriptor's `offset`.
    image: Vec<u8>,
}

impl PrerenderedBody {
    /// Parses `bytes` as a `.tft` body, given the header validated at map
    /// time.
    ///
    /// # Errors
    ///
    /// Returns a [`PrerenderedFontError`] when the index points outside the
    /// file or a size computation overflows. The reference never checks these
    /// bounds at map time; failing here is the first-use analogue of the
    /// reference's unchecked reads.
    pub(crate) fn parse(
        bytes: Vec<u8>,
        header: &PrerenderedHeader,
    ) -> Result<Self, PrerenderedFontError> {
        let index_count = header.index_count;
        let ch_index_offset = header.ch_index_offset;
        let index_offset = header.index_offset;

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

        Ok(Self {
            version: header.version,
            index_count,
            ch_index_offset,
            index_offset,
            image: bytes,
        })
    }

    /// Returns the descriptor for a UTF-16 code unit, or `None`.
    pub(crate) fn find_code_unit(&self, ch: u16) -> Option<CharItem> {
        if self.index_count == 0 {
            return None;
        }
        let mut s = 0usize;
        let mut e = self.index_count;
        loop {
            let d = e - s;
            if d <= 1 {
                return if self.ch_index_at(s)? == ch {
                    self.item_at(s)
                } else {
                    None
                };
            }
            let m = s + (d >> 1);
            if self.ch_index_at(m)? > ch {
                e = m;
            } else {
                s = m;
            }
        }
    }

    /// Reads the sorted code unit at `index` from `image`.
    ///
    /// `parse` validated that the whole character index lies inside `image`, so
    /// the read is in bounds for every `index < index_count`.
    fn ch_index_at(&self, index: usize) -> Option<u16> {
        let at = self.ch_index_offset.checked_add(index.checked_mul(2)?)?;
        self.u16_at(at)
    }

    /// Reads the character descriptor at `index` from `image`.
    ///
    /// `parse` validated that the whole descriptor index lies inside `image`,
    /// so the read is in bounds for every `index < index_count`.
    fn item_at(&self, index: usize) -> Option<CharItem> {
        let at = self
            .index_offset
            .checked_add(index.checked_mul(PRERENDERED_ITEM_LEN)?)?;
        Some(CharItem {
            offset: self.u32_at(at)?,
            width: self.u16_at(at + 4)?,
            height: self.u16_at(at + 6)?,
            origin_x: self.u16_at(at + 8)? as i16,
            origin_y: self.u16_at(at + 10)? as i16,
            inc_x: self.u16_at(at + 12)? as i16,
            inc_y: self.u16_at(at + 14)? as i16,
            inc: self.u16_at(at + 16)? as i16,
        })
    }

    /// Reads a little-endian `u16` at `offset` from `image`.
    fn u16_at(&self, offset: usize) -> Option<u16> {
        let bytes = self.image.get(offset..offset.checked_add(2)?)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Reads a little-endian `u32` at `offset` from `image`.
    fn u32_at(&self, offset: usize) -> Option<u32> {
        let bytes = self.image.get(offset..offset.checked_add(4)?)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
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

/// A mapped pre-rendered font.
///
/// The fixed header is validated when the mapping is created; the whole file
/// is read and its index bounds checked on first use. The reference reads the
/// whole file at map time and holds it; here only the resolved storage name
/// and the header are kept until a glyph is requested. The divergence is not
/// observable through the script surface because the reference validates only
/// the header at map time, and the index bounds we defer are unchecked reads
/// there.
pub(crate) struct PrerenderedFont {
    /// The bare storage name, used for the first-use read.
    storage: String,
    /// The header validated at map time.
    header: PrerenderedHeader,
    /// The whole file, read once on first use.
    body: RefCell<Option<Rc<PrerenderedBody>>>,
}

impl PrerenderedFont {
    /// Reads and validates the fixed header of `storage` through `store`.
    ///
    /// # Errors
    ///
    /// The [`Storage::stat`]/[`Storage::read_prefix`] error as a string, the
    /// header [`PrerenderedFontError`]s, or `Cannot find storage` when the
    /// name does not resolve (`TVPSearchPlacedPath`,
    /// `reference/cpp/core/base/StorageIntf.cpp:1111-1115`).
    pub(crate) fn map(store: &Storage, storage: &str) -> Result<Self, String> {
        // The media prefix is L3 syntax; the L2 namespace resolves the bare name.
        let lookup = strip_file_media(storage);
        let Some(meta) = store.stat(lookup).map_err(|error| error.to_string())? else {
            return Err(format!("Cannot find storage {storage}"));
        };
        if meta.size == 0 {
            return Err(PrerenderedFontError::ZeroSize.to_string());
        }
        let prefix = store
            .read_prefix(lookup, PRERENDERED_HEADER_LEN)
            .map_err(|error| error.to_string())?;
        let header = validate_header(&prefix).map_err(|error| error.to_string())?;
        Ok(Self {
            storage: lookup.to_string(),
            header,
            body: RefCell::new(None),
        })
    }

    /// Reads and validates the whole file on first use, returning the cached
    /// body.
    ///
    /// # Errors
    ///
    /// The storage error as a string when the file cannot be re-read, or the
    /// index [`PrerenderedFontError`] when the deferred bounds check fails.
    pub(crate) fn ensure_body(&self, ctx: &CompatCtx) -> Result<Rc<PrerenderedBody>, String> {
        if let Some(body) = self.body.borrow().as_ref() {
            return Ok(Rc::clone(body));
        }
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        let Some(store) = services.storage() else {
            return Err(format!("Cannot find storage {}", self.storage));
        };
        // SAFETY: the host keeps the attached storage alive for the binding's
        // lifetime (the `attach_storage` contract).
        let store = unsafe { &*store };
        let bytes = store
            .read(&self.storage)
            .map_err(|error| error.to_string())?;
        let body = Rc::new(
            PrerenderedBody::parse(bytes, &self.header).map_err(|error| error.to_string())?,
        );
        *self.body.borrow_mut() = Some(Rc::clone(&body));
        Ok(body)
    }
}

/// The reference's placed-path key for a resolved storage location
/// (`TVPSearchPlacedPath`, `reference/cpp/core/base/StorageIntf.cpp:1111-1115`).
fn placed_path_key(location: &Location) -> String {
    match location {
        Location::Disk(path) => path.to_string_lossy().into_owned(),
        Location::Archive(archive, in_archive) => format!("{}>{}", archive.display(), in_archive),
    }
}

/// Reads a little-endian `u32` at `offset`.
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, PrerenderedFontError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(PrerenderedFontError::Read)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// The per-engine seam for the host's system-font directories.
///
/// L3 owns no platform paths, so the composition root supplies the list the
/// L1 platform crate returns. The directories are scanned lazily, the first
/// time a face is resolved from the host; install them before the game's
/// startup script runs so no face has been resolved yet.
impl crate::CompatHost {
    /// Install the host's system-font directories on the shared text engine.
    pub fn set_system_font_dirs(&self, dirs: Vec<std::path::PathBuf>) {
        self.services()
            .state::<ResourceResolver>()
            .engine
            .set_system_font_dirs(dirs);
    }
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

    /// Validates the header of `bytes` and parses the body, as the lazy path
    /// does at map time and first use.
    fn body_of(bytes: Vec<u8>) -> PrerenderedBody {
        let header = validate_header(&bytes[..PRERENDERED_HEADER_LEN]).expect("header");
        PrerenderedBody::parse(bytes, &header).expect("parse")
    }

    /// Reference: the header, sorted `ChIndex` binary search and the version-1
    /// run-length decoder are `tTVPPrerenderedFont`
    /// (`reference/cpp/core/visual/PrerenderedFont.cpp:47-175`).
    #[test]
    fn parses_finds_and_retrieves_a_version_one_font() {
        let font = body_of(build_tft(&[0x41, 0x42], 7));
        let item = font.find_code_unit(0x42).expect("B is indexed");
        assert_eq!(item.inc, 7);
        let coverage = font.retrieve(&item).expect("retrieve");
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
        let font = body_of(bytes);
        let item = font.find_code_unit(0x41).expect("A is indexed");
        assert_eq!(
            font.retrieve(&item).expect("retrieve"),
            vec![0x40, 0x40, 0x40, 0x40]
        );
    }

    /// Reference: `tTVPPrerenderedFont` keeps `ChIndex`/`Index` as pointers
    /// into the mapped file (`reference/cpp/core/visual/PrerenderedFont.cpp:60-62`),
    /// so a descriptor read reflects an edit to the file image.
    #[test]
    fn reads_the_descriptor_from_the_file_image() {
        let mut font = body_of(build_tft(&[0x41, 0x42], 7));
        assert_eq!(font.find_code_unit(0x42).expect("B is indexed").inc, 7);
        // B's descriptor is the second 20-byte entry; `inc` sits at +16.
        let at = font.index_offset + PRERENDERED_ITEM_LEN + 16;
        font.image[at..at + 2].copy_from_slice(&9i16.to_le_bytes());
        assert_eq!(font.find_code_unit(0x42).expect("B is indexed").inc, 9);
    }

    /// Reference: the constructor reads the file and sets up the index only
    /// (`reference/cpp/core/visual/PrerenderedFont.cpp:9-76`); coverage is
    /// expanded later by `Retrieve` (`:122-175`). Parsing therefore succeeds
    /// even when the compressed glyph data is missing, and the read error
    /// surfaces only on retrieval.
    #[test]
    fn parse_does_not_decode_glyph_coverage() {
        let mut bytes = build_tft(&[0x41], 7);
        // Keep the header and the 20-byte descriptor, drop the glyph data.
        bytes.truncate(PRERENDERED_HEADER_LEN + 2 + PRERENDERED_ITEM_LEN);
        let font = body_of(bytes);
        let item = font.find_code_unit(0x41).expect("A is indexed");
        assert!(matches!(
            font.retrieve(&item),
            Err(PrerenderedFontError::Read)
        ));
    }

    /// Reference: a wrong signature, a non-16-bit flag and a bad version each
    /// raise `TVPPrerenderedFontMappingFailed`
    /// (`reference/cpp/core/visual/PrerenderedFont.cpp:22-58`).
    #[test]
    fn rejects_a_bad_header() {
        assert!(matches!(
            validate_header(&[0u8; 64]),
            Err(PrerenderedFontError::InvalidSignature)
        ));
        let mut bytes = build_tft(&[0x41], 7);
        bytes[23] = 1;
        assert!(matches!(
            validate_header(&bytes),
            Err(PrerenderedFontError::NotUnicode16)
        ));
        let mut bytes = build_tft(&[0x41], 7);
        bytes[22] = 2;
        assert!(matches!(
            validate_header(&bytes),
            Err(PrerenderedFontError::InvalidVersion)
        ));
    }

    /// Reference: the constructor `memcmp`s 22 bytes and then indexes
    /// `Image[23]` and the offset words without checking the length
    /// (`reference/cpp/core/visual/PrerenderedFont.cpp:40-67`); a file shorter
    /// than the header is an out-of-bounds read there. We raise the
    /// invalid-file error for a truncated non-empty file and the zero-size
    /// error for an empty one, rather than copy that defect.
    #[test]
    fn truncated_header_raises_signature_error() {
        assert!(matches!(
            validate_header(PRERENDERED_SIGNATURE),
            Err(PrerenderedFontError::InvalidSignature)
        ));
        assert!(matches!(
            validate_header(&[]),
            Err(PrerenderedFontError::ZeroSize)
        ));
    }

    /// Reference: `TVPMapPrerenderedFont` shares one `tTVPPrerenderedFont` per
    /// storage and `AddRef`s it, and `TVPUnmapPrerenderedFont` releases the
    /// mapping; `tTVPPrerenderedFont::Release` deletes the object (and its
    /// file image) at the last reference
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:127-185`,
    /// `reference/cpp/core/visual/PrerenderedFont.cpp:83-90`).
    #[test]
    fn unmap_releases_the_parsed_font_at_the_last_mapping() {
        let mut resolver = ResourceResolver::default();
        let header =
            validate_header(&build_tft(&[0x41], 7)[..PRERENDERED_HEADER_LEN]).expect("header");
        let font = Rc::new(PrerenderedFont {
            storage: "sample.tft".to_string(),
            header,
            body: RefCell::new(None),
        });
        resolver
            .prerendered_storage
            .insert("sample.tft".to_string(), Rc::clone(&font));
        let first = PrerenderedKey {
            face: "Ghost".to_string(),
            height: 12,
            bold: false,
            italic: false,
            angle: 0,
        };
        let second = PrerenderedKey {
            height: 16,
            ..first.clone()
        };
        resolver.prerendered.push((first.clone(), Rc::clone(&font)));
        resolver
            .prerendered
            .push((second.clone(), Rc::clone(&font)));

        // One of two mappings going away keeps the shared storage resident.
        resolver.unmap_prerendered(&first);
        assert!(resolver.prerendered_storage.contains_key("sample.tft"));

        // The last mapping releases the parsed font.
        resolver.unmap_prerendered(&second);
        assert!(resolver.prerendered_storage.is_empty());
    }
}
