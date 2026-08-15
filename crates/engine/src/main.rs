//! krkr-rs CLI — load module milestone.
//!
//!   krkr-rs load <game-dir>          mount storage + run startup.tjs
//!   krkr-rs run <game-dir> <script>  mount storage + run one script
//!   krkr-rs list <game-dir>          list mounted archives and their entries

use std::process::ExitCode;

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
         \x20 krkr-rs load <game-dir>          mount storage + run startup.tjs\n\
         \x20 krkr-rs run <game-dir> <script>  mount storage + run one script\n\
         \x20 krkr-rs list <game-dir>          list archives and entries\n\
         \n\
         options:\n\
         \x20 -v, --verbose  debug logging\n\
         \n\
         the C++ TJS2 VM is compiled by build.rs (zig + bison, no cmake) and\n\
         statically linked; see crates/tjs2-sys/build.rs"
    );
}

fn run_load(game_dir: &str) -> Result<(), String> {
    let report = engine::load_game(game_dir).map_err(|e| e.to_string())?;
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
    let mut storage = engine::Storage::mount(game_dir).map_err(|e| e.to_string())?;
    let engine = tjs2_sys::Tjs2Engine::new().map_err(|e| e.to_string())?;
    match engine::loader::execute_storage_script(&engine, &mut storage, script) {
        Ok(value) => {
            println!("{script} -> {value:?}");
            Ok(())
        }
        Err(e) => Err(format!("{script}: {e}")),
    }
}

fn run_list(game_dir: &str) -> Result<(), String> {
    let storage = engine::Storage::mount(game_dir).map_err(|e| e.to_string())?;
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
