//! Diagnostic harness for the logo→title ATTENTION transition (TODO 3.1).
//!
//! Replicates the `krkr-rs run --headless` pipeline (mount → register
//! natives → startup.tjs) but then drives the VM poll loop
//! (`async_trigger_poll` + `timer_poll` + `continuous_handler_poll` +
//! `sound_poll`) at ~60 Hz on this single thread — exactly what Bevy's
//! `run_vm` system does each frame — and probes game state through TJS
//! `eval` once per second.
//!
//! Usage: attention-harness <game-dir> [seconds] [--trace]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tvp_visual::scene::Scene;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let trace = args.iter().any(|a| a == "--trace");
    let game_dir = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_default();
    let seconds: u64 = args
        .iter()
        .filter_map(|a| a.parse::<u64>().ok())
        .next()
        .unwrap_or(75);
    engine::init_logging(true);

    let (storage, engine) = match engine::loader::prepare(&game_dir) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("prepare failed: {e}");
            std::process::exit(1);
        }
    };
    let _ = std::fs::create_dir_all(PathBuf::from(&game_dir).join("savedata"));

    tvp_natives::set_system_context(tvp_natives::SystemContext {
        project_dir: PathBuf::from(&game_dir),
        app_data_dir: std::env::temp_dir(),
        screen_size: (1280, 720),
        desktop_origin: (0, 0),
        desktop_size: (1280, 720),
        touch_device: false,
    });

    engine.set_data_dir(&storage.lock().unwrap().game_dir().display().to_string());
    let scene = Arc::new(RwLock::new(Scene::default()));
    register_all(&engine, &storage, &scene);

    // Open the audio output on this thread (like the real runner); no
    // device is fine — decode still works and statuses still advance.
    let audio_guard = tvp_sound::set_sound_output_enabled(std::thread::current().id(), true)
        .map_err(|e| eprintln!("audio output disabled: {e}"))
        .unwrap_or(None);
    let _ = audio_guard;

    match engine::loader::run_startup(&engine, &storage) {
        Ok(report) => {
            if let Some(err) = &report.startup_error {
                eprintln!("startup.tjs error (non-fatal): {err}");
            }
        }
        Err(e) => {
            eprintln!("startup.tjs failed: {e}");
            std::process::exit(1);
        }
    }

    let started = Instant::now();
    let frame = Duration::from_millis(16);
    let mut last_probe = 0u64;
    let mut last_progress = Instant::now();
    if trace {
        eprintln!("[trace] entering poll loop");
    }
    // Watchdog: if the loop stalls in one stage, say where.
    STUCK_STAGE.get_or_init(|| Arc::new(Mutex::new(String::from("init"))));
    {
        let reporter = STUCK_STAGE.get().unwrap().clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let s = reporter.lock().unwrap().clone();
                eprintln!("[watchdog] current/last stage: {s}");
            }
        });
    }

    while started.elapsed() < Duration::from_secs(seconds) {
        let now_ms = started.elapsed().as_millis() as u64;
        // Same four polls as render's run_vm, single-threaded here.
        for stage in [
            "async_trigger_poll",
            "timer_poll",
            "continuous_handler_poll",
            "sound_poll",
        ] {
            set_stage(stage);
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match stage {
                "async_trigger_poll" => tvp_natives::async_trigger_poll(&engine),
                "timer_poll" => tvp_visual::timer_poll(&engine, now_ms),
                "continuous_handler_poll" => {
                    tvp_natives::continuous_handler_poll(&engine);
                }
                _ => tvp_sound::sound_poll(&engine, now_ms as f64 / 1000.0),
            }));
            if r.is_err() {
                eprintln!("[trace] PANIC in {stage} at t={now_ms}ms");
            }
        }
        set_stage("probe/idle");

        if now_ms - last_probe >= 1000 {
            last_probe = now_ms;
            probe(&engine, now_ms);
        }
        let since = last_progress.elapsed();
        if since > Duration::from_secs(3) && trace {
            eprintln!("[trace] slow frame: {since:?}");
        }
        last_progress = Instant::now();
        std::thread::sleep(frame);
    }

    probe(&engine, started.elapsed().as_millis() as u64);
    println!("harness: done");
}

static STUCK_STAGE: std::sync::OnceLock<Arc<Mutex<String>>> = std::sync::OnceLock::new();

fn set_stage(s: &str) {
    if let Some(g) = STUCK_STAGE.get() {
        *g.lock().unwrap() = s.to_string();
    }
}

/// Register every native class in the same order as the runner.
fn register_all(
    engine: &Arc<tjs2_sys::Tjs2Engine>,
    storage: &Arc<Mutex<engine::Storage>>,
    scene: &Arc<RwLock<Scene>>,
) {
    tvp_natives::register_all(engine).expect("register base natives");
    tvp_kagparser::register_kagparser(engine).expect("register kagparser");
    tvp_kagparser::set_context(Some(engine.clone()), Some(storage.clone()));
    tvp_storages::register_storages(engine).expect("register storages");
    tvp_scripts::register_scripts(engine).expect("register scripts");
    tvp_visual::register_visual(engine, scene.clone(), storage.clone()).expect("register visual");
    tvp_sound::register_sound(engine, storage.clone()).expect("register sound");
    tvp_storages::set_storage(Some(storage.clone()));
    tvp_scripts::set_context(Some(engine.clone()), Some(storage.clone()));
}

/// One-second status probe, all through TJS eval so we need no native-side
/// instrumentation. Every expression tolerates absence (void/exception).
fn probe(engine: &Arc<tjs2_sys::Tjs2Engine>, now_ms: u64) {
    set_stage("probe");
    let ev = |expr: &str| -> String {
        match engine.eval(expr, "probe") {
            Ok(v) => format!("{v:?}"),
            Err(e) => format!("<{e}>"),
        }
    };
    println!(
        "t={:>5}s scene={} nextScene={} logoValid={} logoState={} attIndex={} titleValid={}",
        now_ms / 1000,
        ev("game._scene"),
        ev("game._nextScene"),
        ev(
            "(game.getScene(0) !== void && game.getScene(0) !== null ? game.getScene(0).valid : -1)"
        ),
        ev(
            "(game.getScene(0) !== void && game.getScene(0) !== null ? int game.getScene(0)._state : -1)"
        ),
        ev(
            "(game.getScene(0) !== void && game.getScene(0)._attentionVoice !== void ? game.getScene(0)._attentionVoice._index : -1)"
        ),
        ev(
            "(game.getScene(1) !== void && game.getScene(1) !== null ? game.getScene(1).valid : -1)"
        ),
    );
}
