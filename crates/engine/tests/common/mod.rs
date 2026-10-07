//! Shared helpers for the `engine` integration tests.
//!
//! The helpers here build real bottom-layer [`fs::FileSystem`] values
//! (in-memory) and mount them as [`Storage`], so the tests exercise the public
//! storage API end-to-end instead of reaching into crate internals.

use std::path::Path;

use api::fs::{FsPath, OpenMode};
use engine::Storage;
use fs::FileSystem;

/// Creates an in-memory file-system root with the given files.
pub fn memory_with(files: &[(&str, &[u8])]) -> FileSystem {
    let fs = FileSystem::memory();
    for (path, data) in files {
        write_mem(&fs, path, data);
    }
    fs
}

/// Writes one file into an in-memory root, creating parent directories.
pub fn write_mem(fs: &FileSystem, path: &str, data: &[u8]) {
    let path_obj = Path::new(path);
    if let Some(parent) = path_obj.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(fs, &FsPath::host(parent)).expect("create parent dirs");
    }
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
    fs::write_at(fs, &file, 0, data).expect("write memory file");
    fs::flush(fs, &file).expect("flush memory file");
}

/// Mounts an in-memory file system populated with `files` at `/`, with no
/// archives mounted. The loader never scans for archives, so callers that
/// need one must name it explicitly via [`mount_memory_with_archives`].
pub fn mount_memory(files: &[(&str, &[u8])]) -> Storage {
    mount_memory_with_archives(files, &[])
}

/// Mounts an in-memory file system populated with `files` at `/`, then mounts
/// each path in `archives` in order (later archives win).
pub fn mount_memory_with_archives(files: &[(&str, &[u8])], archives: &[&str]) -> Storage {
    let fs = memory_with(files);
    Storage::mount_archives(fs, "/", archives).expect("mount")
}
