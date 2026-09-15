//! Game runner (`krkr-rs run <game-dir>`): mount the game, register the TVP
//! natives, run `startup.tjs`, and drive the VM → scene-sync pipeline in Bevy.
//!
//! This lives in the `krkr_render` library (not the `krkr-rs` binary) so a
//! second frontend — the Android `cdylib` — can build and run the identical
//! Bevy app via [`game_app`] and [`run_game`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use bevy::app::{App, AppExit};
use bevy::asset::Assets;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::prelude::{
    Commands, DefaultPlugins, MessageWriter, MinimalPlugins, PluginGroup, Query, Res, Resource,
    Startup, Update, With,
};
use bevy::window::{Monitor, PrimaryMonitor, Window, WindowPlugin};
use engine::loader::LoadReport;
use tvp_visual::scene::Scene;

use crate::blend::LayerBlendPlugin;
use crate::input_bridge;
use crate::sync::{
    BitmapAssets, FrameBlendMaterials, GpuPrimitives, HostWindowResolution, SharedScene,
    SystemContextState, WindowRedrawRequested, sync_host_window_resolution, sync_scene,
};

/// Game logical window size (1280x720, the title screen's native size).
const GAME_SIZE: (u32, u32) = (1280, 720);

// ---------------------------------------------------------------------------
// Game runner (`krkr-rs run <game-dir>`)
// ---------------------------------------------------------------------------

/// `krkr-rs run <game-dir>` configuration (windowed or headless).
///
/// A frontend other than the `krkr-rs` binary (e.g. the Android `cdylib`) can
/// build this itself and pass it to [`game_app`], or simply call
/// [`run_game`].
#[derive(Resource)]
pub struct GameConfig {
    pub game_dir: PathBuf,
    /// CLI `--font-config <path>` override (highest precedence; see
    /// [`load_font_config`]).
    pub font_config: Option<PathBuf>,
}

/// The running TJS2 VM + its start [`Instant`], inserted by [`game_startup`].
/// [`run_vm`] polls it every frame for `timer_poll`'s `now_ms`.
#[derive(Resource)]
pub(crate) struct VmRuntime {
    pub(crate) engine: Arc<tjs2_sys::Tjs2Engine>,
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
pub fn run_game(game_dir: &std::path::Path, font_config: Option<PathBuf>, headless: bool) {
    let shared = SharedScene(Arc::new(RwLock::new(Scene::default())));
    if headless {
        run_headless(shared, game_dir.to_path_buf(), font_config.clone());
    }
    println!("krkr-rs: running {game_dir:?} (close the window to exit)");
    // Returns when the app exits (window closed, `System.exit`, or an exit
    // requested by a script). `run_app` also records a non-panic engine
    // failure in the state file.
    run_app(game_app(shared, game_dir.to_path_buf(), font_config));
}

/// Run a game app and record its terminal state.
///
/// Besides driving [`App::run`], this distinguishes a deliberate game exit
/// from an engine failure so the state file does not hide the latter.
///
/// Renderer *initialization* failures (no GPU adapter, surface creation) panic
/// inside Bevy (`bevy_render`'s `initialize_renderer` uses `expect`), and the
/// panic hook already records those. But Bevy also exits with
/// [`AppExit::Error`] **without** panicking: its render-error handler quits on
/// a wgpu validation/device-lost error, and pipelined rendering reports a dead
/// render thread that way. `System.exit(n)` from the game is likewise mapped to
/// `AppExit::Error(n)` by [`poll_game_exit`], so that one is explicitly exempt:
/// only an error the game did **not** ask for is treated as an engine failure.
/// `write_state` is a no-op off Android, so the desktop runner only pays the
/// log line.
pub fn run_app(mut app: App) -> AppExit {
    let exit = app.run();
    match exit {
        AppExit::Error(code) if !game_exit_requested() => {
            let message = format!(
                "engine exited with code {code} before the game finished \
                 (renderer or engine failure)"
            );
            log::error!("krkr-rs: {message}");
            engine::state::write_state(engine::state::State::Failed, Some(&message));
        }
        // Success, and the game's own (possibly non-zero) `System.exit`, are
        // clean shutdowns: the next launch must not look like a crash.
        _ => engine::state::write_state(engine::state::State::Stopped, None),
    }
    exit
}

/// The directory scripts see as `System.appDataPath`.
///
/// Android's [`std::env::temp_dir`] is `/data/local/tmp`, which an ordinary
/// app cannot write, so handing it to scripts makes `System.appDataPath` fail
/// with `EACCES`. On Android use the engine's state directory — the app's
/// `filesDir`, already configured by the launcher and writable — and create it
/// if it is missing. Everywhere else keep `temp_dir()`. If the directory
/// cannot be created we warn and fall back rather than expose a path that
/// cannot exist.
fn app_data_dir() -> PathBuf {
    // A runtime `cfg!` rather than `#[cfg]` so both branches are compiled (and
    // linted/checked on the host); the fallback is only reachable on Android.
    if cfg!(target_os = "android") {
        match engine::state::state_dir() {
            Some(dir) => match std::fs::create_dir_all(dir) {
                Ok(()) => return dir.to_path_buf(),
                Err(e) => log::warn!(
                    "krkr-rs: cannot create app data dir {}: {e}; \
                     falling back to {}",
                    dir.display(),
                    std::env::temp_dir().display()
                ),
            },
            None => log::warn!(
                "krkr-rs: no state directory configured; \
                 falling back to {} for app data",
                std::env::temp_dir().display()
            ),
        }
    }
    std::env::temp_dir()
}

/// The windowed game app: default plugins (window + renderer), the shared
/// scene, and the game pipeline. The window is 1280x720 "krkr-rs"; closing
/// it exits (Bevy's default `ExitCondition::OnAllClosed`; the app has a
/// single primary window, so closing it fires the exit).
///
/// Public so a different frontend (the Android `cdylib`) can construct and
/// run the identical app.
pub fn game_app(shared: SharedScene, game_dir: PathBuf, font_config: Option<PathBuf>) -> App {
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
        .add_plugins(crate::menu::MenuPlugin)
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

/// Set when the game itself asks to exit (`System.exit` / `System.terminate`).
///
/// [`poll_game_exit`] maps those to [`AppExit`], including the non-zero
/// `AppExit::Error` for `System.exit(n)`. [`run_app`] uses this to tell that
/// deliberate shutdown apart from an engine/renderer failure that also exits
/// through `AppExit::Error`.
static GAME_EXIT_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether the app exited because the game asked it to rather than because of
/// an engine failure (see [`run_app`]).
fn game_exit_requested() -> bool {
    GAME_EXIT_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
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
    // Mark this as a game-requested exit before writing it, so `run_app` does
    // not mistake the (possibly non-zero) code for an engine failure.
    GAME_EXIT_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
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
        .add_plugins(crate::menu::MenuPlugin)
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
            let message = format!("cannot load game {game_dir:?}: {e}");
            log::error!("krkr-rs: {message}");
            // Report the real reason to the launcher, then leave: a black
            // surface with a live process is worse than a clear failure.
            engine::state::write_state(engine::state::State::Failed, Some(&message));
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
        app_data_dir: app_data_dir(),
        screen_size: GAME_SIZE,
        desktop_origin,
        desktop_size,
        // The input bridge already maps touch to mouse; this only corrects what
        // the game is *told* so its `System.touchDevice` branch matches the
        // device. Phones/tablets report true, desktops false.
        touch_device: cfg!(target_os = "android"),
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
        let message = format!("native registration failed: {e}");
        log::error!("krkr-rs: {message}");
        engine::state::write_state(engine::state::State::Failed, Some(&message));
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
            let message = format!("cannot run startup.tjs: {e}");
            log::error!("krkr-rs: {message}");
            engine::state::write_state(engine::state::State::Failed, Some(&message));
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

    // The VM is mounted and running. A crash from here on leaves `running`
    // behind, which is how the launcher distinguishes it from a clean exit
    // (which writes `stopped`).
    engine::state::write_state(engine::state::State::Running, None);
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
    // Dev hook: run `$KRKR_EVAL` once, `$KRKR_EVAL_AT_MS` ms into the game
    // loop (default 3000), so a headless script can drive the *running* game
    // (scene changes, save/load, transitions) that `krkr-cli load` cannot
    // reach because it never ticks. Debug aid only.
    if let Ok(script) = std::env::var("KRKR_EVAL") {
        let at = std::env::var("KRKR_EVAL_AT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(3000);
        static EVALED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        // Optional repeat interval: with `KRKR_EVAL_EVERY_MS` set the script
        // runs every N ms (after `at`) instead of once, so a single headless
        // run can drive a sequence of scene changes.
        let every = std::env::var("KRKR_EVAL_EVERY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok());
        static LAST_EVAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let due = match every {
            Some(every) => {
                now_ms >= at
                    && now_ms.saturating_sub(LAST_EVAL.load(std::sync::atomic::Ordering::SeqCst))
                        >= every
            }
            None => now_ms >= at && !EVALED.load(std::sync::atomic::Ordering::SeqCst),
        };
        if due {
            if every.is_none() {
                EVALED.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            LAST_EVAL.store(now_ms, std::sync::atomic::Ordering::SeqCst);
            match vm.engine.eval_retained(&script, "krkr-eval") {
                Ok(_) => log::info!("KRKR_EVAL -> ok"),
                // `eval` compiles an **expression**; a multi-statement probe
                // (the usual shape for instrumentation) is a syntax error
                // there, so fall back to running it as a script.
                Err(eval_err) => match vm.engine.exec_script_retained(&script, "krkr-eval") {
                    Ok(_) => log::info!("KRKR_EVAL -> ok (exec)"),
                    Err(exec_err) => {
                        log::error!("KRKR_EVAL !! eval: {eval_err} / exec: {exec_err}")
                    }
                },
            }
        }
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tvp_natives::async_trigger_poll(&vm.engine);
        tvp_visual::timer_poll(&vm.engine, now_ms);
        tvp_natives::continuous_handler_poll(&vm.engine);
        // Idle compaction — `SystemControl.cpp:173-179`: with no continuous
        // handlers registered and more than 4 s since the last compaction,
        // the reference delivers a compact event at the idle level, whose
        // hook runs `tTJS::DoGarbageCollection` (`ScriptMgnIntf.cpp:376-388`).
        // TJS2 reclaims reference **cycles** only in the GC, and the scene
        // graph is full of them: every layer's `ActionOwner` strongly retains
        // its window while the window's script fields reference the layers.
        // Without this, a destroyed scene's layers — and their images, sounds
        // and videos — are never collected, which is why the previous scene's
        // assets stayed resident after loading a save. The reference also
        // rehashes TJS objects every 1.5 s idle; that is a performance hint
        // only and is not reproduced here.
        static LAST_IDLE_LOG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if now_ms.saturating_sub(LAST_IDLE_LOG.load(std::sync::atomic::Ordering::SeqCst)) > 4000 {
            LAST_IDLE_LOG.store(now_ms, std::sync::atomic::Ordering::SeqCst);
            log::info!(
                "idle check at {now_ms} ms: continuous_handlers_active={}",
                tvp_natives::continuous_handlers_active()
            );
        }
        if !tvp_natives::continuous_handlers_active()
            && now_ms.saturating_sub(LAST_COMPACT.load(std::sync::atomic::Ordering::SeqCst))
                > IDLE_COMPACT_MS
        {
            LAST_COMPACT.store(now_ms, std::sync::atomic::Ordering::SeqCst);
            log::info!("idle: running the TJS garbage collector at {now_ms} ms");
            if let Err(e) = vm.engine.do_gc() {
                log::warn!("idle garbage collection failed: {e}");
            }
        }
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

/// Idle window before the TJS garbage collector runs, matching the
/// reference's `tick - LastCompactedTick > 4000` check
/// (`environ/impl/SystemControl.cpp:173`).
const IDLE_COMPACT_MS: u64 = 4000;

/// Tick of the last idle compaction (see [`IDLE_COMPACT_MS`]).
static LAST_COMPACT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use bevy::ecs::prelude::Entity;

    /// The VM is process-global and single-threaded; serialize the render
    /// tests that register an engine (they would otherwise corrupt the C++
    /// heap when run in parallel). Use the *shared* `tvp_visual` lock rather
    /// than a module-local one: the input-bridge tests in this same test
    /// binary also register into the global VM context, and two independent
    /// locks let a menu test and an input test run at once (observed as a
    /// sporadic SIGSEGV).
    fn menu_vm_lock() -> std::sync::MutexGuard<'static, ()> {
        tvp_visual::natives::vm_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
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
    /// script `onClick` through `crate::menu`'s activation path.
    #[test]
    fn menu_ui_renders_tree_and_activation_fires_callback() {
        use crate::menu::MenuEntry;
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
            .add_plugins(crate::menu::MenuPlugin);
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

        // Cleanup: the native menu registry is process-global. Hide this
        // test's rows so a later test in this binary does not render them.
        engine
            .exec_script(
                "root.children[0].visible = false; root.children[1].visible = false; root.children[2].visible = false;",
                "menu-ui-cleanup",
            )
            .unwrap();
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

        // Cleanup: hide the tree so it cannot leak into another test's bar.
        engine
            .exec_script("other.children[0].visible = false;", "window-menu-cleanup")
            .unwrap();
    }
}
