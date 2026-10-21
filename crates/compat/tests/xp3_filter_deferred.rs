//! The deferred `xp3filter` activation path, isolated in its own test binary.
//!
//! # Why this test is in its own file
//!
//! The compat binding is process-global and single (AGENTS §9.7), and the
//! shared harness in `common::Env` now installs it (and calls
//! `activate_deferred_plugins`, which completes the deferred plugin work).
//! This test must be the **first** thing in its process to call
//! `CompatHost::install`, because it needs to observe the state where the
//! binding is installed and the storage is attached *afterwards* — the
//! late-attach case the deferred hook
//! exists for. If it shared a binary with the `common::Env` tests, the
//! harness's `install` would already have happened, this test's own `install`
//! would be a no-op on the occupied binding, its `attach_storage` would never
//! take effect, and the archive member would look absent. Do not move it back
//! into `xp3_filter.rs`.

mod common;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use testkit::build_xp3;
use tjs::Tjs2Engine;

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-xp3filter-deferred-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp game dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn xor(data: &[u8], key: u8) -> Vec<u8> {
    data.iter().map(|byte| byte ^ key).collect()
}

// The composition root may attach the L2 storage after `CompatHost::install`,
// so `xp3filter.tjs` is not readable at registration. The reference reads the
// script in its post-registration callback before the game runs
// (`reference/cpp/plugins/xp3filter.cpp:550-564`); the port completes that
// read at `activate_deferred_plugins`, which must still install the seam
// before the first archive read, or a filtered archive's content stays
// encrypted. (Production attaches before install, so registration already
// completes it; this is the safety-net path for an embedder that attaches
// late, and it must keep working.)
#[test]
fn storage_attached_after_install_is_completed_before_startup() {
    let _lock = vm_lock();
    let key = 0x5Au8;
    let plain = b"deferred activation decrypts";
    let cipher = xor(plain, key);
    let script = format!(
        "function xp3extract(file_hash, offset, buffer, buffer_size, file_name, ctx) {{\n\
         \x20\x20for (var i = 0; i < buffer_size; i++) buffer[i] = buffer[i] ^ {key};\n\
         }}\n\
         Storages.setXP3ArchiveExtractionFilter(xp3extract);\n"
    );

    let dir = GameDir::new();
    std::fs::write(dir.path().join("xp3filter.tjs"), script).expect("write filter script");
    std::fs::write(
        dir.path().join("data.xp3"),
        build_xp3(&[("secret.txt", &cipher, false)]),
    )
    .expect("write archive");

    let mut storage = Box::new(
        Storage::mount_archives(
            FileSystem::host(dir.path()),
            dir.path().to_path_buf(),
            ["data.xp3"],
        )
        .expect("mount storage"),
    );
    let engine = Box::new(Tjs2Engine::new().expect("create engine"));
    let mut host = CompatHost::new();
    // Install first, attach the storage afterwards: the ordering the app's
    // composition root uses. The registration-time read has no storage here.
    host.install(&engine).expect("install compat surface");
    // SAFETY: `storage` is boxed and outlives the binding, which `host`
    // removes on drop.
    unsafe {
        host.services_mut()
            .attach_storage(storage.as_mut() as *mut Storage);
    }
    // The named lifecycle step completes the deferred `xp3filter.tjs` read and
    // seam install; a read failure would surface here.
    host.activate_deferred_plugins()
        .expect("deferred plugin activation");
    // `Storages.exportFile` is published by `fstat.dll`
    // (`reference/cpp/plugins/fstat/main.cpp:913-925`), so the plugin must be
    // linked before the archive member is read back.
    common::link_plugin(&engine, "fstat.dll");

    let dest = dir.path().join("out.bin");
    engine
        .exec_script(
            &format!("Storages.exportFile(\"secret.txt\", {dest:?});"),
            "xp3-filter-test",
        )
        .expect("export filtered entry");
    let exported = std::fs::read(&dest).expect("read exported file");
    assert_eq!(exported, plain);

    drop(host);
    drop(engine);
    drop(storage);
}
