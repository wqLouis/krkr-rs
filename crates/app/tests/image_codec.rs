//! End-to-end test for the `media`-backed image codec the app installs on the
//! compat host.
//!
//! `compat` never links `media`, so `Bitmap.save` is only real once the host
//! fills the `set_image_codec` seam. This test installs that seam through
//! [`app::install_image_codec`], loads a PNG through the same seam, saves it
//! back to a different storage path, and decodes the written file with the
//! L1 `media` decoder — proving the whole encode → write → decode path.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use api::fs::{FsPath, OpenMode};
use app::install_image_codec;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use media::{Media, decode_image, encode_image};
use tjs::Tjs2Engine;

/// Serializes the process-global TJS VM and its single engine binding.
fn vm_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

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
