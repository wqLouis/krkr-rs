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
//! [`Scene::window_layer_order`] already returns layers back → front (sorted
//! by z-order/insertion); we assign `z = index as f32` (backmost = 0,
//! frontmost = N-1). Bevy sorts 2D sprites back-to-front by view-space
//! depth, so higher z renders on top. Only parent-less layers are in the
//! order today; hierarchical children are a later milestone.
//!
//! # Blend modes
//!
//! TVP layers carry a blend `type` (`ltOpaque=1`, `ltAlpha=2`,
//! `ltAdditive=3`, `ltSubtractive=4`, `ltAddAlpha=12`, ... — enum values
//! per `reference/cpp/core/visual/drawable.h`; the game's constants are
//! registered in `tvp-natives/src/constants.rs`). The scene model does NOT
//! store the type (the natives keep it per-instance for script round-trips,
//! see `layer_type_stored_per_instance_not_in_scene` in
//! `tvp-visual/src/natives/layer.rs`), so every sprite here renders with
//! plain straight-alpha blending regardless of type. **Additive /
//! subtractive / multiplicative / ... blends are NOT implemented**: a layer
//! whose script type is `ltAdditive` renders exactly like `ltAlpha` (with
//! the composed window × layer opacity as alpha). This affects the title
//! scene's COVER fade-in (`system/title.tjs` sets `type = ltAdditive`) and
//! the ADV flash effects (`system/advscreen.tjs` sets ltAdditive/
//! ltSubtractive), so such frames composite darker than the reference until
//! real blend modes land.
//!
//! # Rebuild strategy
//!
//! Milestone approach: **full rebuild every frame** — every [`SceneSprite`]
//! and [`WindowRoot`] entity is despawned and respawned from the current
//! scene snapshot. The scene is small (a few dozen layers), so the churn is
//! negligible and correctness wins; layer/window removal is handled for free.
//! The only incremental part is **texture upload**: [`BitmapAssets`] caches
//! one [`Handle<Image>`] per bitmap id, and [`tvp_visual::scene::BitmapState::dirty`]
//! is the sole trigger for a re-upload (the flag is cleared here after the
//! upload).
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
use bevy::camera::{Camera2d, ClearColorConfig};
use bevy::ecs::prelude::{Commands, Component, Entity, Query, Res, ResMut, Resource, With};
use bevy::image::Image;
use bevy::math::Vec2;
use bevy::prelude::{Camera, Color, Sprite, Transform, Visibility};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use tvp_visual::scene::{BitmapState, LayerState, Rect, Scene};

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

/// Root entity for one logical window: the background/anchor node that owns
/// the window's layer sprites.
#[derive(Component)]
pub struct WindowRoot {
    pub window_id: u32,
}

/// One spawned layer sprite.
#[derive(Component)]
pub struct SceneSprite {
    pub layer_id: u32,
}

/// The 2D camera we manage (for the first window). Kept across frames — it
/// has no per-frame state worth churning, unlike the full layer rebuild.
#[derive(Component)]
pub struct SceneCamera;

/// The scene → Bevy sync system. Run in `Update`, after the VM tick mutates
/// the scene.
pub fn sync_scene(
    mut commands: Commands,
    shared: Res<SharedScene>,
    mut bitmaps: ResMut<BitmapAssets>,
    mut images: ResMut<Assets<Image>>,
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
            let (x, y) = rect_center(layer.rect, win_w, win_h);
            let alpha = win_opacity * clamp_opacity(layer.opacity);
            let sprite = build_sprite(
                layer,
                alpha,
                &mut bitmaps,
                &scene,
                &mut images,
                &mut uploaded,
            );

            let sprite = commands
                .spawn((
                    SceneSprite { layer_id },
                    sprite,
                    Transform::from_xyz(x, y, sprite_z(index)),
                    if layer.visible {
                        Visibility::Visible
                    } else {
                        Visibility::Hidden
                    },
                ))
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

/// Build the [`Sprite`] for one layer.
///
/// * Bitmap layer → texture from [`BitmapAssets`], tinted white with the
///   composed alpha (the texture carries the actual RGB/alpha).
/// * Fill layer → solid color from `fill_color` (straight alpha), alpha
///   multiplied with the composed window × layer opacity.
/// * Bitmap missing/invalid, no fill → transparent black.
fn build_sprite(
    layer: &LayerState,
    alpha: f32,
    bitmaps: &mut BitmapAssets,
    scene: &Scene,
    images: &mut Assets<Image>,
    uploaded: &mut Vec<u32>,
) -> Sprite {
    match layer
        .bitmap
        .and_then(|id| bitmaps.handle_for(id, scene, images, uploaded))
    {
        Some(handle) => Sprite {
            image: handle,
            color: bitmap_tint(alpha),
            custom_size: Some(Vec2::new(layer.rect.w as f32, layer.rect.h as f32)),
            ..Default::default()
        },
        None => {
            let color = layer
                .fill_color
                .map(|fill| fill_sprite_color(fill, alpha))
                .unwrap_or_else(|| Color::srgba(0.0, 0.0, 0.0, alpha));
            Sprite::from_color(color, Vec2::new(layer.rect.w as f32, layer.rect.h as f32))
        }
    }
}

// ---------------------------------------------------------------------------
// Pure conversion helpers (unit-tested; no Bevy world access).
// ---------------------------------------------------------------------------

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
    fn app_with_sync(shared: SharedScene) -> App {
        let mut app = App::new();
        app.insert_resource(shared)
            .insert_resource(Assets::<Image>::default())
            .init_resource::<BitmapAssets>()
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
        let mut app = app_with_sync(shared.clone());
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
}
