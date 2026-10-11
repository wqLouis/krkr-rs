//! Shared L3 resource resolution: text faces, default-face fallback and the
//! host image codec.
//!
//! [`ResourceResolver`] is the per-engine dispatch state (never a process
//! global) that owns the L2 [`TextEngine`], the requested-face cache, the
//! registered-face order and the host image codec. The visual `Font` native,
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
    FontId, TextEngine, TextStyle, blit_coverage_into, blur_coverage, compose_text_into,
    rasterize_text_coverage,
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
