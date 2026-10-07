//! Projection of the L2 TVP layer tree into the L1 [`RenderScene`] tree.
//!
//! The middle layer owns the logical [`Scene`] (windows, layers, bitmaps)
//! and every TVP property. The bottom [`krkr_render::RenderPipeline`]
//! consumes an abstract [`RenderScene`] tree and owns traversal, batching and
//! drawing. This module is the single seam between the two: it is a **pure
//! field copy** that turns one window (or a whole scene) into a
//! [`RenderScene`] with one [`RenderNode`] per *drawn* layer. It is not a
//! draw list: no batching, sorting or traversal happens here (D10).
//!
//! The projection is a pure function over caller-owned state. It holds no
//! globals, caches or locks; the texture table is supplied by the caller
//! through a lookup closure (typically `|id| textures.get(&id).cloned()`),
//! so the same function works for a test double and a live pipeline.
//!
//! Semantics are ported from the old Bevy `render/src/sync.rs` path:
//!
//! * [`compose_layer`] accumulates ancestor position, opacity and visibility;
//! * [`visible_image_region`] resolves `Layer.rect` + `ImageLeft/Top/Width/
//!   Height` into a destination [`RectF`] and a source [`PixelRegion`];
//! * [`blend_mode_for`] maps the native `Layer.type` (`LT_*`) numbering onto
//!   the closed [`BlendMode`] enum (`ltBinder` maps to `None`: skip).
//!
//! Screen-buffer primary layers, hidden/empty layers, `ltBinder` containers
//! and layers whose texture is missing (with no solid fill fallback) are
//! skipped, matching `sync_scene`.

use std::collections::HashMap;

use krkr_api::blend::{BlendMode, DstBlend};
use krkr_api::geometry::{PixelRegion, RectF};
use krkr_render::{RenderContent, RenderNode, RenderNodeId, RenderScene, Texture};

use crate::scene::{LT_ADDITIVE, LT_ALPHA, LT_OPAQUE, LT_SUBTRACTIVE, LayerState, Rect, Scene};

// ---------------------------------------------------------------------------
// Native `tTVPLayerType` values (`reference/cpp/core/visual/drawable.h:20`).
//
// `crate::scene` already exports the four original types (`LT_OPAQUE`,
// `LT_ALPHA`, `LT_ADDITIVE`, `LT_SUBTRACTIVE`); the remaining
// Photoshop-style values live here so [`blend_mode_for`] is total over the
// documented range without duplicating the public constants.
// ---------------------------------------------------------------------------

/// `ltBinder` — a container that draws nothing (`Blt` returns "no action").
pub const LT_BINDER: i64 = 0;
/// `ltMultiplicative` — multiply blend.
pub const LT_MULTIPLICATIVE: i64 = 5;
/// `ltEffect` — layer-effect callback, not a bitmap blend.
pub const LT_EFFECT: i64 = 6;
/// `ltFilter` — layer-filter callback, not a bitmap blend.
pub const LT_FILTER: i64 = 7;
/// `ltDodge` — colour dodge.
pub const LT_DODGE: i64 = 8;
/// `ltDarken` — per-channel minimum.
pub const LT_DARKEN: i64 = 9;
/// `ltLighten` — per-channel maximum.
pub const LT_LIGHTEN: i64 = 10;
/// `ltScreen` — screen blend.
pub const LT_SCREEN: i64 = 11;
/// `ltAddAlpha` — additive with an explicit alpha channel.
pub const LT_ADD_ALPHA: i64 = 12;
/// `ltPsNormal` — Photoshop-style normal (plain source-over).
pub const LT_PS_NORMAL: i64 = 13;
/// `ltPsAdditive`.
pub const LT_PS_ADDITIVE: i64 = 14;
/// `ltPsSubtractive`.
pub const LT_PS_SUBTRACTIVE: i64 = 15;
/// `ltPsMultiplicative`.
pub const LT_PS_MULTIPLICATIVE: i64 = 16;
/// `ltPsScreen`.
pub const LT_PS_SCREEN: i64 = 17;
/// `ltPsOverlay`.
pub const LT_PS_OVERLAY: i64 = 18;
/// `ltPsHardLight`.
pub const LT_PS_HARD_LIGHT: i64 = 19;
/// `ltPsSoftLight`.
pub const LT_PS_SOFT_LIGHT: i64 = 20;
/// `ltPsColorDodge`.
pub const LT_PS_COLOR_DODGE: i64 = 21;
/// `ltPsColorDodge5` — the legacy KiriKiri approximation.
pub const LT_PS_COLOR_DODGE5: i64 = 22;
/// `ltPsColorBurn`.
pub const LT_PS_COLOR_BURN: i64 = 23;
/// `ltPsLighten`.
pub const LT_PS_LIGHTEN: i64 = 24;
/// `ltPsDarken`.
pub const LT_PS_DARKEN: i64 = 25;
/// `ltPsDifference`.
pub const LT_PS_DIFFERENCE: i64 = 26;
/// `ltPsDifference5` — the legacy KiriKiri approximation.
pub const LT_PS_DIFFERENCE5: i64 = 27;
/// `ltPsExclusion`.
pub const LT_PS_EXCLUSION: i64 = 28;

/// Background clear colour used when a projected scene has no explicit one.
///
/// The old Bevy camera cleared to opaque black
/// (`ClearColorConfig::Custom(Color::srgb(0.0, 0.0, 0.0))`), so the
/// projection preserves that.
pub const BACKGROUND_CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// Clamp an opacity to `[0, 1]`, mapping NaN to `0`.
///
/// Ported from the old `sync::clamp_opacity` so window and layer opacities
/// compose identically.
pub fn clamp_opacity(opacity: f32) -> f32 {
    if opacity.is_nan() {
        0.0
    } else {
        opacity.clamp(0.0, 1.0)
    }
}

/// Map a native TVP `Layer.type` value to the render pipeline's blend mode.
///
/// This is total over the documented `tTVPLayerType` range: every known type
/// maps to its own mode and only truly unknown/out-of-range values degrade to
/// [`BlendMode::SourceOver`]. `ltBinder` returns `None` because a binder is a
/// container the reference never blits (its children are separate layers).
///
/// Ported from the old `render/src/blend.rs::blend_mode_for`.
pub const fn blend_mode_for(blend_type: i64) -> Option<BlendMode> {
    Some(match blend_type {
        LT_BINDER => return None,
        LT_OPAQUE => BlendMode::Replace,
        LT_ALPHA | LT_PS_NORMAL => BlendMode::SourceOver,
        LT_ADDITIVE | LT_PS_ADDITIVE => BlendMode::Additive,
        LT_SUBTRACTIVE | LT_PS_SUBTRACTIVE => BlendMode::Subtractive,
        LT_ADD_ALPHA => BlendMode::AddAlpha,
        LT_MULTIPLICATIVE | LT_PS_MULTIPLICATIVE => BlendMode::Multiply,
        LT_SCREEN | LT_PS_SCREEN => BlendMode::Screen,
        // `ltDodge` uses the same colour-dodge math as `ltPsColorDodge`.
        LT_DODGE | LT_PS_COLOR_DODGE => BlendMode::DestinationDependent(DstBlend::ColorDodge),
        LT_PS_COLOR_DODGE5 => BlendMode::DestinationDependent(DstBlend::ColorDodge5),
        LT_PS_COLOR_BURN => BlendMode::DestinationDependent(DstBlend::ColorBurn),
        LT_DARKEN | LT_PS_DARKEN => BlendMode::DestinationDependent(DstBlend::Darken),
        LT_LIGHTEN | LT_PS_LIGHTEN => BlendMode::DestinationDependent(DstBlend::Lighten),
        LT_PS_DIFFERENCE => BlendMode::DestinationDependent(DstBlend::Difference),
        LT_PS_DIFFERENCE5 => BlendMode::DestinationDependent(DstBlend::Difference5),
        LT_PS_EXCLUSION => BlendMode::DestinationDependent(DstBlend::Exclusion),
        LT_PS_OVERLAY => BlendMode::DestinationDependent(DstBlend::Overlay),
        LT_PS_HARD_LIGHT => BlendMode::DestinationDependent(DstBlend::HardLight),
        LT_PS_SOFT_LIGHT => BlendMode::DestinationDependent(DstBlend::SoftLight),
        // `ltEffect`/`ltFilter` are layer-effect callbacks, not bitmap blend
        // functions; a plain source-over composite matches the reference.
        LT_EFFECT | LT_FILTER => BlendMode::SourceOver,
        _ => BlendMode::SourceOver,
    })
}

/// Resolve the effective blend mode for a layer.
///
/// Like the old `render_path_for`, an `ltOpaque` layer only *replaces* the
/// destination at full opacity; below that it must still blend and therefore
/// degrades to [`BlendMode::SourceOver`]. Returns `None` for `ltBinder`.
pub fn resolve_blend(blend_type: i64, composed_alpha: f32) -> Option<BlendMode> {
    match blend_mode_for(blend_type)? {
        BlendMode::Replace if clamp_opacity(composed_alpha) < 1.0 => Some(BlendMode::SourceOver),
        mode => Some(mode),
    }
}

/// A layer after its ancestors' position, opacity and visibility have been
/// composed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComposedLayer {
    /// Absolute rectangle in window coordinates: the x/y origins accumulate
    /// through the ancestor chain while the size is the layer's own.
    pub rect: Rect,
    /// Product of the layer and every ancestor opacity, each clamped to
    /// `[0, 1]`.
    pub opacity: f32,
    /// `true` only when the layer and every ancestor is visible.
    pub visible: bool,
}

/// Compose a layer's position, opacity and visibility through its ancestors.
///
/// The layer's `x`/`y` origins are accumulated (`saturating_add`) through the
/// ancestor chain while its own `w`/`h` size is kept — a child is positioned
/// relative to its parent but owns its size. Invalid links stop the chain and
/// cycles are ignored after the first visit.
///
/// Ported from the old `sync::compose_layer`.
pub fn compose_layer(scene: &Scene, layer_id: u32) -> Option<ComposedLayer> {
    let mut chain = Vec::new();
    let mut current = Some(layer_id);
    let window = scene.layer(layer_id)?.window;
    // A simple ancestor chain cannot be longer than the number of layers in
    // the scene, so a depth cap detects cycles without allocating a
    // `HashSet` per call.
    let max_depth = scene.layers.len();
    while let Some(id) = current {
        if chain.len() >= max_depth {
            break;
        }
        let layer = scene.layer(id)?;
        if layer.window != window {
            break;
        }
        chain.push(layer);
        current = layer.parent;
    }
    chain.reverse();

    let mut result = ComposedLayer {
        rect: Rect::default(),
        opacity: 1.0,
        visible: true,
    };
    for layer in chain {
        result.rect.x = result.rect.x.saturating_add(layer.rect.x);
        result.rect.y = result.rect.y.saturating_add(layer.rect.y);
        result.rect.w = layer.rect.w;
        result.rect.h = layer.rect.h;
        result.opacity *= clamp_opacity(layer.opacity);
        result.visible &= layer.visible;
    }
    Some(result)
}

/// Compute the visible part of a layer's image after clipping it to the
/// layer's absolute rect.
///
/// The image is placed at `(layer_rect.x + image_left, layer_rect.y +
/// image_top)` at size `image_width × image_height`, then clipped to
/// `layer_rect`. Returns `(source, dest)`:
///
/// * `source`: the sub-region of the bitmap to sample, in bitmap pixels;
/// * `dest`: the screen-space rectangle to draw it at, in window coordinates.
///
/// `None` means the image lies entirely outside the layer. The game's buttons
/// select a sprite-sheet frame by passing a negative `image_left`; this is the
/// same routine the old sync used (`setImagePos(-frame_w * n, 0)`).
///
/// Ported from the old `sync::visible_image_region`.
#[allow(clippy::too_many_arguments)]
pub fn visible_image_region(
    layer_rect: Rect,
    image_left: i32,
    image_top: i32,
    image_width: u32,
    image_height: u32,
    bmp_w: u32,
    bmp_h: u32,
) -> Option<(PixelRegion, RectF)> {
    let img_x = layer_rect.x.saturating_add(image_left);
    let img_y = layer_rect.y.saturating_add(image_top);
    let layer_x1 = layer_rect.x.saturating_add(dim_i32(layer_rect.w));
    let layer_y1 = layer_rect.y.saturating_add(dim_i32(layer_rect.h));
    let vx0 = img_x.max(layer_rect.x);
    let vy0 = img_y.max(layer_rect.y);
    let vx1 = img_x.saturating_add(dim_i32(image_width)).min(layer_x1);
    let vy1 = img_y.saturating_add(dim_i32(image_height)).min(layer_y1);
    if vx1 <= vx0 || vy1 <= vy0 {
        return None;
    }
    let dest = RectF::new(
        vx0 as f32,
        vy0 as f32,
        (vx1 - vx0) as f32,
        (vy1 - vy0) as f32,
    );
    // `image_width/height` is the drawn size; map the visible region back to
    // bitmap pixels (equal in the common `loadImages` case).
    let scale_x = if image_width == 0 {
        1.0
    } else {
        bmp_w as f32 / image_width as f32
    };
    let scale_y = if image_height == 0 {
        1.0
    } else {
        bmp_h as f32 / image_height as f32
    };
    let sx = (((vx0 - img_x) as f32) * scale_x).floor().max(0.0) as u32;
    let sy = (((vy0 - img_y) as f32) * scale_y).floor().max(0.0) as u32;
    let sw = (((dest.width * scale_x).ceil() as u32).min(bmp_w.saturating_sub(sx))).max(1);
    let sh = (((dest.height * scale_y).ceil() as u32).min(bmp_h.saturating_sub(sy))).max(1);
    Some((PixelRegion::new(sx, sy, sw, sh), dest))
}

/// Convert a straight-alpha `[u8; 4]` fill colour to float straight alpha.
///
/// The composed opacity is carried separately in [`RenderNode::opacity`], so
/// the RGB channels are left untouched (the old renderer baked opacity into
/// the alpha-only tint; here the pipeline owns that multiplication).
fn solid_color(color: [u8; 4]) -> [f32; 4] {
    [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        color[3] as f32 / 255.0,
    ]
}

/// Whether `layer_id`'s bitmap is a synthesized **screen buffer** (a window
/// primary layer's composited `MainImage`) rather than drawable content.
///
/// The reference composites the layer tree into the primary layer's
/// `MainImage` and scripts read it back (`piledCopy`, `saveLayerImage`), so
/// drawing it would blit the snapshot captured when the script last read it.
fn is_screen_buffer_layer(scene: &Scene, layer_id: u32) -> bool {
    scene
        .layer(layer_id)
        .and_then(|layer| layer.bitmap)
        .and_then(|bitmap| scene.bitmap(bitmap))
        .is_some_and(|bitmap| bitmap.screen_buffer)
}

/// Clamp a `u32` pixel dimension to a non-negative `i32` (a plain `as i32`
/// would wrap for sizes above `i32::MAX`).
fn dim_i32(v: u32) -> i32 {
    v.min(i32::MAX as u32) as i32
}

/// The dimensions a bitmap presents to the projection, at least 1×1.
fn bitmap_dims(scene: &Scene, layer: &LayerState) -> (u32, u32) {
    layer
        .bitmap
        .and_then(|id| scene.bitmap(id))
        .map_or((1, 1), |bitmap| (bitmap.width.max(1), bitmap.height.max(1)))
}

/// Mutable bookkeeping shared while appending several windows to one scene.
#[derive(Default)]
struct ProjectionState {
    /// Monotonic sibling sort key across all windows of the scene.
    next_z: i32,
}

/// Append one window's drawn layers to `out`.
///
/// `Scene::window_layer_order` is parent-first, so a single forward pass
/// composes every layer's ancestor position/opacity/visibility (via
/// [`ComposedLayer`]) and resolves its nearest drawn ancestor. The old code
/// rebuilt that information per layer with a `Vec` and a `HashSet` walk; this
/// version allocates only two maps per window.
fn append_window<F>(
    scene: &Scene,
    window: u32,
    out: &mut RenderScene,
    state: &mut ProjectionState,
    texture: &mut F,
) where
    F: FnMut(u32) -> Option<Texture>,
{
    let Some(window_state) = scene.window(window) else {
        return;
    };
    if !window_state.visible {
        return;
    }
    let win_opacity = clamp_opacity(window_state.opacity);

    // Per-window composition state, keyed by layer id. Both maps only ever
    // hold this window's layers, so a cross-window `parent` link simply misses
    // and stops the chain exactly like the old ancestor walk.
    let mut composed: HashMap<u32, ComposedLayer> = HashMap::new();
    // Maps a layer id to the node its children should link to (its own node
    // when drawn, otherwise its nearest drawn ancestor).
    let mut nearest: HashMap<u32, RenderNodeId> = HashMap::new();

    for layer_id in scene.window_layer_order(window) {
        let Some(layer) = scene.layer(layer_id) else {
            continue;
        };

        let mut node_composed = layer
            .parent
            .and_then(|parent| composed.get(&parent).copied())
            .unwrap_or(ComposedLayer {
                rect: Rect::default(),
                opacity: 1.0,
                visible: true,
            });
        node_composed.rect.x = node_composed.rect.x.saturating_add(layer.rect.x);
        node_composed.rect.y = node_composed.rect.y.saturating_add(layer.rect.y);
        node_composed.rect.w = layer.rect.w;
        node_composed.rect.h = layer.rect.h;
        node_composed.opacity *= clamp_opacity(layer.opacity);
        node_composed.visible &= layer.visible;
        composed.insert(layer_id, node_composed);

        // The nearest drawn ancestor's node, or the root.
        let parent_node = layer
            .parent
            .and_then(|parent| nearest.get(&parent).copied())
            .unwrap_or(out.root);

        // Build the node only when the layer actually draws. Every `break`
        // means "skipped": the layer still passes `parent_node` on to its
        // children through `nearest` below.
        let node = 'layer: {
            if is_screen_buffer_layer(scene, layer_id) {
                break 'layer None;
            }
            if !node_composed.visible {
                break 'layer None;
            }
            let opacity = win_opacity * node_composed.opacity;
            let Some(blend) = resolve_blend(layer.blend_type, opacity) else {
                // `ltBinder`: a container, not a drawable layer. Its children
                // are separate nodes and still render.
                break 'layer None;
            };

            // A bitmap layer draws its texture; a fill layer draws its solid
            // colour; a layer with neither draws nothing.
            let resolved_texture = if let Some(id) = layer.bitmap {
                texture(id)
            } else {
                None
            };
            let content = match resolved_texture {
                Some(handle) => RenderContent::Texture(handle),
                None => match layer.fill_color {
                    Some(fill) => RenderContent::Solid(solid_color(fill)),
                    None => break 'layer None,
                },
            };

            // A texture's destination/source come from the image placement
            // model; a solid fill covers the whole layer rect with no source
            // region.
            let (dst, src) = match &content {
                RenderContent::Texture(_) => {
                    let (bmp_w, bmp_h) = bitmap_dims(scene, layer);
                    let img_w = if layer.image_width > 0 {
                        layer.image_width
                    } else {
                        bmp_w
                    };
                    let img_h = if layer.image_height > 0 {
                        layer.image_height
                    } else {
                        bmp_h
                    };
                    match visible_image_region(
                        node_composed.rect,
                        layer.image_left,
                        layer.image_top,
                        img_w,
                        img_h,
                        bmp_w,
                        bmp_h,
                    ) {
                        Some((src, dst)) => (dst, src),
                        // Fully clipped away: the layer draws nothing.
                        None => break 'layer None,
                    }
                }
                _ => (
                    RectF::new(
                        node_composed.rect.x as f32,
                        node_composed.rect.y as f32,
                        node_composed.rect.w as f32,
                        node_composed.rect.h as f32,
                    ),
                    PixelRegion::ZERO,
                ),
            };

            Some(RenderNode {
                parent: Some(parent_node),
                content,
                dst,
                src,
                opacity,
                visible: true,
                z: state.next_z,
                blend,
                hold_alpha: layer.hold_alpha,
                ..RenderNode::default()
            })
        };

        match node {
            Some(node) => {
                let id = out.push(node);
                if let Some(parent_node) = out.node_mut(parent_node) {
                    parent_node.children.push(id);
                }
                nearest.insert(layer_id, id);
                state.next_z = state.next_z.saturating_add(1);
            }
            None => {
                nearest.insert(layer_id, parent_node);
            }
        }
    }
}

/// Project a single window's layer subtree into a fresh [`RenderScene`].
///
/// The returned scene contains a default root node plus one node per drawn
/// layer, in `Scene::window_layer_order`, with ancestor position, opacity and
/// visibility already composed. `texture` resolves a bitmap id to an L1
/// texture handle; the caller owns the texture table and can pass, for
/// example, `|id| textures.get(&id).cloned()`.
pub fn project_window<F>(scene: &Scene, window: u32, mut texture: F) -> RenderScene
where
    F: FnMut(u32) -> Option<Texture>,
{
    let mut out = RenderScene::with_root_node();
    out.background = Some(BACKGROUND_CLEAR);
    let mut state = ProjectionState::default();
    append_window(scene, window, &mut out, &mut state, &mut texture);
    out
}

/// Project every window of a [`Scene`] into one fresh [`RenderScene`].
///
/// Windows are appended in `Scene::windows` order; the sibling `z` sequence
/// and parent links are shared across windows so the whole tree renders
/// deterministically from one root. Hidden windows contribute no nodes.
///
/// `texture` resolves a bitmap id to an L1 texture handle (see
/// [`project_window`]).
pub fn project_scene<F>(scene: &Scene, mut texture: F) -> RenderScene
where
    F: FnMut(u32) -> Option<Texture>,
{
    let mut out = RenderScene::with_root_node();
    out.background = Some(BACKGROUND_CLEAR);
    let mut state = ProjectionState::default();
    for window in &scene.windows {
        append_window(scene, window.id, &mut out, &mut state, &mut texture);
    }
    out
}
