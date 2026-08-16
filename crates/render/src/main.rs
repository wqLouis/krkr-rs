//! `krkr-vn` binary: the demo harness and the graphical game runner.
//!
//! * `krkr-vn demo` — builds a small [`Scene`] in code and renders it for a
//!   few seconds (or until the window closes), proving the scene → Bevy
//!   pipeline end to end.
//! * `krkr-vn run <game-dir> [--headless]` — the real game runner: mounts
//!   the game's storage, registers the TVP native classes (`System`,
//!   `Storages`, `Scripts`, then the visual `Window`/`Layer`/`Bitmap`/
//!   `Font`/`Timer` bound to the shared [`Scene`]), runs `startup.tjs`, and
//!   then drives the VM's timers every frame **before** syncing the scene
//!   into Bevy entities (script mutations render the same frame).
//!   `--headless` runs one pass of the same pipeline with no window or GPU
//!   (MinimalPlugins) and dumps the resulting scene state to stdout.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use bevy::app::{App, AppExit};
use bevy::asset::Assets;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::prelude::{
    Commands, DefaultPlugins, MessageWriter, MinimalPlugins, PluginGroup, Res, ResMut, Resource,
    Startup, Time, Update,
};
use bevy::window::{Window, WindowPlugin};
use engine::loader::LoadReport;
use krkr_render::sync::{BitmapAssets, SharedScene, sync_scene};
use tvp_visual::scene::{BitmapState, Rect, Scene};

/// Game logical window size (1280x720, the title screen's native size).
const GAME_SIZE: (u32, u32) = (1280, 720);
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
            let headless = rest.iter().any(|a| a.as_str() == "--headless");
            let Some(game_dir) = rest
                .iter()
                .find(|a| !a.starts_with('-'))
                .map(|a| PathBuf::from(a.as_str()))
            else {
                eprintln!("krkr-vn: usage: krkr-vn run <game-dir> [--headless]");
                std::process::exit(2);
            };
            run_game(&game_dir, headless);
        }
        _ => {
            eprintln!("krkr-vn: usage: krkr-vn demo | krkr-vn run <game-dir> [--headless]");
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------------------
// Game runner (`krkr-vn run <game-dir>`)
// ---------------------------------------------------------------------------

/// `krkr-vn run <game-dir>` configuration (windowed or headless).
#[derive(Resource)]
struct GameConfig {
    game_dir: PathBuf,
}

/// The running TJS2 VM + its start [`Instant`], inserted by [`game_startup`].
/// [`run_vm`] polls it every frame for `timer_poll`'s `now_ms`.
#[derive(Resource)]
struct VmRuntime {
    engine: Arc<tjs2_sys::Tjs2Engine>,
    started: Instant,
}

/// Result of executing `startup.tjs` — kept so `--headless` and the
/// integration test can inspect what actually ran.
#[derive(Resource)]
struct StartupReport(LoadReport);

/// `krkr-vn run <game-dir>`: mount the game, register natives, run
/// `startup.tjs`, then loop (VM timers → scene sync → render).
/// `--headless` runs one pass with no window and dumps the scene instead.
fn run_game(game_dir: &std::path::Path, headless: bool) -> ! {
    let shared = SharedScene(Arc::new(RwLock::new(Scene::default())));
    if headless {
        run_headless(shared, game_dir.to_path_buf());
    }
    println!("krkr-vn: running {game_dir:?} (close the window to exit)");
    game_app(shared, game_dir.to_path_buf()).run();
    unreachable!("App::run returns only after the app exits")
}

/// The windowed game app: default plugins (window + renderer), the shared
/// scene, and the game pipeline. The window is 1280x720 "krkr-rs"; closing
/// it exits (default `ExitCondition::OnPrimaryClosed`).
fn game_app(shared: SharedScene, game_dir: PathBuf) -> App {
    let mut app = App::new();
    app.insert_resource(shared)
        .insert_resource(GameConfig { game_dir })
        .init_resource::<BitmapAssets>()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "krkr-rs".into(),
                resolution: GAME_SIZE.into(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .add_systems(Startup, game_startup)
        // run_vm BEFORE sync_scene: script mutations must render the same
        // frame, not one frame later.
        .add_systems(Update, (run_vm, sync_scene).chain());
    app
}

/// The headless game app: `MinimalPlugins` (no window/renderer — works on
/// GPU-less machines), the shared scene, and the same pipeline. One
/// `App::update()` drives Startup (prepare + register + startup.tjs) and one
/// Update (timer_poll + sync_scene).
fn headless_game_app(shared: SharedScene, game_dir: PathBuf) -> App {
    let mut app = App::new();
    app.insert_resource(shared)
        .insert_resource(GameConfig { game_dir })
        .insert_resource(Assets::<Image>::default())
        .init_resource::<BitmapAssets>()
        .add_plugins(MinimalPlugins)
        .add_systems(Startup, game_startup)
        .add_systems(Update, (run_vm, sync_scene).chain());
    app
}

/// Startup system (runs once): mount storage, bootstrap the TJS2 VM,
/// register every TVP native class, set their contexts, and run
/// `startup.tjs`. Mount/VM/registration errors are fatal (log + exit); a
/// **script-level** error in `startup.tjs` is NOT fatal — the game
/// continues into its timer event loop (WAVE3 SA-5), so it is logged only.
fn game_startup(config: Res<GameConfig>, shared: Res<SharedScene>, mut commands: Commands) {
    // 1. Mount storage (game dir + xp3 archives) and bootstrap the VM.
    let game_dir = config.game_dir.display().to_string();
    let (storage, engine) = match engine::loader::prepare(&game_dir) {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("krkr-vn: cannot load game {game_dir:?}: {e}");
            std::process::exit(1);
        }
    };

    // 2. Point the `System.*` property getters at the mounted game.
    tvp_natives::set_system_context(tvp_natives::SystemContext {
        project_dir: config.game_dir.clone(),
        app_data_dir: std::env::temp_dir(),
        screen_size: GAME_SIZE,
        touch_device: false,
    });

    // 3. Register the native classes; the visual ones bind to our shared
    //    scene (natives mutate it under a write lock, sync_scene renders
    //    it under a read lock).
    register_natives(&engine, &storage, &shared).unwrap_or_else(|e| {
        log::error!("krkr-vn: native registration failed: {e}");
        std::process::exit(1);
    });

    // 4. Run startup.tjs; a script error is non-fatal (log it and keep
    //    going — timers still fire and the scene keeps syncing).
    match engine::loader::run_startup(&engine, &storage) {
        Ok(report) => {
            log::info!(
                "startup report: {} archive(s), startup.tjs at {}",
                report.archives_mounted,
                report.startup_location.as_deref().unwrap_or("<not found>")
            );
            if let Some(err) = &report.startup_error {
                log::warn!("startup.tjs error (non-fatal, game continues): {err}");
            }
            commands.insert_resource(StartupReport(report));
        }
        Err(e) => {
            log::error!("krkr-vn: cannot run startup.tjs: {e}");
            std::process::exit(1);
        }
    }

    // 5. Hand the VM to the update loop; `now_ms` is measured from here.
    commands.insert_resource(VmRuntime {
        engine,
        started: Instant::now(),
    });
}

/// Register every TVP native class and set the global contexts they read
/// (order matters: base natives first, the visual ones last).
fn register_natives(
    engine: &Arc<tjs2_sys::Tjs2Engine>,
    storage: &Arc<Mutex<engine::Storage>>,
    shared: &SharedScene,
) -> Result<(), String> {
    engine
        .as_ref()
        .set_data_dir(&storage.lock().unwrap().game_dir().display().to_string());
    tvp_natives::register_all(engine)?;
    tvp_kagparser::register_kagparser(engine)?;
    tvp_kagparser::set_context(Some(engine.clone()), Some(storage.clone()));
    tvp_storages::register_storages(engine)?;
    tvp_scripts::register_scripts(engine)?;
    tvp_visual::register_visual(engine, shared.0.clone(), storage.clone())?;
    tvp_storages::set_storage(Some(storage.clone()));
    tvp_scripts::set_context(Some(engine.clone()), Some(storage.clone()));
    Ok(())
}

/// Drive the TJS2 VM once per frame: fire due timers (the game's event
/// loop). A panic inside a TJS callback must not kill the app — catch it,
/// log it, and keep the frame going.
fn run_vm(vm: Res<VmRuntime>) {
    let now_ms = vm.started.elapsed().as_millis() as u64;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tvp_natives::async_trigger_poll(&vm.engine);
        tvp_visual::timer_poll(&vm.engine, now_ms);
        tvp_natives::continuous_handler_poll(&vm.engine);
    }));
    if let Err(payload) = result {
        log::error!(
            "timer_poll panicked in a TJS callback (caught; the app continues): {}",
            panic_message(&payload)
        );
    }
}

/// Human-readable message from a `catch_unwind` panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// `krkr-vn run <game-dir> --headless`: drive ONE pass of the app's systems
/// manually (no window — this machine has no GPU adapter, so a window
/// cannot open), then dump the resulting [`Scene`] state to stdout and exit
/// 0. Proves the whole chain: real startup.tjs → natives → Scene populated
/// with the title-screen data.
fn run_headless(shared: SharedScene, game_dir: PathBuf) -> ! {
    let mut app = headless_game_app(shared.clone(), game_dir);
    app.update(); // Startup (prepare + register + startup.tjs) + one Update (timer_poll + sync_scene)

    if let Some(report) = app.world().get_resource::<StartupReport>() {
        println!(
            "startup report: {} archive(s), startup.tjs at {}",
            report.0.archives_mounted,
            report
                .0
                .startup_location
                .as_deref()
                .unwrap_or("<not found>")
        );
        if let Some(err) = &report.0.startup_error {
            println!("startup.tjs error (non-fatal): {err}");
        }
    }

    let scene = shared.0.read().expect("shared scene lock poisoned");
    println!("{}", dump_scene(&scene));
    drop(scene);

    std::process::exit(0);
}

/// One-line-per-window/layer/bitmap summary of the scene state (pixel
/// contents are counted, not printed).
fn dump_scene(scene: &Scene) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "scene dump: {} window(s), {} layer(s), {} bitmap(s), {} font(s)",
        scene.windows.len(),
        scene.layers.len(),
        scene.bitmaps.len(),
        scene.fonts.len()
    ));
    for w in &scene.windows {
        lines.push(format!(
            "window #{} {:?} {}x{} visible={} opacity={} primary_layer={}",
            w.id,
            w.title,
            w.inner_size.0,
            w.inner_size.1,
            w.visible,
            w.opacity,
            w.primary_layer
                .map_or_else(|| "none".to_string(), |id| id.to_string())
        ));
    }
    for l in &scene.layers {
        lines.push(format!(
            "layer #{} win={} rect=({},{},{}x{}) bitmap={} fill={} visible={} opacity={} z={}",
            l.id,
            l.window,
            l.rect.x,
            l.rect.y,
            l.rect.w,
            l.rect.h,
            l.bitmap
                .map_or_else(|| "none".to_string(), |id| id.to_string()),
            l.fill_color
                .map_or_else(|| "none".to_string(), |f| format!("{f:?}")),
            l.visible,
            l.opacity,
            l.z_order
        ));
    }
    for b in &scene.bitmaps {
        lines.push(format!(
            "bitmap #{} {}x{} name={}",
            b.id,
            b.width,
            b.height,
            b.name.as_deref().unwrap_or("<unnamed>")
        ));
    }
    lines.join("\n")
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

/// `krkr-vn demo`: build a scene in code, render it, animate it.
fn run_demo() -> ! {
    println!(
        "krkr-vn demo: rendering {}x{} scene for {DEMO_SECONDS}s (close the window to exit early)",
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
        println!("krkr-vn demo: done, exiting");
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
            .init_resource::<BitmapAssets>()
            .init_resource::<DemoAnimateState>()
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

    /// Real-game headless integration test: the same pipeline the bin runs
    /// (`krkr-vn run <game-dir> --headless`), driven in-process — Startup
    /// (prepare + register natives + startup.tjs) then one Update
    /// (timer_poll + sync_scene).
    ///
    /// Requires the real game at `/mnt/DATA/Games/Others/test` on this
    /// machine, which is NOT committed to the repo, so this test is
    /// `#[ignore]`d by default. Run it with:
    /// `cargo test -p render -- --ignored real_game_headless_populates_scene`
    /// or verify the same path manually via
    /// `cargo run -p render --bin krkr-vn -- run /mnt/DATA/Games/Others/test --headless`.
    #[test]
    #[ignore = "needs the real game at /mnt/DATA/Games/Others/test (not in the repo); use --ignored or the --headless manual run"]
    fn real_game_headless_populates_scene() {
        let game = PathBuf::from("/mnt/DATA/Games/Others/test");
        assert!(game.is_dir(), "real game dir must exist for this test");
        let shared = SharedScene(Arc::new(RwLock::new(Scene::default())));

        let mut app = headless_game_app(shared.clone(), game);
        app.update();

        // startup.tjs executed: the report exists; a remaining script error
        // must be a *known missing-native* one — the parallel tvp-visual
        // natives may not cover every class the init path touches.
        let report = &app
            .world()
            .get_resource::<StartupReport>()
            .expect("game_startup must run startup.tjs and store the report")
            .0;
        assert!(report.archives_mounted > 0, "game xp3 archives must mount");
        if let Some(err) = &report.startup_error {
            assert!(
                is_known_missing_native_error(err),
                "unexpected startup error (should be none or a missing-native one): {err}"
            );
        }

        // The title screen: ≥1 window with a primary layer, and ≥1 bitmap
        // decoded from the game's xp3 archives (FRM_0501b / bg images).
        let scene = shared.0.read().expect("shared scene lock poisoned");
        assert!(
            !scene.windows.is_empty(),
            "startup.tjs must create at least one window"
        );
        assert!(
            scene.windows.iter().any(|w| w.primary_layer.is_some()),
            "at least one window must have a primary layer"
        );
        assert!(
            !scene.bitmaps.is_empty(),
            "startup.tjs must decode ≥1 bitmap from the game's xp3 archives"
        );
        drop(scene);

        // Print what loaded (mirrors the --headless dump).
        let scene = shared.0.read().unwrap();
        println!("{}", dump_scene(&scene));
        for bmp in &scene.bitmaps {
            println!(
                "loaded bitmap: {} ({}x{})",
                bmp.name.as_deref().unwrap_or("<unnamed>"),
                bmp.width,
                bmp.height
            );
        }
    }

    /// Known TJS "missing native" error patterns — a startup error that
    /// matches one of these is the parallel natives work being incomplete,
    /// not a regression; anything else is unexpected.
    fn is_known_missing_native_error(err: &str) -> bool {
        ["does not exist", "identifier not found"]
            .iter()
            .any(|pat| err.contains(pat))
    }
}
