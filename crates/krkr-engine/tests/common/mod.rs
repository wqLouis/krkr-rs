//! Shared helpers for the `krkr-engine` integration tests.
//!
//! The helpers here build real bottom-layer [`krkr_fs::FileSystem`] values
//! (in-memory) and mount them as [`Storage`], so the tests exercise the public
//! storage API end-to-end instead of reaching into crate internals.

use std::path::Path;

use krkr_api::fs::{FsPath, OpenMode};
use krkr_engine::Storage;
use krkr_fs::FileSystem;

pub mod xp3_builder;

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
        krkr_fs::create_dir_all(fs, &FsPath::host(parent)).expect("create parent dirs");
    }
    let file = krkr_fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
    krkr_fs::write_at(fs, &file, 0, data).expect("write memory file");
    krkr_fs::flush(fs, &file).expect("flush memory file");
}

/// Mounts an in-memory file system populated with `files` at `/`.
pub fn mount_memory(files: &[(&str, &[u8])]) -> Storage {
    let fs = memory_with(files);
    Storage::mount(fs, "/").expect("mount")
}
