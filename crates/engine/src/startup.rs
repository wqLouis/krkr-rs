//! Startup-script discovery (L2).
//!
//! Pure storage discovery, per `docs/migration/09-game-loading.md` §5. This
//! module locates the scripts but never executes them, so it is testable
//! without the TJS VM. The reference order is:
//!
//! 1. `patch.tjs` — app path only, no search;
//! 2. `startup.tjs` — searched across the storage (disk + mounted archives);
//! 3. `AfterStartup.tjs` — app path only, no search;
//! 4. `system/Initialize.tjs` — the reference compatibility fallback when
//!    `startup.tjs` is not found.
//!
//! Discovering none of `startup.tjs` / `system/Initialize.tjs` is a typed
//! error ([`StartupError::NoStartupScript`]), never a warn-and-continue
//! (D11).

use api::error::FsError;
use api::fs::FsPath;

use crate::storage::{Location, Storage, StorageReadError};

/// The resolved sequence of startup scripts.
///
/// `patch` and `after_startup` are resolved at the app path only;
/// `startup` is the searched bootstrap script (or the fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StartupScripts {
    /// `patch.tjs` next to the archives, when it exists (app path only).
    pub patch: Option<Location>,
    /// The bootstrap script the engine should execute.
    pub startup: StartupScript,
    /// `AfterStartup.tjs` next to the archives, when it exists (app path
    /// only).
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

    /// A bottom file-system operation failed while probing the app path.
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
/// * `patch.tjs` and `AfterStartup.tjs` are looked up at the app path
///   (`storage.game_dir()`) only.
/// * `startup.tjs` is searched across the storage (disk + mounted archives)
///   through [`Storage::find`].
/// * When `startup.tjs` is absent, `system/Initialize.tjs` is the fallback;
///   a miss on both is [`StartupError::NoStartupScript`].
pub fn discover_startup(storage: &Storage) -> Result<StartupScripts, StartupError> {
    let patch = app_path_file(storage, "patch.tjs")?;
    let after_startup = app_path_file(storage, "AfterStartup.tjs")?;

    let startup = match storage.find("startup.tjs")? {
        Some(location) => StartupScript::Startup(location),
        None => match storage.find("system/Initialize.tjs")? {
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

/// Resolves `name` at the app path exactly: no search and no archive lookup.
/// A missing (or directory) entry is `Ok(None)`.
fn app_path_file(storage: &Storage, name: &str) -> Result<Option<Location>, StartupError> {
    let candidate = storage.game_dir().join(name);
    match fs::stat(storage.file_system(), &FsPath::host(&candidate)) {
        Ok(meta) if !meta.is_dir => Ok(Some(Location::Disk(candidate))),
        Ok(_) => Ok(None),
        Err(FsError::NotFound { .. }) => Ok(None),
        Err(error) => Err(StartupError::Fs(error)),
    }
}
