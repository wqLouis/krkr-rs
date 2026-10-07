//! Integration tests for the game storage namespace (`Storage`).
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/storage.rs`. The
//! synthetic XP3 builder now lives in `tests/common/xp3_builder.rs`; the
//! in-memory file-system helpers live in `tests/common/mod.rs`.

use std::path::PathBuf;

use krkr_engine::{ATTR_DIRECTORY, ATTR_NORMAL, Location, normalize_in_archive_name};

mod common;
use common::mount_memory;
use common::xp3_builder::build_xp3;

#[test]
fn normalize_matches_reference() {
    assert_eq!(
        normalize_in_archive_name(r"Data\BG\Title.jpg"),
        "data/bg/title.jpg"
    );
    assert_eq!(
        normalize_in_archive_name("Data//BG//title.jpg"),
        "data/bg/title.jpg"
    );
    assert_eq!(normalize_in_archive_name("/leading"), "leading");
}

#[test]
fn disk_wins_over_archive() {
    let xp3 = build_xp3(&[("data/winner.txt", b"from archive", false)]);
    let storage = mount_memory(&[("data.xp3", &xp3), ("data/winner.txt", b"from disk")]);

    assert_eq!(
        storage.find("data/winner.txt"),
        Some(Location::Disk(PathBuf::from("/data/winner.txt")))
    );
    assert_eq!(storage.read("data/winner.txt").unwrap(), b"from disk");
}

#[test]
fn archive_is_used_when_no_disk_file() {
    let xp3 = build_xp3(&[
        ("data/inside.txt", b"hello from archive", false),
        ("data/other.txt", b"other", false),
    ]);
    let storage = mount_memory(&[("data.xp3", &xp3)]);

    assert_eq!(
        storage.find("data/inside.txt"),
        Some(Location::Archive(
            PathBuf::from("/data.xp3"),
            "data/inside.txt".into()
        ))
    );
    assert_eq!(
        storage.read("data/inside.txt").unwrap(),
        b"hello from archive"
    );
    assert!(storage.exists("data/inside.txt"));
    assert_eq!(storage.stat("data/inside.txt").unwrap().size, 18);
}

#[test]
fn arc_xp3_path_targets_one_archive() {
    let a = build_xp3(&[("shared.txt", b"from a", false)]);
    let b = build_xp3(&[("shared.txt", b"from b", false)]);
    let storage = mount_memory(&[("a.xp3", &a), ("b.xp3", &b)]);

    assert_eq!(
        storage.read("a.xp3>shared.txt").unwrap(),
        b"from a".to_vec()
    );
    assert_eq!(
        storage.read("b.xp3>shared.txt").unwrap(),
        b"from b".to_vec()
    );
    // A specific archive miss does not fall through to the other archive.
    assert!(!storage.exists("a.xp3>missing.txt"));
}

#[test]
fn mixed_case_disk_name_resolves() {
    let storage = mount_memory(&[("savedata/savemng.dat", b"save data")]);
    for name in ["savedata/saveMng.dat", "SAVEDATA/SAVEMNG.DAT"] {
        assert!(
            storage.find(name).is_some(),
            "{name} must resolve case-insensitively"
        );
        assert_eq!(storage.read(name).unwrap(), b"save data");
    }
}

#[test]
fn traversal_does_not_resolve() {
    let storage = mount_memory(&[("escape.dat", b"in")]);
    assert!(storage.find("../escape.dat").is_none());
    assert!(storage.find("savedata/../../escape.dat").is_none());
    assert!(!storage.exists("/outside.dat"));
}

#[test]
fn lowercase_canonical_wins_over_exact_case_variant() {
    let storage = mount_memory(&[
        ("savedata/savemng.dat", b"canonical"),
        ("savedata/saveMng.dat", b"stray"),
    ]);
    assert_eq!(storage.read("savedata/saveMng.dat").unwrap(), b"canonical");
}

#[test]
fn archive_reads_raw_and_zlib_segments() {
    let xp3 = build_xp3(&[
        ("plain.bin", b"raw bytes", false),
        ("packed.bin", b"zlib compressed payload", true),
    ]);
    let storage = mount_memory(&[("packed.xp3", &xp3)]);

    assert_eq!(storage.read("plain.bin").unwrap(), b"raw bytes");
    assert_eq!(
        storage.read("packed.bin").unwrap(),
        b"zlib compressed payload"
    );
}

#[test]
fn mount_and_unmount_archive() {
    let a = build_xp3(&[("only/a.txt", b"a", false)]);
    let mut storage = mount_memory(&[("a.xp3", &a)]);
    assert_eq!(storage.archive_count(), 1);
    assert!(storage.exists("only/a.txt"));

    assert!(storage.unmount_archive("a.xp3"));
    assert_eq!(storage.archive_count(), 0);
    assert!(!storage.exists("only/a.txt"));
}

#[test]
fn list_merges_disk_and_archives() {
    let xp3 = build_xp3(&[
        ("assets/arc_only.dat", b"arc", false),
        ("assets/sub/nested.dat", b"nested", false),
    ]);
    let storage = mount_memory(&[("data.xp3", &xp3), ("assets/disk_only.dat", b"disk")]);

    let listed = storage.list("assets").unwrap();
    assert_eq!(
        listed,
        vec![
            "assets/arc_only.dat".to_string(),
            "assets/disk_only.dat".to_string(),
            "assets/sub/".to_string(),
        ]
    );
}

#[test]
fn get_file_list_wildcards_and_attrs() {
    let xp3 = build_xp3(&[("scripts/arc.tjs", b"x", false)]);
    let storage = mount_memory(&[
        ("data.xp3", &xp3),
        ("scripts/disk.tjs", b"y"),
        ("scripts/sub/nested.tjs", b"n"),
        ("scripts/readme.txt", b"z"),
    ]);

    let mut tjs = storage.get_file_list("scripts/*.tjs", ATTR_NORMAL);
    tjs.sort();
    assert_eq!(
        tjs,
        vec![
            "data.xp3>scripts/arc.tjs".to_string(),
            "scripts/disk.tjs".to_string(),
            "scripts/sub/nested.tjs".to_string(),
        ]
    );

    let dirs = storage.get_file_list("scripts/", ATTR_DIRECTORY);
    assert!(dirs.contains(&"scripts/sub/".to_string()), "{dirs:?}");

    let arc_entries = storage.get_file_list("*.xp3>scripts/*.tjs", 0);
    assert!(
        arc_entries.contains(&"data.xp3>scripts/arc.tjs".to_string()),
        "{arc_entries:?}"
    );
}

#[test]
fn auto_path_resolves_base_name() {
    let xp3 = build_xp3(&[("config.tjs", b"cfg", false)]);
    let mut storage = mount_memory(&[("data.xp3", &xp3)]);
    storage.add_auto_path("data.xp3>");

    assert_eq!(
        storage.find("config.tjs"),
        Some(Location::Archive(
            PathBuf::from("/data.xp3"),
            "config.tjs".into()
        ))
    );
}

/// A large-but-valid archive entry still reads end-to-end: no size cap is
/// imposed, only genuine overflow/invalid data is rejected.
#[test]
fn reads_a_large_but_valid_archive_entry() {
    let payload: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let xp3 = build_xp3(&[("data/large.bin", &payload, true)]);
    let storage = mount_memory(&[("data.xp3", &xp3)]);

    let read = storage.read("data/large.bin").unwrap();
    assert_eq!(read.len(), payload.len());
    assert_eq!(read, payload);
}
