//! Integration tests for the host backend.

mod common;

use api::error::FsError;
use api::fs::{FsPath, OpenMode};
use common::temp_dir;
use fs::{
    FileSystem, copy, create_dir_all, exists, flush, list, open, read_at, read_seq, remove_file,
    rename, stat, write_at,
};

#[test]
fn host_write_read_round_trip() {
    let dir = temp_dir("host-round");
    let fs = FileSystem::host(dir.clone());
    create_dir_all(&fs, &FsPath::host("data")).unwrap();

    let file = open(&fs, &FsPath::host("data/a.bin"), OpenMode::Write).unwrap();
    assert_eq!(write_at(&fs, &file, 0, b"hello world").unwrap(), 11);
    flush(&fs, &file).unwrap();
    assert_eq!(fs::file_size(&fs, &file).unwrap(), 11);
    drop(file);

    let mut file = open(&fs, &FsPath::host("data/a.bin"), OpenMode::Read).unwrap();
    let mut first = [0u8; 5];
    assert_eq!(read_seq(&fs, &mut file, &mut first).unwrap(), 5);
    assert_eq!(&first, b"hello");
    assert_eq!(file.cursor(), 5);
    let mut rest = [0u8; 6];
    assert_eq!(read_seq(&fs, &mut file, &mut rest).unwrap(), 6);
    assert_eq!(&rest, b" world");
    assert_eq!(read_seq(&fs, &mut file, &mut rest).unwrap(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn host_stat_list_mkdir_remove_copy_rename() {
    let dir = temp_dir("host-ops");
    let fs = FileSystem::host(dir.clone());
    create_dir_all(&fs, &FsPath::host("sub")).unwrap();

    let file = open(&fs, &FsPath::host("sub/x.txt"), OpenMode::Write).unwrap();
    write_at(&fs, &file, 0, b"xyz").unwrap();
    flush(&fs, &file).unwrap();
    drop(file);

    assert_eq!(stat(&fs, &FsPath::host("sub/x.txt")).unwrap().size, 3);
    let names: Vec<String> = list(&fs, &FsPath::host("sub"))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["x.txt"]);

    copy(&fs, &FsPath::host("sub/x.txt"), &FsPath::host("sub/y.txt")).unwrap();
    rename(&fs, &FsPath::host("sub/y.txt"), &FsPath::host("sub/z.txt")).unwrap();
    assert!(exists(&fs, &FsPath::host("sub/z.txt")));
    remove_file(&fs, &FsPath::host("sub/z.txt")).unwrap();
    assert!(!exists(&fs, &FsPath::host("sub/z.txt")));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn host_parent_file_yields_not_a_directory() {
    let dir = temp_dir("host-notdir");
    let fs = FileSystem::host(dir.clone());

    let file = open(&fs, &FsPath::host("blocker"), OpenMode::Write).unwrap();
    write_at(&fs, &file, 0, b"x").unwrap();
    drop(file);

    // Creating a directory below a regular file fails with the typed
    // `NotADirectory`, not an opaque I/O error.
    assert!(matches!(
        create_dir_all(&fs, &FsPath::host("blocker/sub")),
        Err(FsError::NotADirectory { .. })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backend_mismatch_is_an_error() {
    let mem = FileSystem::memory();
    create_dir_all(&mem, &FsPath::host("/d")).unwrap();
    let file = open(&mem, &FsPath::host("/d/a"), OpenMode::Write).unwrap();
    write_at(&mem, &file, 0, b"a").unwrap();
    drop(file);
    let file = open(&mem, &FsPath::host("/d/a"), OpenMode::Read).unwrap();

    let dir = temp_dir("mismatch");
    let host = FileSystem::host(dir.clone());
    assert!(matches!(
        read_at(&host, &file, 0, &mut [0u8; 1]),
        Err(FsError::InvalidState { .. })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
