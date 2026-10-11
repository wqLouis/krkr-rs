//! Projection of the L2 TVP layer tree into the L1 [`RenderScene`] tree.
//!
//! The middle layer owns the logical [`Scene`] (windows, layers, bitmaps)
//! and every TVP property. The bottom [`render::RenderPipeline`]
//! consumes an abstract [`RenderScene`] tree and owns traversal, batching and
//! drawing. This module is the single seam between the two: it is a **pure
//! field copy** that turns one window (or a whole scene) into a
//! [`RenderScene`] with one [`RenderNode`] per *drawn* layer. It is not a
//! draw list: no batching, sorting or traversal happens here.
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
//! * [`resolve_blend`] passes the layer's generic [`BlendMode`] through to
//!   the render node (downgrading an opaque `Replace` below full opacity).
//!
//! Screen-buffer primary layers, hidden/empty layers and layers whose texture
//! is missing are skipped, matching `sync_scene`.

use std::collections::{HashMap, HashSet};

use api::blend::BlendMode;
use api::geometry::{PixelRegion, RectF};
use render::{RenderContent, RenderNode, RenderNodeId, RenderScene, Texture, ViewTransform};

use crate::scene::{LayerState, Rect, Scene};

/// How the engine presents the logical scene inside the framebuffer.
///
/// `Game` uses the declared size one-to-one; the other three modes scale the
/// game's declared size to the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScalingMode {
    /// Present the logical size one-to-one (the Win32 environment default).
    #[default]
    Game,
    /// Uniform scale that fits the whole scene, letterboxed.
    Fit,
    /// Non-uniform scale that fills the window.
    Fill,
    /// Grow the logical size to the window's aspect, then scale uniformly.
    Expand,
}

/// The logical size and framebuffer transform for one presentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presentation {
    /// Logical scene size reported to scripts (`System.screenWidth`).
    pub logical_size: (u32, u32),
    /// Framebuffer transform the pipeline applies while compositing.
    pub view: ViewTransform,
}

impl Presentation {
    /// The untransformed presentation of `size`.
    pub const fn identity(size: (u32, u32)) -> Self {
        Self {
            logical_size: size,
            view: ViewTransform::IDENTITY,
        }
    }

    /// Computes the presentation for `mode` from the game's declared `design`
    /// size and the `framebuffer` size in pixels.
    ///
    /// Scaling is computed once from the integer sizes, so it does not
    /// accumulate across frames. A zero design or framebuffer dimension is
    /// degenerate (there is no framebuffer to present to): the logical size
    /// stays `design` and the transform is the identity, so no division by
    /// zero can occur.
    pub fn compute(mode: ScalingMode, design: (u32, u32), framebuffer: (u32, u32)) -> Self {
        let (design_w, design_h) = design;
        let (frame_w, frame_h) = framebuffer;
        if design_w == 0 || design_h == 0 || frame_w == 0 || frame_h == 0 {
            return Self::identity(design);
        }
        let (dw, dh) = (design_w as f64, design_h as f64);
        let (fw, fh) = (frame_w as f64, frame_h as f64);
        match mode {
            // The logical screen is the window client area, one logical pixel
            // per framebuffer pixel.
            ScalingMode::Game => Self::identity(framebuffer),
            // One uniform scale, centred; the free axis is letterboxed.
            ScalingMode::Fit => {
                let scale = (fw / dw).min(fh / dh);
                Self {
                    logical_size: design,
                    view: ViewTransform {
                        scale: (scale as f32, scale as f32),
                        offset: (
                            ((fw - dw * scale) * 0.5) as f32,
                            ((fh - dh * scale) * 0.5) as f32,
                        ),
                    },
                }
            }
            // Independent axis scales; the frame is covered.
            ScalingMode::Fill => Self {
                logical_size: design,
                view: ViewTransform {
                    scale: ((fw / dw) as f32, (fh / dh) as f32),
                    offset: (0.0, 0.0),
                },
            },
            // Keep the design height and rewrite the width to the frame's
            // aspect, so the uniform height scale fills exactly.
            ScalingMode::Expand => {
                let logical_w = (dh * fw / fh).round().clamp(0.0, u32::MAX as f64) as u32;
                let scale = (fh / dh) as f32;
                Self {
                    logical_size: (logical_w, design_h),
                    view: ViewTransform {
                        scale: (scale, scale),
                        offset: (0.0, 0.0),
                    },
                }
            }
        }
    }
}

/// Background clear colour used when a projected scene has no explicit one.
///
/// The old Bevy camera cleared to opaque black
/// (`ClearColorConfig::Custom(Color::srgb(0.0, 0.0, 0.0))`), so the
/// projection preserves that.
pub(crate) const BACKGROUND_CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// Clamp an opacity to `[0, 1]`, mapping NaN to `0`.
///
/// Ported from the old `sync::clamp_opacity` so window and layer opacities
/// compose identically.
pub(crate) fn clamp_opacity(opacity: f32) -> f32 {
    if opacity.is_nan() {
        0.0
    } else {
        opacity.clamp(0.0, 1.0)
    }
}

/// Resolve the effective blend mode for a layer.
///
/// L2 stores the render pipeline's generic [`BlendMode`] directly, so the
/// resolved mode is the stored one unchanged — except that an opaque
/// [`BlendMode::Replace`] only actually *replaces* the destination at full
/// opacity; below that it must still blend and therefore degrades to
/// [`BlendMode::SourceOver`].
pub fn resolve_blend(blend: BlendMode, composed_alpha: f32) -> BlendMode {
    match blend {
        BlendMode::Replace if clamp_opacity(composed_alpha) < 1.0 => BlendMode::SourceOver,
        mode => mode,
    }
}

/// A layer after its ancestors' position, opacity and visibility have been
/// composed.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
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
pub(crate) fn visible_image_region(
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
    // Layers whose subtree is drawn through a cache bitmap: the cached layer
    // itself once its cache is rendered, and every descendant the cache
    // already contains.
    let mut cached_subtree: HashSet<u32> = HashSet::new();

    // Why layers produced no node — a scene that renders almost nothing is
    // invisible from the outside (`RUST_LOG=debug` reports it).
    let (mut drawn, mut screen_buffer, mut hidden, mut empty, mut clipped) =
        (0usize, 0usize, 0usize, 0usize, 0usize);

    for layer_id in scene.window_layer_order(window) {
        let Some(layer) = scene.layer(layer_id) else {
            continue;
        };

        // A cached ancestor already contains this layer's pixels in its
        // rendered bitmap (`InternalComplete2` draws the subtree into
        // `CacheBitmap`, `LayerIntf.cpp:7156-7226`), so it contributes no
        // node of its own. The flag propagates so the whole subtree is
        // skipped.
        let ancestor_cached = layer
            .parent
            .is_some_and(|parent| cached_subtree.contains(&parent));
        if ancestor_cached {
            cached_subtree.insert(layer_id);
            continue;
        }
        // A layer whose cache has been rendered draws that cache instead of
        // its own image and descendants. A layer that merely enabled caching
        // but has no cache bitmap yet (its subtree is trivial) draws
        // normally.
        let cached_bitmap = layer
            .cache_bitmap
            .filter(|_| layer.cache_enabled_count != 0);
        if cached_bitmap.is_some() {
            cached_subtree.insert(layer_id);
        }

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
                screen_buffer += 1;
                break 'layer None;
            }
            if !node_composed.visible {
                hidden += 1;
                break 'layer None;
            }
            let opacity = win_opacity * node_composed.opacity;
            let blend = resolve_blend(layer.blend, opacity);

            // A layer draws its texture; a cached layer draws its rendered
            // bitmap in place of its MainImage; a layer with neither draws
            // nothing.
            let resolved_texture = match cached_bitmap.or(layer.bitmap) {
                Some(id) => texture(id),
                None => None,
            };
            let Some(content) = resolved_texture.map(RenderContent::Texture) else {
                empty += 1;
                break 'layer None;
            };

            // A texture's destination/source come from the image placement
            // model.
            let (dst, src) = match &content {
                RenderContent::Texture(_) => {
                    // The cache is the layer's own rendered content: it draws
                    // over the whole layer rect with no image placement of its
                    // own (the reference hands `CacheBitmap` to the parent at
                    // the layer's rect, `LayerIntf.cpp:7221-7226`).
                    let (bmp_w, bmp_h) = match cached_bitmap {
                        Some(id) => scene
                            .bitmap(id)
                            .map_or((1, 1), |b| (b.width.max(1), b.height.max(1))),
                        None => bitmap_dims(scene, layer),
                    };
                    let (image_left, image_top, img_w, img_h) = match cached_bitmap {
                        Some(_) => (0, 0, bmp_w, bmp_h),
                        None => (
                            layer.image_left,
                            layer.image_top,
                            if layer.image_width > 0 {
                                layer.image_width
                            } else {
                                bmp_w
                            },
                            if layer.image_height > 0 {
                                layer.image_height
                            } else {
                                bmp_h
                            },
                        ),
                    };
                    match visible_image_region(
                        node_composed.rect,
                        image_left,
                        image_top,
                        img_w,
                        img_h,
                        bmp_w,
                        bmp_h,
                    ) {
                        Some((src, dst)) => (dst, src),
                        // Fully clipped away: the layer draws nothing.
                        None => {
                            clipped += 1;
                            log::debug!(
                                "projection: layer {layer_id} clipped away: rect={:?} \
                                 image=({},{}) {}x{} bitmap={}x{} parent={:?}",
                                node_composed.rect,
                                image_left,
                                image_top,
                                img_w,
                                img_h,
                                bmp_w,
                                bmp_h,
                                layer.parent
                            );
                            break 'layer None;
                        }
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
                drawn += 1;
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

    log::debug!(
        "projection: window {window}: drew {drawn} layer(s); skipped screen_buffer={screen_buffer} \
         hidden={hidden} empty={empty} clipped={clipped} of {}",
        scene.window_layer_order(window).len()
    );
}

/// Project a single window's layer subtree into a fresh [`RenderScene`].
///
/// The returned scene contains a default root node plus one node per drawn
/// layer, in `Scene::window_layer_order`, with ancestor position, opacity and
/// visibility already composed. `view` is the framebuffer transform the
/// pipeline applies to the whole tree. `texture` resolves a bitmap id to an
/// L1 texture handle; the caller owns the texture table and can pass, for
/// example, `|id| textures.get(&id).cloned()`.
pub fn project_window<F>(
    scene: &Scene,
    window: u32,
    view: ViewTransform,
    mut texture: F,
) -> RenderScene
where
    F: FnMut(u32) -> Option<Texture>,
{
    let mut out = RenderScene::with_root_node();
    out.background = Some(BACKGROUND_CLEAR);
    out.view = view;
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
/// `view` is the framebuffer transform applied to the whole tree and
/// `texture` resolves a bitmap id to an L1 texture handle (see
/// [`project_window`]).
pub fn project_scene<F>(scene: &Scene, view: ViewTransform, mut texture: F) -> RenderScene
where
    F: FnMut(u32) -> Option<Texture>,
{
    let mut out = RenderScene::with_root_node();
    out.background = Some(BACKGROUND_CLEAR);
    out.view = view;
    let mut state = ProjectionState::default();
    for window in &scene.windows {
        append_window(scene, window.id, &mut out, &mut state, &mut texture);
    }
    out
}
