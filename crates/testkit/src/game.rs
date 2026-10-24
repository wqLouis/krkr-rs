//! Helpers for the tests that open a real game directory.

use std::io;
use std::path::{Path, PathBuf};

/// Lists the `*.xp3` archives in `dir`, sorted by file name.
///
/// The listing is a test convenience: the product takes an explicit archive
/// list and never scans a folder, so a test derives the list from the
/// directory it was given.
///
/// # Errors
///
/// Returns the I/O error when the directory cannot be read.
pub fn game_archives(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut archives = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("xp3"))
        {
            archives.push(path);
        }
    }
    archives.sort();
    Ok(archives)
}
