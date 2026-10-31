//! Resolution of the fixtures that ship with the checkout cloned beside this
//! repository.
//!
//! The checkout is a separate clone. Its fixture files live under
//! `tests/test_files` in the checkout root; [`REFERENCE_ENV`] names that root,
//! and the gate reads its source scans through the same variable.

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Once;

/// The environment variable that names the checkout root.
///
/// It is the variable `krkr-gate` reads; the gate's checkout option sets it
/// for the checks it runs.
pub const REFERENCE_ENV: &str = "KRKR_REFERENCE";

/// The fixture directory inside the checkout root.
const TEST_FILES: &str = "tests/test_files";

/// The checkout directory name under the repository root.
const DEFAULT_DIR: &str = "reference";

/// A fixture below the checkout could not be resolved.
///
/// The [`std::fmt::Display`] text names the fixture, the root that was tried,
/// [`REFERENCE_ENV`] and the gate's checkout option.
#[derive(Debug)]
pub struct ReferenceFixtureError {
    message: String,
}

impl fmt::Display for ReferenceFixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ReferenceFixtureError {}

/// Resolves a path below the checkout's `tests/test_files`.
///
/// The checkout root is [`REFERENCE_ENV`] when it is set and non-empty,
/// otherwise the conventional checkout directory under `<repo>`, where
/// `<repo>` is the directory above this crate that holds both `Cargo.toml`
/// and `crates`. A relative [`REFERENCE_ENV`] value is resolved against
/// `<repo>`, because a test binary runs with the package directory as its
/// working directory.
///
/// # Errors
///
/// Returns [`ReferenceFixtureError`] when the fixture does not exist, naming
/// the fixture, the root that was tried, [`REFERENCE_ENV`] and the gate's
/// checkout option.
pub fn reference_fixture(relative: impl AsRef<Path>) -> Result<PathBuf, ReferenceFixtureError> {
    let relative = relative.as_ref();
    let repo = repo_root();
    let named = env::var_os(REFERENCE_ENV).filter(|value| !value.is_empty());
    let root = match &named {
        Some(value) => {
            let path = PathBuf::from(value.as_os_str());
            match &repo {
                Some(repo) if path.is_relative() => repo.join(path),
                _ => path,
            }
        }
        None => match &repo {
            Some(repo) => repo.join(DEFAULT_DIR),
            None => {
                return Err(ReferenceFixtureError {
                    message: format!(
                        "cannot locate the repository root above {} (no Cargo.toml with a \
                         crates directory); set {REFERENCE_ENV} to the checkout root (the \
                         clone holding {TEST_FILES}) or pass --reference PATH to krkr-gate",
                        Path::new(env!("CARGO_MANIFEST_DIR")).display(),
                    ),
                });
            }
        },
    };
    let fixture = root.join(TEST_FILES).join(relative);
    if fixture.exists() {
        return Ok(fixture);
    }
    let origin = match &named {
        Some(value) => format!("{REFERENCE_ENV}={}", Path::new(value).display()),
        None => format!(
            "{REFERENCE_ENV} is unset, so the default {} was tried",
            root.display()
        ),
    };
    Err(ReferenceFixtureError {
        message: format!(
            "reference fixture {} is missing ({origin}); the checkout root must hold \
             {TEST_FILES}; set {REFERENCE_ENV} to the clone root or pass --reference \
             PATH to krkr-gate",
            fixture.display(),
        ),
    })
}

/// Resolves a fixture of the checkout, printing why once and returning `None`
/// when it is unavailable.
///
/// This is the skip form the suite uses for fixtures that live outside the
/// repository: a test returns when it gets `None` and still runs its
/// assertions when the checkout is present.
pub fn reference_fixture_or_skip(relative: impl AsRef<Path>) -> Option<PathBuf> {
    match reference_fixture(relative) {
        Ok(fixture) => Some(fixture),
        Err(error) => {
            report_unavailable(&error);
            None
        }
    }
}

/// Prints the reason a fixture is unavailable once per test binary.
fn report_unavailable(error: &ReferenceFixtureError) {
    static REPORTED: Once = Once::new();
    REPORTED.call_once(|| eprintln!("skipping: {error}"));
}

/// The repository root above this crate.
fn repo_root() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|candidate| {
            candidate.join("Cargo.toml").is_file() && candidate.join("crates").is_dir()
        })
        .map(Path::to_path_buf)
}
