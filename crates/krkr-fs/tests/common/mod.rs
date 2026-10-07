//! Shared helpers for the `krkr-fs` integration tests.

use std::path::PathBuf;

/// Builds an isolated temporary directory for one host test.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("krkr-fs-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}
