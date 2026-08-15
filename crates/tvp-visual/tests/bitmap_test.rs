//! Integration tests for `crates/tvp-visual/src/bitmap.rs`.
//!
//! Storage-backed tests mount `tests/fixtures/` (a committed game-image
//! fixture: `frm_06r97.webp`, extracted from the real `data.xp3` archive)
//! as the game directory, and a throwaway temp directory for encode
//! round-trips.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use engine::Storage;
use image::{ExtendedColorType, ImageEncoder};
use tvp_visual::bitmap::{BitmapError, add_blank_bitmap, load_bitmap_from_storage};
use tvp_visual::scene::{BitmapCache, Scene};

/// The committed real-game webp fixture: `thumb/frm_06r97.webp` from
/// `data.xp3` (84x90, VP8 with alpha, 1336 bytes).
const FIXTURE_WEBP: &str = "frm_06r97.webp";
const FIXTURE_W: u32 = 84;
const FIXTURE_H: u32 = 90;

/// Mount the committed fixture directory as game storage.
fn fixture_storage() -> Storage {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    Storage::mount(&dir).expect("fixture dir must mount")
}

/// A unique scratch directory that cleans itself up on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "tvp-visual-bitmap-test-{tag}-{}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Encode an RGBA buffer with the image crate and write it to `dir/name`.
fn write_encoded(dir: &Path, name: &str, rgba: &[u8], w: u32, h: u32, fmt: image::ImageFormat) {
    let mut bytes = Vec::new();
    match fmt {
        image::ImageFormat::Png => {
            image::codecs::png::PngEncoder::new(&mut bytes)
                .write_image(rgba, w, h, ExtendedColorType::Rgba8)
                .expect("png encode");
        }
        image::ImageFormat::Jpeg => {
            // JPEG has no alpha channel: drop it before encoding.
            let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| p[..3].to_vec()).collect();
            image::codecs::jpeg::JpegEncoder::new(&mut bytes)
                .encode(&rgb, w, h, ExtendedColorType::Rgb8)
                .expect("jpeg encode");
        }
        image::ImageFormat::WebP => {
            image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
                .encode(rgba, w, h, ExtendedColorType::Rgba8)
                .expect("webp encode");
        }
        other => panic!("unsupported test format {other:?}"),
    }
    fs::write(dir.join(name), bytes).expect("write encoded image");
}

#[test]
fn decodes_real_game_webp() {
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    let mut storage = fixture_storage();

    let id = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, FIXTURE_WEBP, None)
        .expect("real game webp must decode");
    let bmp = scene.bitmap(id).expect("bitmap registered");
    assert_eq!(bmp.width, FIXTURE_W);
    assert_eq!(bmp.height, FIXTURE_H);
    assert_eq!(bmp.rgba.len(), (FIXTURE_W * FIXTURE_H * 4) as usize);
    assert!(bmp.dirty, "freshly loaded bitmap must be dirty");
    assert_eq!(bmp.name.as_deref(), Some(FIXTURE_WEBP));
    // Real content: has alpha data and is not a uniform blank.
    assert!(
        bmp.rgba.iter().any(|&p| p != 0),
        "fixture must have content"
    );
}

#[test]
fn blank_bitmap_is_transparent_black() {
    let mut scene = Scene::default();

    let id = add_blank_bitmap(&mut scene, 64, 64);
    let bmp = scene.bitmap(id).expect("blank bitmap registered");
    assert_eq!((bmp.width, bmp.height), (64, 64));
    assert!(bmp.dirty, "blank bitmap must be dirty");
    assert_eq!(bmp.name, None, "blank bitmaps have no storage name");
    assert_eq!(bmp.rgba.len(), 64 * 64 * 4);
    assert!(
        bmp.rgba.iter().all(|&p| p == 0),
        "blank bitmap is transparent black (RGBA all zero)"
    );

    // Zero sizes clamp to 1x1, matching the reference SetSize.
    let id0 = add_blank_bitmap(&mut scene, 0, 0);
    let bmp0 = scene.bitmap(id0).unwrap();
    assert_eq!((bmp0.width, bmp0.height), (1, 1));
    assert_eq!(bmp0.rgba.len(), 4);
}

#[test]
fn extension_probing_finds_extensionless_name() {
    // The fixture is stored as `frm_06r97.webp`; `Bitmap("frm_06r97")` must
    // find it via the probe list, exactly like `Bitmap("FRM_0501b")` finds
    // `FRM_0501b.webp` in the reference.
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    let mut storage = fixture_storage();

    let id = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, "frm_06r97", None)
        .expect("extensionless query resolves to .webp");
    let bmp = scene.bitmap(id).unwrap();
    assert_eq!((bmp.width, bmp.height), (FIXTURE_W, FIXTURE_H));
    assert_eq!(bmp.name.as_deref(), Some(FIXTURE_WEBP));
}

#[test]
fn cache_returns_same_id_for_same_file() {
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    let mut storage = fixture_storage();

    // First load resolves `frm_06r97` → `frm_06r97.webp` and caches it
    // under the resolved name; the second query (with the explicit
    // extension) must hit that cache entry and return the same id — the
    // reference shares cached bitmaps by storage name.
    let a = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, "frm_06r97", None)
        .expect("load 1");
    let b = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, FIXTURE_WEBP, None)
        .expect("load 2");
    assert_eq!(a, b, "same file must map to one shared bitmap");
    assert_eq!(scene.bitmaps.len(), 1, "no duplicate bitmap registered");
    assert!(cache.by_name.contains_key(FIXTURE_WEBP));
}

#[test]
fn not_found_errors() {
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    let mut storage = fixture_storage();

    let err = load_bitmap_from_storage(
        &mut scene,
        &mut cache,
        &mut storage,
        "nope_does_not_exist",
        None,
    )
    .expect_err("missing file must fail");
    match err {
        BitmapError::NotFound(n) => assert!(
            n.contains("nope_does_not_exist"),
            "error mentions the missing name: {n}"
        ),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn case_insensitive_queries_alias() {
    // Storage names are case-insensitive (the engine normalizes every
    // lookup); the module normalizes its cache key the same way, so an
    // all-caps query aliases to the earlier lowercase load.
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    let mut storage = fixture_storage();

    let a = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, "FRM_06R97.WEBP", None)
        .expect("uppercase query resolves");
    let b = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, "frm_06r97", None)
        .expect("lowercase query resolves");
    assert_eq!(a, b, "case variants alias to the same bitmap");
    assert_eq!(scene.bitmaps.len(), 1);
}

#[test]
fn roundtrip_png_jpeg_webp_through_storage() {
    // Encode a known RGBA pattern with the image crate (png + jpeg + webp),
    // store the files on disk, mount them as game storage and load them back
    // through the module. PNG/WebP are lossless: pixels must match exactly;
    // JPEG is lossy (and alpha-less), so only size + non-blank are checked.
    let (w, h) = (8u32, 6u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[(x * 31) as u8, (y * 47) as u8, 128, 255]);
        }
    }

    let dir = TempDir::new("roundtrip");
    write_encoded(
        dir.path(),
        "sprite.png",
        &rgba,
        w,
        h,
        image::ImageFormat::Png,
    );
    write_encoded(
        dir.path(),
        "sprite.jpg",
        &rgba,
        w,
        h,
        image::ImageFormat::Jpeg,
    );
    write_encoded(
        dir.path(),
        "sprite.webp",
        &rgba,
        w,
        h,
        image::ImageFormat::WebP,
    );

    let mut storage = Storage::mount(dir.path()).expect("temp dir mounts");
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();

    for (name, lossless) in [
        ("sprite", true),      // extension probing: no extension given
        ("sprite.jpg", false), // explicit extension, lossy
        ("sprite.webp", true),
    ] {
        let id = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, name, None)
            .unwrap_or_else(|e| panic!("load {name}: {e}"));
        let bmp = scene.bitmap(id).unwrap();
        assert_eq!((bmp.width, bmp.height), (w, h), "{name} dims");
        if lossless {
            assert_eq!(bmp.rgba, rgba, "{name} pixels must round-trip exactly");
        } else {
            assert!(bmp.rgba.iter().any(|&p| p != 0), "{name} must have content");
        }
    }
}
