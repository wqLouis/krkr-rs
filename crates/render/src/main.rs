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
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use bevy::app::{App, AppExit};
use bevy::asset::Assets;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::prelude::{
    Commands, DefaultPlugins, MessageWriter, MinimalPlugins, PluginGroup, Query, Res, ResMut,
    Resource, Startup, Time, Update, With,
};
use bevy::window::{Monitor, PrimaryMonitor, Window, WindowPlugin};
use engine::loader::LoadReport;
use krkr_render::GpuPrimitives;
use krkr_render::blend::LayerBlendPlugin;
use krkr_render::sync::{
    BitmapAssets, FrameBlendMaterials, HostWindowResolution, SharedScene, SystemContextState,
    WindowRedrawRequested, sync_host_window_resolution, sync_scene,
};
use tvp_visual::scene::{BitmapState, Rect, Scene};

mod input_bridge;

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
            run_game(&game_dir, font_config, headless);
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
// Game runner (`krkr-rs run <game-dir>`)
// ---------------------------------------------------------------------------

/// `krkr-rs run <game-dir>` configuration (windowed or headless).
#[derive(Resource)]
struct GameConfig {
    game_dir: PathBuf,
    /// CLI `--font-config <path>` override (highest precedence; see
    /// [`load_font_config`]).
    font_config: Option<PathBuf>,
}

/// The running TJS2 VM + its start [`Instant`], inserted by [`game_startup`].
/// [`run_vm`] polls it every frame for `timer_poll`'s `now_ms`.
#[derive(Resource)]
pub(crate) struct VmRuntime {
    engine: Arc<tjs2_sys::Tjs2Engine>,
    started: Instant,
    /// Optional live audio output guard (rodio/cpal stream owned by the
    /// main thread). Held here so it lives for the app's lifetime and is
    /// dropped on the main thread when the app exits (cpal 0.15's `Stream`
    /// is `!Send`/`!Sync`; the wrapper makes it safe to hold in a
    /// `Resource` as long as it is created and dropped on the same thread).
    #[allow(dead_code)]
    audio_output: Option<tvp_sound::MainThreadOutputGuard>,
}

/// Result of executing `startup.tjs` — kept so `--headless` and the
/// integration test can inspect what actually ran.
#[derive(Resource)]
struct StartupReport(LoadReport);

/// `krkr-rs run <game-dir>`: mount the game, register natives, run
/// `startup.tjs`, then loop (VM timers → scene sync → render).
/// `--headless` runs one pass with no window and dumps the scene instead.
fn run_game(game_dir: &std::path::Path, font_config: Option<PathBuf>, headless: bool) {
    let shared = SharedScene(Arc::new(RwLock::new(Scene::default())));
    if headless {
        run_headless(shared, game_dir.to_path_buf(), font_config.clone());
    }
    println!("krkr-rs: running {game_dir:?} (close the window to exit)");
    // Returns when the app exits (window closed, `System.exit`, or an exit
    // requested by a script).
    game_app(shared, game_dir.to_path_buf(), font_config).run();
}

/// The windowed game app: default plugins (window + renderer), the shared
/// scene, and the game pipeline. The window is 1280x720 "krkr-rs"; closing
/// it exits (Bevy's default `ExitCondition::OnAllClosed`; the app has a
/// single primary window, so closing it fires the exit).
fn game_app(shared: SharedScene, game_dir: PathBuf, font_config: Option<PathBuf>) -> App {
    let mut app = App::new();
    app.insert_resource(shared)
        .insert_resource(GameConfig {
            game_dir,
            font_config,
        })
        .init_resource::<BitmapAssets>()
        .init_resource::<GpuPrimitives>()
        .init_resource::<FrameBlendMaterials>()
        .init_resource::<HostWindowResolution>()
        .init_resource::<WindowRedrawRequested>()
        .init_resource::<input_bridge::BridgeState>()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "krkr-rs".into(),
                resolution: GAME_SIZE.into(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .add_plugins(LayerBlendPlugin)
        .add_plugins(krkr_render::menu::MenuPlugin)
        .add_systems(Startup, game_startup)
        // run_vm BEFORE sync_scene: script mutations must render the same
        // frame, not one frame later. `consume_window_requests` drains the
        // `bringToFront`/`update`/`resetMouseVelocity` queues that run_vm may
        // have filled and, for `update`, asks `sync_scene` to skip its idle
        // fast path this frame. `sync_host_window_resolution` runs between
        // them so a `Window.setSize`/`setZoom` this frame resizes the OS
        // window before the scene is synced.
        .add_systems(
            Update,
            (
                run_vm,
                input_bridge::consume_window_requests,
                sync_host_window_resolution,
                sync_scene,
            )
                .chain(),
        )
        // `System.exit` / `System.terminate` from the VM → Bevy `AppExit`.
        // Runs after `run_vm` so an exit requested by this frame's script
        // shuts the app down immediately (the window-close path stays with
        // Bevy's default `WindowPlugin`; single window ⇒ `OnAllClosed` fires).
        .add_systems(Update, poll_game_exit.after(run_vm))
        // Input: Bevy events → tvp-input state → game window script methods.
        // Chained and ordered after run_vm so the bridge never touches the
        // single-threaded TJS VM concurrently with the timer polls.
        .add_systems(
            Update,
            (input_bridge::capture_input, input_bridge::dispatch_input)
                .chain()
                .after(run_vm),
        );
    app
}

/// Translate a TJS `System.exit` / `System.terminate` request into a Bevy
/// [`AppExit`]. The VM request is consumed exactly once by
/// [`tvp_natives::take_exit_request`]; `AppExit::Error` only carries a `u8`,
/// so non-zero `i32` codes are clamped into `1..=255` (a non-zero code always
/// stays a failure).
fn poll_game_exit(mut exit: MessageWriter<AppExit>) {
    let Some(code) = tvp_natives::take_exit_request() else {
        return;
    };
    let app_exit = if code == 0 {
        AppExit::Success
    } else {
        AppExit::from_code(code.unsigned_abs().clamp(1, u8::MAX as u32) as u8)
    };
    log::info!("System.exit({code}) — shutting down the game runner");
    exit.write(app_exit);
}

/// The headless game app: `MinimalPlugins` (no window/renderer — works on
/// GPU-less machines), the shared scene, and the same pipeline. One
/// `App::update()` drives Startup (prepare + register + startup.tjs) and one
/// Update (timer_poll + sync_scene).
fn headless_game_app(shared: SharedScene, game_dir: PathBuf, font_config: Option<PathBuf>) -> App {
    let mut app = App::new();
    app.insert_resource(shared)
        .insert_resource(GameConfig {
            game_dir,
            font_config,
        })
        .insert_resource(Assets::<Image>::default())
        // The sync may spawn Mesh2d quads for GPU-blended layers; keep the
        // mesh store alive even without an asset plugin/renderer.
        .insert_resource(Assets::<bevy::mesh::Mesh>::default())
        .init_resource::<BitmapAssets>()
        .init_resource::<GpuPrimitives>()
        .init_resource::<FrameBlendMaterials>()
        .init_resource::<WindowRedrawRequested>()
        .add_plugins(LayerBlendPlugin)
        .add_plugins(krkr_render::menu::MenuPlugin)
        .add_plugins(MinimalPlugins)
        .add_systems(Startup, game_startup)
        .add_systems(
            Update,
            (run_vm, input_bridge::consume_window_requests, sync_scene).chain(),
        );
    app
}

/// Load the explicit font configuration for this run.
///
/// Precedence:
/// 1. CLI `--font-config <path>` (stored in [`GameConfig::font_config`]);
/// 2. the `KRKR_RS_FONT_CONFIG` environment variable;
/// 3. `<game-dir>/fonts.json` (auto-detected);
/// 4. nothing.
///
/// Returns `None` when no config is present, which keeps the pre-config
/// behavior (system discovery) so a game works out of the box. A config that
/// is present but cannot be read/parsed logs a clear error and installs an
/// **empty** config (discovery disabled): a user who asked for explicit font
/// selection must not silently get implicit system fonts.
fn load_font_config(config: &GameConfig) -> Option<tvp_visual::FontConfig> {
    let explicit = config
        .font_config
        .clone()
        .or_else(|| std::env::var_os("KRKR_RS_FONT_CONFIG").map(PathBuf::from));
    let path = match explicit {
        Some(path) => path,
        None => {
            let candidate = config.game_dir.join("fonts.json");
            if !candidate.is_file() {
                return None;
            }
            candidate
        }
    };
    match tvp_visual::FontConfig::from_file(&path) {
        Ok(cfg) => {
            log::info!("krkr-rs: font config loaded from {}", path.display());
            Some(cfg)
        }
        Err(e) => {
            log::error!("krkr-rs: {e}");
            Some(tvp_visual::FontConfig::default())
        }
    }
}

/// Startup system (runs once): mount storage, bootstrap the TJS2 VM,
/// register every TVP native class, set their contexts, and run
/// `startup.tjs`. Mount/VM/registration errors are fatal (log + exit); a
/// **script-level** error in `startup.tjs` is NOT fatal — the game
/// continues into its timer event loop (WAVE3 SA-5), so it is logged only.
fn game_startup(
    config: Res<GameConfig>,
    shared: Res<SharedScene>,
    monitors: Query<&Monitor, With<PrimaryMonitor>>,
    mut commands: Commands,
) {
    // 1. Mount storage (game dir + xp3 archives) and bootstrap the VM.
    let game_dir = config.game_dir.display().to_string();
    let (storage, engine) = match engine::loader::prepare(&game_dir) {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("krkr-rs: cannot load game {game_dir:?}: {e}");
            std::process::exit(1);
        }
    };

    // 2. Ensure the save-data directory exists before scripts enumerate or
    // copy save slots.
    if let Err(e) = std::fs::create_dir_all(config.game_dir.join("savedata")) {
        log::warn!("cannot create savedata directory: {e}");
    }

    // 3. Point the `System.*` property getters at the mounted game.
    let (desktop_origin, desktop_size) = monitors
        .single()
        .map(|monitor| {
            (
                (monitor.physical_position.x, monitor.physical_position.y),
                (monitor.physical_width, monitor.physical_height),
            )
        })
        .unwrap_or(((0, 0), GAME_SIZE));
    let system_context = tvp_natives::SystemContext {
        project_dir: config.game_dir.clone(),
        app_data_dir: std::env::temp_dir(),
        screen_size: GAME_SIZE,
        desktop_origin,
        desktop_size,
        touch_device: false,
    };
    tvp_natives::set_system_context(system_context.clone());
    // Keep the installed context as a resource so the render bridge can keep
    // `System.screenWidth`/`screenHeight` current as the game resizes its
    // window (see `sync_host_window_resolution`).
    commands.insert_resource(SystemContextState(system_context));

    // 4. Register the native classes; the visual ones bind to our shared
    //    scene (natives mutate it under a write lock, sync_scene renders
    //    it under a read lock).
    register_natives(&engine, &storage, &shared).unwrap_or_else(|e| {
        log::error!("krkr-rs: native registration failed: {e}");
        std::process::exit(1);
    });

    // 4b. Install the explicit font configuration (if any) before
    //     `startup.tjs` creates its text layers. No config found keeps the
    //     out-of-the-box system discovery (see `load_font_config`).
    tvp_visual::set_font_config(load_font_config(&config));

    // 5. Run startup.tjs; a script error is non-fatal (log it and keep
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
            log::error!("krkr-rs: cannot run startup.tjs: {e}");
            std::process::exit(1);
        }
    }

    // 6. Hand the VM to the update loop; `now_ms` is measured from here.
    //    Open the audio output on the main thread (rodio/cpal; no device →
    //    logs and continues silently). The guard must be created and
    //    dropped on the same thread, so it lives in the `VmRuntime`
    //    resource which is inserted and dropped on the main thread.
    let audio_output = tvp_sound::set_sound_output_enabled(std::thread::current().id(), true)
        .map_err(|e| log::warn!("krkr-rs: audio output disabled: {e}"))
        .unwrap_or(None);
    commands.insert_resource(VmRuntime {
        engine,
        started: Instant::now(),
        audio_output,
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
    // `Mouse`/`Key` live in `tvp-input`; register them so scripts can reach
    // the shared input state (and so `Mouse.getCursorPos(obj)` has the engine
    // context it fills the object through).
    tvp_input::register_all(engine)?;
    tvp_kagparser::register_kagparser(engine)?;
    tvp_kagparser::set_context(Some(engine.clone()), Some(storage.clone()));
    tvp_storages::register_storages(engine)?;
    tvp_scripts::register_scripts(engine)?;
    tvp_visual::register_visual(engine, shared.0.clone(), storage.clone())?;
    // The sound natives: WaveSoundBuffer (the game's SoundBuffer derives
    // from it) + the fixture SoundBuffer/SoundChannel, backed by the
    // clock-driven mixer.
    tvp_sound::register_sound(engine, storage.clone())?;
    tvp_storages::set_storage(Some(storage.clone()));
    tvp_scripts::set_context(Some(engine.clone()), Some(storage.clone()));
    Ok(())
}

/// Drive the TJS2 VM once per frame: fire due timers (the game's event
/// loop). A panic inside a TJS callback must not kill the app — catch it,
/// log it, and keep the frame going.
/// Serialize the whole VM-driving system across Bevy worker threads.
///
/// The TJS2 VM is single-threaded and the natives share process-global
/// mutable state (sound `STREAMS`/mixer, timer registry, continuous
/// handlers). Bevy's parallel scheduler moves `run_vm` between worker
/// threads across frames while timer callbacks run on the main thread, so
/// without this lock the shared sound locks deadlock (a ctor on thread A
/// blocks on `STREAMS` held by `sound_poll` on thread B and vice versa).
static VM_RUN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run_vm(vm: Res<VmRuntime>) {
    // Hold the lock for the entire VM step so no other thread can touch the
    // shared native state mid-frame. One shot: if the lock is already held
    // by a re-entrant call on this thread, skip (the outer call completes
    // the step).
    let guard = VM_RUN_LOCK.try_lock();
    let _guard = match guard {
        Ok(g) => g,
        Err(std::sync::TryLockError::WouldBlock) => return,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
    };
    let now_ms = vm.started.elapsed().as_millis() as u64;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tvp_natives::async_trigger_poll(&vm.engine);
        tvp_visual::timer_poll(&vm.engine, now_ms);
        tvp_natives::continuous_handler_poll(&vm.engine);
        // Sound: advance the mixer and deliver onStatusChanged /
        // onFadeCompleted to live WaveSoundBuffer objects (a panic inside a
        // script handler is caught below like the other polls).
        tvp_sound::sound_poll(&vm.engine, now_ms as f64 / 1000.0);
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
        s.to_string()
    } else {
        "unknown panic payload".to_string()
    }
}

/// `krkr-rs run <game-dir> --headless`: drive ONE pass of the app's systems
/// manually (no window — this machine has no GPU adapter, so a window
/// cannot open), then dump the resulting [`Scene`] state to stdout and exit
/// 0. Proves the whole chain: real startup.tjs → natives → Scene populated
/// with the title-screen data.
fn run_headless(shared: SharedScene, game_dir: PathBuf, font_config: Option<PathBuf>) -> ! {
    let mut app = headless_game_app(shared.clone(), game_dir, font_config);
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
            "layer #{} win={} parent={} children={:?} rect=({},{},{}x{}) bitmap={} fill={} visible={} opacity={} z={} type={}",
            l.id,
            l.window,
            l.parent.map_or_else(|| "none".to_string(), |id| id.to_string()),
            l.children,
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
            l.z_order,
            l.blend_type
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

    /// The VM is process-global and single-threaded; serialize the render
    /// tests that register an engine (they would otherwise corrupt the C++
    /// heap when run in parallel). Use the *shared* `tvp_visual` lock rather
    /// than a bin-local one: the input-bridge tests in this same test binary
    /// also register into the global VM context, and two independent locks
    /// let a menu test and an input test run at once (observed as a sporadic
    /// SIGSEGV).
    fn menu_vm_lock() -> std::sync::MutexGuard<'static, ()> {
        tvp_visual::natives::vm_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

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

    /// Real-game headless integration test: the same pipeline the bin runs
    /// (`krkr-rs run <game-dir> --headless`), driven in-process — Startup
    /// (prepare + register natives + startup.tjs) then one Update
    /// (timer_poll + sync_scene).
    ///
    /// Requires the real game at `/mnt/DATA/Games/Others/test` on this
    /// machine, which is NOT committed to the repo, so this test is
    /// `#[ignore]`d by default. Run it with:
    /// `cargo test -p render -- --ignored real_game_headless_populates_scene`
    /// or verify the same path manually via
    /// `cargo run -p render --bin krkr-rs -- run /mnt/DATA/Games/Others/test --headless`.
    #[test]
    #[ignore = "needs the real game at /mnt/DATA/Games/Others/test (not in the repo); use --ignored or the --headless manual run"]
    fn real_game_headless_populates_scene() {
        let game = PathBuf::from("/mnt/DATA/Games/Others/test");
        assert!(game.is_dir(), "real game dir must exist for this test");
        let shared = SharedScene(Arc::new(RwLock::new(Scene::default())));

        let mut app = headless_game_app(shared.clone(), game, None);
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

    /// Real-game headless timer-loop test: drive the KAG-style event loop
    /// (`run_vm`: async triggers + timers + continuous handlers) for a few
    /// wall-clock seconds and check the scene actually advances (the Logo
    /// scene constructed at startup closes itself after its keyframe
    /// animation and the SceneManager's 100 ms `onWaitSceneChange` timer
    /// switches to the Title scene). No update may panic, and the scene
    /// must change between the first and last dump.
    ///
    /// Requires the real game at `/mnt/DATA/Games/Others/test`; run with:
    /// `cargo test -p render -- --ignored real_game_timer_loop_advances_scene`
    #[test]
    #[ignore = "needs the real game at /mnt/DATA/Games/Others/test (not in the repo); drives real-time timers, so it is slow"]
    fn real_game_timer_loop_advances_scene() {
        let game = PathBuf::from("/mnt/DATA/Games/Others/test");
        assert!(game.is_dir(), "real game dir must exist for this test");
        let shared = SharedScene(Arc::new(RwLock::new(Scene::default())));

        let mut app = headless_game_app(shared.clone(), game, None);
        app.update(); // Startup (prepare + register + startup.tjs) + first Update

        let scene = shared.0.read().expect("shared scene lock poisoned");
        let baseline = dump_scene(&scene);
        let baseline_bitmaps: Vec<String> = scene
            .bitmaps
            .iter()
            .filter_map(|b| b.name.clone())
            .collect();
        drop(scene);

        // Drive ~18 s of wall-clock game time (the timers use the real
        // elapsed clock, so we must actually wait). The logo scene's
        // keyframe chain (OnceCall 1s + fades + OnceCall 3s/8s) takes
        // ~13s before the SceneManager switches to the Title scene.
        let mut last_dump = baseline.clone();
        for i in 0..180 {
            std::thread::sleep(Duration::from_millis(100));
            if i % 10 == 0 {
                eprintln!("[hb] tick {i}");
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.update()));
            assert!(result.is_ok(), "app.update() panicked at tick {i}");
            if i % 20 == 19 {
                let scene = shared.0.read().unwrap();
                println!("t={}s FULL DUMP:", (i + 1) / 10);
                println!("{}", dump_scene(&scene));
                let dump = dump_scene(&scene);
                if dump != last_dump {
                    last_dump = dump;
                }
            }
        }

        let scene = shared.0.read().unwrap();
        let final_dump = dump_scene(&scene);
        let final_bitmaps: Vec<String> = scene
            .bitmaps
            .iter()
            .filter_map(|b| b.name.clone())
            .collect();
        drop(scene);
        println!("final scene:\n{final_dump}");

        // The Logo scene must have closed itself and the manager switched
        // to the Title scene: the layer set (and typically the bitmap set)
        // must differ from the startup baseline.
        assert!(
            final_dump != baseline,
            "scene never changed over the timer loop (Logo never closed, \
             SceneManager.onWaitSceneChange never fired)"
        );
        // Sanity: the game's scene layer ids are stable, so a different
        // dump means a real transition, not churn.
        assert!(
            baseline_bitmaps.iter().any(|n| final_bitmaps.contains(n)),
            "bitmap set changed completely; unexpected teardown"
        );
    }

    /// The in-engine menu: a native `MenuItem` tree yields the expected Bevy
    /// UI nodes, opening a submenu adds its rows, and pressing a leaf runs the
    /// script `onClick` through `krkr_render::menu`'s activation path.
    #[test]
    fn menu_ui_renders_tree_and_activation_fires_callback() {
        use krkr_render::menu::MenuEntry;
        let _lock = menu_vm_lock();

        // 1. Build a native tree in the shared registry.
        let engine = Box::leak(Box::new(tjs2_sys::Tjs2Engine::new().unwrap()));
        tvp_natives::register_all(engine).unwrap();
        engine
            .exec_script(
                "var w = %[id: 9001];\
                 var root = __krkr_make_window_menu(w);\
                 var open = new MenuItem(null, 'Open'); open.shortcut = 'Ctrl+O';\
                 var recent = new MenuItem(null, 'Recent');\
                 var quit = new MenuItem(null, 'Quit'); quit.enabled = false;\
                 root.add(open); root.add(recent); root.add(quit);\
                 recent.add(new MenuItem(null, 'File A'));\
                 var fired = 0; open.onClick = function() { fired++; };",
                "menu-ui",
            )
            .unwrap();

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(krkr_render::menu::MenuPlugin);
        app.update();

        // 2. Top level only: Open, Recent, Quit.
        let entries: Vec<(Entity, MenuEntry)> = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &MenuEntry)>();
            query.iter(world).map(|(e, m)| (e, *m)).collect()
        };
        assert_eq!(
            entries.len(),
            3,
            "only top-level rows until a submenu opens"
        );
        let recent = entries
            .iter()
            .find(|(_, m)| m.has_children)
            .map(|(e, _)| *e)
            .expect("the Recent submenu is present");

        // 3. Opening Recent adds its child row (the open path is re-rendered).
        app.world_mut()
            .entity_mut(recent)
            .insert(bevy::prelude::Interaction::Pressed);
        app.update();
        let entries: Vec<(Entity, MenuEntry)> = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &MenuEntry)>();
            query.iter(world).map(|(e, m)| (e, *m)).collect()
        };
        assert_eq!(entries.len(), 4, "Recent contributes one dropdown row");

        // 4. Press the enabled leaf; its onClick must fire.
        let open = entries
            .iter()
            .find(|(_, m)| m.enabled && !m.has_children)
            .map(|(e, _)| *e)
            .expect("the Open leaf is present");
        app.world_mut()
            .entity_mut(open)
            .insert(bevy::prelude::Interaction::Pressed);
        app.update();
        assert_eq!(
            engine.eval("fired", "menu-ui").unwrap(),
            tjs2_sys::TjsValue::Integer(1),
            "pressing the row must run the script onClick"
        );
    }

    /// `Window.menu` attaches a native `MenuItem` root and registers it for
    /// the renderer; repeated reads return the same object.
    #[test]
    fn window_menu_getter_attaches_native_root() {
        let _lock = menu_vm_lock();
        let engine = Box::leak(Box::new(tjs2_sys::Tjs2Engine::new().unwrap()));
        tvp_natives::register_all(engine).unwrap();
        let scene = std::sync::Arc::new(std::sync::RwLock::new(Scene::default()));
        let storage = std::sync::Arc::new(std::sync::Mutex::new(
            engine::Storage::mount(std::env::temp_dir()).expect("mount temp dir"),
        ));
        tvp_visual::register_visual(engine, scene, storage).unwrap();
        engine
            .exec_script(
                "var w = new Window();\
                 var a = w.menu; var b = w.menu;\
                 a.add(new MenuItem(null, 'File'));",
                "window-menu",
            )
            .unwrap();
        assert_eq!(
            engine.eval("a === b", "window-menu").unwrap(),
            tjs2_sys::TjsValue::Integer(1),
            "Window.menu must have stable identity"
        );
        let id = match engine.eval("w.id", "window-menu").unwrap() {
            tjs2_sys::TjsValue::Integer(v) => v as u32,
            other => panic!("window id must be an integer, got {other:?}"),
        };
        let snapshot =
            tvp_natives::menu_snapshot(id).expect("Window.menu must register the root tree");
        assert_eq!(snapshot.root.children.len(), 1);
        assert_eq!(snapshot.root.children[0].caption, "File");

        // The setter replaces the root and re-registers it for the renderer.
        engine
            .exec_script(
                "var other = new MenuItem(null, null);\
                 other.add(new MenuItem(null, 'Edit'));\
                 w.menu = other;",
                "window-menu",
            )
            .unwrap();
        assert_eq!(
            engine.eval("w.menu === other", "window-menu").unwrap(),
            tjs2_sys::TjsValue::Integer(1),
            "Window.menu must return the assigned root"
        );
        let snapshot =
            tvp_natives::menu_snapshot(id).expect("setter must re-register the root tree");
        assert_eq!(snapshot.root.children.len(), 1);
        assert_eq!(snapshot.root.children[0].caption, "Edit");
    }
}
