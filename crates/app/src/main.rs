//! krkr-rs CLI — wires the TVP native classes into the engine and loads games.
//!
//!   krkr-rs load <game-dir>          register natives + run startup.tjs
//!   krkr-rs run <game-dir> <script>  register natives + run one script
//!   krkr-rs list <game-dir>          list mounted archives and their entries

use std::process::ExitCode;
use std::sync::{Arc, Mutex, RwLock};

use engine::storage::Storage;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_usage();
        return ExitCode::SUCCESS;
    }

    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    engine::init_logging(verbose);
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();

    let result = match positional.as_slice() {
        [cmd, game] if cmd.as_str() == "load" => run_load(game),
        [cmd, game, script] if cmd.as_str() == "run" => run_script(game, script),
        [cmd, game] if cmd.as_str() == "list" => run_list(game),
        _ => {
            print_usage();
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "krkr-rs — KiriKiri2 rewrite (load module)\n\
         \n\
         usage:\n\
         \x20 krkr-rs load <game-dir>          register natives + run startup.tjs\n\
         \x20 krkr-rs run <game-dir> <script>  register natives + run one script\n\
         \x20 krkr-rs list <game-dir>          list archives and entries\n\
         \n\
         options:\n\
         \x20 -v, --verbose  debug logging"
    );
}

/// Full load path: mount storage, bootstrap the VM, register the TVP native
/// classes (System/Debug/Window, Storages, Scripts), point their contexts at
/// this engine + storage, then find and run `startup.tjs`.
fn run_load(game_dir: &str) -> Result<(), String> {
    let (storage, engine) = engine::loader::prepare(game_dir).map_err(|e| e.to_string())?;
    register_natives(&engine, &storage)?;
    let report = engine::loader::run_startup(&engine, &storage).map_err(|e| e.to_string())?;
    println!(
        "loaded {}: {} archive(s), startup.tjs at {}",
        report.game_dir,
        report.archives_mounted,
        report.startup_location.as_deref().unwrap_or("<not found>"),
    );
    if let Some(err) = &report.startup_error {
        println!("startup.tjs error: {err}");
    }
    Ok(())
}

fn run_script(game_dir: &str, script: &str) -> Result<(), String> {
    let (storage, engine) = engine::loader::prepare(game_dir).map_err(|e| e.to_string())?;
    register_natives(&engine, &storage)?;
    match engine::loader::execute_storage_script(&engine, &storage, script) {
        Ok(value) => {
            println!("{script} -> {value:?}");
            Ok(())
        }
        Err(e) => Err(format!("{script}: {e}")),
    }
}

fn run_list(game_dir: &str) -> Result<(), String> {
    let storage = Storage::mount(game_dir).map_err(|e| e.to_string())?;
    for (path, arc) in storage.archives() {
        println!("{}  ({} entries)", path.display(), arc.len());
        for entry in arc.entries().take(20) {
            println!(
                "    {:>12}  {:<40}  {} seg(s)",
                entry.org_size,
                entry.name,
                entry.segments.len()
            );
        }
        if arc.len() > 20 {
            println!("    ... {} more", arc.len() - 20);
        }
    }
    Ok(())
}

/// Register every TVP native class and set the global contexts they read.
/// Mirrors `crates/render/src/main.rs::register_natives` so `krkr-cli load`
/// exercises the same class surface as the graphical runner (`KAGParser`,
/// visual `Window`/`Layer`/`Bitmap`/`Font`/`Timer`, sound `WaveSoundBuffer`).
fn register_natives(
    engine: &Arc<tjs2_sys::Tjs2Engine>,
    storage: &Arc<Mutex<Storage>>,
) -> Result<(), String> {
    // Point the System property getters at the mounted game.
    let game_dir = storage.lock().unwrap().game_dir().to_path_buf();
    let _ = std::fs::create_dir_all(game_dir.join("savedata"));
    tvp_natives::set_system_context(tvp_natives::SystemContext {
        project_dir: game_dir,
        app_data_dir: std::env::temp_dir(), // platform data dir (headless for now)
        screen_size: (640, 480),            // virtual screen the game sees
        desktop_origin: (0, 0),
        desktop_size: (640, 480),
        touch_device: false,
    });
    engine
        .as_ref()
        .set_data_dir(&storage.lock().unwrap().game_dir().display().to_string());
    tvp_natives::register_all(engine)?;
    // `Mouse`/`Key` (the port compatibility input surface) live in
    // `tvp-input`; register them so `load`/`run` see the same class surface
    // as the graphical runner.
    tvp_input::register_all(engine)?;
    tvp_kagparser::register_kagparser(engine)?;
    tvp_kagparser::set_context(Some(engine.clone()), Some(storage.clone()));
    tvp_storages::register_storages(engine)?;
    tvp_scripts::register_scripts(engine)?;
    // The visual natives only need a scene to mutate; the CLI never renders
    // it, but registering them keeps `startup.tjs` from throwing on
    // `Window`/`Layer`/`Bitmap` construction.
    let scene = Arc::new(RwLock::new(tvp_visual::scene::Scene::default()));
    tvp_visual::register_visual(engine, scene, storage.clone())?;
    tvp_sound::register_sound(engine, storage.clone())?;
    tvp_storages::set_storage(Some(storage.clone()));
    tvp_scripts::set_context(Some(engine.clone()), Some(storage.clone()));
    Ok(())
}
