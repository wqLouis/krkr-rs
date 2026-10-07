//! Integration tests for the in-memory backend.

use krkr_api::error::FsError;
use krkr_api::fs::{FsPath, OpenMode, SeekFrom};
use krkr_fs::{
    FileSystem, copy, create_dir_all, exists, file_size, flush, list, open, read_at, read_seq,
    remove_file, rename, seek, stat, write_at,
};

#[test]
fn memory_write_read_round_trip() {
    let fs = FileSystem::memory();
    create_dir_all(&fs, &FsPath::host("/data")).unwrap();

    let file = open(&fs, &FsPath::host("/data/a.bin"), OpenMode::Write).unwrap();
    assert_eq!(write_at(&fs, &file, 0, b"hello").unwrap(), 5);
    assert_eq!(write_at(&fs, &file, 5, b" world").unwrap(), 6);
    flush(&fs, &file).unwrap();
    assert_eq!(file_size(&fs, &file).unwrap(), 11);
    assert_eq!(file.cursor(), 0, "positional writes do not move the cursor");

    let file = open(&fs, &FsPath::host("/data/a.bin"), OpenMode::Read).unwrap();
    let mut buf = [0u8; 5];
    assert_eq!(read_at(&fs, &file, 6, &mut buf).unwrap(), 5);
    assert_eq!(&buf, b"world");
    assert_eq!(read_at(&fs, &file, 11, &mut buf).unwrap(), 0);
}

#[test]
fn memory_read_seq_advances_cursor() {
    let fs = FileSystem::memory();
    create_dir_all(&fs, &FsPath::host("/d")).unwrap();
    let file = open(&fs, &FsPath::host("/d/a"), OpenMode::Write).unwrap();
    write_at(&fs, &file, 0, b"abcdef").unwrap();
    drop(file);

    let mut file = open(&fs, &FsPath::host("/d/a"), OpenMode::Read).unwrap();
    let mut first = [0u8; 2];
    assert_eq!(read_seq(&fs, &mut file, &mut first).unwrap(), 2);
    assert_eq!(&first, b"ab");
    assert_eq!(file.cursor(), 2);
    let mut rest = [0u8; 8];
    assert_eq!(read_seq(&fs, &mut file, &mut rest).unwrap(), 4);
    assert_eq!(&rest[..4], b"cdef");
    assert_eq!(file.cursor(), 6);
    assert_eq!(read_seq(&fs, &mut file, &mut rest).unwrap(), 0);
}

#[test]
fn memory_positional_write_extends_with_zeros() {
    let fs = FileSystem::memory();
    create_dir_all(&fs, &FsPath::host("/d")).unwrap();
    let file = open(&fs, &FsPath::host("/d/a"), OpenMode::Write).unwrap();
    write_at(&fs, &file, 2, b"xy").unwrap();
    assert_eq!(file_size(&fs, &file).unwrap(), 4);
    drop(file);

    let file = open(&fs, &FsPath::host("/d/a"), OpenMode::Read).unwrap();
    let mut buf = [0u8; 4];
    read_at(&fs, &file, 0, &mut buf).unwrap();
    assert_eq!(&buf, &[0, 0, b'x', b'y']);
}

#[test]
fn memory_seek_positions_cursor() {
    let fs = FileSystem::memory();
    create_dir_all(&fs, &FsPath::host("/d")).unwrap();
    let file = open(&fs, &FsPath::host("/d/a"), OpenMode::Write).unwrap();
    write_at(&fs, &file, 0, b"abcd").unwrap();
    drop(file);

    let mut file = open(&fs, &FsPath::host("/d/a"), OpenMode::Read).unwrap();
    assert_eq!(seek(&fs, &mut file, SeekFrom::Start(2)).unwrap(), 2);
    assert_eq!(seek(&fs, &mut file, SeekFrom::Current(1)).unwrap(), 3);
    assert_eq!(seek(&fs, &mut file, SeekFrom::End(-1)).unwrap(), 3);
    assert!(seek(&fs, &mut file, SeekFrom::Current(-10)).is_err());
}

#[test]
fn memory_append_positions_cursor_at_end() {
    let fs = FileSystem::memory();
    create_dir_all(&fs, &FsPath::host("/d")).unwrap();
    let file = open(&fs, &FsPath::host("/d/a"), OpenMode::Write).unwrap();
    write_at(&fs, &file, 0, b"abc").unwrap();
    drop(file);

    let file = open(&fs, &FsPath::host("/d/a"), OpenMode::Append).unwrap();
    assert_eq!(file.cursor(), 3);
    assert_eq!(file_size(&fs, &file).unwrap(), 3);
}

#[test]
fn memory_stat_list_mkdir_remove_copy_rename() {
    let fs = FileSystem::memory();
    create_dir_all(&fs, &FsPath::host("/a/b")).unwrap();
    assert!(exists(&fs, &FsPath::host("/a")));
    assert!(stat(&fs, &FsPath::host("/a")).unwrap().is_dir);

    let file = open(&fs, &FsPath::host("/a/b/x.txt"), OpenMode::Write).unwrap();
    write_at(&fs, &file, 0, b"xyz").unwrap();
    drop(file);

    let entry = stat(&fs, &FsPath::host("/a/b/x.txt")).unwrap();
    assert_eq!(entry.size, 3);
    assert!(entry.is_file());

    let one = list(&fs, &FsPath::host("/a/b")).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].name, "x.txt");
    assert!(!one[0].is_dir);

    create_dir_all(&fs, &FsPath::host("/a/b/sub")).unwrap();
    let file = open(&fs, &FsPath::host("/a/b/a.txt"), OpenMode::Write).unwrap();
    write_at(&fs, &file, 0, b"a").unwrap();
    drop(file);

    let names: Vec<String> = list(&fs, &FsPath::host("/a/b"))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["a.txt", "sub", "x.txt"]);

    copy(
        &fs,
        &FsPath::host("/a/b/x.txt"),
        &FsPath::host("/a/b/y.txt"),
    )
    .unwrap();
    assert_eq!(stat(&fs, &FsPath::host("/a/b/y.txt")).unwrap().size, 3);

    rename(
        &fs,
        &FsPath::host("/a/b/y.txt"),
        &FsPath::host("/a/b/z.txt"),
    )
    .unwrap();
    assert!(!exists(&fs, &FsPath::host("/a/b/y.txt")));
    assert!(exists(&fs, &FsPath::host("/a/b/z.txt")));

    remove_file(&fs, &FsPath::host("/a/b/z.txt")).unwrap();
    assert!(!exists(&fs, &FsPath::host("/a/b/z.txt")));

    // Directory rename moves the whole subtree.
    rename(&fs, &FsPath::host("/a/b/sub"), &FsPath::host("/a/moved")).unwrap();
    assert!(stat(&fs, &FsPath::host("/a/moved")).unwrap().is_dir);
    assert!(!exists(&fs, &FsPath::host("/a/b/sub")));
}

#[test]
fn memory_missing_and_directory_errors_are_typed() {
    let fs = FileSystem::memory();
    create_dir_all(&fs, &FsPath::host("/d")).unwrap();
    assert!(matches!(
        stat(&fs, &FsPath::host("/nope")),
        Err(FsError::NotFound { .. })
    ));
    assert!(matches!(
        open(&fs, &FsPath::host("/d"), OpenMode::Read),
        Err(FsError::IsADirectory { .. })
    ));
    assert!(matches!(
        list(&fs, &FsPath::host("/nope")),
        Err(FsError::NotFound { .. })
    ));
    assert!(!exists(&fs, &FsPath::host("/nope")));
    assert!(matches!(
        open(&fs, &FsPath::uri("content://x"), OpenMode::Read),
        Err(FsError::Unsupported { .. })
    ));
}

#[test]
fn memory_roots_are_independent() {
    let a = FileSystem::memory();
    let b = FileSystem::memory();
    create_dir_all(&a, &FsPath::host("/d")).unwrap();
    let file = open(&a, &FsPath::host("/d/a"), OpenMode::Write).unwrap();
    write_at(&a, &file, 0, b"a").unwrap();
    drop(file);
    assert!(exists(&a, &FsPath::host("/d/a")));
    assert!(!exists(&b, &FsPath::host("/d/a")));

    // A clone shares the backing store (same resource, still stateless).
    let clone = a.clone();
    assert!(exists(&clone, &FsPath::host("/d/a")));
}
