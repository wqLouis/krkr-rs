//! Startup-script discovery (L2).
//!
//! Pure storage discovery. This
//! module locates the scripts but never executes them, so it is testable
//! without the TJS VM. The order is:
//!
//! 1. `patch.tjs` — project directory only, no search;
//! 2. `startup.tjs` — searched across the storage (disk + mounted archives);
//! 3. `AfterStartup.tjs` — project directory only, no search;
//! 4. `system/Initialize.tjs` — the compatibility fallback when
//!    `startup.tjs` is not found.
//!
//! Discovering none of `startup.tjs` / `system/Initialize.tjs` is a typed
//! error ([`StartupError::NoStartupScript`]), never a warn-and-continue
//!.

use api::error::FsError;
use api::fs::FsPath;

use crate::storage::{
    AFTER_STARTUP_SCRIPT, INITIALIZE_SCRIPT, Location, PATCH_SCRIPT, STARTUP_SCRIPT, Storage,
    StorageReadError,
};

/// The resolved sequence of startup scripts.
///
/// `patch` and `after_startup` are resolved at the project directory only;
/// `startup` is the searched bootstrap script (or the fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StartupScripts {
    /// `patch.tjs` next to the archives, when it exists (project directory only).
    pub patch: Option<Location>,
    /// The bootstrap script the engine should execute.
    pub startup: StartupScript,
    /// `AfterStartup.tjs` next to the archives, when it exists (project
    /// directory only).
    pub after_startup: Option<Location>,
}

/// The bootstrap script selected by startup discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StartupScript {
    /// `startup.tjs`, located by searching the disk and mounted archives.
    Startup(Location),
    /// `system/Initialize.tjs`, used when `startup.tjs` is absent.
    Initialize(Location),
}

impl StartupScript {
    /// The resolved location of the bootstrap script.
    pub fn location(&self) -> &Location {
        match self {
            StartupScript::Startup(location) | StartupScript::Initialize(location) => location,
        }
    }

    /// Whether this is the `system/Initialize.tjs` compatibility fallback.
    pub fn is_initialize_fallback(&self) -> bool {
        matches!(self, StartupScript::Initialize(_))
    }
}

/// Errors from startup discovery.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StartupError {
    /// Neither `startup.tjs` nor `system/Initialize.tjs` exists.
    #[error("no startup script: neither startup.tjs nor system/Initialize.tjs was found")]
    NoStartupScript,

    /// A bottom file-system operation failed while probing the project
    /// directory.
    #[error(transparent)]
    Fs(#[from] FsError),

    /// Storage resolution failed while searching for the bootstrap script
    /// (for example a permission or I/O error). A plain miss is
    /// [`StartupError::NoStartupScript`], never this variant.
    #[error(transparent)]
    Storage(#[from] StorageReadError),
}

/// Discovers the startup scripts for `storage` without executing anything.
///
/// * `patch.tjs` and `AfterStartup.tjs` are looked up at the project
///   directory (`storage.app_dir()`) only.
/// * `startup.tjs` is searched across the storage (disk + mounted archives)
///   through [`Storage::find`].
/// * When `startup.tjs` is absent, `system/Initialize.tjs` is the fallback;
///   a miss on both is [`StartupError::NoStartupScript`].
///
/// # Errors
///
/// [`StartupError::NoStartupScript`] when neither `startup.tjs` nor
/// `system/Initialize.tjs` exists, [`StartupError::Fs`] when the
/// project-directory probe fails, and [`StartupError::Storage`] when a
/// storage lookup fails. A plain miss on the project directory is not an
/// error: the optional scripts are reported as absent.
pub fn discover_startup(storage: &Storage) -> Result<StartupScripts, StartupError> {
    let patch = app_path_file(storage, PATCH_SCRIPT)?;
    let after_startup = app_path_file(storage, AFTER_STARTUP_SCRIPT)?;

    let startup = match storage.find(STARTUP_SCRIPT)? {
        Some(location) => StartupScript::Startup(location),
        None => match storage.find(INITIALIZE_SCRIPT)? {
            Some(location) => StartupScript::Initialize(location),
            None => return Err(StartupError::NoStartupScript),
        },
    };

    Ok(StartupScripts {
        patch,
        startup,
        after_startup,
    })
}

/// Resolves `name` at the project directory exactly: no search and no archive
/// lookup. A missing (or directory) entry is `Ok(None)`.
fn app_path_file(storage: &Storage, name: &str) -> Result<Option<Location>, StartupError> {
    let candidate = storage.app_dir().join(name);
    match fs::stat(storage.file_system(), &FsPath::host(&candidate)) {
        Ok(meta) if !meta.is_dir => Ok(Some(Location::Disk(candidate))),
        Ok(_) => Ok(None),
        Err(FsError::NotFound { .. }) => Ok(None),
        Err(error) => Err(StartupError::Fs(error)),
    }
}
