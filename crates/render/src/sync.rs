//! Scene → Bevy sync: turns the logical [`Scene`] into Bevy entities.
//!
//! # Coordinate convention
//!
//! TVP uses window coordinates: y grows **down** from the **top-left**
//! corner, and rects are `(x, y, w, h)` from that corner. Bevy's 2D camera
//! is y-**up** with the origin at the **center** of the window. We flip the
//! y axis and re-anchor the origin:
//!
//! ```text
//! bevy_x = (rect.x + rect.w / 2) - window_w / 2
//! bevy_y = (window_h / 2) - (rect.y + rect.h / 2)
//! ```
//!
//! A rect at the top of the window (small `rect.y`) therefore ends up with a
//! positive Bevy y, and one at the bottom with a negative y. Sprites use the
//! default [`bevy::sprite::Anchor::CENTER`], so the transform position is
//! the rect's center.
//!
//! # Z order
//!
//! [`Scene::window_layer_order`] returns a flattened back → front traversal:
//! every parent is immediately followed by its sorted child subtree. We assign
//! `z = index as f32` (backmost = 0, frontmost = N-1). Bevy sorts 2D sprites
//! back-to-front by view-space depth, so higher z renders on top. Child rects
//! are composed with all ancestor positions and child opacity is multiplied
//! through all ancestors before spawning the flat sprite list.
//!
//! # Blend modes
//!
//! TVP layers carry a blend `type` (`ltOpaque=1`, `ltAlpha=2`,
//! `ltAdditive=3`, `ltSubtractive=4`, `ltAddAlpha=12`, ...). The type
//! propagates from the native into [`LayerState`] and [`SceneSprite`], and
//! since Stage 4 it also drives **real GPU blending**: [`render_path_for`]
//! routes each layer either onto the built-in straight-alpha `Sprite`
//! pipeline (source-over modes) or onto a `Mesh2d` quad with the custom
//! [`LayerBlendMaterial`], whose per-mode pipeline variants carry the actual
//! fixed-function `BlendState` (add = `src*a + dst`, subtractive =
//! reverse-subtract, opaque = replace). See [`crate::blend`] for the full
//! mapping table. Blended quads render in the same z-sorted transparent 2D
//! phase as sprites, so mixed stacks keep their back-to-front order.
//!
//! # Rebuild strategy
//!
//! Milestone approach: **full rebuild every frame** — every [`SceneSprite`]
//! and [`WindowRoot`] entity is despawned and respawned from the current
//! scene snapshot. The scene is small (a few dozen layers), so the churn is
//! negligible and correctness wins; layer/window removal is handled for free.
//! The only incremental parts are **texture upload**: [`BitmapAssets`] caches
//! one [`Handle<Image>`] per bitmap id ([`tvp_visual::scene::BitmapState::dirty`]
//! is the sole trigger for a re-upload, cleared here after the upload), and
//! the shared GPU primitives ([`GpuPrimitives`]: unit quad mesh + white 1×1
//! texture) created once and reused. Per-frame [`LayerBlendMaterial`] assets
//! are tracked in [`FrameBlendMaterials`] and freed at the next sync so they
//! never accumulate.
//!
//! # Locking
//!
//! The scene is snapshotted under a read lock; afterwards a short write lock
//! clears the `dirty` flags of uploaded bitmaps. Everything runs on Bevy's
//! main thread today, but the lock discipline is kept explicit for the
//! future threading model (WAVE3.md).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::{Camera2d, ClearColorConfig, OrthographicProjection, Projection, ScalingMode};
use bevy::color::Color;
use bevy::ecs::prelude::{Commands, Component, Entity, Query, Res, ResMut, Resource, With};
use bevy::image::Image;
use bevy::math::primitives::Rectangle;
use bevy::math::{Vec2, Vec3};
use bevy::mesh::{Mesh, Mesh2d};
use bevy::prelude::{Camera, Sprite, Transform, Visibility};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::sprite_render::MeshMaterial2d;
use tvp_visual::scene::{BitmapState, LayerState, Rect, Scene};

use crate::blend::{LayerBlendMaterial, LayerRenderPath, render_path_for};

/// The shared logical scene: natives (VM) write under the lock, this crate
/// reads under it. See WAVE3.md.
#[derive(Resource, Clone)]
pub struct SharedScene(pub Arc<RwLock<Scene>>);

/// Per-bitmap GPU texture cache — the incremental part of the sync.
///
/// One strong [`Handle<Image>`] per bitmap id; strong handles keep textures
/// alive even during frames where no sprite references them. Bitmaps are
/// (re-)uploaded only when new or [`BitmapState::dirty`].
#[derive(Resource, Default)]
pub struct BitmapAssets {
    handles: HashMap<u32, Handle<Image>>,
}

impl BitmapAssets {
    /// Handle for `bitmap_id`, uploading/re-uploading when it is new or
    /// dirty. Returns `None` for unknown or invalid (zero-sized / short
    /// data) bitmaps — the caller then falls back to a solid fill.
    /// Uploaded ids are recorded in `uploaded` so the caller can clear their
    /// dirty flags afterwards.
    fn handle_for(
        &mut self,
        bitmap_id: u32,
        scene: &Scene,
        images: &mut Assets<Image>,
        uploaded: &mut Vec<u32>,
    ) -> Option<Handle<Image>> {
        let bitmap = scene.bitmap(bitmap_id)?;
        if bitmap.width == 0 || bitmap.height == 0 {
            return None;
        }
        let expected = (bitmap.width as usize)
            .saturating_mul(bitmap.height as usize)
            .saturating_mul(4);
        if bitmap.rgba.len() < expected {
            return None;
        }
        if let Some(handle) = self.handles.get(&bitmap_id).filter(|_| !bitmap.dirty) {
            return Some(handle.clone());
        }
        let handle = upload_bitmap(bitmap, images);
        self.handles.insert(bitmap_id, handle.clone());
        uploaded.push(bitmap_id);
        Some(handle)
    }
}

/// Upload one bitmap's RGBA8 data as a GPU texture (`Rgba8UnormSrgb` —
/// TVP bitmaps are straight-alpha sRGB pixel data).
fn upload_bitmap(bitmap: &BitmapState, images: &mut Assets<Image>) -> Handle<Image> {
    let image = Image::new(
        Extent3d {
            width: bitmap.width,
            height: bitmap.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bitmap.rgba.clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    images.add(image)
}

/// Lazily-created shared GPU primitives reused across frames (and shared by
/// every blended layer): a centered 1×1 quad mesh (scaled to the layer rect
/// via the transform) and a 1×1 white texture that stands in for solid-fill
/// layers on the custom-material path (the tint alone drives the color).
#[derive(Resource, Default)]
pub struct GpuPrimitives {
    unit_quad: Option<Handle<Mesh>>,
    white_1x1: Option<Handle<Image>>,
}

/// Strong handles of the [`LayerBlendMaterial`] assets created by the last
/// sync. The full rebuild despawns their entities every frame, so these are
/// removed at the start of the next sync — material assets never accumulate.
#[derive(Resource, Default)]
pub struct FrameBlendMaterials(Vec<Handle<LayerBlendMaterial>>);

/// Root entity for one logical window: the background/anchor node that owns
/// the window's layer sprites.
#[derive(Component)]
pub struct WindowRoot {
    pub window_id: u32,
}

/// One spawned layer sprite.
///
/// Layers whose native blend type maps to plain source-over carry the
/// built-in [`Sprite`]; layers needing a real GPU blend (additive,
/// subtractive, opaque-replace) instead carry a `Mesh2d` quad with a
/// [`LayerBlendMaterial`] and no `Sprite`. Both shapes share this marker so
/// the rebuild and tests see one flat list of layer entities.
#[derive(Component)]
pub struct SceneSprite {
    pub layer_id: u32,
    /// Native TVP `Layer.type`; also drives the render path via
    /// [`render_path_for`] (see [`crate::blend`] for the mapping).
    pub blend_type: i64,
}

/// The 2D camera we manage (for the first window). Kept across frames — it
/// has no per-frame state worth churning, unlike the full layer rebuild.
#[derive(Component)]
pub struct SceneCamera;

/// The logical scene size used when a window carries a degenerate
/// (`0 × 0`) [`WindowState::inner_size`](tvp_visual::scene::WindowState::inner_size).
pub const DEFAULT_LOGICAL_SIZE: (u32, u32) = (1280, 720);

/// Logical scene size for a window: its `inner_size`, or
/// [`DEFAULT_LOGICAL_SIZE`] when that is zero-sized.
fn logical_size(inner_size: (u32, u32)) -> (u32, u32) {
    if inner_size.0 == 0 || inner_size.1 == 0 {
        DEFAULT_LOGICAL_SIZE
    } else {
        inner_size
    }
}

/// The orthographic 2D projection for a logical scene of `inner_size` world
/// units.
///
/// `ScalingMode::AutoMin` keeps the aspect ratio and never shows less than
/// the logical scene: resizing the OS window *scales* the game (no
/// stretching), letterboxing extra space on the non-16:9 axis. (`Fixed`
/// would stretch the scene instead, which distorts a visual novel.)
fn scene_projection(inner_size: (u32, u32)) -> Projection {
    let (width, height) = logical_size(inner_size);
    Projection::Orthographic(OrthographicProjection {
        scaling_mode: ScalingMode::AutoMin {
            min_width: width as f32,
            min_height: height as f32,
        },
        ..OrthographicProjection::default_2d()
    })
}

/// The scene → Bevy sync system. Run in `Update`, after the VM tick mutates
/// the scene.
#[allow(clippy::too_many_arguments)]
pub fn sync_scene(
    mut commands: Commands,
    shared: Res<SharedScene>,
    mut bitmaps: ResMut<BitmapAssets>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<LayerBlendMaterial>>,
    mut gpu: ResMut<GpuPrimitives>,
    mut frame_materials: ResMut<FrameBlendMaterials>,
    previous_roots: Query<Entity, With<WindowRoot>>,
    previous_sprites: Query<Entity, With<SceneSprite>>,
    cameras: Query<Entity, With<SceneCamera>>,
) {
    // Full rebuild: drop everything we spawned last frame (camera excluded).
    // Roots auto-despawn their sprite children (despawning a parent removes
    // the whole subtree), so only the roots need explicit despawn commands.
    for entity in &previous_roots {
        commands.entity(entity).despawn();
    }
    let _ = previous_sprites;

    // The entities referencing last frame's blend materials are gone; free
    // the assets so they don't accumulate (one fresh material per blended
    // layer per frame is fine, a growing pool is not).
    for handle in frame_materials.0.drain(..) {
        materials.remove(handle.id());
    }

    let scene = shared.0.read().expect("shared scene lock poisoned");
    let mut uploaded: Vec<u32> = Vec::new();

    let mut spawned_camera = !cameras.is_empty();
    for window in &scene.windows {
        // Background / root node for this window.
        let root = commands
            .spawn((
                WindowRoot {
                    window_id: window.id,
                },
                Transform::default(),
            ))
            .id();

        // The first window gets the 2D camera. Extra windows have no camera
        // yet (single-primary-window milestone; documented in WAVE3).
        if !spawned_camera {
            spawned_camera = true;
            commands.spawn((
                SceneCamera,
                Camera2d,
                scene_projection(window.inner_size),
                Camera {
                    clear_color: ClearColorConfig::Custom(Color::srgb(0.0, 0.0, 0.0)),
                    ..Default::default()
                },
            ));
        }

        if !window.visible {
            continue;
        }
        let (win_w, win_h) = window.inner_size;
        let win_opacity = clamp_opacity(window.opacity);

        for (index, layer_id) in scene.window_layer_order(window.id).into_iter().enumerate() {
            let Some(layer) = scene.layer(layer_id) else {
                continue;
            };
            let Some(composed) = compose_layer(&scene, layer_id) else {
                continue;
            };
            let alpha = win_opacity * composed.opacity;
            let (visual, draw_rect) = build_layer_visual(
                layer,
                composed.rect,
                alpha,
                &mut bitmaps,
                &scene,
                &mut images,
                &mut uploaded,
                &mut meshes,
                &mut materials,
                &mut gpu,
                &mut frame_materials.0,
            );
            let (x, y) = rect_center(draw_rect, win_w, win_h);
            let transform = Transform::from_xyz(x, y, sprite_z(index));
            let visibility = if composed.visible {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
            let marker = SceneSprite {
                layer_id,
                blend_type: layer.blend_type,
            };
            // Blended quads reuse the shared unit quad scaled to the drawn
            // rect; sprites carry their size via custom_size instead.
            let sprite = match visual {
                LayerVisual::Sprite(sprite) => {
                    commands.spawn((marker, sprite, transform, visibility))
                }
                LayerVisual::Blended { mesh, material } => {
                    let mut transform = transform;
                    transform.scale = Vec3::new(draw_rect.w as f32, draw_rect.h as f32, 1.0);
                    commands.spawn((
                        marker,
                        Mesh2d(mesh),
                        MeshMaterial2d(material),
                        transform,
                        visibility,
                    ))
                }
            }
            .id();
            commands.entity(root).add_child(sprite);
        }
    }

    drop(scene);

    // Clear the dirty flags of the bitmaps we uploaded this frame. Short
    // write lock; same thread today, kept explicit for the future.
    let mut scene = shared.0.write().expect("shared scene lock poisoned");
    for id in uploaded {
        if let Some(bitmap) = scene.bitmap_mut(id) {
            bitmap.dirty = false;
        }
    }
}

/// What the sync spawns for one layer: a plain [`Sprite`] or a blended
/// mesh + [`LayerBlendMaterial`] pair.
enum LayerVisual {
    Sprite(Sprite),
    Blended {
        mesh: Handle<Mesh>,
        material: Handle<LayerBlendMaterial>,
    },
}

/// Build the visual for one layer.
///
/// `layer_rect` is the layer's **absolute** (composed) rect; image placement
/// is relative to it. Returns the visual plus the screen rect it should be
/// positioned at (the clipped image rect for bitmap layers, the layer rect
/// for fills).
///
/// The native blend type decides the render path ([`render_path_for`]):
/// * source-over modes (and unknown types) → built-in [`Sprite`] (straight
///   alpha, unchanged from earlier milestones);
/// * additive / subtractive / opaque-at-full-opacity → unit quad with a
///   fresh [`LayerBlendMaterial`] whose pipeline variant carries the actual
///   fixed-function blend state.
///
/// Content rules are shared by both paths:
/// * Bitmap layer → texture from [`BitmapAssets`], white tint at the
///   composed alpha; only the image region visible inside the layer rect is
///   drawn (reference `ImageLeft`/`ImageTop`/`ImageWidth`/`ImageHeight`,
///   which is how the game's buttons select a sprite-sheet frame).
/// * Fill layer → solid color from `fill_color` (straight alpha), alpha
///   multiplied with the composed window × layer opacity; on the material
///   path a shared 1×1 white texture stands in for the bitmap.
#[allow(clippy::too_many_arguments)]
fn build_layer_visual(
    layer: &LayerState,
    layer_rect: Rect,
    alpha: f32,
    bitmaps: &mut BitmapAssets,
    scene: &Scene,
    images: &mut Assets<Image>,
    uploaded: &mut Vec<u32>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<LayerBlendMaterial>,
    gpu: &mut GpuPrimitives,
    frame_materials: &mut Vec<Handle<LayerBlendMaterial>>,
) -> (LayerVisual, Rect) {
    let path = render_path_for(layer.blend_type, alpha);
    let size = Vec2::new(layer_rect.w as f32, layer_rect.h as f32);

    // A layer with neither an image nor a solid fill draws nothing (the
    // reference's `MainImage` is filled with the transparent `NeutralColor`,
    // not opaque black). Rendering it as black painted over the background
    // art and made the title screen look stacked.
    if layer.bitmap.is_none() && layer.fill_color.is_none() {
        return (
            LayerVisual::Sprite(Sprite {
                color: Color::NONE,
                custom_size: Some(Vec2::ZERO),
                ..Default::default()
            }),
            layer_rect,
        );
    }

    match path {
        LayerRenderPath::Sprite => {
            if let Some(bitmap) = layer
                .bitmap
                .and_then(|id| bitmaps.handle_for(id, scene, images, uploaded))
            {
                let (bmp_w, bmp_h) = layer
                    .bitmap
                    .and_then(|id| scene.bitmap(id))
                    .map_or((1, 1), |b| (b.width.max(1), b.height.max(1)));
                let (img_w, img_h) = (
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
                );
                match visible_image_region(
                    layer_rect,
                    layer.image_left,
                    layer.image_top,
                    img_w,
                    img_h,
                    bmp_w,
                    bmp_h,
                ) {
                    Some((src, dest)) => {
                        let sprite = Sprite {
                            image: bitmap,
                            color: bitmap_tint(alpha),
                            rect: Some(bevy::math::Rect::new(
                                src.x as f32,
                                src.y as f32,
                                (src.x + src.w as i32) as f32,
                                (src.y + src.h as i32) as f32,
                            )),
                            custom_size: Some(Vec2::new(dest.w as f32, dest.h as f32)),
                            ..Default::default()
                        };
                        return (LayerVisual::Sprite(sprite), dest);
                    }
                    None => {
                        // Fully clipped: spawn an invisible zero-size sprite.
                        let sprite = Sprite {
                            image: bitmap,
                            color: Color::NONE,
                            custom_size: Some(Vec2::ZERO),
                            rect: Some(bevy::math::Rect::new(0.0, 0.0, 0.0, 0.0)),
                            ..Default::default()
                        };
                        return (LayerVisual::Sprite(sprite), layer_rect);
                    }
                }
            }
            let color = layer
                .fill_color
                .map(|fill| fill_sprite_color(fill, alpha))
                .unwrap_or_else(|| Color::srgba(0.0, 0.0, 0.0, clamp_opacity(alpha)));
            (
                LayerVisual::Sprite(Sprite::from_color(color, size)),
                layer_rect,
            )
        }
        LayerRenderPath::Material(mode) => {
            // Bitmap texture if present, else a shared white 1×1 stand-in
            // for solid fills (never bind Bevy's fallback: it is transparent
            // black and would zero out additive/subtractive fills).
            let bitmap_texture = layer
                .bitmap
                .and_then(|id| bitmaps.handle_for(id, scene, images, uploaded));
            let texture = match bitmap_texture {
                Some(handle) => handle,
                None => {
                    if gpu.white_1x1.is_none() {
                        gpu.white_1x1 = Some(images.add(Image::new(
                            Extent3d {
                                width: 1,
                                height: 1,
                                depth_or_array_layers: 1,
                            },
                            TextureDimension::D2,
                            vec![255; 4],
                            TextureFormat::Rgba8UnormSrgb,
                            RenderAssetUsages::default(),
                        )));
                    }
                    gpu.white_1x1
                        .clone()
                        .expect("white 1×1 texture just created")
                }
            };
            let color = layer
                .fill_color
                .map_or_else(|| bitmap_tint(alpha), |fill| fill_sprite_color(fill, alpha));
            let quad = gpu
                .unit_quad
                .get_or_insert_with(|| meshes.add(Rectangle::new(1.0, 1.0)))
                .clone();
            let material = materials.add(LayerBlendMaterial::new(color.to_linear(), texture, mode));
            frame_materials.push(material.clone());
            (
                LayerVisual::Blended {
                    mesh: quad,
                    material,
                },
                layer_rect,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Pure conversion helpers (unit-tested; no Bevy world access).
// ---------------------------------------------------------------------------

/// A layer after parent transforms, opacity, and visibility have been
/// composed. The renderer intentionally keeps entities flat under WindowRoot
/// so one global z sequence can represent the depth-first layer tree.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ComposedLayer {
    rect: Rect,
    opacity: f32,
    visible: bool,
}

/// Compose a layer's position, opacity, and visibility through its ancestors.
/// Invalid links stop the chain; cycles are ignored after the first visit.
fn compose_layer(scene: &Scene, layer_id: u32) -> Option<ComposedLayer> {
    let mut chain = Vec::new();
    let mut current = Some(layer_id);
    let mut visited = std::collections::HashSet::new();
    let window = scene.layer(layer_id)?.window;
    while let Some(id) = current {
        if !visited.insert(id) {
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
        // A child owns its own size; only its origin is relative to its parent.
        result.rect.w = layer.rect.w;
        result.rect.h = layer.rect.h;
        result.opacity *= clamp_opacity(layer.opacity);
        result.visible &= layer.visible;
    }
    Some(result)
}

/// Compute the visible part of a layer's image after clipping it to the
/// layer's absolute rect. Returns `(source_pixels, dest_tvp)`:
/// * `source_pixels`: the sub-rectangle of the bitmap to sample (bitmap
///   pixel coordinates).
/// * `dest_tvp`: the screen-space rect (absolute TVP coordinates) to draw it
///   at.
///
/// This implements the reference `ImageLeft`/`ImageTop`/`ImageWidth`/
/// `ImageHeight` model: the image is placed at `(layer.x + image_left,
/// layer.y + image_top)` at size `image_width × image_height`, then clipped
/// to the layer rect. The game's buttons select a sprite-sheet frame with a
/// negative `image_left`.
#[allow(clippy::too_many_arguments)]
fn visible_image_region(
    layer_rect: Rect,
    image_left: i32,
    image_top: i32,
    image_width: u32,
    image_height: u32,
    bmp_w: u32,
    bmp_h: u32,
) -> Option<(Rect, Rect)> {
    let img_x = layer_rect.x + image_left;
    let img_y = layer_rect.y + image_top;
    let vx0 = img_x.max(layer_rect.x);
    let vy0 = img_y.max(layer_rect.y);
    let vx1 = (img_x + image_width as i32).min(layer_rect.x + layer_rect.w as i32);
    let vy1 = (img_y + image_height as i32).min(layer_rect.y + layer_rect.h as i32);
    if vx1 <= vx0 || vy1 <= vy0 {
        return None;
    }
    let dest = Rect {
        x: vx0,
        y: vy0,
        w: (vx1 - vx0) as u32,
        h: (vy1 - vy0) as u32,
    };
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
    let sw = (((dest.w as f32) * scale_x).ceil() as u32)
        .min(bmp_w.saturating_sub(sx))
        .max(1);
    let sh = (((dest.h as f32) * scale_y).ceil() as u32)
        .min(bmp_h.saturating_sub(sy))
        .max(1);
    Some((
        Rect {
            x: sx as i32,
            y: sy as i32,
            w: sw,
            h: sh,
        },
        dest,
    ))
}

/// TVP (y-down, top-left origin) rect center → Bevy (y-up, centered origin)
/// world position. See the module docs for the convention.
pub fn rect_center(rect: Rect, window_w: u32, window_h: u32) -> (f32, f32) {
    let x = rect.x as f32 + rect.w as f32 / 2.0 - window_w as f32 / 2.0;
    let y = window_h as f32 / 2.0 - (rect.y as f32 + rect.h as f32 / 2.0);
    (x, y)
}

/// Z depth for a layer at `index` in the back → front render order.
/// Backmost (index 0) sits at z = 0; Bevy sorts 2D sprites by view depth, so
/// higher z renders on top.
pub fn sprite_z(index: usize) -> f32 {
    index as f32
}

/// Clamp an opacity to `0.0..=1.0` (NaN → 0.0, since `f32::clamp` leaves
/// NaN untouched).
pub fn clamp_opacity(opacity: f32) -> f32 {
    if opacity.is_nan() {
        0.0
    } else {
        opacity.clamp(0.0, 1.0)
    }
}

/// Sprite tint for a bitmap layer: white with the composed opacity as alpha
/// (the texture supplies the actual RGB and alpha).
pub fn bitmap_tint(alpha: f32) -> Color {
    Color::srgba(1.0, 1.0, 1.0, clamp_opacity(alpha))
}

/// Sprite color for a solid-fill layer: the fill's straight-alpha RGBA with
/// alpha scaled by the composed window × layer opacity.
pub fn fill_sprite_color(fill: [u8; 4], alpha: f32) -> Color {
    Color::srgba(
        fill[0] as f32 / 255.0,
        fill[1] as f32 / 255.0,
        fill[2] as f32 / 255.0,
        fill[3] as f32 / 255.0 * clamp_opacity(alpha),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::{LayerBlendMode, LayerBlendPlugin};
    use bevy::app::{App, Update};
    use bevy::math::Vec3;
    use tvp_visual::scene::Rect;

    /// Build a tiny two-layer scene: full-window solid fill + one bitmap
    /// layer. Returns the shared scene, the bitmap layer id and the bitmap
    /// id.
    fn two_layer_scene() -> (SharedScene, u32, u32) {
        let mut scene = Scene::default();
        let win = scene.add_window("test", (640, 480));
        let bg = scene.add_layer(win, None);
        scene.layer_mut(bg).unwrap().rect = Rect {
            x: 0,
            y: 0,
            w: 640,
            h: 480,
        };
        scene.layer_mut(bg).unwrap().fill_color = Some([10, 20, 30, 255]);
        let bmp = scene.add_bitmap(64, 64, vec![0u8; 64 * 64 * 4]);
        let l1 = scene.add_layer(win, None);
        scene.layer_mut(l1).unwrap().bitmap = Some(bmp);
        scene.layer_mut(l1).unwrap().rect = Rect {
            x: 100,
            y: 50,
            w: 64,
            h: 64,
        };
        (SharedScene(Arc::new(RwLock::new(scene))), l1, bmp)
    }

    /// An App with just the sync system and the resources it needs (no
    /// renderer/window — this runs the real sync code without a GPU).
    /// [`LayerBlendPlugin`] registers the blend-material asset store (its
    /// render-app parts are skipped without a RenderApp).
    fn app_with_sync(shared: SharedScene) -> App {
        let mut app = App::new();
        app.insert_resource(shared)
            .insert_resource(Assets::<Image>::default())
            .insert_resource(Assets::<Mesh>::default())
            .init_resource::<BitmapAssets>()
            .init_resource::<GpuPrimitives>()
            .init_resource::<FrameBlendMaterials>()
            .add_plugins(LayerBlendPlugin)
            .add_systems(Update, sync_scene);
        app
    }

    fn sprite_count(world: &mut bevy::ecs::world::World) -> usize {
        world
            .query_filtered::<Entity, With<SceneSprite>>()
            .iter(world)
            .count()
    }

    #[test]
    fn sync_scene_spawns_camera_root_and_sprites() {
        let (shared, l1, bmp) = two_layer_scene();
        let mut app = app_with_sync(shared.clone());

        app.update();

        // One camera (first window), one window root, two layer sprites.
        let world = app.world_mut();
        assert_eq!(
            world
                .query_filtered::<Entity, With<SceneCamera>>()
                .iter(world)
                .count(),
            1
        );
        assert_eq!(
            world
                .query_filtered::<Entity, With<WindowRoot>>()
                .iter(world)
                .count(),
            1
        );
        assert_eq!(sprite_count(world), 2);

        // Bitmap layer (rect (100,50,64,64) in 640x480): bevy center
        // x = 100 + 32 - 320 = -188, y = 240 - (50 + 32) = +158, z = 1
        // (second in back→front order). Texture uploaded (strong handle),
        // sized to the layer rect, tinted white.
        let mut q =
            world.query_filtered::<(&SceneSprite, &Transform, &Sprite), With<SceneSprite>>();
        let mut found = 0;
        for (marker, transform, sprite) in q.iter(world) {
            match marker.layer_id {
                id if id == l1 => {
                    assert_eq!(transform.translation, Vec3::new(-188.0, 158.0, 1.0));
                    assert!(sprite.image.is_strong());
                    assert_eq!(sprite.custom_size, Some(Vec2::new(64.0, 64.0)));
                    assert_eq!(sprite.color.to_srgba().alpha, 1.0);
                    found += 1;
                }
                _ => {
                    // Background fill layer: centered on the origin.
                    assert_eq!(transform.translation, Vec3::ZERO);
                    assert_eq!(sprite.color.to_srgba().red, 10.0 / 255.0);
                    found += 1;
                }
            }
        }
        assert_eq!(found, 2, "expected both layers to be checked");

        // The dirty flag was cleared after upload.
        let scene = shared.0.read().unwrap();
        assert!(!scene.bitmap(bmp).unwrap().dirty);
    }

    /// The camera must pin the visible world to the window's logical
    /// `inner_size`, so resizing the OS window scales the game instead of
    /// revealing more/less of the 1280x720 scene.
    #[test]
    fn camera_projection_is_fixed_logical_scene() {
        let mut scene = Scene::default();
        scene.add_window("t", (1280, 720));
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared);
        app.update();

        let world = app.world_mut();
        let mut q = world.query_filtered::<&Projection, With<SceneCamera>>();
        let projection = q.single(world).expect("camera spawned with a projection");
        let Projection::Orthographic(ortho) = projection else {
            panic!("expected an orthographic 2D projection, got {projection:?}");
        };
        assert!(
            matches!(
                ortho.scaling_mode,
                ScalingMode::AutoMin { min_width, min_height }
                    if min_width == 1280.0 && min_height == 720.0
            ),
            "projection must fit the 1280x720 logical scene, got {:?}",
            ortho.scaling_mode
        );
    }

    /// A window without a usable inner size falls back to 1280x720.
    #[test]
    fn camera_projection_falls_back_to_1280x720() {
        let mut scene = Scene::default();
        scene.add_window("t", (0, 0));
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared);
        app.update();

        let world = app.world_mut();
        let mut q = world.query_filtered::<&Projection, With<SceneCamera>>();
        let projection = q.single(world).expect("camera spawned with a projection");
        let Projection::Orthographic(ortho) = projection else {
            panic!("expected an orthographic 2D projection, got {projection:?}");
        };
        assert!(
            matches!(
                ortho.scaling_mode,
                ScalingMode::AutoMin { min_width, min_height }
                    if min_width == 1280.0 && min_height == 720.0
            ),
            "degenerate window size must fall back to 1280x720, got {:?}",
            ortho.scaling_mode
        );
    }

    #[test]
    fn hierarchy_composes_position_opacity_visibility_and_order() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (640, 480));
        scene.window_mut(win).unwrap().opacity = 0.8;
        let parent = scene.add_layer(win, None);
        scene.layer_mut(parent).unwrap().rect = Rect {
            x: 100,
            y: 40,
            w: 200,
            h: 100,
        };
        scene.layer_mut(parent).unwrap().opacity = 0.5;
        scene.layer_mut(parent).unwrap().visible = true;
        scene.layer_mut(parent).unwrap().fill_color = Some([255, 255, 255, 255]);
        let child = scene.add_layer(win, Some(parent));
        scene.layer_mut(child).unwrap().rect = Rect {
            x: 12,
            y: 8,
            w: 20,
            h: 10,
        };
        scene.layer_mut(child).unwrap().opacity = 0.5;
        scene.layer_mut(child).unwrap().visible = true;
        scene.layer_mut(child).unwrap().fill_color = Some([255, 255, 255, 255]);
        let sibling = scene.add_layer(win, None);
        scene.layer_mut(sibling).unwrap().rect = Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        };

        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared);
        app.update();

        let world = app.world_mut();
        let mut q =
            world.query_filtered::<(&SceneSprite, &Transform, &Sprite), With<SceneSprite>>();
        let mut by_id = std::collections::HashMap::new();
        for (marker, transform, sprite) in q.iter(world) {
            by_id.insert(
                marker.layer_id,
                (transform.translation, sprite.color.to_srgba().alpha),
            );
        }
        // Parent is centered at TVP (200,90) -> Bevy (-120,150); child is
        // relative to its parent at TVP rect origin (112,48), with its own
        // 20x10 center at (122,53) -> Bevy (-198,187).
        assert_eq!(by_id[&parent].0, Vec3::new(-120.0, 150.0, 0.0));
        assert_eq!(by_id[&child].0, Vec3::new(-198.0, 187.0, 1.0));
        assert_eq!(by_id[&sibling].0.z, 2.0);
        assert!((by_id[&child].1 - 0.8 * 0.5 * 0.5).abs() < 1e-6);
        assert!(by_id[&parent].0.z < by_id[&child].0.z);
        assert!(by_id[&child].0.z < by_id[&sibling].0.z);
    }

    #[test]
    fn sync_scene_propagates_blend_type_marker() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (64, 64));
        let layer = scene.add_layer(win, None);
        scene.layer_mut(layer).unwrap().blend_type = tvp_visual::scene::LT_ADDITIVE;
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared);
        app.update();
        let world = app.world_mut();
        let mut q = world.query_filtered::<&SceneSprite, With<SceneSprite>>();
        assert_eq!(
            q.single(world).unwrap().blend_type,
            tvp_visual::scene::LT_ADDITIVE
        );
    }

    /// Stage 4: an ltAdditive layer must become a Mesh2d quad with a
    /// [`LayerBlendMaterial`] whose mode is a real GPU Additive blend (not
    /// a source-over sprite), positioned/sized like any other layer.
    #[test]
    fn additive_layer_spawns_gpu_blend_material_quad() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1280, 720));
        let bmp_id = scene.add_bitmap(32, 32, vec![0u8; 32 * 32 * 4]);
        let layer = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(layer).unwrap();
            l.blend_type = tvp_visual::scene::LT_ADDITIVE;
            l.bitmap = Some(bmp_id);
            l.rect = Rect {
                x: 100,
                y: 200,
                w: 32,
                h: 32,
            };
        }
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared.clone());
        app.update();

        let world = app.world_mut();
        // No plain Sprite: the built-in pipeline cannot composite additively.
        assert_eq!(
            world
                .query_filtered::<&Sprite, With<SceneSprite>>()
                .iter(world)
                .count(),
            0,
            "additive layer must not render as a source-over sprite"
        );
        let (marker, mesh, material, transform) = world
            .query_filtered::<(
                &SceneSprite,
                &Mesh2d,
                &MeshMaterial2d<LayerBlendMaterial>,
                &Transform,
            ), With<SceneSprite>>()
            .single(world)
            .expect("one blended quad");
        assert_eq!(marker.blend_type, tvp_visual::scene::LT_ADDITIVE);
        assert!(mesh.0.is_strong(), "unit quad mesh bound");

        let mat_assets = world.resource::<Assets<LayerBlendMaterial>>();
        let mat = mat_assets.get(&material.0).expect("material exists");
        assert_eq!(
            LayerBlendMode::from(mat),
            LayerBlendMode::Additive,
            "material carries the GPU blend mode"
        );
        assert!(mat.texture().is_some(), "bitmap texture bound");

        // Same placement rules as sprites: center + rect size as scale.
        // TVP rect (100,200,32x32) in 1280x720 → Bevy
        // (100+16-640, 360-(200+16)) = (-524, 144), z=0.
        assert_eq!(transform.translation, Vec3::new(-524.0, 144.0, 0.0));
        assert_eq!(transform.scale, Vec3::new(32.0, 32.0, 1.0));

        // The dirty flag of the uploaded bitmap was still cleared.
        assert!(!shared.0.read().unwrap().bitmap(bmp_id).unwrap().dirty);
    }

    /// Subtractive layers take the material path too, with the Subtractive
    /// mode on their material.
    #[test]
    fn subtractive_layer_spawns_subtractive_material() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (640, 480));
        let layer = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(layer).unwrap();
            l.blend_type = tvp_visual::scene::LT_SUBTRACTIVE;
            l.fill_color = Some([40, 40, 40, 255]);
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 64,
                h: 64,
            };
        }
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared);
        app.update();

        let world = app.world_mut();
        let (_, material) = world
            .query_filtered::<(&Mesh2d, &MeshMaterial2d<LayerBlendMaterial>), With<SceneSprite>>()
            .single(world)
            .expect("one blended quad");
        let mat = world
            .resource::<Assets<LayerBlendMaterial>>()
            .get(&material.0)
            .unwrap();
        assert_eq!(LayerBlendMode::from(mat), LayerBlendMode::Subtractive);
        // Fill-only layers bind the shared white 1×1 texture so the tint
        // alone drives the color.
        assert!(mat.texture().is_some());
    }

    /// An ltOpaque layer at full opacity takes the replace (no-blend)
    /// material path; under partial window/layer opacity it must degrade to
    /// a source-over Sprite so the opacity still applies.
    #[test]
    fn opaque_layer_replace_at_full_opacity_sprite_under_opacity() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (64, 64));
        let full = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(full).unwrap();
            l.blend_type = tvp_visual::scene::LT_OPAQUE;
            l.fill_color = Some([1, 2, 3, 255]);
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 64,
                h: 64,
            };
        }
        let faded = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(faded).unwrap();
            l.blend_type = tvp_visual::scene::LT_OPAQUE;
            l.opacity = 0.5;
            l.fill_color = Some([4, 5, 6, 255]);
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 64,
                h: 64,
            };
        }
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared);
        app.update();

        let world = app.world_mut();
        let faded_is_sprite = world
            .query_filtered::<(&SceneSprite, &Sprite), With<SceneSprite>>()
            .iter(world)
            .any(|(m, _)| m.layer_id == faded);
        let full_replaces = world
            .query_filtered::<(
                &SceneSprite,
                &MeshMaterial2d<LayerBlendMaterial>,
            ), With<SceneSprite>>()
            .iter(world)
            .any(|(m, _)| m.layer_id == full);
        assert!(
            faded_is_sprite,
            "faded opaque layer stays on the sprite path"
        );
        assert!(full_replaces, "full-opacity opaque layer replaces");
    }

    /// Blended quads share the flat z sequence with sprites: an additive
    /// layer spawned last must get the highest z even though it renders via
    /// a different pipeline.
    #[test]
    fn blended_quads_share_z_order_with_sprites() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (64, 64));
        let back = scene.add_layer(win, None);
        scene.layer_mut(back).unwrap().fill_color = Some([0, 0, 0, 255]);
        let front = scene.add_layer(win, None);
        scene.layer_mut(front).unwrap().blend_type = tvp_visual::scene::LT_ADDITIVE;
        scene.layer_mut(front).unwrap().fill_color = Some([9, 9, 9, 255]);
        scene.layer_mut(front).unwrap().rect = Rect {
            x: 0,
            y: 0,
            w: 64,
            h: 64,
        };
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared);
        app.update();

        let world = app.world_mut();
        let mut q = world.query_filtered::<(&SceneSprite, &Transform), With<SceneSprite>>();
        let mut by_id = std::collections::HashMap::new();
        for (m, t) in q.iter(world) {
            by_id.insert(m.layer_id, t.translation.z);
        }
        assert_eq!(by_id[&back], 0.0);
        assert_eq!(by_id[&front], 1.0, "blended quad keeps the z sequence");
    }

    #[test]
    fn sync_scene_rebuilds_and_handles_removal() {
        let (shared, bg, _) = two_layer_scene();
        let mut app = app_with_sync(shared.clone());

        app.update();
        app.update();
        assert_eq!(
            sprite_count(app.world_mut()),
            2,
            "second frame rebuilds same count"
        );
        assert_eq!(
            app.world_mut()
                .query_filtered::<Entity, With<SceneCamera>>()
                .iter(app.world())
                .count(),
            1,
            "camera is kept across frames, not churned"
        );

        // Remove the background layer; the next sync must drop its sprite.
        shared.0.write().unwrap().remove_layer(bg);
        app.update();
        assert_eq!(
            sprite_count(app.world_mut()),
            1,
            "removed layer's sprite is gone"
        );
    }

    #[test]
    fn rect_center_full_window_is_origin() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 720,
        };
        assert_eq!(rect_center(rect, 1280, 720), (0.0, 0.0));
    }

    #[test]
    fn rect_center_top_left_corner() {
        // A 100x50 rect at the window's top-left corner: TVP center
        // (50, 25) → Bevy center (-590, +335).
        let rect = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 50,
        };
        assert_eq!(rect_center(rect, 1280, 720), (-590.0, 335.0));
    }

    #[test]
    fn rect_center_flips_y_axis() {
        // The same rect at the very top and the very bottom of the window
        // must land on mirrored Bevy y positions.
        let top = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 50,
        };
        let bottom = Rect {
            x: 0,
            y: 720 - 50,
            w: 100,
            h: 50,
        };
        let (_, y_top) = rect_center(top, 1280, 720);
        let (_, y_bottom) = rect_center(bottom, 1280, 720);
        assert!(y_top > 0.0);
        assert!(y_bottom < 0.0);
        assert_eq!(y_bottom, -y_top);
    }

    #[test]
    fn sprite_z_is_render_index() {
        assert_eq!(sprite_z(0), 0.0);
        assert_eq!(sprite_z(3), 3.0);
        assert!(sprite_z(1) > sprite_z(0), "backmost layer renders behind");
    }

    #[test]
    fn clamp_opacity_bounds() {
        assert_eq!(clamp_opacity(-0.5), 0.0);
        assert_eq!(clamp_opacity(0.5), 0.5);
        assert_eq!(clamp_opacity(1.5), 1.0);
        assert_eq!(clamp_opacity(f32::NAN), 0.0, "NaN clamps to 0");
    }

    #[test]
    fn bitmap_tint_is_white_with_alpha() {
        let color = bitmap_tint(0.25).to_srgba();
        assert_eq!(color.red, 1.0);
        assert_eq!(color.green, 1.0);
        assert_eq!(color.blue, 1.0);
        assert_eq!(color.alpha, 0.25);
    }

    #[test]
    fn fill_color_scales_alpha_only() {
        let color = fill_sprite_color([255, 128, 0, 200], 0.5).to_srgba();
        assert!((color.red - 1.0).abs() < 1e-6);
        assert!((color.green - 128.0 / 255.0).abs() < 1e-6);
        assert!(color.blue.abs() < 1e-6);
        assert!((color.alpha - 200.0 / 255.0 * 0.5).abs() < 1e-6);
    }

    #[test]
    fn fill_color_clamps_opacity() {
        let color = fill_sprite_color([0, 0, 0, 255], 2.0).to_srgba();
        assert_eq!(color.alpha, 1.0);
    }

    #[test]
    fn bitmap_upload_respects_dirty_flag() {
        // handle_for must upload on first sight and on dirty, and reuse the
        // cached handle otherwise. Uses a real Assets<Image> (no GPU needed
        // for asset storage).
        let mut scene = Scene::default();
        let b1 = scene.add_bitmap(2, 2, vec![0u8; 16]);
        let b2 = scene.add_bitmap(2, 2, vec![255u8; 16]);

        let mut cache = BitmapAssets::default();
        let mut images = Assets::<Image>::default();
        let mut uploaded = Vec::new();

        let h1 = cache
            .handle_for(b1, &scene, &mut images, &mut uploaded)
            .unwrap();
        assert_eq!(uploaded, vec![b1], "first sight uploads");
        assert!(h1.is_strong());

        // The caller (sync_scene) clears dirty after the upload; emulate it.
        scene.bitmap_mut(b1).unwrap().dirty = false;

        // Not dirty → cached, no new upload.
        let h1b = cache
            .handle_for(b1, &scene, &mut images, &mut uploaded)
            .unwrap();
        assert!(h1b == h1);
        assert_eq!(uploaded, vec![b1]);

        // New bitmap → uploads too.
        let h2 = cache
            .handle_for(b2, &scene, &mut images, &mut uploaded)
            .unwrap();
        assert_eq!(uploaded, vec![b1, b2]);
        assert!(h2 != h1);

        // Mark dirty → re-upload.
        scene.bitmap_mut(b1).unwrap().dirty = true;
        cache
            .handle_for(b1, &scene, &mut images, &mut uploaded)
            .unwrap();
        assert_eq!(uploaded, vec![b1, b2, b1]);

        // Unknown / invalid bitmaps → None.
        assert!(
            cache
                .handle_for(999, &scene, &mut images, &mut uploaded)
                .is_none()
        );
        let bad = scene.add_bitmap(0, 4, Vec::new());
        assert!(
            cache
                .handle_for(bad, &scene, &mut images, &mut uploaded)
                .is_none()
        );
    }

    /// The real title scene's stack z-values (system_status.tjs:
    /// LAYER_LOGO=110000, LAYER_COVER=150000, LAYER_HINT=210000; content
    /// layers sit at z=0). The game's own comment: "レイヤー優先度（数字が
    /// 大きいほど手前）" — larger z = closer/front. The sync must spawn the
    /// z=210000 layer with the HIGHEST sprite z so it renders on top.
    #[test]
    fn sync_spawns_real_title_z_order() {
        let mut scene = Scene::default();
        let win = scene.add_window("title", (1280, 720));
        // Insertion order mirrors the game: HINT (MainWindow ctor) and LOGO
        // are created before the z=0 content layers.
        let hint = scene.add_layer(win, None);
        let logo = scene.add_layer(win, None);
        let bg = scene.add_layer(win, None);
        let cover = scene.add_layer(win, None);
        let art = scene.add_layer(win, None);
        scene.layer_mut(hint).unwrap().z_order = 210000;
        scene.layer_mut(logo).unwrap().z_order = 110000;
        scene.layer_mut(cover).unwrap().z_order = 150000;
        for l in [bg, art] {
            scene.layer_mut(l).unwrap().fill_color = Some([0, 0, 0, 255]);
        }
        for l in [logo, cover, hint] {
            scene.layer_mut(l).unwrap().fill_color = Some([255, 255, 255, 255]);
        }

        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared.clone());
        app.update();

        let world = app.world_mut();
        let mut q = world.query_filtered::<(&SceneSprite, &Transform), With<SceneSprite>>();
        let mut by_layer = std::collections::HashMap::new();
        for (m, t) in q.iter(world) {
            by_layer.insert(m.layer_id, t.translation.z);
        }
        assert_eq!(by_layer[&bg], 0.0, "z=0 content is backmost");
        assert_eq!(by_layer[&art], 1.0);
        assert_eq!(
            by_layer[&logo], 2.0,
            "LAYER_LOGO (110000) in front of z=0 content"
        );
        assert_eq!(
            by_layer[&cover], 3.0,
            "LAYER_COVER (150000) in front of LOGO"
        );
        assert_eq!(by_layer[&hint], 4.0, "LAYER_HINT (210000) is frontmost");
    }

    /// Window opacity × layer opacity compose into the sprite alpha (TVP
    /// opacity is 0..255 → scene stores 0..1; the sync multiplies the two).
    #[test]
    fn opacity_composition_window_times_layer() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (640, 480));
        scene.window_mut(win).unwrap().opacity = 0.5;
        let fill = scene.add_layer(win, None);
        scene.layer_mut(fill).unwrap().rect = Rect {
            x: 0,
            y: 0,
            w: 640,
            h: 480,
        };
        scene.layer_mut(fill).unwrap().fill_color = Some([0, 0, 0, 255]);
        scene.layer_mut(fill).unwrap().opacity = 0.5;
        let bmp_id = scene.add_bitmap(8, 8, vec![255u8; 8 * 8 * 4]);
        let bmp = scene.add_layer(win, None);
        scene.layer_mut(bmp).unwrap().bitmap = Some(bmp_id);
        scene.layer_mut(bmp).unwrap().rect = Rect {
            x: 0,
            y: 0,
            w: 8,
            h: 8,
        };
        scene.layer_mut(bmp).unwrap().opacity = 0.5;

        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared.clone());
        app.update();

        let world = app.world_mut();
        let mut q = world.query_filtered::<(&SceneSprite, &Sprite), With<SceneSprite>>();
        let mut alphas = Vec::new();
        for (_m, s) in q.iter(world) {
            alphas.push(s.color.to_srgba().alpha);
        }
        assert_eq!(alphas.len(), 2);
        for a in alphas {
            assert!(
                (a - 0.25).abs() < 1e-6,
                "0.5 window × 0.5 layer must give sprite alpha 0.25; got {a}"
            );
        }
    }

    /// Fill colors are straight alpha: the RGB channels must NOT be scaled
    /// by the composed opacity (Bevy sprites blend straight alpha by
    /// default, matching TVP's straight-alpha bitmaps).
    #[test]
    fn fill_color_straight_alpha_keeps_rgb() {
        let color = fill_sprite_color([255, 0, 0, 128], 1.0).to_srgba();
        assert_eq!(color.red, 1.0, "RGB untouched by alpha");
        assert_eq!(color.green, 0.0);
        assert_eq!(color.blue, 0.0);
        assert!((color.alpha - 128.0 / 255.0).abs() < 1e-6);

        // Composed opacity scales alpha only.
        let half = fill_sprite_color([255, 0, 0, 128], 0.5).to_srgba();
        assert_eq!(half.red, 1.0);
        assert!((half.alpha - 128.0 / 255.0 * 0.5).abs() < 1e-6);
    }

    /// The real title scene's bitmap layers land in the scene already sized
    /// to their bitmap (the game calls `setSizeToImageSize`, which the
    /// sprite's `_image` child forwards to the native); the sync renders
    /// each at exactly that size with the texture uploaded.
    #[test]
    fn bitmap_layer_rect_matching_bitmap_size() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1280, 720));
        let bmp_id = scene.add_bitmap(251, 254, vec![0u8; 251 * 254 * 4]);
        let l = scene.add_layer(win, None);
        scene.layer_mut(l).unwrap().bitmap = Some(bmp_id);
        // rect == bitmap size (setSizeToImageSize result; frm_0502c SKR).
        scene.layer_mut(l).unwrap().rect = Rect {
            x: 297,
            y: 436,
            w: 251,
            h: 254,
        };

        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared.clone());
        app.update();

        let world = app.world_mut();
        let mut q = world.query_filtered::<(&SceneSprite, &Sprite), With<SceneSprite>>();
        let (_m, sprite) = q.single(world).expect("one sprite");
        assert_eq!(sprite.custom_size, Some(Vec2::new(251.0, 254.0)));
        assert!(sprite.image.is_strong(), "texture uploaded");
        // White tint at full opacity: the texture's own RGBA shows through.
        let c = sprite.color.to_srgba();
        assert_eq!((c.red, c.green, c.blue, c.alpha), (1.0, 1.0, 1.0, 1.0));
    }

    /// A layer whose rect is still 1x1 (the game has not called
    /// setSizeToImageSize yet) renders as a 1px sprite — the sync does NOT
    /// auto-size to the bitmap. Expected: sizing is the script's job
    /// (`Layer.setSizeToImageSize`), and the real title scene always sizes
    /// its bitmap layers (verified via the headless scene dump).
    #[test]
    fn bitmap_layer_1x1_rect_stays_1px() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1280, 720));
        let bmp_id = scene.add_bitmap(64, 64, vec![0u8; 64 * 64 * 4]);
        let l = scene.add_layer(win, None);
        scene.layer_mut(l).unwrap().bitmap = Some(bmp_id);
        scene.layer_mut(l).unwrap().rect = Rect {
            x: 10,
            y: 10,
            w: 1,
            h: 1,
        };

        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = app_with_sync(shared);
        app.update();

        let world = app.world_mut();
        let mut q = world.query_filtered::<(&SceneSprite, &Sprite), With<SceneSprite>>();
        let (_m, sprite) = q.single(world).expect("one sprite");
        assert_eq!(sprite.custom_size, Some(Vec2::new(1.0, 1.0)));
    }

    /// Y-flip + center-anchor placement with the real title scene's
    /// positions (window 1280x720, Bevy y-up origin at center).
    #[test]
    fn rect_center_real_title_positions() {
        // Full-window layer → origin.
        assert_eq!(
            rect_center(
                Rect {
                    x: 0,
                    y: 0,
                    w: 1280,
                    h: 720
                },
                1280,
                720
            ),
            (0.0, 0.0)
        );
        // frm_0501b "gear" strip at TVP (0,317,1280x403): Bevy center
        // x = 0, y = 360 - (317 + 201.5) = -158.5 (below center, y-down
        // flipped to y-up).
        assert_eq!(
            rect_center(
                Rect {
                    x: 0,
                    y: 317,
                    w: 1280,
                    h: 403
                },
                1280,
                720
            ),
            (0.0, -158.5)
        );
        // A sprite whose TVP rect center is (780, 460) (rect
        // (640,360,280x200)) lands at Bevy (780-640, 360-460) = (140, -100).
        assert_eq!(
            rect_center(
                Rect {
                    x: 640,
                    y: 360,
                    w: 280,
                    h: 200
                },
                1280,
                720
            ),
            (140.0, -100.0)
        );
        // frm_0507 title logo (371,286,539x149): center (640.5, 360.5) →
        // (0.5, -0.5).
        assert_eq!(
            rect_center(
                Rect {
                    x: 371,
                    y: 286,
                    w: 539,
                    h: 149
                },
                1280,
                720
            ),
            (0.5, -0.5)
        );
    }

    /// Sprite-sheet frame selection: a 96×32 sheet in a 32×32 layer with
    /// `imageLeft = -32` shows the middle frame, and the source rect is
    /// mapped back to bitmap pixels. This is exactly the game's
    /// `Button.setButton(n)` path.
    #[test]
    fn visible_image_region_selects_sprite_frame() {
        let layer = Rect {
            x: 100,
            y: 100,
            w: 32,
            h: 32,
        };
        let (src, dest) = visible_image_region(layer, -32, 0, 96, 32, 96, 32).unwrap();
        assert_eq!(
            src,
            Rect {
                x: 32,
                y: 0,
                w: 32,
                h: 32
            }
        );
        assert_eq!(
            dest,
            Rect {
                x: 100,
                y: 100,
                w: 32,
                h: 32
            }
        );

        // First frame (no offset).
        let (src, _) = visible_image_region(layer, 0, 0, 96, 32, 96, 32).unwrap();
        assert_eq!(
            src,
            Rect {
                x: 0,
                y: 0,
                w: 32,
                h: 32
            }
        );
        // Third frame.
        let (src, _) = visible_image_region(layer, -64, 0, 96, 32, 96, 32).unwrap();
        assert_eq!(
            src,
            Rect {
                x: 64,
                y: 0,
                w: 32,
                h: 32
            }
        );
    }

    /// A large image is clipped to the layer: the visible dest is the layer
    /// rect and the source is the matching region of the bitmap.
    #[test]
    fn visible_image_region_clips_large_image() {
        // A 100×100 layer showing a 200×200 image offset by (-20,-30):
        // visible screen rect = (x+? ...) clipped to the layer.
        let layer = Rect {
            x: 50,
            y: 60,
            w: 100,
            h: 100,
        };
        let (src, dest) = visible_image_region(layer, -20, -30, 200, 200, 200, 200).unwrap();
        assert_eq!(
            dest,
            Rect {
                x: 50,
                y: 60,
                w: 100,
                h: 100
            }
        );
        assert_eq!(
            src,
            Rect {
                x: 20,
                y: 30,
                w: 100,
                h: 100
            }
        );

        // An image placed entirely outside the layer yields nothing.
        assert!(visible_image_region(layer, -500, 0, 200, 200, 200, 200).is_none());
        assert!(visible_image_region(layer, 0, -500, 200, 200, 200, 200).is_none());
    }
}
