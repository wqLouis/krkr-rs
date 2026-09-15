//! `krkr-rs` binary: the demo harness and the graphical game runner.
//!
//! * `krkr-rs demo` — builds a small [`Scene`] in code and renders it for a
//!   few seconds (or until the window closes), proving the scene → Bevy
//!   pipeline end to end.
//! * `krkr-rs run <game-dir> [--headless]` — the real game runner: mounts
//!   the game's storage, registers the TVP native classes (`System`,
//!   `Storages`, `Scripts`, then the visual `Window`/`Layer`/`Bitmap`/
//!   `Font`/`Timer` bound to the shared [`Scene`]), runs `startup.tjs`, and
//!   then drives the VM's timers every frame **before** syncing the scene
//!   into Bevy entities (script mutations render the same frame).
//!   `--headless` runs one pass of the same pipeline with no window or GPU
//!   (MinimalPlugins) and dumps the resulting scene state to stdout.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bevy::app::{App, AppExit};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::prelude::{
    DefaultPlugins, MessageWriter, PluginGroup, Res, ResMut, Resource, Time, Update,
};
use bevy::window::{Window, WindowPlugin};
use krkr_render::GpuPrimitives;
use krkr_render::blend::LayerBlendPlugin;
use krkr_render::sync::{BitmapAssets, FrameBlendMaterials, SharedScene, sync_scene};
use tvp_visual::scene::{BitmapState, Rect, Scene};

/// Demo window size (also the game's logical size, 1280x720).
const DEMO_SIZE: (u32, u32) = (1280, 720);
/// Demo duration before auto-exit (window close also exits).
const DEMO_SECONDS: f32 = 8.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    engine::init_logging(args.iter().any(|a| a == "-v" || a == "--verbose"));
    match args.first().map(String::as_str) {
        Some("demo") => run_demo(),
        Some("run") => {
            let rest: Vec<&String> = args.iter().skip(1).collect();
            let mut headless = false;
            let mut game_dir: Option<PathBuf> = None;
            let mut font_config: Option<PathBuf> = None;
            let mut i = 0;
            while i < rest.len() {
                let arg = rest[i].as_str();
                match arg {
                    "--headless" => headless = true,
                    // Verbosity is handled by `engine::init_logging`; the
                    // `run` subcommand just tolerates it anywhere.
                    "-v" | "--verbose" => {}
                    "--font-config" => {
                        i += 1;
                        match rest.get(i) {
                            Some(path) => font_config = Some(PathBuf::from(path.as_str())),
                            None => {
                                eprintln!("krkr-rs: --font-config requires a path");
                                std::process::exit(2);
                            }
                        }
                    }
                    _ if arg.starts_with("--font-config=") => {
                        font_config = Some(PathBuf::from(&arg["--font-config=".len()..]));
                    }
                    _ if arg.starts_with('-') => {
                        eprintln!("krkr-rs: unknown option {arg}");
                        std::process::exit(2);
                    }
                    _ => {
                        if game_dir.is_none() {
                            game_dir = Some(PathBuf::from(arg));
                        }
                    }
                }
                i += 1;
            }
            let Some(game_dir) = game_dir else {
                eprintln!(
                    "krkr-rs: usage: krkr-rs run <game-dir> [--headless] [--font-config <path>]"
                );
                std::process::exit(2);
            };
            krkr_render::runner::run_game(&game_dir, font_config, headless);
        }
        _ => {
            eprintln!(
                "krkr-rs: usage: krkr-rs demo | krkr-rs run <game-dir> [--headless] [--font-config <path>]"
            );
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------------------
// Demo
// ---------------------------------------------------------------------------

/// Layer/bitmap ids the animator needs (the demo is single-run, so we build
/// the scene once and remember the handles).
#[derive(Resource, Clone, Copy)]
struct DemoIds {
    pulse_layer: u32,
    mover_layer: u32,
    mover_bitmap: u32,
}

/// Tracks which 2s palette-swap epoch has been applied, so a bitmap
/// re-upload happens exactly once per epoch.
#[derive(Resource, Default)]
struct DemoAnimateState {
    last_swap_epoch: i32,
}

/// Auto-exit after `DEMO_SECONDS` (window close also exits via the default
/// `ExitCondition::OnPrimaryClosed`).
#[derive(Resource)]
struct DemoTimer(Duration);

/// `krkr-rs demo`: build a scene in code, render it, animate it.
fn run_demo() -> ! {
    println!(
        "krkr-rs demo: rendering {}x{} scene for {DEMO_SECONDS}s (close the window to exit early)",
        DEMO_SIZE.0, DEMO_SIZE.1
    );

    let (scene, ids) = build_demo_scene();
    let shared = SharedScene(Arc::new(RwLock::new(scene)));

    demo_app(shared, ids).run();

    unreachable!("App::run returns only after the app exits")
}

/// The demo Bevy app: full default plugins (window + renderer), the shared
/// scene, the sync system and the demo animators.
fn demo_app(shared: SharedScene, ids: DemoIds) -> App {
    let mut app = App::new();
    app.insert_resource(shared)
        .insert_resource(ids)
        .init_resource::<BitmapAssets>()
        .init_resource::<GpuPrimitives>()
        .init_resource::<FrameBlendMaterials>()
        .init_resource::<DemoAnimateState>()
        .insert_resource(DemoTimer(Duration::from_secs_f32(DEMO_SECONDS)))
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "krkr-rs".into(),
                resolution: DEMO_SIZE.into(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .add_plugins(LayerBlendPlugin)
        .add_plugins(krkr_render::menu::MenuPlugin)
        .add_systems(Update, (animate_demo, sync_scene).chain())
        .add_systems(Update, demo_auto_exit);
    app
}

/// Build the demo scene: one 1280x720 window with a solid-fill background
/// layer and three bitmap layers (generated RGBA8 art in code).
fn build_demo_scene() -> (Scene, DemoIds) {
    let mut scene = Scene::default();
    let win = scene.add_window("krkr-rs", DEMO_SIZE);

    // Background: full-window solid fill.
    let bg = scene.add_layer(win, None);
    {
        let layer = scene.layer_mut(bg).unwrap();
        layer.rect = Rect {
            x: 0,
            y: 0,
            w: DEMO_SIZE.0,
            h: DEMO_SIZE.1,
        };
        layer.fill_color = Some([18, 22, 40, 255]);
        layer.z_order = -10;
    }

    // Layer 1: red checkerboard bitmap (its opacity pulses).
    let checker = scene.add_bitmap(
        256,
        256,
        checker_rgba(256, 256, [240, 90, 90, 255], [70, 10, 10, 255]),
    );
    let pulse_layer = scene.add_layer(win, None);
    {
        let layer = scene.layer_mut(pulse_layer).unwrap();
        layer.bitmap = Some(checker);
        layer.rect = Rect {
            x: 100,
            y: 100,
            w: 256,
            h: 256,
        };
    }

    // Layer 2: gradient bitmap (static).
    let gradient = scene.add_bitmap(
        320,
        180,
        vertical_gradient_rgba(320, 180, [80, 200, 120, 255], [10, 40, 90, 255]),
    );
    let l2 = scene.add_layer(win, None);
    {
        let layer = scene.layer_mut(l2).unwrap();
        layer.bitmap = Some(gradient);
        layer.rect = Rect {
            x: 480,
            y: 60,
            w: 320,
            h: 180,
        };
    }

    // Layer 3: "logo" checkerboard that slides horizontally and re-uploads
    // its texture every 2 seconds (dirty flag → incremental upload).
    let mover_bitmap = scene.add_bitmap(
        300,
        120,
        checker_rgba(300, 120, [250, 220, 120, 255], [40, 30, 80, 255]),
    );
    let mover_layer = scene.add_layer(win, None);
    {
        let layer = scene.layer_mut(mover_layer).unwrap();
        layer.bitmap = Some(mover_bitmap);
        layer.rect = Rect {
            x: 300,
            y: 320,
            w: 300,
            h: 120,
        };
    }

    (
        scene,
        DemoIds {
            pulse_layer,
            mover_layer,
            mover_bitmap,
        },
    )
}

/// Animate the demo scene under the write lock: pulse one layer's opacity,
/// slide another, and periodically repaint + dirty a bitmap to exercise the
/// incremental texture upload path.
fn animate_demo(
    time: Res<Time>,
    ids: Res<DemoIds>,
    shared: Res<SharedScene>,
    mut state: ResMut<DemoAnimateState>,
) {
    let t = time.elapsed_secs();
    let mut scene = shared.0.write().expect("shared scene lock poisoned");

    // Pulse layer opacity (sin 0..1 → 0.35..1.0).
    if let Some(layer) = scene.layer_mut(ids.pulse_layer) {
        layer.opacity = 0.35 + 0.65 * (t * 2.0).sin().abs();
    }

    // Slide the mover layer horizontally (ping-pong over 240px).
    if let Some(layer) = scene.layer_mut(ids.mover_layer) {
        let span = 240.0f32;
        let phase = (t * 60.0) % (span * 2.0);
        layer.rect.x = 300
            + if phase <= span {
                phase
            } else {
                span * 2.0 - phase
            } as i32;
    }

    // Repaint the mover bitmap every 2s and flag it dirty → re-upload.
    let epoch = (t / 2.0) as i32;
    if epoch > state.last_swap_epoch {
        state.last_swap_epoch = epoch;
        if let Some(bitmap) = scene.bitmap_mut(ids.mover_bitmap) {
            repaint_checker(bitmap, epoch);
        }
    }
}

/// Swap the mover bitmap's palette and mark it dirty so `sync_scene`
/// re-uploads the texture (incremental path).
fn repaint_checker(bitmap: &mut BitmapState, epoch: i32) {
    const PALETTES: [([u8; 4], [u8; 4]); 3] = [
        ([250, 220, 120, 255], [40, 30, 80, 255]),
        ([120, 220, 250, 255], [10, 40, 80, 255]),
        ([220, 120, 250, 255], [60, 10, 60, 255]),
    ];
    let (c1, c2) = PALETTES[(epoch as usize) % PALETTES.len()];
    bitmap.rgba = checker_rgba(bitmap.width, bitmap.height, c1, c2);
    bitmap.dirty = true;
}

fn demo_auto_exit(time: Res<Time>, timer: Res<DemoTimer>, mut exit: MessageWriter<AppExit>) {
    if time.elapsed() >= timer.0 {
        println!("krkr-rs demo: done, exiting");
        exit.write(AppExit::Success);
    }
}

/// RGBA8 checkerboard art (16px cells), straight alpha.
fn checker_rgba(w: u32, h: u32, c1: [u8; 4], c2: [u8; 4]) -> Vec<u8> {
    const CELL: u32 = 16;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let on = (x / CELL + y / CELL).is_multiple_of(2);
            data.extend_from_slice(&if on { c1 } else { c2 });
        }
    }
    data
}

/// RGBA8 vertical gradient from `top` to `bottom`.
fn vertical_gradient_rgba(w: u32, h: u32, top: [u8; 4], bottom: [u8; 4]) -> Vec<u8> {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let t = if h <= 1 {
            0.0
        } else {
            y as f32 / (h - 1) as f32
        };
        let px = [
            (top[0] as f32 * (1.0 - t) + bottom[0] as f32 * t).round() as u8,
            (top[1] as f32 * (1.0 - t) + bottom[1] as f32 * t).round() as u8,
            (top[2] as f32 * (1.0 - t) + bottom[2] as f32 * t).round() as u8,
            (top[3] as f32 * (1.0 - t) + bottom[3] as f32 * t).round() as u8,
        ];
        for _ in 0..w {
            data.extend_from_slice(&px);
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::app::{App, Update};
    use bevy::asset::Assets;
    use bevy::ecs::prelude::{Entity, With};
    use bevy::prelude::{Image, MinimalPlugins, Virtual};
    use bevy::time::Time;
    use krkr_render::sync::SceneSprite;

    /// The demo app without a window/renderer (MinimalPlugins has `Time`, so
    /// the animator runs) — lets us exercise the full animate → sync loop on
    /// machines without a GPU.
    fn headless_demo_app() -> (App, SharedScene, DemoIds) {
        let (scene, ids) = build_demo_scene();
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(shared.clone())
            .insert_resource(ids)
            .insert_resource(Assets::<Image>::default())
            .insert_resource(Assets::<bevy::mesh::Mesh>::default())
            .init_resource::<BitmapAssets>()
            .init_resource::<GpuPrimitives>()
            .init_resource::<FrameBlendMaterials>()
            .init_resource::<DemoAnimateState>()
            .add_plugins(krkr_render::blend::LayerBlendPlugin)
            .insert_resource(DemoTimer(Duration::from_secs_f32(DEMO_SECONDS)))
            .add_systems(Update, (animate_demo, sync_scene).chain())
            .add_systems(Update, demo_auto_exit);
        (app, shared, ids)
    }

    #[test]
    fn demo_scene_renders_and_animates_without_panic() {
        let (mut app, shared, ids) = headless_demo_app();

        // Several frames: the animator mutates the scene under a write lock
        // (pulse/move/repaint+dirty), the sync system rebuilds entities
        // under a read lock and clears dirty. No deadlock, no panic.
        for _ in 0..5 {
            app.update();
        }

        let world = app.world_mut();
        // Background fill + 3 bitmap layers.
        assert_eq!(
            world
                .query_filtered::<Entity, With<SceneSprite>>()
                .iter(world)
                .count(),
            4
        );
        // The first window's camera.
        assert_eq!(
            world
                .query_filtered::<Entity, With<krkr_render::sync::SceneCamera>>()
                .iter(world)
                .count(),
            1
        );

        // Drive one animation step deterministically: advance the *virtual*
        // clock (time_system copies it into `Time` every frame, so advancing
        // the plain `Time` would be overwritten). Then the animator pulses
        // opacity, slides the mover layer and repaints the mover bitmap
        // (epoch 1); the sync rebuilds and clears dirty after re-upload.
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .advance_by(Duration::from_secs_f32(3.0));
        app.update();

        {
            let scene = shared.0.read().unwrap();
            // t = 3 → phase = (3*60) % 480 = 180 → x = 300 + 180.
            assert_eq!(scene.layer(ids.mover_layer).unwrap().rect.x, 480);
            // The animator repainted + flagged dirty; the sync re-uploaded
            // and cleared the flag.
            assert!(!scene.bitmap(ids.mover_bitmap).unwrap().dirty);
            // Pulsed opacity is strictly between 0.35 and 1.0.
            let opacity = scene.layer(ids.pulse_layer).unwrap().opacity;
            assert!((0.35..=1.0).contains(&opacity));
        }
    }
}
