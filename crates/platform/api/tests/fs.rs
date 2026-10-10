//! Integration tests for [`api::fs`].

use std::path::{Path, PathBuf};

use api::fs::*;

#[test]
fn fs_path_host_and_uri_accessors() {
    let host = FsPath::host("/tmp/game");
    assert!(host.is_host());
    assert!(!host.is_uri());
    assert_eq!(host.as_host(), Some(Path::new("/tmp/game")));
    assert_eq!(host.as_uri(), None);

    let uri = FsPath::uri("content://game/data");
    assert!(uri.is_uri());
    assert_eq!(uri.as_uri(), Some("content://game/data"));
    assert_eq!(uri.as_host(), None);
}

#[test]
fn fs_path_from_host_conversions() {
    // Only `PathBuf`/`&Path` convert implicitly (to `Host`); URI strings
    // must use the explicit `FsPath::uri` constructor so a plain `&str`
    // is never silently reinterpreted as a platform URI.
    assert_eq!(
        FsPath::from(PathBuf::from("/c")),
        FsPath::Host(PathBuf::from("/c"))
    );
    assert_eq!(
        FsPath::from(Path::new("/d")),
        FsPath::Host(PathBuf::from("/d"))
    );
    assert_eq!(
        FsPath::uri("content://x"),
        FsPath::Uri("content://x".to_owned())
    );
}

#[test]
fn open_mode_capabilities() {
    assert!(OpenMode::Read.is_readable());
    assert!(!OpenMode::Read.is_writable());
    assert!(OpenMode::ReadWrite.is_readable());
    assert!(OpenMode::ReadWrite.is_writable());
    assert!(!OpenMode::Append.is_readable());
    assert!(OpenMode::Append.is_writable());
    assert_eq!(OpenMode::default(), OpenMode::Read);
}

#[test]
fn file_stat_is_file() {
    assert!(FileStat::default().is_file());
    assert!(
        !FileStat {
            size: 0,
            is_dir: true,
            modified_ms: Some(1),
        }
        .is_file()
    );
}
