//! L2 text metrics for the `TextRenderBase` plugin.
//!
//! The reference asks the active [`FontRasterizer`] for each character's
//! advance (`GetTextExtent`) and for the ascent (`GetAscentHeight`). Compat
//! has exactly one rasterizer, so those metrics come from the L2
//! `engine_api::TextEngine` (cosmic-text shaping); a face is resolved
//! through the `System.addFont` registry, the attached `engine_api::Storage`,
//! then the host's installed system fonts, exactly like the visual `Font`
//! native.
//!
//! [`FontRasterizer`]: https://github.com/khronos/kirikiri
#![allow(unsafe_code)]

use std::collections::HashMap;

use crate::engine_api::{FontId, TextEngine, TextStyle};

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

/// Resolve `face` to an L2 [`FontId`], registering it on first use.
///
/// The resolution logic is shared with the visual `Font` native through
/// [`crate::resource::resolve_face`] (`System.addFont` registry, attached
/// storage, installed system fonts, then the default-face fallback).
pub(super) fn ensure_face(
    ctx: &CompatCtx,
    state: &mut TextRenderEngine,
    face: &str,
) -> Result<FontId, String> {
    crate::resource::resolve_face(ctx, &mut state.engine, &mut state.faces, face)
}

/// The face the reference's `"user"` placeholder resolves to: the newest font
/// registered in the attached L2 `engine_api::Scene`. The reference's rasterizer
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
