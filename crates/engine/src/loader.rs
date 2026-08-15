//! The load pipeline: mount storage → bootstrap the TJS2 VM → run
//! `startup.tjs`.

use std::sync::OnceLock;

use tjs2_sys::{Tjs2Engine, TjsValue};

use crate::storage::Storage;

/// Name of the startup script, matching `TVPStartupScriptName` in the
/// reference engine.
pub const STARTUP_SCRIPT: &str = "startup.tjs";

/// Outcome of a load attempt.
#[derive(Debug)]
pub struct LoadReport {
    /// Game directory that was mounted.
    pub game_dir: String,
    /// How many `.xp3` archives were mounted.
    pub archives_mounted: usize,
    /// Where `startup.tjs` was found (if at all).
    pub startup_location: Option<String>,
    /// Result value of executing `startup.tjs` (usually Void).
    pub startup_result: Option<TjsValue>,
    /// Script-level error message if execution failed.
    pub startup_error: Option<String>,
}

/// Load a game: mount storage, find and execute `startup.tjs` in a fresh
/// TJS2 VM. Logs progress through `log`.
///
/// This is the "no rendering" milestone: archives are mounted and scripts
/// run, but the TVP native classes games call (`Storages`, `System`, ...)
/// are not registered yet, so real games will typically fail inside
/// `startup.tjs` with a clear "identifier not found" error until those
/// natives land.
pub fn load_game(game_dir: &str) -> Result<LoadReport, LoadError> {
    let mut storage = Storage::mount(game_dir).map_err(LoadError::Mount)?;

    // Bootstrap the VM.
    let engine = Tjs2Engine::new().map_err(|e| LoadError::Vm(e.to_string()))?;
    // SAFETY: null user pointer, no user data accessed.
    unsafe { engine.set_log_cb(Some(log_cb), std::ptr::null_mut()) };

    // Find startup.tjs.
    let startup_location = storage.find(STARTUP_SCRIPT);
    let mut report = LoadReport {
        game_dir: game_dir.to_string(),
        archives_mounted: storage.archives().count(),
        startup_location: startup_location.as_ref().map(|loc| format!("{loc:?}")),
        startup_result: None,
        startup_error: None,
    };

    match startup_location {
        Some(_) => {
            log::info!("found {STARTUP_SCRIPT}, executing...");
            match execute_storage_script(&engine, &mut storage, STARTUP_SCRIPT) {
                Ok(value) => {
                    report.startup_result = Some(value);
                    log::info!("startup.tjs executed successfully");
                }
                Err(e) => {
                    log::error!("startup.tjs failed: {e}");
                    report.startup_error = Some(e);
                }
            }
        }
        None => log::warn!("no {STARTUP_SCRIPT} found in game directory"),
    }

    Ok(report)
}

/// Read a script from storage and execute it in the engine.
pub fn execute_storage_script(
    engine: &Tjs2Engine,
    storage: &mut Storage,
    name: &str,
) -> Result<TjsValue, String> {
    let source = storage.read(name).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&source).into_owned();
    engine.exec_script(&text, name).map_err(|e| e.to_string())
}

/// Errors from the load pipeline.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot mount game storage: {0}")]
    Mount(#[from] crate::storage::MountError),
    #[error("cannot start TJS2 VM: {0}")]
    Vm(String),
}

/// Route tjs2 console output into `log`.
extern "C" fn log_cb(level: i32, msg: *const std::ffi::c_char, _user: *mut std::ffi::c_void) {
    use std::ffi::CStr;
    if msg.is_null() {
        return;
    }
    // SAFETY: msg is a valid NUL-terminated string for the duration of the call.
    let msg = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    match level {
        tjs2_sys::LOG_DEBUG => log::debug!("[tjs2] {msg}"),
        tjs2_sys::LOG_INFO => log::info!("[tjs2] {msg}"),
        tjs2_sys::LOG_WARN => log::warn!("[tjs2] {msg}"),
        _ => log::error!("[tjs2] {msg}"),
    }
}

/// `log` facade: default to a stderr logger that prints INFO by default;
/// consumers (the bin) can install their own logger.
static LOGGER: OnceLock<StderrLogger> = OnceLock::new();

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!("{}", record.args());
    }
    fn flush(&self) {}
}

/// Install the default stderr logger if none is set.
pub fn init_logging(verbose: bool) {
    let _ = log::set_logger(LOGGER.get_or_init(|| StderrLogger));
    log::set_max_level(if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
}
