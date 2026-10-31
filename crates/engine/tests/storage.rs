//! Integration tests for the game storage namespace (`Storage`).
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/storage.rs`. The
//! synthetic XP3 builder lives in the `testkit` dev crate; the
//! in-memory file-system helpers live in `tests/common/mod.rs`.

use std::path::{Path, PathBuf};

use api::fs::FsPath;
use engine::storage::{ATTR_DIRECTORY, ATTR_NORMAL, MountOptions};
use engine::{Location, Storage, normalize_in_archive_name};
use testkit::build_xp3;

mod common;
use common::{memory_with, mount_memory, mount_memory_with_archives};

// ---------------------------------------------------------------------------
// Log capture (for the storage miss line)
// ---------------------------------------------------------------------------

thread_local! {
    /// Log lines written on this thread since the last clear. The tests run in
    /// parallel, so the capture is per-thread.
    static CAPTURED_LOGS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A `log` sink that records every record on the calling thread.
struct CaptureLogger;

impl log::Log for CaptureLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        CAPTURED_LOGS.with(|logs| {
            logs.borrow_mut()
                .push(format!("{} {}", record.level(), record.args()));
        });
    }

    fn flush(&self) {}
}

/// Installs the recording sink once per test binary, clears this thread's
/// buffer, runs `f`, and returns its result plus the thread's log lines.
///
/// The global level is set to `trace` so both the bounded `debug` miss line and
/// the full `trace` miss line are visible to every test; the per-thread buffer
/// keeps concurrently running tests from seeing each other's records.
fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = log::set_boxed_logger(Box::new(CaptureLogger));
        log::set_max_level(log::LevelFilter::Trace);
    });
    CAPTURED_LOGS.with(|logs| logs.borrow_mut().clear());
    let result = f();
    let logs = CAPTURED_LOGS.with(|logs| logs.borrow().clone());
    (result, logs)
}

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
fn discover_archives_orders_by_name_then_shallower_last() {
    let fs = memory_with(&[
        ("game/data.xp3", b""),
        ("game/patch.xp3", b""),
        ("game/sub/extra.xp3", b""),
        ("game/sub/patch.xp3", b""),
        ("game/readme.txt", b""),
        ("game/UPPER.XP3", b""),
    ]);
    let found = engine::storage::discover_archives(&fs, Path::new("/game")).expect("discover");
    let names: Vec<String> = found
        .iter()
        .map(|path| {
            path.strip_prefix("/game")
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();
    // Name (case-insensitive) is the primary key; among equal names the
    // shallower path sorts last (and therefore mounts last and wins). So
    // `patch.xp3` overrides `data.xp3`, and the root `patch.xp3` overrides
    // `sub/patch.xp3`; a non-`.xp3` file is ignored.
    assert_eq!(
        names,
        [
            "data.xp3",
            "sub/extra.xp3",
            "sub/patch.xp3",
            "patch.xp3",
            "UPPER.XP3",
        ]
    );
}

#[test]
fn discover_archives_orders_numbered_patches_naturally() {
    let fs = memory_with(&[
        ("game/data.xp3", b""),
        ("game/patch@r785.xp3", b""),
        ("game/patch@r996.xp3", b""),
        ("game/patch@r1031.xp3", b""),
        ("game/patch@r1071.xp3", b""),
        ("game/patch_append5.xp3", b""),
        ("game/patch_append10.xp3", b""),
    ]);
    let found = engine::storage::discover_archives(&fs, Path::new("/game")).expect("discover");
    let names: Vec<String> = found
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    // Embedded decimal runs are compared by value, so the highest revision and
    // the highest append number mount last and win.
    // Reference: the game adds its numbered patch archives through
    // `Storages.addAutoPath` and documents "later-added wins"
    // (`reference/cpp/core/base/StorageIntf.cpp:921-937`); the vendored
    // directory listing order is not defined, so this is the D35 discovery
    // extension's own natural order.
    assert_eq!(
        names,
        [
            "data.xp3",
            "patch@r785.xp3",
            "patch@r996.xp3",
            "patch@r1031.xp3",
            "patch@r1071.xp3",
            "patch_append5.xp3",
            "patch_append10.xp3",
        ]
    );
}

/// A folder project checks the app directory first: the current path is the
/// folder, so a loose file wins over a mounted archive with the same name.
///
/// Reference: the project path is the current directory, with a trailing `/`
/// when it is a folder (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`),
/// and the search checks that current storage before the auto-path table
/// (`reference/cpp/core/base/StorageIntf.cpp:1061-1108`).
#[test]
fn folder_project_disk_wins_over_archive() {
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

/// An archive project checks its project archive first: that archive is the
/// reference's current directory (with the archive delimiter appended), before
/// the app directory and the auto-path list.
///
/// Reference: `reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`,
/// `reference/cpp/core/base/StorageIntf.cpp:1061-1108`.
#[test]
fn archive_project_wins_over_disk() {
    let xp3 = build_xp3(&[("data/winner.txt", b"from archive", false)]);
    let mut storage = mount_memory_with_archives(
        &[("data.xp3", &xp3), ("data/winner.txt", b"from disk")],
        &["data.xp3"],
    );
    storage.set_project("/data.xp3");

    assert_eq!(
        storage.find("data/winner.txt").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/data.xp3"),
            "data/winner.txt".into()
        ))
    );
    assert_eq!(storage.read("data/winner.txt").unwrap(), b"from archive");
}

/// The app directory is the final fallback: a loose file still resolves when
/// no mounted archive and no auto path holds it.
#[test]
fn disk_resolves_when_no_archive_or_auto_path_holds_the_name() {
    let xp3 = build_xp3(&[("other.txt", b"from archive", false)]);
    let fs = memory_with(&[("game/data.xp3", &xp3), ("game/loose.txt", b"from disk")]);
    let storage = Storage::mount_archives(fs, "/game", ["data.xp3"]).expect("mount");

    assert_eq!(
        storage.find("loose.txt").unwrap(),
        Some(Location::Disk(PathBuf::from("/game/loose.txt")))
    );
    assert_eq!(storage.read("loose.txt").unwrap(), b"from disk");
}

/// The app path resolves against the project directory, not against the mount
/// root: a launch whose base directory is the *parent* of the game folder
/// still finds the game's own `patch.tjs`.
///
/// The reference's app path is the project directory
/// (`reference/cpp/core/base/impl/StorageImpl.cpp:503-509`), and the app-path
/// scripts are read from it
/// (`reference/cpp/core/base/ScriptMgnIntf.cpp:903`, `:970`).
#[test]
fn the_app_path_resolves_against_the_project_directory() {
    let xp3 = build_xp3(&[("startup.tjs", b"// startup", false)]);
    let fs = memory_with(&[
        ("parent/game/data.xp3", &xp3),
        ("parent/game/patch.tjs", b"// patch"),
    ]);
    let mut storage = Storage::mount_archives(fs, "/parent", ["game/data.xp3"]).expect("mount");

    // The mount root is the launch directory; the project directory is the
    // folder the project archive lives in.
    assert_eq!(storage.game_dir(), Path::new("/parent"));
    assert_eq!(storage.app_dir(), Path::new("/parent"));
    assert!(
        storage.startup_plan().unwrap().patch.is_none(),
        "the patch script is not below the mount root"
    );

    storage.set_app_dir("/parent/game");
    let plan = storage.startup_plan().expect("startup plan");
    assert_eq!(plan.patch.as_deref(), Some("patch.tjs"));
    assert_eq!(plan.startup.as_deref(), Some("startup.tjs"));
    assert_eq!(
        storage.read("patch.tjs").unwrap(),
        b"// patch",
        "the loose file is read below the project directory"
    );
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

// L3 registers a TVP storage-media entry (`psb://…`, `psd://…`) with
// `register_media`; L2 serves it before disk and archive resolution so a
// script can open the registered name (reference `TVPRegisterStorageMedia`
// feeding `PSBMedia::CheckExistentStorage`/`Open`, `PSBMedia.cpp:24-46`).
#[test]
fn registered_media_is_served_before_disk_and_archive() {
    let storage = mount_memory(&[]);
    storage.register_media("psb://fixture.psb/res".into(), b"resource bytes".to_vec());

    assert!(storage.exists("psb://fixture.psb/res").unwrap());
    assert_eq!(
        storage.read("psb://fixture.psb/res").unwrap(),
        b"resource bytes"
    );
    assert_eq!(
        storage.read_prefix("psb://fixture.psb/res", 4).unwrap(),
        b"reso"
    );
    assert!(!storage.exists("psb://fixture.psb/other").unwrap());
    assert!(storage.read("psb://fixture.psb/other").is_err());
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

/// An absolute name below the mount resolves even when its spelling differs
/// from the mount point only in case: the reference walks the components
/// against the real directory entries
/// (`reference/cpp/core/base/impl/StorageImpl.cpp:301-407`).
#[test]
fn absolute_name_resolves_case_insensitively_below_the_mount() {
    let fs = memory_with(&[("Game/Savedata/Data.bin", b"payload")]);
    let storage = Storage::mount_archives(fs, "/Game", std::iter::empty::<&str>()).expect("mount");

    for name in [
        "/Game/Savedata/Data.bin",
        "/game/savedata/data.bin",
        "/GAME/SAVEDATA/DATA.BIN",
    ] {
        assert!(
            storage.exists(name).unwrap(),
            "{name} must resolve case-insensitively"
        );
        assert_eq!(storage.read(name).unwrap(), b"payload");
    }

    // An absolute name outside the mount is still a miss.
    assert!(!storage.exists("/Other/Savedata/Data.bin").unwrap());
}

/// [`Storage::local_name`] case-corrects every existing component and returns
/// the real on-disk spelling, for files and directories alike — the reference
/// `tTVPFileMedia::GetLocallyAccessibleName`
/// (`reference/cpp/core/base/impl/StorageImpl.cpp:339-407`).
#[test]
fn local_name_case_corrects_components() {
    let fs = memory_with(&[
        ("Games/KR/Assets/config.tjs", b"cfg"),
        ("Games/KR/Assets/sub/data.bin", b"data"),
    ]);
    let storage =
        Storage::mount_archives(fs, "/Games/KR", std::iter::empty::<&str>()).expect("mount");

    // reference/cpp/core/base/impl/StorageImpl.cpp:339-407
    assert_eq!(
        storage.local_name("/Games/KR/ASSETS/").unwrap(),
        Some(PathBuf::from("/Games/KR/Assets"))
    );
    assert_eq!(
        storage.local_name("assets/CONFIG.TJS").unwrap(),
        Some(PathBuf::from("/Games/KR/Assets/config.tjs"))
    );
    // The fully lowercased form of an absolute mixed-case path, the shape
    // `System.dataPath` produces, resolves to the real spelling.
    assert_eq!(
        storage.local_name("/games/kr/assets/config.tjs").unwrap(),
        Some(PathBuf::from("/Games/KR/Assets/config.tjs"))
    );
    // A component that does not exist keeps the requested spelling, and the
    // remaining components are appended unchanged.
    assert_eq!(
        storage.local_name("assets/absent/deep.bin").unwrap(),
        Some(PathBuf::from("/Games/KR/Assets/absent/deep.bin"))
    );
    // A name outside the mount is a miss.
    assert_eq!(storage.local_name("/elsewhere/x").unwrap(), None);
    // A `..` that escapes the mount is the reference's `TVPInvalidPathName`.
    // reference/cpp/core/base/StorageIntf.cpp:400
    assert!(matches!(
        storage.local_name("../escape"),
        Err(engine::storage::StorageReadError::InvalidPathName(_))
    ));
}

/// The case-correction walk reads the directory live: a listing warmed while
/// the directory was empty must not hide a directory created afterwards. The
/// reference `tTVPFileMedia::GetLocallyAccessibleName` calls `opendir`/
/// `readdir` on every invocation
/// (`reference/cpp/core/base/impl/StorageImpl.cpp:339-407`).
#[test]
fn local_name_reads_the_directory_live() {
    let file_system = memory_with(&[("game/keep.txt", b"x")]);
    let storage =
        Storage::mount_archives(file_system, "/game", std::iter::empty::<&str>()).expect("mount");

    // Warm the game-directory listing while it has no `Assets` entry.
    assert!(storage.find("missing.txt").unwrap().is_none());
    fs::create_dir_all(storage.file_system(), &FsPath::host("/game/Assets"))
        .expect("create Assets");

    // reference/cpp/core/base/impl/StorageImpl.cpp:339-407
    assert_eq!(
        storage.local_name("ASSETS/").unwrap(),
        Some(PathBuf::from("/game/Assets"))
    );
}

/// A `..` that leaves the mount is the reference's `TVPInvalidPathName`
/// (`reference/cpp/core/base/StorageIntf.cpp:400`), not a plain miss; a `..`
/// that stays inside the mount still resolves.
#[test]
fn traversal_escape_raises_and_inner_traversal_resolves() {
    let storage = mount_memory(&[("escape.dat", b"in"), ("savedata/keep.dat", b"x")]);

    for name in ["../escape.dat", "savedata/../../escape.dat", "../../x"] {
        let result = storage.find(name);
        assert!(
            matches!(
                result.as_ref(),
                Err(engine::storage::StorageReadError::InvalidPathName(_))
            ),
            "{name} must raise InvalidPathName, got {result:?}"
        );
        assert!(matches!(
            storage.exists(name),
            Err(engine::storage::StorageReadError::InvalidPathName(_))
        ));
    }

    // A `..` that stays inside the mount resolves.
    assert!(storage.exists("savedata/../escape.dat").unwrap());
    assert_eq!(storage.read("savedata/../escape.dat").unwrap(), b"in");

    // An absolute name that maps below the root is not a traversal.
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
fn file_list_wildcards_and_attrs() {
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

    let mut tjs = storage.file_list("scripts/*.tjs", ATTR_NORMAL).unwrap();
    tjs.sort();
    assert_eq!(
        tjs,
        vec![
            "data.xp3>scripts/arc.tjs".to_string(),
            "scripts/disk.tjs".to_string(),
            "scripts/sub/nested.tjs".to_string(),
        ]
    );

    let dirs = storage.file_list("scripts/", ATTR_DIRECTORY).unwrap();
    assert!(dirs.contains(&"scripts/sub/".to_string()), "{dirs:?}");

    let arc_entries = storage.file_list("*.xp3>scripts/*.tjs", 0).unwrap();
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

/// An absolute plain-directory auto path — the
/// `Storages.addAutoPath(System.exePath + "chs/")` form the shipped
/// `system/Initialize.tjs` uses — resolves a base name in that absolute
/// directory, not below the game directory.
///
/// Reference: `TVPAddAutoPath` stores the normalized absolute path and
/// `TVPRebuildAutoPathTable` lists it, so `TVPGetPlacedPath` returns
/// `path + storagename` (`reference/cpp/core/base/StorageIntf.cpp:921-937`,
/// `:1024-1034`).
#[test]
fn absolute_auto_path_resolves_in_that_directory() {
    let fs = memory_with(&[("game/chs/config.tjs", b"translated")]);
    let mut storage =
        Storage::mount_archives(fs, "/game", std::iter::empty::<&str>()).expect("mount");
    storage.add_auto_path("/game/chs/");

    assert_eq!(
        storage.find("config.tjs").unwrap(),
        Some(Location::Disk(PathBuf::from("/game/chs/config.tjs")))
    );
    assert_eq!(storage.read("config.tjs").unwrap(), b"translated");
    assert!(storage.exists("config.tjs").unwrap());
}

/// The `>` form of the same absolute pattern — the more common KAG form
/// `Storages.addAutoPath(System.exePath + "data.xp3>")` — already resolved
/// against the mounted archive by base file name: `find_in_archive` matches the
/// archive on its file name and `archive_disk_path` already kept an absolute
/// path. This test pins that behavior for the mounted case.
///
/// Reference: `TVPAddAutoPath` (`reference/cpp/core/base/StorageIntf.cpp:921-937`)
/// and `TVPRebuildAutoPathTable` (`:979-1013`).
#[test]
fn absolute_archive_auto_path_resolves_a_mounted_archive() {
    let xp3 = build_xp3(&[("system/Config.tjs", b"mounted", false)]);
    let fs = memory_with(&[("game/data.xp3", &xp3)]);
    let mut storage = Storage::mount_archives(fs, "/game", ["data.xp3"]).expect("mount");
    storage.add_auto_path("/game/data.xp3>system/");

    assert_eq!(
        storage.find("Config.tjs").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/game/data.xp3"),
            "system/config.tjs".into()
        ))
    );
    assert_eq!(storage.read("Config.tjs").unwrap(), b"mounted");
}

/// A relative folder auto path resolves against the current path. When the
/// project is archive-backed the reference appends `>` to `TVPProjectDir` and
/// sets it as the current directory
/// (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`), so a relative
/// `Storages.addAutoPath("system/")` is normalized to `data.xp3>system/`
/// (`reference/cpp/core/base/StorageIntf.cpp:921-937`) and the lookup searches
/// the archive's `system/` children. This is the `Config.tjs が見つかりません`
/// false negative: the archive holds `system/Config.tjs` but the relative auto
/// path was resolved against disk only.
#[test]
fn relative_auto_path_resolves_inside_a_mounted_archive() {
    let xp3 = build_xp3(&[("system/Config.tjs", b"cfg", false)]);
    let mut storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);
    storage.set_project("/data.xp3");
    storage.add_auto_path("system/");

    assert_eq!(
        storage.find("Config.tjs").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/data.xp3"),
            "system/config.tjs".into()
        ))
    );
    assert_eq!(storage.read("Config.tjs").unwrap(), b"cfg");
}

/// An archive-backed miss names the archive's size, its index sections and the
/// closest names, so one device log is enough to see what the archive contains
/// and how the request almost matched.
#[test]
fn archive_miss_reports_near_misses() {
    let xp3 = build_xp3(&[("system/Config.tjs", b"cfg", false)]);
    let storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    let (found, logs) = capture_logs(|| storage.find("Config.tjs"));
    assert_eq!(found.unwrap(), None);
    let miss = logs
        .iter()
        .find(|line| line.starts_with("TRACE storage miss"))
        .expect("a miss logs a full trace line");
    assert!(miss.contains("1 entries, 1 index section(s)"), "{miss}");
    assert!(miss.contains("system/config.tjs"), "{miss}");
    assert!(miss.contains("nearest 1/1"), "{miss}");
}

/// An archive-backed relative auto path resolves by the request's base name,
/// in both the plain form the port stores (`system/`, which the reference
/// resolves against the project archive) and the explicit archive form
/// (`data.xp3>system/`). The reference indexes the direct children of an
/// auto path once and looks a request up by its base name
/// (`TVPRebuildAutoPathTable` `reference/cpp/core/base/StorageIntf.cpp:979-1013`,
/// `TVPGetPlacedPath` `:1088-1093`).
#[test]
fn both_auto_path_forms_resolve_a_bare_name() {
    let xp3 = build_xp3(&[("system/Config.tjs", b"cfg", false)]);

    let mut plain = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);
    plain.set_project("/data.xp3");
    plain.add_auto_path("system/");
    assert_eq!(plain.read("Config.tjs").unwrap(), b"cfg");

    let mut explicit = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);
    explicit.add_auto_path("data.xp3>system/");
    assert_eq!(explicit.read("Config.tjs").unwrap(), b"cfg");
}

/// Only the **direct** children of an auto-path prefix are indexed, so a name
/// below a subdirectory is not reachable by its base name. The reference
/// skips an in-archive name that has another `/` after the prefix
/// (`reference/cpp/core/base/StorageIntf.cpp:1001-1003`).
#[test]
fn auto_path_registers_direct_children_only() {
    let xp3 = build_xp3(&[("system/sub/Config.tjs", b"deep", false)]);
    let mut storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);
    storage.set_project("/data.xp3");
    storage.add_auto_path("system/");

    assert_eq!(storage.find("Config.tjs").unwrap(), None);
    // The full in-archive path still resolves.
    assert_eq!(storage.read("system/sub/Config.tjs").unwrap(), b"deep");
}

/// A resolved hit is cached; a miss is not (the reference caches the placed
/// path for a hit and explicitly does not cache a miss,
/// `reference/cpp/core/base/StorageIntf.cpp:1101`).
#[test]
fn a_resolved_hit_is_cached_and_a_miss_is_not() {
    let xp3 = build_xp3(&[("system/Config.tjs", b"cfg", false)]);
    let mut storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);
    storage.set_project("/data.xp3");
    storage.add_auto_path("system/");

    assert!(storage.find("Config.tjs").unwrap().is_some());
    let after_hit = storage.probe_counts();
    assert!(storage.find("Config.tjs").unwrap().is_some());
    assert_eq!(
        storage.probe_counts().stat,
        after_hit.stat,
        "a cached hit must not re-probe"
    );
    assert_eq!(
        storage.probe_counts().list,
        after_hit.list,
        "a cached hit must not re-list"
    );

    assert!(storage.find("Missing.tjs").unwrap().is_none());
    assert!(storage.find("Missing.tjs").unwrap().is_none());
    assert_eq!(
        storage.probe_counts().misses,
        2,
        "a miss must not be cached"
    );
}

/// The project archive is an explicit input, not the first archive mounted.
/// D35 discovery sorts case-insensitively, so `bgimage.xp3` mounts before
/// `data.xp3`; the relative `system/` auto path must still resolve inside the
/// archive the launcher named as the project
/// (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`).
#[test]
fn project_archive_is_not_the_first_mounted() {
    let bgimage = build_xp3(&[("system/bg.dat", b"bg", false)]);
    let data = build_xp3(&[("system/Config.tjs", b"cfg", false)]);
    let mut storage = mount_memory_with_archives(
        &[("bgimage.xp3", &bgimage), ("data.xp3", &data)],
        &["bgimage.xp3", "data.xp3"],
    );
    storage.set_project("/data.xp3");
    storage.add_auto_path("system/");

    assert_eq!(
        storage.find("Config.tjs").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/data.xp3"),
            "system/config.tjs".into()
        ))
    );
}

/// A folder project appends `/` and leaves a relative auto path disk-relative
/// (the reference's `else` branch,
/// `reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`).
#[test]
fn folder_project_resolves_a_relative_auto_path_on_disk() {
    let mut storage = mount_memory(&[("chs/Config.tjs", b"disk")]);
    storage.set_project("/");
    storage.add_auto_path("chs/");

    assert_eq!(
        storage.find("Config.tjs").unwrap(),
        Some(Location::Disk(PathBuf::from("/chs/Config.tjs")))
    );
}

/// With no project set, a relative auto path stays disk-relative: the project
/// is never inferred from the mounted archives, so a re-introduced
/// first-mount inference would resolve `bg.dat` here and fail.
#[test]
fn no_project_leaves_a_relative_auto_path_disk_relative() {
    let bgimage = build_xp3(&[("system/bg.dat", b"bg", false)]);
    let mut storage = mount_memory_with_archives(&[("bgimage.xp3", &bgimage)], &["bgimage.xp3"]);
    storage.add_auto_path("system/");

    assert_eq!(storage.find("bg.dat").unwrap(), None);
}

/// The same absolute `>` auto path opens and parses an archive that was not
/// mounted: `TVPArchiveCache.Get` opens the archive named in the path when it
/// exists and is not already cached
/// (`reference/cpp/core/base/StorageIntf.cpp:695-724`).
#[test]
fn absolute_archive_auto_path_opens_an_unmounted_archive() {
    let xp3 = build_xp3(&[("config.tjs", b"on demand", false)]);
    let fs = memory_with(&[("game/extra.xp3", &xp3)]);
    let mut storage =
        Storage::mount_archives(fs, "/game", std::iter::empty::<&str>()).expect("mount");
    storage.add_auto_path("/game/extra.xp3>");

    assert_eq!(
        storage.find("config.tjs").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/game/extra.xp3"),
            "config.tjs".into()
        ))
    );
    assert_eq!(storage.read("config.tjs").unwrap(), b"on demand");
}

/// `.` and `..` components fold the way the reference normalizes a storage
/// name, so `./config.tjs` and `sub/../config.tjs` resolve to `config.tjs`;
/// a `..` that would escape the root raises `InvalidPathName`.
///
/// Reference: the redundant-path compression in `TVPNormalizeStorageName`
/// (`reference/cpp/core/base/StorageIntf.cpp:371-425`) and the
/// `TVPInvalidPathName` raise at `:400`.
#[test]
fn dot_components_fold_and_escape_raises() {
    let xp3 = build_xp3(&[("config.tjs", b"cfg", false)]);
    let storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    assert_eq!(storage.read("./config.tjs").unwrap(), b"cfg");
    assert_eq!(storage.read("sub/../config.tjs").unwrap(), b"cfg");
    assert!(matches!(
        storage.find("../../escape.dat"),
        Err(engine::storage::StorageReadError::InvalidPathName(_))
    ));
}

/// The script-visible name `Config.tjs` resolves through each path the search
/// order defines: the app directory, a mounted archive, a relative folder
/// auto path, and an archive auto path with an in-archive prefix.
#[test]
fn each_resolution_path_resolves_a_script_name() {
    // The app directory (the folder project's current path).
    let disk = mount_memory(&[("Config.tjs", b"disk")]);
    assert_eq!(disk.read("Config.tjs").unwrap(), b"disk");

    // A mounted archive at the requested path.
    let xp3 = build_xp3(&[("Config.tjs", b"archive", false)]);
    let mounted = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);
    assert_eq!(mounted.read("Config.tjs").unwrap(), b"archive");

    // A relative folder auto path.
    let mut folder = mount_memory(&[("chs/Config.tjs", b"folder")]);
    folder.add_auto_path("chs/");
    assert_eq!(folder.read("Config.tjs").unwrap(), b"folder");

    // An archive auto path with an in-archive prefix.
    let prefixed = build_xp3(&[("system/Config.tjs", b"prefixed", false)]);
    let mut prefixed = mount_memory_with_archives(&[("data.xp3", &prefixed)], &["data.xp3"]);
    prefixed.add_auto_path("data.xp3>system/");
    assert_eq!(prefixed.read("Config.tjs").unwrap(), b"prefixed");
}

/// A miss logs one bounded `debug` line and the full location list at `trace`:
/// the disk spellings, each auto-path candidate in later-added-first order,
/// and the mounted archives in later-first order. A hit logs nothing.
#[test]
fn miss_reports_every_searched_location() {
    let patch = build_xp3(&[("other.txt", b"x", false)]);
    let mut storage = mount_memory_with_archives(&[("patch.xp3", &patch)], &["patch.xp3"]);
    storage.add_auto_path("data.xp3>");
    storage.add_auto_path("parts/");

    let (found, logs) = capture_logs(|| storage.find("config.tjs"));
    assert_eq!(found.unwrap(), None);

    let misses: Vec<&String> = logs
        .iter()
        .filter(|line| line.contains("storage miss"))
        .collect();
    assert_eq!(misses.len(), 2, "a bounded line and a full line: {logs:?}");
    let bounded = misses
        .iter()
        .find(|line| line.starts_with("DEBUG storage miss"))
        .expect("a bounded debug line");
    let full = misses
        .iter()
        .find(|line| line.starts_with("TRACE storage miss"))
        .expect("a full trace line");
    assert!(
        bounded.starts_with("DEBUG storage miss \"config.tjs\""),
        "{bounded}"
    );
    assert!(
        bounded.contains('…') && bounded.contains(" chars)"),
        "the debug line is bounded: {bounded}"
    );
    assert!(full.contains("disk /config.tjs"), "{full}");
    assert!(full.contains("parts/config.tjs"), "{full}");
    assert!(full.contains("data.xp3"), "{full}");
    assert!(full.contains("/patch.xp3"), "{full}");

    // A hit produces no miss line.
    let (found, logs) = capture_logs(|| storage.find("other.txt"));
    assert_eq!(
        found.unwrap(),
        Some(Location::Archive(
            PathBuf::from("/patch.xp3"),
            "other.txt".into()
        ))
    );
    assert!(
        logs.iter().all(|line| !line.contains("storage miss")),
        "a hit must not log: {logs:?}"
    );
}

/// The resolver builds each auto path's candidate table once, while the
/// current-path case walk stays live: a second miss re-lists only the current
/// path, and the miss line still names every candidate.
#[test]
fn resolver_cache_lists_each_auto_path_once() {
    let xp3 = build_xp3(&[("system/Config.tjs", b"cfg", false)]);
    let fs = memory_with(&[("game/data.xp3", &xp3), ("game/parts/foo.png", b"parts")]);
    let mut storage = Storage::mount_archives(fs, "/game", ["data.xp3"]).expect("mount");
    storage.add_auto_path("data.xp3>system/");
    storage.add_auto_path("/game/parts/");

    let (found, first_logs) = capture_logs(|| storage.find("missing.txt"));
    assert_eq!(found.unwrap(), None);
    let miss_line = first_logs
        .iter()
        .find(|line| line.starts_with("TRACE storage miss"))
        .expect("a miss logs a full trace line");
    assert!(miss_line.contains("parts/missing.txt"), "{miss_line}");
    assert!(miss_line.contains("data.xp3"), "{miss_line}");
    assert!(miss_line.contains("system/missing.txt"), "{miss_line}");
    let after_first = storage.probe_counts();
    assert_eq!(after_first.misses, 1);
    assert!(
        after_first.list >= 2,
        "the two auto-path directories were listed: {after_first:?}"
    );

    // The second miss is served from the cached auto-path tables; only the
    // live disk case walk lists a directory.
    let (found, logs) = capture_logs(|| storage.find("missing.txt"));
    assert_eq!(found.unwrap(), None);
    let after_second = storage.probe_counts();
    assert_eq!(
        after_second.list,
        after_first.list + 1,
        "a second miss must re-list only the live disk walk"
    );
    assert_eq!(after_second.misses, 2);
    assert!(
        logs.iter().any(|line| line.contains("storage miss")),
        "the cached miss is still reported: {logs:?}"
    );
}

/// The startup shape from the device measurement: many auto paths and many
/// misses. The auto-path candidate table is cached (one listing per auto-path
/// directory), while the disk case walk reads live (one listing per miss) —
/// matching the reference, which caches the auto-path table but `opendir`s the
/// real directory on every lookup
/// (`reference/cpp/core/base/StorageIntf.cpp:959-1052`,
/// `reference/cpp/core/base/impl/StorageImpl.cpp:339-407`).
#[test]
fn resolver_cache_probe_count_for_startup_shape() {
    const AUTO_PATHS: usize = 41;
    const MISSES: usize = 291;

    let owned: Vec<(String, Vec<u8>)> = (0..AUTO_PATHS)
        .map(|i| (format!("game/ap{i:02}/present.dat"), b"x".to_vec()))
        .collect();
    let files: Vec<(&str, &[u8])> = owned
        .iter()
        .map(|(path, data)| (path.as_str(), data.as_slice()))
        .collect();
    let fs = memory_with(&files);
    let mut storage =
        Storage::mount_archives(fs, "/game", std::iter::empty::<&str>()).expect("mount");
    for i in 0..AUTO_PATHS {
        storage.add_auto_path(format!("ap{i:02}/"));
    }

    for i in 0..MISSES {
        let name = format!("missing{i:03}.txt");
        assert!(storage.find(&name).unwrap().is_none());
    }

    let counts = storage.probe_counts();
    // One listing per auto-path directory (the candidate table is built once)
    // plus one live listing per miss for the current-path case walk.
    assert_eq!(
        counts.list,
        (AUTO_PATHS + MISSES) as u64,
        "one listing per auto-path directory plus the live current-path walk: {counts:?}"
    );
    assert_eq!(counts.misses, MISSES as u64);
}

/// A miss that searched the 41-auto-path startup shape keeps the `debug` line
/// bounded: the head is kept and the dropped character count is named, while
/// the full list (still naming the last auto path) is available at `trace`.
#[test]
fn startup_miss_debug_line_is_bounded_but_trace_is_complete() {
    const AUTO_PATHS: usize = 41;

    let owned: Vec<(String, Vec<u8>)> = (0..AUTO_PATHS)
        .map(|i| (format!("game/ap{i:02}/present.dat"), b"x".to_vec()))
        .collect();
    let files: Vec<(&str, &[u8])> = owned
        .iter()
        .map(|(path, data)| (path.as_str(), data.as_slice()))
        .collect();
    let fs = memory_with(&files);
    let mut storage =
        Storage::mount_archives(fs, "/game", std::iter::empty::<&str>()).expect("mount");
    for i in 0..AUTO_PATHS {
        storage.add_auto_path(format!("ap{i:02}/"));
    }

    let (found, logs) = capture_logs(|| storage.find("missing000.txt"));
    assert_eq!(found.unwrap(), None);
    let debug = logs
        .iter()
        .find(|line| line.starts_with("DEBUG storage miss"))
        .expect("a bounded debug line");
    let full = logs
        .iter()
        .find(|line| line.starts_with("TRACE storage miss"))
        .expect("a full trace line");

    assert!(debug.contains('…'), "the debug line is bounded: {debug}");
    assert!(
        debug.contains(" chars)"),
        "the debug line names the dropped count: {debug}"
    );
    assert!(debug.len() < full.len(), "debug must be shorter than trace");
    assert!(full.contains("ap40/missing000.txt"), "{full}");
    assert!(full.contains("/game/missing000.txt"), "{full}");
}

/// A prefix read of a zlib entry decodes only the requested bytes: the
/// lazy-font case, where a header probe must not inflate the body.
///
/// The prefix read is our own API (the reference exposes a seekable per-member
/// stream, `tTVPXP3ArchiveStream`, instead); the segment decoding it bounds is
/// the reference's.
#[test]
fn read_prefix_is_lazy_for_a_zlib_entry() {
    let payload = vec![0u8; 1024 * 1024];
    let xp3 = build_xp3(&[("big.bin", &payload, true)]);
    let storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    assert_eq!(storage.read_bytes(), 0);
    let prefix = storage.read_prefix("big.bin", 36).unwrap();
    assert_eq!(prefix.len(), 36);
    assert_eq!(
        storage.read_bytes(),
        36,
        "a header read must not decode the whole entry"
    );

    let whole = storage.read("big.bin").unwrap();
    assert_eq!(whole.len(), payload.len());
    assert_eq!(storage.read_bytes(), 36 + whole.len() as u64);
    assert!(36 < whole.len());
}

/// An entry shorter than `max_len` is returned short; the caller decides
/// whether that is an error.
#[test]
fn read_prefix_returns_a_short_entry_short() {
    let xp3 = build_xp3(&[("tiny.bin", b"hi", false)]);
    let storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);
    assert_eq!(storage.read_prefix("tiny.bin", 64).unwrap(), b"hi");
    assert_eq!(storage.read_bytes(), 2);
}

/// An unresolvable name is `NotFound`, matching `read`.
#[test]
fn read_prefix_of_an_unresolvable_name_is_not_found() {
    let storage = mount_memory(&[("present.bin", b"x")]);
    assert!(matches!(
        storage.read_prefix("missing.bin", 16),
        Err(engine::storage::StorageReadError::NotFound(_))
    ));
}

/// The extraction filter is applied to the prefix bytes exactly as `read`
/// applies it, so the prefix equals the filtered whole-entry prefix.
///
/// Reference: the filter runs on each decoded segment
/// (`reference/cpp/core/archive/xp3/XP3Archive.cpp`); the prefix is the same
/// transform on fewer bytes.
#[test]
fn read_prefix_applies_the_extraction_filter() {
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let xp3 = build_xp3(&[("secret.bin", &payload, true)]);
    let mut storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);
    storage.xp3_extraction_filter = Some(Box::new(
        |_hash: u32,
         _offset: u64,
         buffer: &mut [u8],
         _file_name: &str,
         _archive_name: &str,
         _file_size: u64| {
            for byte in buffer.iter_mut() {
                *byte ^= 0x5A;
            }
        },
    ));

    let whole = storage.read("secret.bin").unwrap();
    let prefix = storage.read_prefix("secret.bin", 64).unwrap();
    assert_eq!(prefix, whole[..64].to_vec());
}

/// A raw archive segment is read positionally up to the limit.
#[test]
fn read_prefix_reads_a_raw_entry_positionally() {
    let payload: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    let xp3 = build_xp3(&[("raw.bin", &payload, false)]);
    let storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    assert_eq!(
        storage.read_prefix("raw.bin", 40).unwrap(),
        payload[..40].to_vec()
    );
    assert_eq!(storage.read_bytes(), 40);
    assert_eq!(storage.read("raw.bin").unwrap(), payload);
}

/// A host file prefix is bounded by the file size and is not filtered.
#[test]
fn read_prefix_reads_a_disk_file() {
    let storage = mount_memory(&[("header.bin", b"0123456789")]);
    assert_eq!(storage.read_prefix("header.bin", 4).unwrap(), b"0123");
    assert_eq!(
        storage.read_prefix("header.bin", 64).unwrap(),
        b"0123456789"
    );
    assert_eq!(storage.read_bytes(), 4 + 10);
}

/// The prefix read opens an unmounted archive on demand, like `read`.
#[test]
fn read_prefix_opens_an_archive_on_demand() {
    let xp3 = build_xp3(&[("config.tjs", b"0123456789", false)]);
    let storage = mount_memory(&[("extra.xp3", &xp3)]);
    assert_eq!(
        storage.read_prefix("extra.xp3>config.tjs", 4).unwrap(),
        b"0123"
    );
    assert_eq!(storage.read_bytes(), 4);
}

/// The preloaded backing serves the same prefix bytes as the streaming one.
#[test]
fn read_prefix_matches_for_preloaded_archives() {
    let payload: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
    let xp3 = build_xp3(&[
        ("data/raw.bin", &payload, false),
        ("data/packed.bin", &payload, true),
    ]);
    let files = [("data.xp3", xp3.as_slice())];

    let streaming = mount_memory_with_archives(&files, &["data.xp3"]);
    let fs = memory_with(&files);
    let preloaded = Storage::mount_archives_with(
        fs,
        "/",
        ["data.xp3"],
        MountOptions {
            preload_archives: true,
        },
    )
    .expect("mount preloaded");

    let want = &payload[..100];
    for name in ["data/raw.bin", "data/packed.bin"] {
        assert_eq!(streaming.read_prefix(name, 100).unwrap().as_slice(), want);
        assert_eq!(preloaded.read_prefix(name, 100).unwrap().as_slice(), want);
    }
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

/// Precedence: later-mounted archives override earlier ones in a folder
/// project, so `--archive data.xp3 --archive patch.xp3` lets `patch.xp3` win.
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

/// An archive project checks its project archive before the other mounts, so
/// the project archive wins even when another archive was mounted later. A
/// folder project keeps the later-mount rule.
///
/// Reference: the project archive is the current path and is checked before
/// the auto-path table and the other mounts
/// (`reference/cpp/core/base/StorageIntf.cpp:1061-1108`); later mounts win among
/// themselves.
#[test]
fn project_archive_is_checked_before_other_mounts() {
    let data = build_xp3(&[("shared.txt", b"from data", false)]);
    let patch = build_xp3(&[("shared.txt", b"from patch", false)]);
    let files = [
        ("data.xp3", data.as_slice()),
        ("patch.xp3", patch.as_slice()),
    ];

    let mut archive_project = mount_memory_with_archives(&files, &["data.xp3", "patch.xp3"]);
    archive_project.set_project("/data.xp3");
    assert_eq!(archive_project.read("shared.txt").unwrap(), b"from data");
    assert_eq!(
        archive_project.find("shared.txt").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/data.xp3"),
            "shared.txt".into()
        ))
    );

    let folder_project = mount_memory_with_archives(&files, &["data.xp3", "patch.xp3"]);
    assert_eq!(folder_project.read("shared.txt").unwrap(), b"from patch");
}

/// The base directory is the app path for disk assets **and** the root a
/// relative archive path resolves against, so `--base-dir /game` finds
/// `/game/data.xp3` whatever the process working directory is.
///
/// Reference: the app path is a single directory used for both archive lookup
/// and loose assets (`TVPGetAppPath`,
/// `reference/cpp/core/base/impl/StorageImpl.cpp:505`).
#[test]
fn archive_resolves_relative_to_base_dir() {
    let xp3 = build_xp3(&[("startup.tjs", b"// startup", false)]);
    let fs = memory_with(&[("game/data.xp3", &xp3)]);
    let storage = Storage::mount_archives(fs, "/game", ["data.xp3"]).expect("mount");

    assert_eq!(storage.archive_count(), 1);
    assert!(storage.exists("startup.tjs").unwrap());
}

/// Mounting never enumerates the game directory: an archive present on disk
/// is ignored until the caller names it explicitly. The reference has no
/// `*.xp3` folder scan either — `TVPCheckStartupArg` only inspects `argv[1]`
/// (`reference/cpp/core/environ/win32/Platform.cpp:93`).
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

/// `Storage` owns the XP3 extraction filter and hands it to every archive
/// read; installing and clearing it changes the bytes a `read` returns while
/// `find`/`stat` stay filter-free.
#[test]
fn xp3_extraction_filter_decrypts_archive_reads() {
    use std::cell::RefCell;

    let key = 0x5Au8;
    let plain = b"decrypted archive payload".to_vec();
    let cipher: Vec<u8> = plain.iter().map(|byte| byte ^ key).collect();
    let xp3 = build_xp3(&[("secret.bin", &cipher, false)]);
    let mut storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    // No filter: the stored bytes come back unchanged.
    assert_eq!(storage.read("secret.bin").unwrap(), cipher);

    let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
    let seen_in_filter = std::rc::Rc::clone(&seen);
    storage.xp3_extraction_filter = Some(Box::new(
        move |file_hash: u32,
              offset: u64,
              buffer: &mut [u8],
              file_name: &str,
              archive_name: &str,
              file_size: u64| {
            seen_in_filter.borrow_mut().push((
                file_hash,
                offset,
                file_name.to_string(),
                archive_name.to_string(),
                file_size,
            ));
            for byte in buffer.iter_mut() {
                *byte ^= key;
            }
        },
    ));

    assert_eq!(storage.read("secret.bin").unwrap(), plain);
    let seen = seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, 0, "build_xp3 writes a zero adlr hash");
    assert_eq!(seen[0].1, 0);
    assert_eq!(seen[0].2, "secret.bin");
    assert_eq!(seen[0].3, "data.xp3");
    assert_eq!(seen[0].4, plain.len() as u64);
    drop(seen);

    // Metadata is read from the index and is unaffected by the filter.
    assert_eq!(
        storage.stat("secret.bin").unwrap().unwrap().size,
        plain.len() as u64
    );

    // Clearing the filter restores the raw read.
    storage.xp3_extraction_filter = None;
    assert_eq!(storage.read("secret.bin").unwrap(), cipher);
}

/// The extraction filter receives the **normalized** in-archive name: the
/// archive is opened with `normalizeName=true`, so `Owner->GetName` returns the
/// name `Init` normalized.
// Reference: `reference/cpp/core/archive/xp3/XP3Archive.cpp:435-437`
// (`NormalizeInArchiveStorageName(item.Name)`) and `:996-1002`
// (`tTVPXP3ExtractionFilterInfo(..., Owner->GetName(StorageIndex))`).
#[test]
fn xp3_extraction_filter_receives_the_normalized_name() {
    use std::cell::RefCell;

    let xp3 = build_xp3(&[(r"Bg\Title.JPG", b"payload", false)]);
    let mut storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    let seen = std::rc::Rc::new(RefCell::new(String::new()));
    let seen_in_filter = std::rc::Rc::clone(&seen);
    storage.xp3_extraction_filter = Some(Box::new(
        move |_hash: u32,
              _offset: u64,
              _buffer: &mut [u8],
              file_name: &str,
              _archive_name: &str,
              _file_size: u64| {
            *seen_in_filter.borrow_mut() = file_name.to_string();
        },
    ));

    assert_eq!(storage.read(r"Bg\Title.JPG").unwrap(), b"payload");
    assert_eq!(&*seen.borrow(), "bg/title.jpg");
}

/// One unreadable archive in a scanned list is skipped with a warning so the
/// rest of the game still mounts.
// Reference: the app-path probe `TVPCheckArchive` catches `eTJSError`, logs,
// and reports "not archive" (`reference/cpp/core/base/impl/StorageImpl.cpp:562-587`).
#[test]
fn mount_discovered_archives_skips_a_bad_archive() {
    let good = build_xp3(&[("keep.bin", b"keep", false)]);
    let fs = memory_with(&[("game/good.xp3", &good), ("game/bad.xp3", b"nope")]);

    let storage =
        Storage::mount_discovered_archives(fs, "/game", ["good.xp3", "bad.xp3"]).expect("mount");
    assert_eq!(storage.archive_count(), 1);
    assert!(storage.exists("keep.bin").unwrap());
}

/// An explicitly requested archive that cannot be parsed is a typed error.
#[test]
fn mount_archives_reports_a_typed_error() {
    let good = build_xp3(&[("keep.bin", b"keep", false)]);
    let fs = memory_with(&[("game/good.xp3", &good), ("game/bad.xp3", b"nope")]);

    assert!(matches!(
        Storage::mount_archives(fs, "/game", ["good.xp3", "bad.xp3"]),
        Err(engine::storage::MountError::Xp3(_))
    ));
}

/// `Storage::mount_archive` (the single-archive call) still surfaces the parse
/// error as a typed value.
#[test]
fn mount_archive_still_reports_a_typed_error() {
    let fs = memory_with(&[("bad.xp3", b"nope")]);
    let mut storage =
        Storage::mount_archives(fs, "/", std::iter::empty::<&str>()).expect("empty mount");
    assert!(matches!(
        storage.mount_archive("bad.xp3"),
        Err(engine::storage::MountError::Xp3(_))
    ));
}

/// A folder project checks the app directory before the auto-path list, so a
/// loose file wins over a later-added auto path.
///
/// Reference: the current folder is checked before the auto-path table
/// (`reference/cpp/core/base/StorageIntf.cpp:1061-1108`).
#[test]
fn folder_project_disk_wins_over_auto_path() {
    let fs = memory_with(&[
        ("game/Config.tjs", b"disk"),
        ("game/chs/Config.tjs", b"auto"),
    ]);
    let mut storage =
        Storage::mount_archives(fs, "/game", std::iter::empty::<&str>()).expect("mount");
    storage.add_auto_path("chs/");

    assert_eq!(storage.read("Config.tjs").unwrap(), b"disk");
    assert_eq!(
        storage.find("Config.tjs").unwrap(),
        Some(Location::Disk(PathBuf::from("/game/Config.tjs")))
    );
}

/// A folder project checks the auto-path list before the mounted archives, so
/// a later-added folder auto path wins over an archive entry with the same
/// base name.
///
/// Reference: `TVPGetPlacedPath` follows the current folder with the auto-path
/// table (`reference/cpp/core/base/StorageIntf.cpp:1061-1108`).
#[test]
fn later_added_auto_path_wins_over_a_mounted_archive() {
    let patch = build_xp3(&[("foo.png", b"from patch", false)]);
    let mut storage = mount_memory_with_archives(
        &[("patch.xp3", &patch), ("parts/foo.png", b"from parts")],
        &["patch.xp3"],
    );
    storage.add_auto_path("patch.xp3>");
    storage.add_auto_path("/parts/");

    assert_eq!(storage.read("foo.png").unwrap(), b"from parts");
    assert_eq!(
        storage.find("foo.png").unwrap(),
        Some(Location::Disk(PathBuf::from("/parts/foo.png")))
    );
}

/// An archive project checks its project archive before the auto-path list,
/// so the project archive's entry wins over a later-added folder auto path.
///
/// Reference: the project archive is the current path and is checked before
/// the auto-path table (`reference/cpp/core/base/StorageIntf.cpp:1061-1108`).
#[test]
fn archive_project_wins_over_a_later_auto_path() {
    let patch = build_xp3(&[("foo.png", b"from patch", false)]);
    let mut storage = mount_memory_with_archives(
        &[("patch.xp3", &patch), ("parts/foo.png", b"from parts")],
        &["patch.xp3"],
    );
    storage.set_project("/patch.xp3");
    storage.add_auto_path("patch.xp3>");
    storage.add_auto_path("/parts/");

    assert_eq!(storage.read("foo.png").unwrap(), b"from patch");
    assert_eq!(
        storage.find("foo.png").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/patch.xp3"),
            "foo.png".into()
        ))
    );
}

/// Auto paths are searched in "later added wins" order among themselves, so a
/// folder added after an earlier one overrides it.
///
/// Reference: `TVPGetPlacedPath` resolves through the auto-path table, which
/// the later `TVPAddAutoPath` entry overwrites
/// (`reference/cpp/core/base/StorageIntf.cpp:921-937`, `:1061-1108`).
#[test]
fn later_added_auto_path_wins_over_an_earlier_one() {
    let fs = memory_with(&[
        ("game/early/Config.tjs", b"early"),
        ("game/late/Config.tjs", b"late"),
    ]);
    let mut storage =
        Storage::mount_archives(fs, "/game", std::iter::empty::<&str>()).expect("mount");
    storage.add_auto_path("early/");
    storage.add_auto_path("late/");

    assert_eq!(storage.read("Config.tjs").unwrap(), b"late");
    assert_eq!(
        storage.find("Config.tjs").unwrap(),
        Some(Location::Disk(PathBuf::from("/game/late/Config.tjs")))
    );
}

/// An auto-path hit in an archive project wins over the app-directory
/// fallback, which is searched after the auto paths and the other mounts.
#[test]
fn auto_path_wins_over_disk_in_an_archive_project() {
    let project = build_xp3(&[("other.txt", b"project", false)]);
    let fs = memory_with(&[
        ("game/project.xp3", &project),
        ("game/Config.tjs", b"disk"),
        ("game/chs/Config.tjs", b"auto"),
        ("game/loose.txt", b"loose"),
    ]);
    let mut storage = Storage::mount_archives(fs, "/game", ["project.xp3"]).expect("mount");
    storage.set_project("/game/project.xp3");
    storage.add_auto_path("/game/chs/");

    assert_eq!(storage.read("Config.tjs").unwrap(), b"auto");
    assert_eq!(
        storage.find("Config.tjs").unwrap(),
        Some(Location::Disk(PathBuf::from("/game/chs/Config.tjs")))
    );
    // A loose app-directory file is still reachable as the last fallback.
    assert_eq!(storage.read("loose.txt").unwrap(), b"loose");
}

/// An archive named by an auto path or by explicit `arc.xp3>` addressing is
/// opened on demand even when it was never mounted.
// Reference: `TVPArchiveCache::Get` opens the archive named in the path when it
// exists (`reference/cpp/core/base/StorageIntf.cpp:704-724`);
// `TVPIsExistentStorageNoSearchNoNormalize` resolves the in-archive lookup
// through the same cache (`:737-764`).
#[test]
fn archive_is_opened_on_demand() {
    let extra = build_xp3(&[("config.tjs", b"cfg", false)]);
    let mut storage = mount_memory(&[("extra.xp3", &extra)]);
    storage.add_auto_path("extra.xp3>");

    assert_eq!(storage.read("config.tjs").unwrap(), b"cfg");
    assert_eq!(
        storage.find("config.tjs").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/extra.xp3"),
            "config.tjs".into()
        ))
    );
    // Explicit addressing opens the same on-disk archive too.
    assert_eq!(storage.read("extra.xp3>config.tjs").unwrap(), b"cfg");
}

/// With `preload_archives` set the whole archive image is held in memory, but
/// every entry must read exactly the bytes the streaming path returns. This is
/// the correctness question for preloading: an off-by-one in the index or a
/// partial read would otherwise surface much later as a corrupt asset.
///
/// Reference: both paths run the same whole-entry read, which concatenates the
/// segments and inflates zlib ones (`tTVPXP3ArchiveStream::Read`,
/// `reference/cpp/core/archive/xp3/XP3Archive.cpp:996-1002`); preloading only
/// changes where the bytes come from.
#[test]
fn preloaded_archive_reads_match_the_streaming_path() {
    let payload: Vec<u8> = (0..96 * 1024).map(|i| (i % 251) as u8).collect();
    let xp3 = build_xp3(&[
        ("data/raw.bin", &payload, false),
        ("data/packed.bin", &payload, true),
        ("small.txt", b"hello", false),
    ]);
    let files = [("data.xp3", xp3.as_slice())];

    let streaming = mount_memory_with_archives(&files, &["data.xp3"]);
    let fs = memory_with(&files);
    let preloaded = Storage::mount_archives_with(
        fs,
        "/",
        ["data.xp3"],
        MountOptions {
            preload_archives: true,
        },
    )
    .expect("mount preloaded");

    for name in ["data/raw.bin", "data/packed.bin", "small.txt"] {
        assert_eq!(
            streaming.read(name).unwrap(),
            preloaded.read(name).unwrap(),
            "{name} differs between the streaming and preloaded paths"
        );
    }
    assert_eq!(preloaded.read("data/packed.bin").unwrap(), payload);
}

/// The `xp3filter` extraction seam rewrites decoded segments in place for a
/// preloaded archive exactly as it does for a streaming one.
#[test]
fn preloading_preserves_the_extraction_filter() {
    let key = 0x5Au8;
    let plain = b"decrypted archive payload".to_vec();
    let cipher: Vec<u8> = plain.iter().map(|byte| byte ^ key).collect();
    let xp3 = build_xp3(&[("secret.bin", &cipher, false)]);
    let files = [("data.xp3", xp3.as_slice())];

    let mut streaming = mount_memory_with_archives(&files, &["data.xp3"]);
    streaming.xp3_extraction_filter = Some(Box::new(
        move |_hash: u32,
              _offset: u64,
              buffer: &mut [u8],
              _name: &str,
              _archive: &str,
              _size: u64| {
            for byte in buffer.iter_mut() {
                *byte ^= key;
            }
        },
    ));

    let fs = memory_with(&files);
    let mut preloaded = Storage::mount_archives_with(
        fs,
        "/",
        ["data.xp3"],
        MountOptions {
            preload_archives: true,
        },
    )
    .expect("mount preloaded");
    preloaded.xp3_extraction_filter = Some(Box::new(
        move |_hash: u32,
              _offset: u64,
              buffer: &mut [u8],
              _name: &str,
              _archive: &str,
              _size: u64| {
            for byte in buffer.iter_mut() {
                *byte ^= key;
            }
        },
    ));

    assert_eq!(streaming.read("secret.bin").unwrap(), plain);
    assert_eq!(preloaded.read("secret.bin").unwrap(), plain);
}

/// Preloading does not change mount precedence: the later-mounted archive
/// still wins, and resolution still reports the winning archive.
#[test]
fn preloading_does_not_change_mount_precedence() {
    let data = build_xp3(&[("shared.txt", b"from data", false)]);
    let patch = build_xp3(&[("shared.txt", b"from patch", false)]);
    let files = [
        ("data.xp3", data.as_slice()),
        ("patch.xp3", patch.as_slice()),
    ];

    let fs = memory_with(&files);
    let storage = Storage::mount_archives_with(
        fs,
        "/",
        ["data.xp3", "patch.xp3"],
        MountOptions {
            preload_archives: true,
        },
    )
    .expect("mount");

    assert_eq!(storage.read("shared.txt").unwrap(), b"from patch");
    assert_eq!(
        storage.find("shared.txt").unwrap(),
        Some(Location::Archive(
            PathBuf::from("/patch.xp3"),
            "shared.txt".into()
        ))
    );
}

/// Reading an entry out of an in-memory image is bounds-checked: an image too
/// short for a segment is a typed corruption error, not an opaque EOF.
#[test]
fn reading_an_in_memory_image_rejects_an_overrunning_segment() {
    let xp3 = build_xp3(&[("data/a.bin", b"payload", false)]);
    let fs = memory_with(&[("data.xp3", &xp3)]);
    let file = fs::open(
        &fs,
        &api::fs::FsPath::host("/data.xp3"),
        api::fs::OpenMode::Read,
    )
    .expect("open archive");
    let archive = fs::xp3::Xp3Archive::open(&fs, &file).expect("parse archive");
    let entry = archive.entry("data/a.bin").cloned().expect("entry");

    let err = archive
        .read_entry_from_bytes(&[], &entry, "data.xp3", None)
        .expect_err("segment overruns an empty image");
    assert!(
        matches!(err, fs::xp3::Xp3Error::CorruptIndex(_)),
        "unexpected error: {err:?}"
    );
}
