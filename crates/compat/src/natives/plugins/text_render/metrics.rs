//! L2 text metrics for the `TextRenderBase` plugin.
//!
//! The reference asks the active [`FontRasterizer`] for each character's
//! advance (`GetTextExtent`) and for the ascent (`GetAscentHeight`). Compat
//! has exactly one rasterizer and no font enumeration, so those metrics come
//! from the L2 [`engine::text::TextEngine`] (cosmic-text shaping); a face is
//! loaded from the attached [`engine::Storage`] on first use, exactly like the
//! visual `Font` native.
//!
//! [`FontRasterizer`]: https://github.com/khronos/kirikiri
#![allow(unsafe_code)]

use std::collections::HashMap;

use engine::text::{FontId, TextEngine, TextStyle};

use crate::CompatCtx;

/// Shared shaping/metrics state: one L2 [`TextEngine`] plus the
/// requested-face cache. The reference has a single process-wide rasterizer;
/// this is the compat equivalent and is created lazily inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
pub(super) struct TextRenderEngine {
    pub(super) engine: TextEngine,
    pub(super) faces: HashMap<String, FontId>,
}

/// Borrow the plugin's shared text-engine state.
pub(super) fn text_engine(ctx: &CompatCtx) -> std::cell::RefMut<'_, TextRenderEngine> {
    // SAFETY: `ctx` was resolved from the installed binding.
    unsafe { ctx.services() }.state::<TextRenderEngine>()
}

/// Resolve `face` to an L2 [`FontId`], loading it from the attached storage on
/// first use.
///
/// Compat has no font enumeration/loading seam, so the only truthful source is
/// the attached storage: `face` is tried as a storage name and parsed by the
/// L2 text engine. Anything else is a typed error, never a fabricated
/// metric.
pub(super) fn ensure_face(
    ctx: &CompatCtx,
    state: &mut TextRenderEngine,
    face: &str,
) -> Result<FontId, String> {
    if let Some(id) = state.faces.get(face).copied() {
        return Ok(id);
    }
    // A face registered through `System.addFont` is resolved by family name
    // (or the storage name it was read from) before falling back to storage.
    // SAFETY: `ctx` was resolved from the installed binding.
    let registered = unsafe { ctx.services() }
        .state::<crate::natives::plugins::add_font::registry::FontRegistry>()
        .lookup(face);
    if let Some((bytes, index, _)) = registered {
        let id = state
            .engine
            .add_font(&bytes, index)
            .map_err(|error| format!("TextRender: face '{face}' is not a usable font: {error}"))?;
        state.faces.insert(face.to_string(), id);
        return Ok(id);
    }
    // SAFETY: `ctx` was resolved from the installed binding.
    let storage = unsafe { ctx.services() }
        .storage()
        .ok_or_else(|| format!("TextRender: no storage attached to load face '{face}'"))?;
    // SAFETY: the host keeps the attached storage alive for the binding's
    // lifetime (the `attach_storage` contract).
    let storage = unsafe { &*storage };
    let bytes = storage.read(face).map_err(|error| {
        format!(
            "TextRender: face '{face}' is not registered with the text engine and cannot be read \
             from storage ({error}); no font enumeration/loading service is exposed"
        )
    })?;
    let id = state
        .engine
        .add_font(&bytes, 0)
        .map_err(|error| format!("TextRender: face '{face}' is not a usable font: {error}"))?;
    state.faces.insert(face.to_string(), id);
    Ok(id)
}

/// The face the reference's `"user"` placeholder resolves to: the newest font
/// registered in the attached L2 [`engine::Scene`]. The reference's rasterizer
/// keeps the face last applied through a layer's `font`, and the scene's font
/// list is the compat equivalent.
fn scene_face(ctx: &CompatCtx) -> Option<String> {
    // SAFETY: the host keeps the attached scene alive for the binding's
    // lifetime (the `attach_scene` contract).
    let ptr = unsafe { ctx.services() }.scene()?;
    // SAFETY: see above.
    let scene = unsafe { &*ptr };
    scene
        .fonts
        .last()
        .map(|font| font.face.clone())
        .filter(|face| !face.is_empty())
}

/// Resolve the state's `face`, mapping the reference's `"user"` placeholder
/// through the scene font and then the storage-backed face cache.
pub(super) fn resolve_face(
    ctx: &CompatCtx,
    state: &mut TextRenderEngine,
    face: &str,
) -> Result<FontId, String> {
    if let Some(id) = state.faces.get(face).copied() {
        return Ok(id);
    }
    if face == "user"
        && let Some(candidate) = scene_face(ctx)
    {
        if let Some(id) = state.faces.get(&candidate).copied() {
            return Ok(id);
        }
        if let Ok(id) = ensure_face(ctx, state, &candidate) {
            return Ok(id);
        }
    }
    ensure_face(ctx, state, face)
}

/// The reference `GetTextExtent(ch, w, h).w`: the shaped advance of `ch` in
/// pixels. Shaping a single character deliberately mirrors the reference's
/// per-character extent (no cross-character kerning).
pub(super) fn advance(
    state: &mut TextRenderEngine,
    font: FontId,
    style: &TextStyle,
    ch: char,
) -> Result<i32, String> {
    let text = ch.to_string();
    let layout = state
        .engine
        .layout(font, &text, style, None)
        .map_err(|error| format!("TextRender: text layout failed: {error}"))?;
    Ok(layout.width.round() as i32)
}

/// The reference `GetAscentHeight()`: the face's ascent in pixels.
pub(super) fn ascent(state: &TextRenderEngine, font: FontId, size_px: f32) -> Result<i32, String> {
    let metrics = state
        .engine
        .font_metrics(font, size_px)
        .map_err(|error| format!("TextRender: font metrics failed: {error}"))?;
    Ok(metrics.ascent.round() as i32)
}
