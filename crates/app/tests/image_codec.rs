//! End-to-end test for the `media`-backed image codec the app installs on the
//! compat host.
//!
//! `compat` never links `media`, so `Bitmap.save` is only real once the host
//! fills the `set_image_codec` seam. This test installs that seam through
//! [`app::install_image_codec`], loads a PNG through the same seam, saves it
//! back to a different storage path, and decodes the written file with the
//! L1 `media` decoder — proving the whole encode → write → decode path.

use std::path::Path;

use api::fs::{FsPath, OpenMode};
use app::install_image_codec;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use media::{Media, decode_image, encode_image};
use testkit::vm_lock;
use tjs::Tjs2Engine;

/// Deterministic RGBA8 pattern used as the source image.
fn pattern(width: u32, height: u32) -> Vec<u8> {
    (0..width * height)
        .flat_map(|i| {
            let x = i % width;
            let y = i / width;
            [(x * 9) as u8, (y * 17) as u8, 77, 255]
        })
        .collect()
}

/// Writes `data` to `path` in an in-memory file system, creating parents.
fn write_memory_file(fs: &FileSystem, path: &str, data: &[u8]) {
    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(fs, &FsPath::host(parent)).expect("create parent dirs");
    }
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
    fs::write_at(fs, &file, 0, data).expect("write memory file");
    fs::flush(fs, &file).expect("flush memory file");
}

#[test]
fn bitmap_save_writes_a_decodable_file_through_the_app_installed_codec() {
    let _lock = vm_lock();

    let (width, height) = (5u32, 3u32);
    let rgba = pattern(width, height);
    let source = encode_image(&Media::new(), width, height, &rgba, "png").expect("encode source");

    let fs = FileSystem::memory();
    write_memory_file(&fs, "/images/source.png", &source);
    let mut storage = Box::new(
        Storage::mount_archives(fs, "/", std::iter::empty::<&str>()).expect("mount storage"),
    );

    let engine = Box::new(Tjs2Engine::new().expect("create engine"));
    let mut scene = Box::new(Scene::default());
    let mut host = CompatHost::new();
    install_image_codec(&host, &Media::new(), &storage);
    // SAFETY: the boxed scene/storage outlive `host` (they are declared after
    // it and drop later), and `host.install` only stores their addresses.
    unsafe {
        host.services_mut()
            .attach_scene(scene.as_mut() as *mut Scene);
        host.services_mut()
            .attach_storage(storage.as_mut() as *mut Storage);
    }
    host.install(&engine).expect("install compat surface");

    engine
        .exec_script(
            "var b = new Bitmap(); \
             b.load('images/source.png'); \
             b.save('out/shots/shot.png', 'png');",
            "image-codec-test",
        )
        .expect("run the load/save script");

    // The save closure wrote through the storage's file system, so the L2
    // reader can see the new file.
    let saved = storage.read("out/shots/shot.png").expect("read saved file");
    let decoded =
        decode_image(&Media::new(), &saved, Some("out/shots/shot.png")).expect("decode saved PNG");
    assert_eq!((decoded.width, decoded.height), (width, height));
    assert_eq!(
        decoded.rgba, rgba,
        "the saved PNG must round-trip the bitmap pixels"
    );
}

/// A save name may be the fully lowercased absolute form `System.dataPath`
/// produces. The write must land on the real mixed-case on-disk path, not on a
/// second lowercase tree; the reference resolves the name through
/// `TVPGetLocallyAccessibleName` before opening it
/// (`reference/cpp/core/base/StorageIntf.cpp:240-430`).
#[test]
fn bitmap_save_resolves_a_lowercased_absolute_name_to_the_real_path() {
    let _lock = vm_lock();

    let unique = format!(
        "krkr-image-codec-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    let root = std::env::temp_dir().join(unique).join("MixedCaseGame");
    std::fs::create_dir_all(root.join("SaveData")).expect("create game dir");

    let rgba = pattern(2, 2);
    let source = encode_image(&Media::new(), 2, 2, &rgba, "png").expect("encode source");
    std::fs::write(root.join("source.png"), source).expect("write source image");

    let fs = FileSystem::host(&root);
    let mut storage = Box::new(
        Storage::mount_archives(fs, root.clone(), std::iter::empty::<&str>())
            .expect("mount storage"),
    );
    let engine = Box::new(Tjs2Engine::new().expect("create engine"));
    let mut scene = Box::new(Scene::default());
    let mut host = CompatHost::new();
    install_image_codec(&host, &Media::new(), &storage);
    // SAFETY: the boxed scene/storage outlive `host` (they are declared after
    // it and drop later), and `host.install` only stores their addresses.
    unsafe {
        host.services_mut()
            .attach_scene(scene.as_mut() as *mut Scene);
        host.services_mut()
            .attach_storage(storage.as_mut() as *mut Storage);
    }
    host.install(&engine).expect("install compat surface");

    let target = root.join("SaveData/shot.png");
    let lower = format!("file://.{}", target.to_string_lossy().to_ascii_lowercase());
    engine
        .exec_script(
            &format!("var b = new Bitmap(); b.load('source.png'); b.save({lower:?}, 'png');"),
            "image-codec-test",
        )
        .expect("run the load/save script");

    let saved = std::fs::read(&target).expect("the save must use the real mixed-case path");
    decode_image(&Media::new(), &saved, Some("SaveData/shot.png")).expect("decode saved PNG");
    assert!(
        !root.join("savedata").exists(),
        "the save created a second lowercase tree"
    );

    std::fs::remove_dir_all(root.parent().expect("temporary root")).expect("remove temp game");
}
