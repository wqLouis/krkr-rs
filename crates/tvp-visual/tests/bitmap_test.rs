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
/// The same real-game frame committed as an 8-bit RGBA PNG (84x90, alpha),
/// covering the reference's core `.png` handler with real pixels.
const FIXTURE_PNG: &str = "frm_06r97.png";
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
        image::ImageFormat::Bmp => {
            image::codecs::bmp::BmpEncoder::new(&mut bytes)
                .write_image(rgba, w, h, ExtendedColorType::Rgba8)
                .expect("bmp encode");
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
fn cache_avoids_redecode_but_each_load_is_independent() {
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    let mut storage = fixture_storage();

    // First load resolves `frm_06r97` → `frm_06r97.webp` and caches a
    // pristine template; the second query (with the explicit extension)
    // hits that cache (no re-decode) but gets its own pixel buffer, matching
    // the reference `AssignToTexture` copy.
    let a = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, "frm_06r97", None)
        .expect("load 1");
    let b = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, FIXTURE_WEBP, None)
        .expect("load 2");
    assert_ne!(a, b, "each Bitmap owns its pixels");
    assert_eq!(scene.bitmaps.len(), 2, "template + one owned copy");
    assert_eq!(scene.bitmap(a).unwrap().rgba, scene.bitmap(b).unwrap().rgba);
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
    // Case variants resolve to one cached template (one decode) but each
    // load owns its pixels.
    assert_ne!(a, b);
    assert_eq!(scene.bitmaps.len(), 2);
    assert_eq!(scene.bitmap(a).unwrap().rgba, scene.bitmap(b).unwrap().rgba);
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

/// A deterministic RGBA test pattern (opaque, so BMP/PNG stay lossless).
fn pattern(w: u32, h: u32) -> Vec<u8> {
    (0..w * h)
        .flat_map(|i| {
            let x = i % w;
            let y = i / w;
            [(x * 13) as u8, (y * 29) as u8, 0x80, 255]
        })
        .collect()
}

/// Every format the native pipeline must decode, including the reference's
/// alternate spellings `.dib` (BMP) and `.jif` (JPEG), with **exact**
/// dimensions. PNG/BMP/WebP are lossless (pixels exact); JPEG is lossy.
#[test]
fn decodes_every_supported_format_with_exact_dimensions() {
    let (w, h) = (13u32, 7u32);
    let rgba = pattern(w, h);
    let dir = TempDir::new("formats");
    write_encoded(dir.path(), "a.png", &rgba, w, h, image::ImageFormat::Png);
    write_encoded(dir.path(), "a.bmp", &rgba, w, h, image::ImageFormat::Bmp);
    write_encoded(dir.path(), "a.dib", &rgba, w, h, image::ImageFormat::Bmp);
    write_encoded(dir.path(), "a.jpg", &rgba, w, h, image::ImageFormat::Jpeg);
    write_encoded(dir.path(), "a.jif", &rgba, w, h, image::ImageFormat::Jpeg);
    write_encoded(dir.path(), "a.webp", &rgba, w, h, image::ImageFormat::WebP);

    let mut storage = Storage::mount(dir.path()).expect("temp dir mounts");
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    for (name, lossless) in [
        ("a.png", true),
        ("a.bmp", true),
        ("a.dib", true),
        ("a.webp", true),
        ("a.jpg", false),
        ("a.jif", false),
    ] {
        let id = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, name, None)
            .unwrap_or_else(|e| panic!("load {name}: {e}"));
        let bmp = scene.bitmap(id).unwrap();
        assert_eq!((bmp.width, bmp.height), (w, h), "{name} dimensions");
        assert_eq!(bmp.rgba.len(), (w * h * 4) as usize, "{name} buffer");
        if lossless {
            assert_eq!(bmp.rgba, rgba, "{name} pixels are lossless");
        }
    }
}

/// The committed real-game fixture is a **lossy** (VP8) WebP with an alpha
/// chunk; the decoder must handle lossy webp and report its exact size.
#[test]
fn real_game_lossy_webp_decodes_with_exact_size() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE_WEBP);
    let bytes = fs::read(&path).expect("fixture reads");
    // RIFF container with a `VP8 ` (lossy) chunk, not `VP8L` (lossless).
    assert!(
        bytes.windows(4).any(|c| c == b"VP8 "),
        "fixture must be lossy VP8 webp"
    );
    assert!(!bytes.windows(4).any(|c| c == b"VP8L"));
    let decoded = tvp_visual::bitmap::decode_image(FIXTURE_WEBP, &bytes).expect("lossy webp");
    assert_eq!((decoded.width, decoded.height), (FIXTURE_W, FIXTURE_H));
    assert!(decoded.has_alpha, "the fixture has an ALPH chunk");
    assert_eq!(decoded.rgba.len(), (FIXTURE_W * FIXTURE_H * 4) as usize);
}

/// A committed real-game PNG fixture (derived losslessly from the game
/// frame) loads through the storage path with exact dimensions and alpha.
#[test]
fn real_game_png_fixture_decodes_with_exact_size() {
    let bytes = fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(FIXTURE_PNG),
    )
    .expect("png fixture reads");
    assert_eq!(&bytes[..4], b"\x89PNG", "fixture carries the PNG magic");

    let mut storage = fixture_storage();
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    let id = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, FIXTURE_PNG, None)
        .expect("real game png decodes");
    let bmp = scene.bitmap(id).unwrap();
    assert_eq!((bmp.width, bmp.height), (FIXTURE_W, FIXTURE_H));
    assert_eq!(bmp.rgba.len(), (FIXTURE_W * FIXTURE_H * 4) as usize);
    assert!(bmp.rgba.iter().any(|&p| p != 0), "fixture has content");
    assert!(
        bmp.rgba.chunks_exact(4).any(|p| p[3] != 0 && p[3] != 255),
        "the real frame carries partial alpha"
    );
    assert_eq!(bmp.name.as_deref(), Some(FIXTURE_PNG));
}

/// The real-game TLG5/TLG6 fixtures decode with exact dimensions through the
/// same `load_bitmap_from_storage` path the natives use, and the `.tlg5` /
/// `.tlg6` spellings route to the same native TLG decoder.
#[test]
fn real_game_tlg_fixtures_decode_with_exact_size() {
    let mut storage = fixture_storage();
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    for name in ["frm_0303a.tlg", "frm_0303b.tlg"] {
        let id = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, name, None)
            .unwrap_or_else(|e| panic!("load {name}: {e}"));
        let bmp = scene.bitmap(id).unwrap();
        assert_eq!((bmp.width, bmp.height), (280, 200), "{name} dimensions");
        assert_eq!(bmp.rgba.len(), (280 * 200 * 4) as usize, "{name} buffer");
        assert!(bmp.rgba.iter().any(|&p| p != 0), "{name} has content");
    }
}

/// `.tlg5` and `.tlg6` are distinct reference handler spellings that must
/// reach the native TLG decoder, not the `image` crate (which has no TLG
/// handler and would mis-sniff the `TLG5.0`/`TLG6.0` magic).
#[test]
fn tlg5_tlg6_spellings_route_to_the_tlg_decoder() {
    let tlg =
        fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frm_0303a.tlg"))
            .unwrap();
    // `frm_0303a.tlg` is a TLG0.0-wrapped TLG6 payload.
    for name in ["frame.tlg", "frame.tlg5", "frame.tlg6"] {
        let decoded = tvp_visual::bitmap::decode_image(name, &tlg)
            .unwrap_or_else(|e| panic!("decode {name}: {e}"));
        assert_eq!(decoded.width, 280, "{name} width");
        assert_eq!(decoded.height, 200, "{name} height");
        assert!(decoded.has_alpha, "{name} alpha descriptor");
    }
}

// ---------------------------------------------------------------------------
// Per-format load matrix
// ---------------------------------------------------------------------------

/// The enabled-format matrix: `(storage name, image format, lossless)`. Names
/// cover the reference core spellings (`.bmp .dib .jpg .jpeg .jif .png .tlg
/// .tlg5 .tlg6 .webp`) plus the common extras, and every entry must load with
/// exact dimensions.
const FORMAT_MATRIX: &[(&str, image::ImageFormat, bool)] = &[
    ("m.png", image::ImageFormat::Png, true),
    ("m.bmp", image::ImageFormat::Bmp, true),
    ("m.dib", image::ImageFormat::Bmp, true),
    ("m.webp", image::ImageFormat::WebP, true),
    ("m.ff", image::ImageFormat::Farbfeld, true),
    ("m.qoi", image::ImageFormat::Qoi, true),
    ("m.tga", image::ImageFormat::Tga, true),
    ("m.tiff", image::ImageFormat::Tiff, true),
    ("m.tif", image::ImageFormat::Tiff, true),
    ("m.pnm", image::ImageFormat::Pnm, true),
    ("m.ico", image::ImageFormat::Ico, true),
    ("m.gif", image::ImageFormat::Gif, false),
    ("m.jpg", image::ImageFormat::Jpeg, false),
    ("m.jif", image::ImageFormat::Jpeg, false),
    ("m.hdr", image::ImageFormat::Hdr, false),
    ("m.exr", image::ImageFormat::OpenExr, false),
    ("m.dds", image::ImageFormat::Dds, false),
];

/// Encode `rgba` to `dir/name`. HDR/EXR need float buffers, DDS has no encoder
/// (a minimal DXT1 file is built instead), everything else goes through the
/// generic 8-bit dispatch.
fn encode_fixture(dir: &Path, name: &str, rgba: &[u8], w: u32, h: u32, format: image::ImageFormat) {
    let path = dir.join(name);
    match format {
        image::ImageFormat::Dds => {
            fs::write(&path, dxt1_dds(rgba, w, h)).expect("write dds");
        }
        image::ImageFormat::Hdr => {
            let rgb: Vec<f32> = rgba
                .chunks_exact(4)
                .flat_map(|p| {
                    [
                        f32::from(p[0]) / 255.0,
                        f32::from(p[1]) / 255.0,
                        f32::from(p[2]) / 255.0,
                    ]
                })
                .collect();
            let bytes: Vec<u8> = rgb.iter().flat_map(|f| f.to_le_bytes()).collect();
            image::save_buffer_with_format(&path, &bytes, w, h, ExtendedColorType::Rgb32F, format)
                .unwrap_or_else(|e| panic!("encode {name}: {e}"));
        }
        image::ImageFormat::OpenExr => {
            let rgba_f: Vec<f32> = rgba.iter().map(|&b| f32::from(b) / 255.0).collect();
            let bytes: Vec<u8> = rgba_f.iter().flat_map(|f| f.to_le_bytes()).collect();
            image::save_buffer_with_format(&path, &bytes, w, h, ExtendedColorType::Rgba32F, format)
                .unwrap_or_else(|e| panic!("encode {name}: {e}"));
        }
        image::ImageFormat::Jpeg => {
            // JPEG has no alpha.
            let rgb: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            image::save_buffer_with_format(&path, &rgb, w, h, ExtendedColorType::Rgb8, format)
                .unwrap_or_else(|e| panic!("encode {name}: {e}"));
        }
        image::ImageFormat::Farbfeld => {
            // farbfeld is 16-bit big-endian per channel; 8-bit values
            // replicate (`v * 257`) and round-trip exactly.
            let mut bytes = Vec::with_capacity(rgba.len() * 2);
            for p in rgba.chunks_exact(4) {
                for &c in p {
                    bytes.extend_from_slice(&(u16::from(c) * 257).to_be_bytes());
                }
            }
            image::save_buffer_with_format(&path, &bytes, w, h, ExtendedColorType::Rgba16, format)
                .unwrap_or_else(|e| panic!("encode {name}: {e}"));
        }
        _ => image::save_buffer_with_format(&path, rgba, w, h, ExtendedColorType::Rgba8, format)
            .unwrap_or_else(|e| panic!("encode {name}: {e}")),
    }
}

/// A minimal uncompressed-size legacy DDS holding DXT1 blocks (the `image`
/// crate's DDS decoder only supports DXT1/3/5). Each 4x4 block is encoded as
/// `color0 == color1 == <block's first pixel>` with all indices 0.
fn dxt1_dds(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    fn rgb565(r: u8, g: u8, b: u8) -> u16 {
        ((u16::from(r) >> 3) << 11) | ((u16::from(g) >> 2) << 5) | (u16::from(b) >> 3)
    }
    let bw = w.div_ceil(4);
    let bh = h.div_ceil(4);
    let mut out = Vec::with_capacity(128 + (bw * bh * 8) as usize);
    out.extend_from_slice(b"DDS ");
    out.extend_from_slice(&124u32.to_le_bytes()); // dwSize
    out.extend_from_slice(&0x0008_1007u32.to_le_bytes()); // CAPS|HEIGHT|WIDTH|PIXELFORMAT|LINEARSIZE
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&(bw * bh * 8).to_le_bytes()); // linear size
    out.extend_from_slice(&0u32.to_le_bytes()); // depth
    out.extend_from_slice(&0u32.to_le_bytes()); // mipmap count
    out.extend_from_slice(&[0u8; 11 * 4]); // reserved1
    out.extend_from_slice(&32u32.to_le_bytes()); // pixel format size
    out.extend_from_slice(&0x4u32.to_le_bytes()); // DDPF_FOURCC
    out.extend_from_slice(b"DXT1");
    out.extend_from_slice(&[0u8; 20]); // bit count + RGBA masks
    out.extend_from_slice(&0x1000u32.to_le_bytes()); // caps = TEXTURE
    out.extend_from_slice(&[0u8; 16]); // caps2..4 + reserved2
    for by in 0..bh {
        for bx in 0..bw {
            let x = (bx * 4).min(w - 1);
            let y = (by * 4).min(h - 1);
            let p = &rgba[((y * w + x) * 4) as usize..][..4];
            let c = rgb565(p[0], p[1], p[2]);
            out.extend_from_slice(&c.to_le_bytes());
            out.extend_from_slice(&c.to_le_bytes());
            out.extend_from_slice(&[0u8; 4]); // all indices -> color0
        }
    }
    out
}

/// Every enabled format loads through the storage probe with exact
/// dimensions; lossless formats round-trip their pixels exactly.
#[test]
fn per_format_load_matrix_exact_dimensions() {
    // DXT (DDS) requires dimensions that are multiples of 4.
    let (w, h) = (16u32, 8u32);
    let rgba = pattern(w, h);
    let dir = TempDir::new("format-matrix");
    for (name, format, _) in FORMAT_MATRIX {
        encode_fixture(dir.path(), name, &rgba, w, h, *format);
    }

    let mut storage = Storage::mount(dir.path()).expect("mount matrix dir");
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();
    for (name, format, exact) in FORMAT_MATRIX {
        let id = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, name, None)
            .unwrap_or_else(|e| panic!("load {name} ({format:?}): {e}"));
        let bmp = scene.bitmap(id).unwrap();
        assert_eq!((bmp.width, bmp.height), (w, h), "{name} dimensions");
        assert_eq!(bmp.rgba.len(), (w * h * 4) as usize, "{name} buffer");
        if *exact {
            assert_eq!(bmp.rgba, rgba, "{name} lossless pixels");
        } else {
            assert!(bmp.rgba.iter().any(|&p| p != 0), "{name} must have content");
        }
    }
}

/// A file whose extension is missing or wrong still decodes from its magic
/// bytes; a TLG mislabeled as `.png` routes through the TLG magic, and a PNG
/// mislabeled as `.tlg` falls through the TLG handler into magic sniffing.
#[test]
fn content_based_detection_ignores_wrong_or_missing_extension() {
    let (w, h) = (7u32, 5u32);
    let rgba = pattern(w, h);
    let dir = TempDir::new("magic-detect");

    // A PNG renamed to have no extension and a bogus one.
    let png_path = dir.path().join("source.png");
    image::save_buffer_with_format(
        &png_path,
        &rgba,
        w,
        h,
        ExtendedColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .expect("png encode");
    fs::copy(&png_path, dir.path().join("noext")).unwrap();
    fs::copy(&png_path, dir.path().join("bogus.dat")).unwrap();

    // A PNG renamed to the TLG-native `.tlg` spelling (a `.tlg` name is
    // resolved by the probe, then must fall through to magic sniffing).
    fs::copy(&png_path, dir.path().join("mislabeled_as_tlg.tlg")).unwrap();

    // A real TLG fixture renamed to a PNG extension.
    let tlg =
        fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frm_0303a.tlg"))
            .unwrap();
    fs::write(dir.path().join("mislabeled.png"), &tlg).unwrap();

    let mut storage = Storage::mount(dir.path()).expect("mount magic dir");
    let mut scene = Scene::default();
    let mut cache = BitmapCache::default();

    for name in ["noext", "bogus.dat", "mislabeled_as_tlg"] {
        let id = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, name, None)
            .unwrap_or_else(|e| panic!("load {name}: {e}"));
        let bmp = scene.bitmap(id).unwrap();
        assert_eq!((bmp.width, bmp.height), (w, h), "{name} magic dims");
    }

    let id = load_bitmap_from_storage(&mut scene, &mut cache, &mut storage, "mislabeled.png", None)
        .expect("TLG magic overrides the .png name");
    assert_eq!(
        (
            scene.bitmap(id).unwrap().width,
            scene.bitmap(id).unwrap().height
        ),
        (280, 200)
    );
}
