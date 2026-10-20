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
//! Hidden/empty layers and layers whose texture is missing are skipped,
//! matching `sync_scene`.

use std::collections::{HashMap, HashSet};

use api::blend::BlendMode;
use api::geometry::{PixelRegion, RectF};
use render::{Clip, RenderContent, RenderEffect, RenderNode, RenderNodeId, RenderScene, Texture};

use crate::scene::{Rect, Scene, TransitionSource};

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
    /// Intersection of the layer's own absolute rectangle and every ancestor
    /// rectangle. A parent rectangle clips its whole subtree, so this is the
    /// clip for the layer and its descendants. `None` only before a layer
    /// rectangle has been composed.
    pub clip: Option<Rect>,
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
        clip: None,
    };
    for layer in chain {
        let rect = Rect::new(
            result.rect.x.saturating_add(layer.rect.x),
            result.rect.y.saturating_add(layer.rect.y),
            layer.rect.w,
            layer.rect.h,
        );
        let clip_rect = scene
            .clipping_rect(layer.id)
            .map(|clip| Rect::new(rect.x, rect.y, clip.w, clip.h))
            .unwrap_or(rect);
        result.clip = Some(match result.clip {
            Some(clip) => intersect_rect(clip, clip_rect),
            None => clip_rect,
        });
        result.rect = rect;
        result.opacity *= clamp_opacity(layer.opacity);
        result.visible &= layer.visible;
    }
    Some(result)
}

/// Intersect two layer rectangles. A non-overlapping pair yields a zero-sized
/// rectangle at the shared origin, which the renderers turn into an empty
/// scissor.
fn intersect_rect(a: Rect, b: Rect) -> Rect {
    let a_right = a.x.saturating_add(dim_i32(a.w));
    let b_right = b.x.saturating_add(dim_i32(b.w));
    let a_bottom = a.y.saturating_add(dim_i32(a.h));
    let b_bottom = b.y.saturating_add(dim_i32(b.h));
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = a_right.min(b_right);
    let bottom = a_bottom.min(b_bottom);
    Rect::new(
        left,
        top,
        right.saturating_sub(left).max(0) as u32,
        bottom.saturating_sub(top).max(0) as u32,
    )
}

/// Clip rectangle for a composed layer's render node.
fn composed_clip(composed: &ComposedLayer) -> Option<Clip> {
    composed.clip.map(|clip| {
        Clip::Rect(RectF::new(
            clip.x as f32,
            clip.y as f32,
            clip.w as f32,
            clip.h as f32,
        ))
    })
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
pub(crate) fn visible_image_region(
    layer_rect: Rect,
    image: Rect,
    bmp: (u32, u32),
) -> Option<(PixelRegion, RectF)> {
    let image_left = image.x;
    let image_top = image.y;
    let image_width = image.w;
    let image_height = image.h;
    let (bmp_w, bmp_h) = bmp;
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

/// Clamp a `u32` pixel dimension to a non-negative `i32` (a plain `as i32`
/// would wrap for sizes above `i32::MAX`).
fn dim_i32(v: u32) -> i32 {
    v.min(i32::MAX as u32) as i32
}

/// One transition source placed in window space.
struct PlacedSource {
    /// Destination rectangle in window coordinates.
    dst: RectF,
    /// Source region, in frame pixels.
    src: PixelRegion,
    /// Composed opacity of this source.
    opacity: f32,
    /// Composition mode over the frame's other source.
    blend: BlendMode,
}

/// The part of a frame rectangle that is visible, in frame pixels.
///
/// `visible_src` is the visible part of the frame; the rectangle is
/// intersected with it. `None` when the rectangle contributes no pixel.
fn visible_frame_rect(dst: Rect, visible_src: PixelRegion) -> Option<(i32, i32, i32, i32)> {
    let x0 = dst.x.max(dim_i32(visible_src.x));
    let y0 = dst.y.max(dim_i32(visible_src.y));
    let x1 = dst
        .x
        .saturating_add(dim_i32(dst.w))
        .min(dim_i32(visible_src.right()));
    let y1 = dst
        .y
        .saturating_add(dim_i32(dst.h))
        .min(dim_i32(visible_src.bottom()));
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// The screen rectangle a visible frame rectangle covers.
///
/// `visible` is the rectangle's visible part in frame pixels and `visible_dst`
/// where the frame's visible part lands on screen; the rectangle is mapped
/// through the same offset and scale.
fn frame_rect_on_screen(
    visible: (i32, i32, i32, i32),
    visible_src: PixelRegion,
    visible_dst: RectF,
) -> RectF {
    let (x0, y0, x1, y1) = visible;
    let scale_x = if visible_src.width == 0 {
        1.0
    } else {
        visible_dst.width / visible_src.width as f32
    };
    let scale_y = if visible_src.height == 0 {
        1.0
    } else {
        visible_dst.height / visible_src.height as f32
    };
    RectF::new(
        visible_dst.x + (x0 - dim_i32(visible_src.x)) as f32 * scale_x,
        visible_dst.y + (y0 - dim_i32(visible_src.y)) as f32 * scale_y,
        (x1 - x0) as f32 * scale_x,
        (y1 - y0) as f32 * scale_y,
    )
}

/// The screen rectangle a frame rectangle covers.
///
/// `visible_dst` is where the frame's visible part lands on screen. `None`
/// when the rectangle contributes no visible pixel.
fn place_frame_rect(dst: Rect, visible_src: PixelRegion, visible_dst: RectF) -> Option<RectF> {
    let visible = visible_frame_rect(dst, visible_src)?;
    Some(frame_rect_on_screen(visible, visible_src, visible_dst))
}

/// Places one source of a transition frame on screen.
///
/// `visible_src` is the visible part of the frame in frame pixels and
/// `visible_dst` where that part lands on screen; the source's own rectangle is
/// intersected with it and mapped through the same offset and scale. `None`
/// when the source contributes no visible pixel.
fn place_frame_source(
    source: TransitionSource,
    visible_src: PixelRegion,
    visible_dst: RectF,
    opacity: f32,
    blend: BlendMode,
) -> Option<PlacedSource> {
    let dst = source.dst;
    let src = source.src;
    let (x0, y0, x1, y1) = visible_frame_rect(dst, visible_src)?;
    let screen = frame_rect_on_screen((x0, y0, x1, y1), visible_src, visible_dst);
    // A source region outside its bitmap would sample clamped texels.
    let src_x = src.x.saturating_add(x0.saturating_sub(dst.x));
    let src_y = src.y.saturating_add(y0.saturating_sub(dst.y));
    if src_x < 0 || src_y < 0 {
        return None;
    }
    Some(PlacedSource {
        dst: screen,
        src: PixelRegion::new(
            src_x as u32,
            src_y as u32,
            (x1 - x0) as u32,
            (y1 - y0) as u32,
        ),
        opacity: opacity * source.alpha.clamp(0.0, 1.0),
        // A source fading over the one drawn before it composites
        // source-over; a full-opacity source is an independent placement and
        // keeps the layer's own blend.
        blend: if source.alpha >= 1.0 {
            blend
        } else {
            BlendMode::SourceOver
        },
    })
}

/// The dimensions a bitmap presents to the projection, at least 1×1.
fn bitmap_dims(scene: &Scene, bitmap: Option<u32>) -> (u32, u32) {
    bitmap
        .and_then(|id| scene.bitmap(id))
        .map_or((1, 1), |bitmap| (bitmap.width.max(1), bitmap.height.max(1)))
}

/// Mutable bookkeeping shared while appending several windows to one scene.
#[derive(Default)]
struct ProjectionState {
    /// Monotonic sibling sort key across all windows of the scene.
    next_z: i32,
    /// The nodes the current layer emits.
    ///
    /// The vector is reused for every layer, so a layer that emits several
    /// nodes (a custom-shader draw with several passes) costs no allocation.
    nodes: Vec<RenderNode>,
}

/// One projection pass plus the layer ids whose pixels it emitted.
///
/// `drawn_layers` follows the actual emit path: a layer contributes its id when
/// it pushes a render node, and a cached layer also contributes the visible
/// descendants whose pixels are baked into that node's texture.
#[derive(Debug)]
pub struct ProjectionReport {
    /// The projected render tree.
    pub scene: RenderScene,
    /// Layer ids whose pixels this projection emitted, in emit order.
    pub drawn_layers: Vec<u32>,
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
    drawn_layers: &mut Vec<u32>,
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
    // Descendants whose pixels are drawn through a cached ancestor, keyed by
    // the layer that owns the emitted cache bitmap.
    let mut cache_owner: HashMap<u32, u32> = HashMap::new();
    // Cached layers whose node was actually emitted this pass.
    let mut emitted_cached: HashSet<u32> = HashSet::new();

    // Why layers produced no node — a scene that renders almost nothing is
    // invisible from the outside (`RUST_LOG=debug` reports it).
    let (mut drawn, mut hidden, mut empty, mut clipped) = (0usize, 0usize, 0usize, 0usize);

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
            .and_then(|parent| cache_owner.get(&parent).copied());
        if let Some(owner) = ancestor_cached {
            cache_owner.insert(layer_id, owner);
            if emitted_cached.contains(&owner) && scene.node_visible(layer_id) {
                drawn_layers.push(layer_id);
            }
            continue;
        }
        // The image this layer draws for itself: a transition that blends the
        // layer's image with another layer's without consuming the subtree
        // stores a self image that stands in for the main image. It covers
        // only this layer's own image — the children keep drawing live — so
        // the layer never becomes a cached-subtree root.
        let self_bitmap = layer.drawn_image();
        // A layer whose cache has been rendered draws that cache instead of
        // its own image and descendants; a layer that merely enabled caching
        // but has no cache bitmap yet (its subtree is trivial) draws normally.
        // A self image covers only the layer's own image and leaves the
        // children live, so it takes precedence over the cached subtree.
        let cached_bitmap = layer
            .cache_bitmap
            .filter(|_| layer.cache_enabled_count != 0 && layer.transition_self.is_none());
        // A running transition the host composites draws its two sources in
        // place of the layer's display bitmap, at the same placement and with
        // the same precedence. Its whole-subtree form holds the children, so
        // it covers the subtree like a cache does.
        let transition_frame = layer.transition_frame;
        let transition_covers_subtree = transition_frame.is_some_and(|frame| frame.covers_layer);
        // A running transition the backend draws with a custom shader emits
        // its passes in place of the layer's image. Its whole-subtree form
        // holds the children, so it covers the subtree like a cache does.
        let effect = layer.effect.as_ref();
        let effect_covers_subtree = effect.is_some_and(|draw| draw.covers_layer);
        if cached_bitmap.is_some() || transition_covers_subtree || effect_covers_subtree {
            cache_owner.insert(layer_id, layer_id);
        }

        let mut node_composed = layer
            .parent
            .and_then(|parent| composed.get(&parent).copied())
            .unwrap_or(ComposedLayer {
                rect: Rect::default(),
                opacity: 1.0,
                visible: true,
                clip: None,
            });
        node_composed.rect.x = node_composed.rect.x.saturating_add(layer.rect.x);
        node_composed.rect.y = node_composed.rect.y.saturating_add(layer.rect.y);
        node_composed.rect.w = layer.rect.w;
        node_composed.rect.h = layer.rect.h;
        let clip_rect = scene
            .clipping_rect(layer_id)
            .map(|clip| Rect::new(node_composed.rect.x, node_composed.rect.y, clip.w, clip.h))
            .unwrap_or(node_composed.rect);
        node_composed.clip = Some(match node_composed.clip {
            Some(clip) => intersect_rect(clip, clip_rect),
            None => clip_rect,
        });
        node_composed.opacity *= clamp_opacity(layer.opacity);
        node_composed.visible &= layer.visible;
        composed.insert(layer_id, node_composed);

        // The nearest drawn ancestor's node, or the root.
        let parent_node = layer
            .parent
            .and_then(|parent| nearest.get(&parent).copied())
            .unwrap_or(out.root);

        // Build the node(s) the layer draws. Every `break` means "skipped":
        // the layer still passes `parent_node` on to its children through
        // `nearest` below. A layer draws one image, except while a transition
        // runs: its frame is two source draws and its custom-shader draw is
        // one node per pass.
        state.nodes.clear();
        'layer: {
            if !node_composed.visible {
                hidden += 1;
                break 'layer;
            }
            if node_composed
                .clip
                .is_some_and(|clip| clip.w == 0 || clip.h == 0)
            {
                clipped += 1;
                break 'layer;
            }
            let opacity = win_opacity * node_composed.opacity;
            let blend = resolve_blend(layer.blend, opacity);

            // A custom-shader draw: the passes are placed inside the draw's
            // own frame, and the frame where the layer's own image would be.
            if let Some(draw) = effect {
                let (frame_w, frame_h) = draw.size;
                let (image_left, image_top, img_w, img_h) = if draw.covers_layer {
                    (0, 0, frame_w, frame_h)
                } else {
                    (
                        layer.image_left,
                        layer.image_top,
                        if layer.image_width > 0 {
                            layer.image_width
                        } else {
                            frame_w
                        },
                        if layer.image_height > 0 {
                            layer.image_height
                        } else {
                            frame_h
                        },
                    )
                };
                let image = Rect::new(image_left, image_top, img_w, img_h);
                let Some((visible_src, visible_dst)) =
                    visible_image_region(node_composed.rect, image, (frame_w, frame_h))
                else {
                    clipped += 1;
                    break 'layer;
                };
                for pass in &draw.passes {
                    let Some(pipeline) = scene.effect_pipelines.handle(pass.program) else {
                        log::warn!(
                            "projection: layer {layer_id} names the {:?} program, which this \
                             scene has no pipeline for; the pass is not drawn",
                            pass.program
                        );
                        continue;
                    };
                    // The bindings are positional: a missing texture would
                    // shift the ones after it, so the pass is not drawn at all.
                    let mut textures = Vec::with_capacity(pass.textures.len());
                    let mut missing = false;
                    for source in &pass.textures {
                        match texture(source.bitmap) {
                            Some(resolved) => textures.push(resolved),
                            None => {
                                missing = true;
                                break;
                            }
                        }
                    }
                    if missing {
                        log::warn!(
                            "projection: layer {layer_id} samples a bitmap that has no texture; \
                             the pass is not drawn"
                        );
                        continue;
                    }
                    let Some(dst) = place_frame_rect(pass.dst, visible_src, visible_dst) else {
                        continue;
                    };
                    let node = RenderNode {
                        parent: Some(parent_node),
                        content: RenderContent::Effect(Box::new(RenderEffect::new(
                            pipeline.clone(),
                            textures,
                            pass.uniforms,
                        ))),
                        dst,
                        src: PixelRegion::ZERO,
                        opacity,
                        visible: true,
                        z: state.next_z,
                        // The pipeline carries the blend; the node's own mode
                        // is only a diagnostic.
                        blend: BlendMode::SourceOver,
                        hold_alpha: layer.hold_alpha,
                        clip: composed_clip(&node_composed),
                        ..RenderNode::default()
                    };
                    state.next_z = state.next_z.saturating_add(1);
                    state.nodes.push(node);
                }
                if state.nodes.is_empty() {
                    empty += 1;
                }
                break 'layer;
            }

            // A transition frame: the two sources are placed inside the frame
            // and the frame where the layer's own image would be.
            if let Some(frame) = transition_frame {
                let (frame_w, frame_h) = frame.size;
                let (image_left, image_top, img_w, img_h) = if frame.covers_layer {
                    (0, 0, frame_w, frame_h)
                } else {
                    (
                        layer.image_left,
                        layer.image_top,
                        if layer.image_width > 0 {
                            layer.image_width
                        } else {
                            frame_w
                        },
                        if layer.image_height > 0 {
                            layer.image_height
                        } else {
                            frame_h
                        },
                    )
                };
                let image = Rect::new(image_left, image_top, img_w, img_h);
                let Some((visible_src, visible_dst)) =
                    visible_image_region(node_composed.rect, image, (frame_w, frame_h))
                else {
                    clipped += 1;
                    break 'layer;
                };
                for source in [frame.src1, frame.src2] {
                    // A source at zero opacity is not part of this frame.
                    if source.alpha <= 0.0 {
                        continue;
                    }
                    let Some(resolved) = texture(source.bitmap) else {
                        continue;
                    };
                    let Some(placed) =
                        place_frame_source(source, visible_src, visible_dst, opacity, blend)
                    else {
                        continue;
                    };
                    let node = RenderNode {
                        parent: Some(parent_node),
                        content: RenderContent::Texture(resolved),
                        dst: placed.dst,
                        src: placed.src,
                        opacity: placed.opacity,
                        visible: true,
                        z: state.next_z,
                        blend: placed.blend,
                        hold_alpha: layer.hold_alpha,
                        clip: composed_clip(&node_composed),
                        ..RenderNode::default()
                    };
                    state.next_z = state.next_z.saturating_add(1);
                    state.nodes.push(node);
                }
                if state.nodes.is_empty() {
                    empty += 1;
                }
                break 'layer;
            }

            // A layer draws its texture; a cached layer draws its rendered
            // bitmap in place of its MainImage; a self image stands where
            // the MainImage would be; a layer with none of them draws
            // nothing.
            let drawn_bitmap = cached_bitmap.or(self_bitmap);
            let resolved_texture = match drawn_bitmap {
                Some(id) => texture(id),
                None => None,
            };
            let Some(content) = resolved_texture.map(RenderContent::Texture) else {
                empty += 1;
                break 'layer;
            };

            // A texture's destination/source come from the image placement
            // model.
            let (dst, src) = match &content {
                RenderContent::Texture(_) => {
                    // The cache is the layer's own rendered content: it draws
                    // over the whole layer rect with no image placement of its
                    // own (the reference hands `CacheBitmap` to the parent at
                    // the layer's rect, `LayerIntf.cpp:7221-7226`).
                    let (bmp_w, bmp_h) = bitmap_dims(scene, drawn_bitmap);
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
                    let image = Rect::new(image_left, image_top, img_w, img_h);
                    match visible_image_region(node_composed.rect, image, (bmp_w, bmp_h)) {
                        Some((src, dst)) => (dst, src),
                        // Fully clipped away: the layer draws nothing.
                        None => {
                            clipped += 1;
                            log::trace!(
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
                            break 'layer;
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

            state.nodes.push(RenderNode {
                parent: Some(parent_node),
                content,
                dst,
                src,
                opacity,
                visible: true,
                z: state.next_z,
                blend,
                hold_alpha: layer.hold_alpha,
                clip: composed_clip(&node_composed),
                ..RenderNode::default()
            });
            state.next_z = state.next_z.saturating_add(1);
        };

        if !state.nodes.is_empty() {
            drawn += 1;
            drawn_layers.push(layer_id);
            if cached_bitmap.is_some() || transition_covers_subtree || effect_covers_subtree {
                emitted_cached.insert(layer_id);
            }
            let mut last = parent_node;
            for node in state.nodes.drain(..) {
                let id = out.push(node);
                if let Some(parent_node) = out.node_mut(parent_node) {
                    parent_node.children.push(id);
                }
                last = id;
            }
            nearest.insert(layer_id, last);
        } else {
            nearest.insert(layer_id, parent_node);
        }
    }

    log::trace!(
        "projection: window {window}: drew {drawn} layer(s); skipped \
         hidden={hidden} empty={empty} clipped={clipped} of {}",
        scene.window_layer_order(window).len()
    );
}

/// Project a single window's layer subtree into a fresh [`RenderScene`].
///
/// The returned scene contains a default root node plus one node per drawn
/// layer, in `Scene::window_layer_order`, with ancestor position, opacity and
/// visibility already composed. `texture` resolves a bitmap id to an L1
/// texture handle; the caller owns the texture table and can pass, for
/// example, `|id| textures.get(&id).cloned()`.
pub fn project_window<F>(scene: &Scene, window: u32, texture: F) -> RenderScene
where
    F: FnMut(u32) -> Option<Texture>,
{
    project_window_report(scene, window, texture).scene
}

/// Project one window and also return the layer ids whose pixels were emitted.
pub fn project_window_report<F>(scene: &Scene, window: u32, mut texture: F) -> ProjectionReport
where
    F: FnMut(u32) -> Option<Texture>,
{
    let mut out = RenderScene::with_root_node();
    out.background = Some(BACKGROUND_CLEAR);
    let mut state = ProjectionState::default();
    let mut drawn_layers = Vec::new();
    append_window(
        scene,
        window,
        &mut out,
        &mut state,
        &mut drawn_layers,
        &mut texture,
    );
    ProjectionReport {
        scene: out,
        drawn_layers,
    }
}

/// Project every window of a [`Scene`] into one fresh [`RenderScene`].
///
/// Windows are appended in `Scene::windows` order; the sibling `z` sequence
/// and parent links are shared across windows so the whole tree renders
/// deterministically from one root. Hidden windows contribute no nodes.
///
/// `texture` resolves a bitmap id to an L1 texture handle (see
/// [`project_window`]).
pub fn project_scene<F>(scene: &Scene, texture: F) -> RenderScene
where
    F: FnMut(u32) -> Option<Texture>,
{
    project_scene_report(scene, texture).scene
}

/// Project every window and also return the layer ids whose pixels were
/// emitted, in emit order.
pub fn project_scene_report<F>(scene: &Scene, mut texture: F) -> ProjectionReport
where
    F: FnMut(u32) -> Option<Texture>,
{
    let mut out = RenderScene::with_root_node();
    out.background = Some(BACKGROUND_CLEAR);
    let mut state = ProjectionState::default();
    let mut drawn_layers = Vec::new();
    for window in &scene.windows {
        append_window(
            scene,
            window.id,
            &mut out,
            &mut state,
            &mut drawn_layers,
            &mut texture,
        );
    }
    ProjectionReport {
        scene: out,
        drawn_layers,
    }
}
