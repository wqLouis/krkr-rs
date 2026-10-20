//! Integration tests for L2 startup-script discovery.
//!
//! Discovery is pure storage semantics, so these tests exercise it without
//! the TJS VM. Order per `docs.old/migration/09-game-loading.md` §5:
//! `patch.tjs` (app path) → `startup.tjs` (search) → `AfterStartup.tjs`
//! (app path) → `system/Initialize.tjs` fallback.

use engine::startup::StartupScript;
use engine::{Location, StartupError, discover_startup};
use testkit::build_xp3;

mod common;
use common::{mount_memory, mount_memory_with_archives};

#[test]
fn disk_startup_with_patch_and_after() {
    let storage = mount_memory(&[
        ("patch.tjs", b"// patch"),
        ("startup.tjs", b"// startup"),
        ("AfterStartup.tjs", b"// after"),
    ]);

    let scripts = discover_startup(&storage).expect("discover");
    assert_eq!(scripts.patch, Some(Location::Disk("/patch.tjs".into())));
    assert_eq!(
        scripts.startup,
        StartupScript::Startup(Location::Disk("/startup.tjs".into()))
    );
    assert_eq!(
        scripts.after_startup,
        Some(Location::Disk("/AfterStartup.tjs".into()))
    );
    assert!(!scripts.startup.is_initialize_fallback());
}

#[test]
fn startup_inside_archive_is_searched() {
    let xp3 = build_xp3(&[("startup.tjs", b"// startup", false)]);
    let storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    let scripts = discover_startup(&storage).expect("discover");
    assert_eq!(
        scripts.startup,
        StartupScript::Startup(Location::Archive("/data.xp3".into(), "startup.tjs".into()))
    );
    assert_eq!(scripts.patch, None);
    assert_eq!(scripts.after_startup, None);
}

#[test]
fn initialize_fallback_when_startup_absent() {
    let storage = mount_memory(&[("system/Initialize.tjs", b"// init")]);

    let scripts = discover_startup(&storage).expect("discover");
    assert_eq!(
        scripts.startup,
        StartupScript::Initialize(Location::Disk("/system/Initialize.tjs".into()))
    );
    assert!(scripts.startup.is_initialize_fallback());
    assert_eq!(
        scripts.startup.location(),
        &Location::Disk("/system/Initialize.tjs".into())
    );
}

#[test]
fn missing_startup_is_a_typed_error() {
    let storage = mount_memory(&[("readme.txt", b"nothing to run")]);
    assert!(matches!(
        discover_startup(&storage),
        Err(StartupError::NoStartupScript)
    ));
}

#[test]
fn patch_and_after_are_app_path_only() {
    let xp3 = build_xp3(&[
        ("patch.tjs", b"// archived patch", false),
        ("AfterStartup.tjs", b"// archived after", false),
        ("startup.tjs", b"// startup", false),
    ]);
    let storage = mount_memory_with_archives(&[("data.xp3", &xp3)], &["data.xp3"]);

    let scripts = discover_startup(&storage).expect("discover");
    // The archive copies are ignored: these two are not searched.
    assert_eq!(scripts.patch, None);
    assert_eq!(scripts.after_startup, None);
    assert_eq!(
        scripts.startup,
        StartupScript::Startup(Location::Archive("/data.xp3".into(), "startup.tjs".into()))
    );
}

/// In a folder project the searched `startup.tjs` resolves disk-first, so a
/// loose file wins over the copy inside a mounted archive. The app directory
/// is the reference's current path for a folder launch
/// (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`) and is checked
/// before the auto-path table (`reference/cpp/core/base/StorageIntf.cpp:1061-1108`).
#[test]
fn disk_startup_wins_over_archive() {
    let xp3 = build_xp3(&[("startup.tjs", b"// archived", false)]);
    let storage = mount_memory_with_archives(
        &[("data.xp3", &xp3), ("startup.tjs", b"// disk")],
        &["data.xp3"],
    );

    let scripts = discover_startup(&storage).expect("discover");
    assert_eq!(
        scripts.startup,
        StartupScript::Startup(Location::Disk("/startup.tjs".into()))
    );
}

/// In an archive project the project archive is the current path, so its
/// `startup.tjs` wins over a loose copy on disk. The reference appends the
/// archive delimiter to the project path before setting it as the current
/// directory (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`) and
/// checks that current storage before the auto-path table
/// (`reference/cpp/core/base/StorageIntf.cpp:1061-1108`).
#[test]
fn archive_startup_wins_over_disk() {
    let xp3 = build_xp3(&[("startup.tjs", b"// archived", false)]);
    let mut storage = mount_memory_with_archives(
        &[("data.xp3", &xp3), ("startup.tjs", b"// disk")],
        &["data.xp3"],
    );
    storage.set_project("/data.xp3");

    let scripts = discover_startup(&storage).expect("discover");
    assert_eq!(
        scripts.startup,
        StartupScript::Startup(Location::Archive("/data.xp3".into(), "startup.tjs".into()))
    );
}

/// `startup.tjs` is preferred over the `system/Initialize.tjs` fallback when
/// both exist (reference `TVPExecuteStartupScript`,
/// `reference/cpp/core/base/ScriptMgnIntf.cpp:900`).
#[test]
fn startup_wins_over_initialize_fallback() {
    let storage = mount_memory(&[
        ("startup.tjs", b"// startup"),
        ("system/Initialize.tjs", b"// init"),
    ]);

    let scripts = discover_startup(&storage).expect("discover");
    assert_eq!(
        scripts.startup,
        StartupScript::Startup(Location::Disk("/startup.tjs".into()))
    );
}
