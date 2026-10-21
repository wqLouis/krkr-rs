//! L2 text metrics for the `TextRenderBase` plugin.
//!
//! The reference asks the active [`FontRasterizer`] for each character's
//! advance (`GetTextExtent`) and for the ascent (`GetAscentHeight`). Compat
//! has exactly one rasterizer, so those metrics come from the L2
//! `engine_api::TextEngine`: the advance from the rasterizer's memoized
//! per-character measurement and the ascent from the face's own metrics. A
//! face is resolved through the `System.addFont` registry, the attached
//! `engine_api::Storage`, then the host's installed system fonts, exactly
//! like the visual `Font` native.
//!
//! [`FontRasterizer`]: https://github.com/khronos/kirikiri
#![allow(unsafe_code)]

use crate::engine_api::{FontId, TextEngine, TextStyle};

use crate::CompatCtx;
use crate::collections::Map;

/// Shared shaping/metrics state: one L2 [`TextEngine`] plus the
/// requested-face cache. The reference has a single process-wide rasterizer;
/// this is the compat equivalent and is created lazily inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
pub(super) struct TextRenderEngine {
    pub(super) engine: TextEngine,
    pub(super) faces: Map<String, FontId>,
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
///
/// The advance is the L2 engine's memoized per-character advance, which is
/// the rasterizer's own measurement of that character. Two cases are not
/// that value and keep the shaped layout: a bold character, whose advance
/// the rasterizer widens by the synthetic bold bump while the shaper
/// measures the face's normal weight, and a control character, which the
/// shaper expands to the next tab stop while the rasterizer reports no
/// advance at all.
pub(super) fn advance(
    state: &mut TextRenderEngine,
    font: FontId,
    style: &TextStyle,
    ch: char,
) -> Result<i32, String> {
    if style.bold || ch.is_control() {
        let text = ch.to_string();
        let layout = state
            .engine
            .layout(font, &text, style, None)
            .map_err(|error| format!("TextRender: text layout failed: {error}"))?;
        return Ok(layout.width.round() as i32);
    }
    let advance = state
        .engine
        .glyph_advance(font, ch, style)
        .map_err(|error| format!("TextRender: text layout failed: {error}"))?;
    Ok(advance.round() as i32)
}

/// The reference `GetAscentHeight()`: the face's ascent in pixels.
///
/// The ascent is the face's ascender in font units scaled by the pixel size
/// and cut at the integer boundary, so a face whose ascender is 1160 units
/// at 48 pixels per em reports 55 and not 56. The layout's line step and the
/// `size` a script reads back are measured against that value.
pub(super) fn ascent(state: &TextRenderEngine, font: FontId, size_px: f32) -> Result<i32, String> {
    let metrics = state
        .engine
        .font_metrics(font, size_px)
        .map_err(|error| format!("TextRender: font metrics failed: {error}"))?;
    // The engine reports the ascent as a scaled float and the cut happens at
    // the integer boundary. The scaled value can land a fraction below an
    // exact integer, so the quotient is nudged up by a relative epsilon
    // before it is truncated.
    Ok((metrics.ascent * (1.0 + ASCENT_TRUNCATION_EPSILON)).floor() as i32)
}

/// The relative slack that keeps the scaled ascent from landing just below
/// the integer the truncation produces.
const ASCENT_TRUNCATION_EPSILON: f32 = 1e-5;
