//! `krkr-cli` — headless KiriKiri2/TVP CLI (no window, no GPU).
//!
//!   krkr-cli load <game-dir>            register natives + run startup.tjs
//!   krkr-cli run <game-dir> <script>    register natives + run one script
//!   krkr-cli list <game-dir>            list mounted archives and their entries
//!   krkr-cli extract <game-dir> <name>  write a storage entry (disk or archive) to stdout
//!
//! This is the headless counterpart to the graphical `krkr-rs` runner
//! (`crates/render`): it wires up the same native class surface but never
//! creates a Bevy app, so it is useful for CI, debugging, and inspecting or
//! extracting game files (mount archives + read entries).

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
        [cmd, game, name] if cmd.as_str() == "extract" => run_extract(game, name),
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
        "krkr-cli — headless KiriKiri2/TVP CLI (the graphical runner is `krkr-rs`)\n\
         \n\
         usage:\n\
         \x20 krkr-cli load <game-dir>            register natives + run startup.tjs\n\
         \x20 krkr-cli run <game-dir> <script>    register natives + run one script\n\
         \x20 krkr-cli list <game-dir>            list archives and entries\n\
         \x20 krkr-cli extract <game-dir> <name>  write a storage entry to stdout\n\
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

/// Write a storage entry (a disk file or an XP3 archive member) to stdout.
///
/// Names are resolved like the engine: the raw name first, then the
/// normalized (lowercased, `/`-separated) form, so both `System/AffineLayer.tjs`
/// and `system/affinelayer.tjs` work. This replaces the old Python extraction
/// helper for most inspection jobs.
fn run_extract(game_dir: &str, name: &str) -> Result<(), String> {
    let mut storage = Storage::mount(game_dir).map_err(|e| e.to_string())?;
    let bytes = storage.read(name).map_err(|e| e.to_string())?;
    use std::io::Write;
    std::io::stdout()
        .write_all(&bytes)
        .map_err(|e| e.to_string())
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
