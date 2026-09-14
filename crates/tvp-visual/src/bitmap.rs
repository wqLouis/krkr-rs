//! Bitmap loading/decoding/saving for the TVP visual natives.
//!
//! Implements the reference image pipeline behind `Bitmap(name)` /
//! `Bitmap(width, height)` / `Bitmap.load(name)` / `Bitmap.loadAsync(name)`
//! (`reference/cpp/core/visual/BitmapIntf.cpp`,
//! `reference/cpp/core/visual/GraphicsLoaderIntf.cpp`): game image files
//! read from game storage are decoded to RGBA8 and registered in the
//! [`scene::Scene`] bitmap table.
//!
//! Supported load formats — the reference's decodable core spellings plus
//! the common extras enabled in `Cargo.toml`:
//! * native TLG (via [`crate::tlg`]): `.tlg` / `.tlg5` / `.tlg6`;
//! * `image`-crate decoders: `.png`, `.jpg` / `.jpeg` / `.jif`,
//!   `.bmp` / `.dib`, `.webp` (lossy **and** lossless), `.gif`, `.tif` /
//!   `.tiff`, `.tga`, `.dds`, `.pnm`, `.ico`, `.qoi`, `.hdr`, `.exr`, `.ff`.
//!
//! The reference routes purely by **magic bytes** in `TVPLoadGraphicRouter`
//! (with extension-based handler lookup); we detect by extension first and
//! fall back to magic sniffing, matching the router's effective behavior
//! for the formats the `image` crate (plus [`crate::tlg`]) can decode.
//! Undecodable reference handlers (`.pvr`, `.jxr`, `.bpg`) and AVIF are
//! deliberately absent: `image`'s pure-Rust `avif` feature is encoder-only,
//! and its AVIF decoder (`avif-native`) needs the system `dav1d` library.
//!
//! Bitmap contents are always straight-alpha RGBA8 in this crate (the
//! renderer's contract); the reference's internal 0xAARRGGBB memory layout
//! is converted at the script boundary.

use std::path::Path;

use engine::Storage;
use image::{DynamicImage, ImageFormat, RgbaImage};

use crate::scene::{self, BitmapCache};

/// One decoded image: RGBA8 pixels plus the intrinsic size and whether the
/// source format carried an alpha channel (used by `loadHeader`'s `bpp`).
#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub has_alpha: bool,
}

impl DecodedImage {
    /// Convert a decoded `image` crate image, preserving whether the source
    /// color type had an alpha channel.
    fn from_dynamic(img: DynamicImage) -> Self {
        let has_alpha = img.color().has_alpha();
        let rgba = img.to_rgba8();
        Self {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
            has_alpha,
        }
    }

    fn from_rgba(img: RgbaImage, has_alpha: bool) -> Self {
        Self {
            width: img.width(),
            height: img.height(),
            rgba: img.into_raw(),
            has_alpha,
        }
    }
}

/// Errors from the storage-backed bitmap pipeline. A native wrapper
/// converts these into TJS exceptions (e.g. "cannot load image ...").
#[derive(Debug, thiserror::Error)]
pub enum BitmapError {
    /// No storage entry matched the name, with or without an image extension.
    #[error("bitmap not found in storage: {0}")]
    NotFound(String),
    /// The storage entry existed but could not be read (I/O or archive error).
    #[error("cannot read bitmap {0}: {1}")]
    Read(String, String),
    /// The bytes were not a decodable image.
    #[error("cannot decode image {0}: {1}")]
    Decode(String, String),
    /// A save request used a type string no handler accepts.
    #[error("unknown graphic format: {0}")]
    UnknownFormat(String),
    /// Encoding/saving failed.
    #[error("cannot save bitmap {0}: {1}")]
    Save(String, String),
}

/// Storage-name normalization, mirroring the engine's storage rules
/// (`xp3::normalize_in_archive_name`, which `engine::Storage` applies to
/// every lookup): storage names are case-insensitive (ASCII), `\`
/// separators map to `/`, and duplicate `/` collapse. Keeping a copy here
/// means cache keys stay consistent no matter which casing a script used.
fn normalize_storage_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'A'..='Z' => out.push(c.to_ascii_lowercase()),
            '\\' => out.push('/'),
            '/' => {
                out.push('/');
                while chars.peek() == Some(&'/') {
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Extension probing list for `Bitmap(name)` / `Bitmap.load(name)`.
///
/// The reference's `TVPFindGraphicLoadHandler` appends each registered
/// extension and returns the first storage hit; the handler table is
/// registered in the order (`GraphicsLoaderIntf.cpp:160-210`)
/// `.pvr .jxr .bpg .webp .bmp .dib .jpeg .jpg .jif .png .tlg .tlg5 .tlg6`.
/// Formats this crate cannot decode are omitted. The reference core order is
/// preserved for the formats we support; common extras follow afterwards so
/// a core format always wins a name collision. `""` is first so an explicit
/// extension in the query always wins.
const EXTENSION_PROBE: [&str; 22] = [
    "", ".webp", ".bmp", ".dib", ".jpeg", ".jpg", ".jif", ".png", ".tlg", ".tlg5", ".tlg6", ".gif",
    ".tif", ".tiff", ".tga", ".dds", ".pnm", ".ico", ".qoi", ".hdr", ".exr", ".ff",
];

/// All TLG spellings the reference registers.
const TLG_EXTENSIONS: [&str; 3] = [".tlg", ".tlg5", ".tlg6"];

/// True for a storage name that routes to the native TLG decoder.
fn is_tlg_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    TLG_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

/// Resolve a storage name to an existing entry by probing [`EXTENSION_PROBE`].
///
/// `""` first means an explicit extension always wins, exactly like the
/// reference's `TVPGuessGraphicLoadHandler`/`TVPFindGraphicLoadHandler`.
fn resolve_storage_name(storage: &Storage, name: &str) -> Result<String, BitmapError> {
    let normalized = normalize_storage_name(name);
    EXTENSION_PROBE
        .iter()
        .map(|ext| format!("{normalized}{ext}"))
        .find(|cand| storage.exists(cand))
        .ok_or_else(|| BitmapError::NotFound(name.to_string()))
}

/// Color-key sentinel meaning "no color key" (reference `TVP_clNone`,
/// `LayerIntf.h:122`).
pub const COLOR_KEY_NONE: u32 = 0x1fff_ffff;
/// Color-key sentinel meaning "adaptive" — the most frequent color on the
/// first scanline becomes transparent (reference `TVP_clAdapt`,
/// `LayerIntf.h:121`).
pub const COLOR_KEY_ADAPT: u32 = 0x01ff_ffff;

/// Apply the reference's color-key transparency to a decoded RGBA8 image
/// (`TVPLoadGraphic` / `TVPMakeAlphaFromKey`,
/// `GraphicsLoaderIntf.cpp:1070-1076`): pixels whose RGB equals the key
/// become fully transparent, all others fully opaque.
///
/// Handles `TVP_clNone` (no key), `TVP_clAdapt` (most frequent first-row
/// color) and an exact `0x00RRGGBB` key. The palette-index and alpha-mat
/// encodings (`TVP_clPalIdx`/`TVP_clAlphaMat`) need the original palette or
/// matte channel, which the `image` crate has already expanded, so they are
/// left as-is (and are not silently reinterpreted as an exact key).
pub fn apply_color_key(rgba: &mut [u8], width: u32, keyidx: u32) {
    if keyidx == COLOR_KEY_NONE {
        return;
    }
    if keyidx == COLOR_KEY_ADAPT {
        let key = adaptive_color_key(rgba, width);
        make_alpha_from_key(rgba, key);
        return;
    }
    if (keyidx & 0xff00_0000) == 0 {
        make_alpha_from_key(rgba, keyidx & 0x00ff_ffff);
    }
}

/// The most frequent RGB value on the first scanline (reference
/// `TVPMakeAlphaFromAdaptiveColor`, `GraphicsLoaderIntf.cpp:1150`).
fn adaptive_color_key(rgba: &[u8], width: u32) -> u32 {
    use std::collections::HashMap;
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut best = 0u32;
    let mut best_count = 0u32;
    for px in rgba.chunks_exact(4).take(width as usize) {
        let rgb = (u32::from(px[0]) << 16) | (u32::from(px[1]) << 8) | u32::from(px[2]);
        let count = counts.entry(rgb).or_insert(0);
        *count += 1;
        if *count > best_count {
            best_count = *count;
            best = rgb;
        }
    }
    best
}

/// Make `key`-colored pixels transparent and everything else opaque.
fn make_alpha_from_key(rgba: &mut [u8], key: u32) {
    for px in rgba.chunks_exact_mut(4) {
        let rgb = (u32::from(px[0]) << 16) | (u32::from(px[1]) << 8) | u32::from(px[2]);
        px[3] = if rgb == key { 0 } else { 255 };
    }
}

/// The `image` crate format for a storage name, mapping the reference's
/// extra spellings (`.jif` → JPEG, `.dib` → BMP) onto their decoders.
fn format_for_name(name: &str) -> Option<ImageFormat> {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "jif" => Some(ImageFormat::Jpeg),
        "dib" => Some(ImageFormat::Bmp),
        other => ImageFormat::from_extension(other),
    }
}

/// Decode image bytes into RGBA8.
///
/// Resolution order mirrors the reference's magic-byte router
/// (`TVPLoadGraphicRouter`, `GraphicsLoaderIntf.cpp:50`) with the extension
/// used as the primary handler hint:
/// 1. an explicit TLG spelling (`.tlg`/`.tlg5`/`.tlg6`) → [`crate::tlg`];
///    a corrupt real TLG reports its decode error, a *mislabeled* non-TLG
///    file falls through,
/// 2. the format implied by the extension (disambiguates weak-magic formats
///    like TGA),
/// 3. content detection: TLG magic first (the `image` crate cannot sniff
///    it), then `image::guess_format`'s magic table — so a wrong or missing
///    extension still decodes.
///
/// A failed decode is a real error — never a placeholder image.
/// First bytes of `bytes` as hex, for decode-failure logs.
fn byte_magic(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn decode_image(name: &str, bytes: &[u8]) -> Result<DecodedImage, BitmapError> {
    if is_tlg_name(name) {
        match decode_tlg_bytes(name, bytes) {
            Ok(decoded) => return Ok(decoded),
            Err(e) if crate::tlg::has_tlg_magic(bytes) => {
                log::warn!(
                    "image decode failed (TLG): name={name:?} bytes={} magic={} reason={e}",
                    bytes.len(),
                    byte_magic(bytes)
                );
                return Err(e);
            }
            // A mislabeled file (e.g. `x.tlg` holding a PNG): fall through.
            Err(e) => {
                log::debug!(
                    "image decode: {name:?} has a .tlg name but no TLG magic ({e}); sniffing content"
                );
            }
        }
    }

    let mut ext_error = None;
    if let Some(fmt) = format_for_name(name) {
        match image::load_from_memory_with_format(bytes, fmt) {
            Ok(img) => return Ok(DecodedImage::from_dynamic(img)),
            Err(e) => ext_error = Some(e),
        }
    }

    if crate::tlg::has_tlg_magic(bytes) {
        return decode_tlg_bytes(name, bytes).inspect_err(|e| {
            log::warn!(
                "image decode failed (TLG by magic): name={name:?} bytes={} magic={} reason={e}",
                bytes.len(),
                byte_magic(bytes)
            );
        });
    }
    match image::guess_format(bytes) {
        Ok(fmt) => match image::load_from_memory_with_format(bytes, fmt) {
            Ok(img) => return Ok(DecodedImage::from_dynamic(img)),
            Err(e) => ext_error = Some(e),
        },
        Err(e) => {
            if ext_error.is_none() {
                ext_error = Some(e);
            }
        }
    }

    let message = ext_error.map_or_else(
        || "not a recognized image (no extension hint and no known magic bytes)".to_string(),
        |e| e.to_string(),
    );
    log::warn!(
        "image decode failed: name={name:?} bytes={} magic={} reason={message}",
        bytes.len(),
        byte_magic(bytes)
    );
    Err(BitmapError::Decode(name.to_string(), message))
}

/// Decode bytes as TLG5/TLG6, returning RGBA8 plus the alpha descriptor.
fn decode_tlg_bytes(name: &str, bytes: &[u8]) -> Result<DecodedImage, BitmapError> {
    crate::tlg::decode_tlg_with_info(bytes)
        .map(|(img, has_alpha)| DecodedImage::from_rgba(img, has_alpha))
        .map_err(|e| BitmapError::Decode(name.to_string(), e))
}

/// Read an image's bytes from storage, logging a warning when the storage
/// lookup/read fails so a missing or unreadable resource is traceable in the
/// log even when the script swallows the resulting exception.
fn read_storage_bytes(storage: &mut Storage, resolved: &str) -> Result<Vec<u8>, BitmapError> {
    storage.read(resolved).map_err(|e| {
        let err = match e {
            engine::storage::ReadError::NotFound(n) => BitmapError::NotFound(n),
            other => BitmapError::Read(resolved.to_string(), other.to_string()),
        };
        log::warn!("image storage read failed: name={resolved:?} reason={err}");
        err
    })
}

/// Resolve, read and decode a storage image without touching the scene.
/// Used by the async loader's background thread, which must not mutate the
/// scene (only the VM thread does that).
pub fn read_and_decode_from_storage(
    storage: &mut Storage,
    name: &str,
) -> Result<(String, DecodedImage), BitmapError> {
    let resolved = resolve_storage_name(storage, name)?;
    let bytes = read_storage_bytes(storage, &resolved)?;
    let decoded = decode_image(&resolved, &bytes)?;
    Ok((resolved, decoded))
}

/// Overwrite an existing bitmap's pixels/size in place (the async loader
/// reuses its instance-owned bitmap instead of orphaning the previous one
/// on every `loadAsync`).
pub fn replace_bitmap_rgba(
    scene: &mut scene::Scene,
    id: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) {
    let Some(bmp) = scene.bitmap_mut(id) else {
        return;
    };
    bmp.width = width;
    bmp.height = height;
    bmp.rgba = rgba;
    bmp.mark_dirty();
}

/// Load a bitmap from game storage and register it in the scene, returning
/// its id. Equivalent to [`load_bitmap_into_storage`] with no target.
pub fn load_bitmap_from_storage(
    scene: &mut scene::Scene,
    cache: &mut BitmapCache,
    storage: &mut Storage,
    name: &str,
    request_hint: Option<(u32, u32)>,
) -> Result<u32, BitmapError> {
    load_bitmap_into_storage(scene, cache, storage, name, None, request_hint)
}

/// Load a bitmap from game storage, optionally **into an existing scene
/// bitmap** (`target`) so a re-`load` replaces the pixels in place instead
/// of orphaning the old bitmap.
///
/// Semantics match the reference `Bitmap(name)` constructor / `load(name)`:
/// 1. The name is normalized like the engine does (case-insensitive,
///    `\` → `/`) and probed with the [`EXTENSION_PROBE`] list, so
///    `Bitmap("FRM_0501b")` finds `FRM_0501b.webp` while
///    `Bitmap("bg/bg01a01.webp")` matches exactly.
/// 2. `target == None` returns a **fresh, independently-owned** scene
///    bitmap. A [`BitmapCache`] hit only avoids re-decoding: its pixels are
///    copied into the new bitmap. This matches the reference, where
///    `TVPLoadGraphic` copies the cached image into every `Bitmap`
///    (`AssignToTexture`), so `Bitmap("x")` twice yields two bitmaps that
///    can be mutated independently.
/// 3. `target == Some(id)` decodes and overwrites that existing bitmap in
///    place (the reference `Bitmap.load` replaces the current image).
///
/// `request_hint` is the reference's desired-size load (`desw`/`desh`); it
/// is not used by the current renderer (which always draws at intrinsic
/// size), so the parameter is accepted and ignored.
pub fn load_bitmap_into_storage(
    scene: &mut scene::Scene,
    cache: &mut BitmapCache,
    storage: &mut Storage,
    name: &str,
    target: Option<u32>,
    request_hint: Option<(u32, u32)>,
) -> Result<u32, BitmapError> {
    let _ = request_hint;

    let resolved = resolve_storage_name(storage, name)?;

    // A fresh load copies from the cached template (independently owned).
    if target.is_none()
        && let Some(&template) = cache.by_name.get(&resolved)
        && let Some(bmp) = scene.bitmap(template)
    {
        let (w, h, rgba) = (bmp.width, bmp.height, bmp.rgba.clone());
        let id = scene.add_bitmap(w, h, rgba);
        if let Some(b) = scene.bitmap_mut(id) {
            b.name = Some(resolved);
        }
        return Ok(id);
    }

    let bytes = read_storage_bytes(storage, &resolved)?;

    let decoded = decode_image(&resolved, &bytes)?;

    let id = match target {
        Some(tid) if scene.bitmap(tid).is_some() => {
            let (w, h, rgba) = (decoded.width, decoded.height, decoded.rgba);
            let b = scene.bitmap_mut(tid).expect("checked above");
            b.width = w;
            b.height = h;
            b.rgba = rgba;
            b.mark_dirty();
            tid
        }
        _ => scene.add_bitmap(decoded.width, decoded.height, decoded.rgba),
    };
    if let Some(b) = scene.bitmap_mut(id) {
        b.name = Some(resolved.clone());
    }
    // Only a freshly-decoded bitmap becomes the pristine cache template; an
    // explicit `target` is instance-owned and must not be mutated through
    // the cache by a later load.
    if target.is_none() {
        cache.by_name.insert(resolved, id);
    }
    Ok(id)
}

/// Read only an image's header (size + alpha) from storage, for
/// `Bitmap.loadHeader`.
pub fn load_image_header(
    storage: &mut Storage,
    name: &str,
) -> Result<(u32, u32, bool), BitmapError> {
    let resolved = resolve_storage_name(storage, name)?;
    let bytes = read_storage_bytes(storage, &resolved)?;
    let decoded = decode_image(&resolved, &bytes)?;
    Ok((decoded.width, decoded.height, decoded.has_alpha))
}

/// Create a blank bitmap and register it in the scene, returning its id.
///
/// Mirrors the reference `Bitmap(width, height)` constructor
/// (`tTJSNC_Bitmap::Construct` → `new tTVPBaseBitmap(w, h, bpp)`). Note on
/// contents: the reference leaves the backing texture **uninitialized**;
/// we instead fill with deterministic transparent black (RGBA 0,0,0,0).
/// Zero sizes are clamped to 1, matching the reference's `SetSize`
/// behavior.
pub fn add_blank_bitmap(scene: &mut scene::Scene, w: u32, h: u32) -> u32 {
    let w = w.max(1);
    let h = h.max(1);
    let rgba = vec![0u8; w as usize * h as usize * 4];
    scene.add_bitmap(w, h, rgba)
}

/// Resize a bitmap, preserving the overlapping top-left region and filling
/// any expanded area with transparent black — the reference
/// `SetSizeWithFill(w, h, 0)` used by `Bitmap.setSize` / `width=` /
/// `height=`. Zero dimensions are clamped to 1.
pub fn resize_bitmap_keep(scene: &mut scene::Scene, id: u32, w: u32, h: u32) {
    let w = w.max(1);
    let h = h.max(1);
    let Some(bmp) = scene.bitmap(id) else {
        return;
    };
    if bmp.width == w && bmp.height == h {
        return;
    }
    let old_w = bmp.width;
    let old_h = bmp.height;
    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    let copy_w = old_w.min(w) as usize;
    let copy_h = old_h.min(h) as usize;
    for y in 0..copy_h {
        let src = &bmp.rgba[y * old_w as usize * 4..(y * old_w as usize + copy_w) * 4];
        let dst = &mut rgba[y * w as usize * 4..(y * w as usize + copy_w) * 4];
        dst.copy_from_slice(src);
    }
    let Some(bmp) = scene.bitmap_mut(id) else {
        return;
    };
    bmp.width = w;
    bmp.height = h;
    bmp.rgba = rgba;
    bmp.mark_dirty();
}

/// Image encoders this crate can write, mirroring the reference save
/// handlers for the formats the `image` crate supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFormat {
    Png,
    Jpeg,
    Bmp,
}

/// Map a script `save(name, type)` type string to an encoder, following the
/// reference `AcceptSave` predicates (`TVPAcceptSaveAsPNG` /
/// `TVPAcceptSaveAsBMP` / `TVPAcceptSaveAsJPG`): a `StartsWith` on the bare
/// name, or the exact dotted extension.
pub fn save_format_from_type(type_name: &str) -> Option<SaveFormat> {
    let t = type_name.to_ascii_lowercase();
    if t.starts_with("png") || t == ".png" {
        Some(SaveFormat::Png)
    } else if t.starts_with("bmp") || t == ".bmp" || t == ".dib" {
        Some(SaveFormat::Bmp)
    } else if t.starts_with("jpg")
        || t.starts_with("jpeg")
        || t == ".jpg"
        || t == ".jpeg"
        || t == ".jif"
    {
        Some(SaveFormat::Jpeg)
    } else {
        None
    }
}

/// Encode an RGBA8 buffer in `format`. JPEG has no alpha, so its alpha is
/// dropped (matching the reference's JPEG save).
pub fn encode_image(
    format: SaveFormat,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<Vec<u8>, BitmapError> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    let res = match format {
        SaveFormat::Png => image::codecs::png::PngEncoder::new(&mut out).write_image(
            rgba,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        ),
        SaveFormat::Jpeg => {
            let rgb: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            image::codecs::jpeg::JpegEncoder::new(&mut out).write_image(
                &rgb,
                width,
                height,
                image::ExtendedColorType::Rgb8,
            )
        }
        SaveFormat::Bmp => image::codecs::bmp::BmpEncoder::new(&mut out).write_image(
            rgba,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        ),
    };
    res.map(|()| out)
        .map_err(|e| BitmapError::Save(format!("{format:?}").to_ascii_lowercase(), e.to_string()))
}

/// Write an encoded bitmap to the game directory (the reference
/// `TVPSaveImage` / `TVPCreateStream(write)` path). The storage name is
/// normalized (`\` → `/`, ASCII-lowercased) and resolved under the mount's
/// game directory; parent directories are created. Path traversal outside
/// the game directory is rejected.
pub fn save_bitmap_to_storage(
    storage: &Storage,
    name: &str,
    format: SaveFormat,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), BitmapError> {
    let bytes = encode_image(format, width, height, rgba)?;
    let normalized = normalize_storage_name(name);
    let relative = Path::new(&normalized);
    if relative.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return Err(BitmapError::Save(
            name.to_string(),
            "path escapes the game directory".into(),
        ));
    }
    let path = storage.game_dir().join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| BitmapError::Save(name.to_string(), e.to_string()))?;
    }
    std::fs::write(&path, bytes).map_err(|e| BitmapError::Save(name.to_string(), e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| {
                let x = i % w;
                let y = i / w;
                [(x * 7) as u8, (y * 11) as u8, 128, 255]
            })
            .collect()
    }

    #[test]
    fn format_for_name_maps_reference_spellings_and_extras() {
        assert_eq!(format_for_name("a.jpg"), Some(ImageFormat::Jpeg));
        assert_eq!(format_for_name("a.jif"), Some(ImageFormat::Jpeg));
        assert_eq!(format_for_name("a.bmp"), Some(ImageFormat::Bmp));
        assert_eq!(format_for_name("a.dib"), Some(ImageFormat::Bmp));
        assert_eq!(format_for_name("a.png"), Some(ImageFormat::Png));
        assert_eq!(format_for_name("a.webp"), Some(ImageFormat::WebP));
        assert_eq!(format_for_name("a.gif"), Some(ImageFormat::Gif));
        assert_eq!(format_for_name("a.tif"), Some(ImageFormat::Tiff));
        assert_eq!(format_for_name("a.tiff"), Some(ImageFormat::Tiff));
        assert_eq!(format_for_name("a.tga"), Some(ImageFormat::Tga));
        assert_eq!(format_for_name("a.dds"), Some(ImageFormat::Dds));
        assert_eq!(format_for_name("a.pnm"), Some(ImageFormat::Pnm));
        assert_eq!(format_for_name("a.ico"), Some(ImageFormat::Ico));
        assert_eq!(format_for_name("a.qoi"), Some(ImageFormat::Qoi));
        assert_eq!(format_for_name("a.hdr"), Some(ImageFormat::Hdr));
        assert_eq!(format_for_name("a.exr"), Some(ImageFormat::OpenExr));
        assert_eq!(format_for_name("a.ff"), Some(ImageFormat::Farbfeld));
        assert_eq!(format_for_name("a.tlg"), None, "TLG is routed separately");
        assert_eq!(format_for_name("a.tlg5"), None, "TLG is routed separately");
        assert_eq!(format_for_name("a.tlg6"), None, "TLG is routed separately");
    }

    #[test]
    fn extension_probe_covers_reference_core_set() {
        // The reference handler table order (`GraphicsLoaderIntf.cpp:160-210`)
        // with the undecodable formats removed; every remaining core spelling
        // must be probed.
        for ext in [
            ".webp", ".bmp", ".dib", ".jpeg", ".jpg", ".jif", ".png", ".tlg", ".tlg5", ".tlg6",
        ] {
            assert!(EXTENSION_PROBE.contains(&ext), "missing core probe {ext}");
        }
        for ext in [
            ".gif", ".tif", ".tiff", ".tga", ".dds", ".pnm", ".ico", ".qoi", ".hdr", ".exr", ".ff",
        ] {
            assert!(EXTENSION_PROBE.contains(&ext), "missing extra probe {ext}");
        }
    }

    #[test]
    fn decode_image_uses_magic_when_extension_is_wrong_or_missing() {
        // A PNG named without an extension and with a bogus extension must
        // still decode via `guess_format`.
        let (w, h) = (6u32, 4u32);
        let rgba = pattern(w, h);
        let png = {
            use image::ImageEncoder;
            let mut out = Vec::new();
            image::codecs::png::PngEncoder::new(&mut out)
                .write_image(&rgba, w, h, image::ExtendedColorType::Rgba8)
                .unwrap();
            out
        };
        let no_ext = decode_image("mystery", &png).expect("magic fallback (no extension)");
        assert_eq!((no_ext.width, no_ext.height), (w, h));
        let wrong_ext =
            decode_image("mystery.dat", &png).expect("magic fallback (wrong extension)");
        assert_eq!((wrong_ext.width, wrong_ext.height), (w, h));
    }

    #[test]
    fn decode_image_routes_mislabeled_tlg_by_magic() {
        // A TLG named `.tlg` and a TLG named `.png` both decode via the TLG
        // magic (the latter proves the name is not trusted over the bytes).
        let bytes = include_bytes!("../tests/fixtures/frm_0303a.tlg");
        assert!(crate::tlg::has_tlg_magic(bytes));
        for name in ["real.tlg", "mislabeled.png", "no_extension"] {
            let decoded = decode_image(name, bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((decoded.width, decoded.height), (280, 200), "{name}");
        }
    }

    #[test]
    fn save_format_matches_reference_accept_predicates() {
        assert_eq!(save_format_from_type("bmp"), Some(SaveFormat::Bmp));
        assert_eq!(save_format_from_type(".dib"), Some(SaveFormat::Bmp));
        assert_eq!(save_format_from_type("png"), Some(SaveFormat::Png));
        assert_eq!(save_format_from_type(".jpeg"), Some(SaveFormat::Jpeg));
        assert_eq!(save_format_from_type(".jif"), Some(SaveFormat::Jpeg));
        assert_eq!(save_format_from_type("tlg6"), None);
    }

    #[test]
    fn encode_decode_roundtrip_lossless_formats() {
        let (w, h) = (9u32, 5u32);
        let rgba = pattern(w, h);
        for fmt in [SaveFormat::Png, SaveFormat::Bmp] {
            let bytes = encode_image(fmt, w, h, &rgba).unwrap();
            let name = match fmt {
                SaveFormat::Png => "x.png",
                SaveFormat::Bmp => "x.bmp",
                SaveFormat::Jpeg => unreachable!(),
            };
            let decoded = decode_image(name, &bytes).unwrap();
            assert_eq!((decoded.width, decoded.height), (w, h));
            assert_eq!(decoded.rgba, rgba, "{fmt:?} is lossless");
        }
    }

    #[test]
    fn encode_jpeg_has_no_alpha_and_correct_size() {
        let (w, h) = (8u32, 6u32);
        let bytes = encode_image(SaveFormat::Jpeg, w, h, &pattern(w, h)).unwrap();
        let decoded = decode_image("x.jpg", &bytes).unwrap();
        assert_eq!((decoded.width, decoded.height), (w, h));
        assert!(!decoded.has_alpha);
        assert!(decoded.rgba.iter().any(|&b| b != 0));
    }
}
