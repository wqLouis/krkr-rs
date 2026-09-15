//! The load pipeline: mount storage → bootstrap the TJS2 VM → run
//! `startup.tjs`.

use std::sync::{Arc, Mutex, OnceLock};

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

/// Mount storage and bootstrap a fresh TJS2 VM. The caller (e.g. the app
/// crate) may register native classes and set native contexts between
/// [`prepare`] and [`run_startup`].
///
/// This is the "no rendering" milestone: archives are mounted and scripts can
/// run, but the TVP native classes games call (`Storages`, `System`, ...) are
/// only registered by the caller.
pub fn prepare(game_dir: &str) -> Result<(Arc<Mutex<Storage>>, Arc<Tjs2Engine>), LoadError> {
    let storage = Storage::mount(game_dir).map_err(LoadError::Mount)?;
    let storage = Arc::new(Mutex::new(storage));

    // Bootstrap the VM.
    let engine = Arc::new(Tjs2Engine::new().map_err(|e| LoadError::Vm(e.to_string()))?);
    // SAFETY: null user pointer, no user data accessed.
    unsafe { engine.set_log_cb(Some(log_cb), std::ptr::null_mut()) };
    Ok((storage, engine))
}

/// Load a game (no native classes registered): mount storage, find and
/// execute `startup.tjs`, in a fresh TJS2 VM. Native-less; used by tests and
/// the pure load path. Logs progress through `log`.
pub fn load_game(game_dir: &str) -> Result<LoadReport, LoadError> {
    let (storage, engine) = prepare(game_dir)?;
    run_startup(&engine, &storage)
}

/// Find and execute `startup.tjs` in the given engine/storage.
pub fn run_startup(
    engine: &Arc<Tjs2Engine>,
    storage: &Arc<Mutex<Storage>>,
) -> Result<LoadReport, LoadError> {
    // Find startup.tjs.
    let startup_location = storage
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .find(STARTUP_SCRIPT);
    let game_dir = storage
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .game_dir()
        .display()
        .to_string();
    let archives_mounted = storage
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .archive_count();
    let mut report = LoadReport {
        game_dir,
        archives_mounted,
        startup_location: startup_location.as_ref().map(|loc| format!("{loc:?}")),
        startup_result: None,
        startup_error: None,
    };

    match startup_location {
        Some(_) => {
            // Execute the optional root `patch.tjs` *before* `startup.tjs`
            // (reference `TVPExecuteStartupScript`, `ScriptMgnIntf.cpp:903`: it
            // runs `TVPGetAppPath() + "patch.tjs"` when it exists). Games use
            // it for `@set(...)` preprocessor flags — e.g. the KR game's
            // `@set(kirikiriz=1)`, which must be set before the engine
            // compiles the game's later system scripts (`@if(!kirikiriz) ...`).
            // `@set` persists per-engine via `tTJS::SetPPValue`.
            let has_patch = storage
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .game_dir()
                .join("patch.tjs")
                .is_file();
            if has_patch {
                log::info!("found patch.tjs, executing...");
                match execute_storage_script(engine, storage, "patch.tjs") {
                    Ok(_) => log::info!("patch.tjs executed successfully"),
                    // The reference records a patch error but still runs
                    // `startup.tjs`; do the same.
                    Err(e) => log::warn!("patch.tjs failed: {e}"),
                }
            }
            log::info!("found {STARTUP_SCRIPT}, executing...");
            match execute_storage_script(engine, storage, STARTUP_SCRIPT) {
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

/// Read a script from storage and execute it in the engine. The storage lock
/// is held only for the read, never across VM execution (natives may re-enter
/// the VM and lock storage again).
pub fn execute_storage_script(
    engine: &Tjs2Engine,
    storage: &Arc<Mutex<Storage>>,
    name: &str,
) -> Result<TjsValue, String> {
    let source = {
        let mut guard = storage.lock().unwrap_or_else(|p| p.into_inner());
        guard.read(name).map_err(|e| e.to_string())?
    };
    let text = tvp_streams::text::decode_bytes(&source).map_err(|e| format!("{name}: {e}"))?;
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
