//! Bitmap loading for the TVP visual natives.
//!
//! Implements the reference `Bitmap(name)` / `Bitmap(width, height)`
//! constructor semantics (`reference/cpp/core/visual/BitmapIntf.cpp`):
//! game image files (`.webp` / `.png` / `.jpg` / `.bmp`) read from game
//! storage are decoded to RGBA8 and registered in the [`scene::Scene`]
//! bitmap table. This is what `new Bitmap("FRM_0501b")` will call once the
//! natives land.

use std::path::Path;

use engine::Storage;

use crate::scene::{self, BitmapCache};

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

/// Extension probing list for `Bitmap(name)`, in reference order: the name
/// as-is first, then with common image extensions appended. This is what
/// lets `Bitmap("FRM_0501b")` resolve to a real `FRM_0501b.webp` (or
/// `.png`/`.jpg`/`.bmp`) entry in storage. Because `""` is tried first, a
/// name that already carries an extension always wins.
const EXTENSION_PROBE: [&str; 6] = ["", ".webp", ".png", ".jpg", ".jpeg", ".bmp"];

/// Errors from [`load_bitmap_from_storage`]. A native wrapper converts these
/// into TJS exceptions (e.g. "cannot load image ...").
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
}

/// Load a bitmap from game storage and register it in the scene's bitmap
/// table, returning its id.
///
/// Semantics match the reference `Bitmap(name)` constructor:
/// 1. The name is normalized like the engine does (case-insensitive, `\` →
///    `/`) and probed with the [`EXTENSION_PROBE`] list, so
///    `Bitmap("FRM_0501b")` finds `FRM_0501b.webp` while
///    `Bitmap("bg/bg01a01.webp")` matches exactly.
/// 2. The [`BitmapCache`] is consulted first: loading the same file twice
///    returns the **same** bitmap id (the reference caches bitmaps by
///    storage name and shares them; `Bitmap("x")` and `Bitmap("x.webp")`
///    therefore alias to one bitmap).
/// 3. The bytes are read and decoded via the `image` crate (format detected
///    by file extension, falling back to magic bytes), converted to RGBA8,
///    and registered with `scene.add_bitmap`; the resolved storage name is
///    recorded on the state and the cache. The state is marked dirty (the
///    scene default) so the renderer uploads it.
/// 4. Failures map to [`BitmapError`]: no storage entry → [`BitmapError::NotFound`],
///    unreadable entry → [`BitmapError::Read`], undecodable bytes →
///    [`BitmapError::Decode`].
///
/// `request_hint` is the optional size some games request (the reference's
/// "province" load / `desw`/`desh` parameters). It is currently ignored:
/// we always decode at the image's intrinsic size. The parameter exists so
/// the natives can pass it through without changing signatures later.
pub fn load_bitmap_from_storage(
    scene: &mut scene::Scene,
    cache: &mut BitmapCache,
    storage: &mut Storage,
    name: &str,
    request_hint: Option<(u32, u32)>,
) -> Result<u32, BitmapError> {
    // `request_hint` is reserved for the reference's size-requested loads;
    // see the doc comment. (Borrowed-until-used so the parameter is not
    // dead while we wait for that feature.)
    let _ = request_hint;

    let normalized = normalize_storage_name(name);

    // Probe extensions. `""` first → an explicit extension always wins.
    let resolved = EXTENSION_PROBE
        .iter()
        .map(|ext| format!("{normalized}{ext}"))
        .find(|cand| storage.exists(cand))
        .ok_or_else(|| BitmapError::NotFound(name.to_string()))?;

    // The reference caches bitmaps by storage name: reuse the registered
    // bitmap if this file is already in the scene.
    if let Some(&id) = cache.by_name.get(&resolved) {
        return Ok(id);
    }

    let bytes = storage.read(&resolved).map_err(|e| match e {
        engine::storage::ReadError::NotFound(n) => BitmapError::NotFound(n),
        other => BitmapError::Read(resolved.clone(), other.to_string()),
    })?;

    let rgba = decode_bytes(&resolved, &bytes)?;
    let id = scene.add_bitmap(rgba.width(), rgba.height(), rgba.into_raw());
    if let Some(b) = scene.bitmap_mut(id) {
        b.name = Some(resolved.clone());
    }
    cache.by_name.insert(resolved, id);
    Ok(id)
}

/// Decode image bytes into an RGBA8 buffer.
///
/// Format is detected by the file extension first (task: extension wins),
/// falling back to magic-byte sniffing when the extension is absent or
/// lies (the reference itself routes purely by magic bytes in
/// `TVPLoadGraphicRouter`).
fn decode_bytes(name: &str, bytes: &[u8]) -> Result<image::RgbaImage, BitmapError> {
    let fmt_by_ext = Path::new(name)
        .extension()
        .and_then(image::ImageFormat::from_extension);
    if let Some(fmt) = fmt_by_ext
        && let Ok(img) = image::load_from_memory_with_format(bytes, fmt)
    {
        return Ok(img.to_rgba8());
    }
    let fmt = image::guess_format(bytes)
        .map_err(|e| BitmapError::Decode(name.to_string(), e.to_string()))?;
    image::load_from_memory_with_format(bytes, fmt)
        .map(|img| img.to_rgba8())
        .map_err(|e| BitmapError::Decode(name.to_string(), e.to_string()))
}

/// Create a blank bitmap and register it in the scene, returning its id.
///
/// Mirrors the reference `Bitmap(width, height)` constructor
/// (`tTJSNC_Bitmap::Construct` → `new tTVPBaseBitmap(w, h, bpp)`). Note on
/// contents: the reference leaves the backing texture **uninitialized**
/// (`glTexImage2D(..., nullptr)` in `RenderManager_ogl.cpp` — actual pixels
/// are driver-dependent garbage). We instead fill with deterministic
/// transparent black (RGBA 0,0,0,0) so the renderer always sees defined
/// data; the reference itself only defines contents via the colorkey/
/// `SetPixel`/`FillRect` APIs. Zero sizes are clamped to 1, matching the
/// reference's `SetSize` behavior.
pub fn add_blank_bitmap(scene: &mut scene::Scene, w: u32, h: u32) -> u32 {
    let w = w.max(1);
    let h = h.max(1);
    let rgba = vec![0u8; w as usize * h as usize * 4];
    scene.add_bitmap(w, h, rgba)
}
