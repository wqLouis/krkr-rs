//! Integration tests for the game storage namespace (`Storage`).
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/storage.rs`. The
//! synthetic XP3 builder lives in the `testkit` dev crate; the
//! in-memory file-system helpers live in `tests/common/mod.rs`.

use std::path::PathBuf;

use engine::{ATTR_DIRECTORY, ATTR_NORMAL, Location, normalize_in_archive_name};
use testkit::build_xp3;

mod common;
use common::{mount_memory, mount_memory_with_archives};

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
    let storage = mount_memory_with_archives(
        &[("data.xp3", &xp3), ("data/winner.txt", b"from disk")],
        &["data.xp3"],
    );

    assert_eq!(
        storage.find("data/winner.txt").unwrap(),
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
    let storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    assert_eq!(
        storage.find("data/inside.txt").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/data.xp3"),
            "data/inside.txt".into()
        ))
    );
    assert_eq!(
        storage.read("data/inside.txt").unwrap(),
        b"hello from archive"
    );
    assert!(storage.exists("data/inside.txt").unwrap());
    assert_eq!(storage.stat("data/inside.txt").unwrap().unwrap().size, 18);
}

#[test]
fn arc_xp3_path_targets_one_archive() {
    let a = build_xp3(&[("shared.txt", b"from a", false)]);
    let b = build_xp3(&[("shared.txt", b"from b", false)]);
    let storage = mount_memory_with_archives(&[("a.xp3", &a), ("b.xp3", &b)], &["a.xp3", "b.xp3"]);

    assert_eq!(
        storage.read("a.xp3>shared.txt").unwrap(),
        b"from a".to_vec()
    );
    assert_eq!(
        storage.read("b.xp3>shared.txt").unwrap(),
        b"from b".to_vec()
    );
    // A specific archive miss does not fall through to the other archive.
    assert!(!storage.exists("a.xp3>missing.txt").unwrap());
}

#[test]
fn mixed_case_disk_name_resolves() {
    let storage = mount_memory(&[("savedata/savemng.dat", b"save data")]);
    for name in ["savedata/saveMng.dat", "SAVEDATA/SAVEMNG.DAT"] {
        assert!(
            storage.find(name).unwrap().is_some(),
            "{name} must resolve case-insensitively"
        );
        assert_eq!(storage.read(name).unwrap(), b"save data");
    }
}

#[test]
fn traversal_does_not_resolve() {
    let storage = mount_memory(&[("escape.dat", b"in")]);
    assert!(storage.find("../escape.dat").unwrap().is_none());
    assert!(storage.find("savedata/../../escape.dat").unwrap().is_none());
    assert!(!storage.exists("/outside.dat").unwrap());
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
    let storage = mount_memory_with_archives(&[("packed.xp3", &xp3)], &["packed.xp3"]);

    assert_eq!(storage.read("plain.bin").unwrap(), b"raw bytes");
    assert_eq!(
        storage.read("packed.bin").unwrap(),
        b"zlib compressed payload"
    );
}

#[test]
fn mount_and_unmount_archive() {
    let a = build_xp3(&[("only/a.txt", b"a", false)]);
    let mut storage = mount_memory_with_archives(&[("a.xp3", &a)], &["a.xp3"]);
    assert_eq!(storage.archive_count(), 1);
    assert!(storage.exists("only/a.txt").unwrap());

    assert!(storage.unmount_archive("a.xp3"));
    assert_eq!(storage.archive_count(), 0);
    assert!(!storage.exists("only/a.txt").unwrap());
}

#[test]
fn list_merges_disk_and_archives() {
    let xp3 = build_xp3(&[
        ("assets/arc_only.dat", b"arc", false),
        ("assets/sub/nested.dat", b"nested", false),
    ]);
    let storage = mount_memory_with_archives(
        &[("data.xp3", &xp3), ("assets/disk_only.dat", b"disk")],
        &["data.xp3"],
    );

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
    let storage = mount_memory_with_archives(
        &[
            ("data.xp3", &xp3),
            ("scripts/disk.tjs", b"y"),
            ("scripts/sub/nested.tjs", b"n"),
            ("scripts/readme.txt", b"z"),
        ],
        &["data.xp3"],
    );

    let mut tjs = storage.get_file_list("scripts/*.tjs", ATTR_NORMAL).unwrap();
    tjs.sort();
    assert_eq!(
        tjs,
        vec![
            "data.xp3>scripts/arc.tjs".to_string(),
            "scripts/disk.tjs".to_string(),
            "scripts/sub/nested.tjs".to_string(),
        ]
    );

    let dirs = storage.get_file_list("scripts/", ATTR_DIRECTORY).unwrap();
    assert!(dirs.contains(&"scripts/sub/".to_string()), "{dirs:?}");

    let arc_entries = storage.get_file_list("*.xp3>scripts/*.tjs", 0).unwrap();
    assert!(
        arc_entries.contains(&"data.xp3>scripts/arc.tjs".to_string()),
        "{arc_entries:?}"
    );
}

#[test]
fn auto_path_resolves_base_name() {
    let xp3 = build_xp3(&[("config.tjs", b"cfg", false)]);
    let mut storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);
    storage.add_auto_path("data.xp3>");

    assert_eq!(
        storage.find("config.tjs").unwrap(),
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
    let storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    let read = storage.read("data/large.bin").unwrap();
    assert_eq!(read.len(), payload.len());
    assert_eq!(read, payload);
}

/// Precedence: later-mounted archives override earlier ones, so
/// `--archive data.xp3 --archive patch.xp3` lets `patch.xp3` win.
#[test]
fn later_mounted_archive_wins() {
    let data = build_xp3(&[("shared.txt", b"from data", false)]);
    let patch = build_xp3(&[("shared.txt", b"from patch", false)]);
    let files = [
        ("data.xp3", data.as_slice()),
        ("patch.xp3", patch.as_slice()),
    ];

    let storage = mount_memory_with_archives(&files, &["data.xp3", "patch.xp3"]);
    assert_eq!(storage.read("shared.txt").unwrap(), b"from patch");
    assert_eq!(
        storage.find("shared.txt").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/patch.xp3"),
            "shared.txt".into()
        ))
    );

    let storage = mount_memory_with_archives(&files, &["patch.xp3", "data.xp3"]);
    assert_eq!(storage.read("shared.txt").unwrap(), b"from data");
}

/// Mounting never enumerates the game directory: an archive present on disk
/// is ignored until the caller names it explicitly.
#[test]
fn mounting_does_not_scan_for_archives() {
    let xp3 = build_xp3(&[("inside.txt", b"archive", false)]);
    let storage = mount_memory(&[("data.xp3", &xp3)]);

    assert_eq!(storage.archive_count(), 0);
    assert!(!storage.exists("inside.txt").unwrap());
}

/// `find` is file-only, but `exists` reports directories as existing, matching
/// `TVPIsExistentStorage` (review 1.10).
#[test]
fn directories_exist_but_do_not_find() {
    let storage = mount_memory(&[("savedata/keep.dat", b"x")]);

    assert_eq!(storage.find("savedata").unwrap(), None);
    assert!(storage.exists("savedata").unwrap());
    assert!(storage.exists("SAVEDATA").unwrap());
    assert!(!storage.exists("missing-dir").unwrap());
}

/// Unmounting by a bare name must not remove every archive that shares it, and
/// an exact path removes exactly one archive (review 1.6).
#[test]
fn unmount_archive_does_not_remove_same_named_archives() {
    let a = build_xp3(&[("a.bin", b"a", false)]);
    let b = build_xp3(&[("b.bin", b"b", false)]);
    let mut storage = mount_memory_with_archives(
        &[("one/data.xp3", &a), ("two/data.xp3", &b)],
        &["one/data.xp3", "two/data.xp3"],
    );
    assert_eq!(storage.archive_count(), 2);

    // Ambiguous bare base name: nothing is removed.
    assert!(!storage.unmount_archive("data.xp3"));
    assert_eq!(storage.archive_count(), 2);

    // Exact path removes only that archive.
    assert!(storage.unmount_archive("two/data.xp3"));
    assert_eq!(storage.archive_count(), 1);
    assert!(storage.exists("a.bin").unwrap());
    assert!(!storage.exists("b.bin").unwrap());
}
