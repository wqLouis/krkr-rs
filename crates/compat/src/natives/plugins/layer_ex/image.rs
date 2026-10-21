//! The `GdiPlus.Image` value type.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp`
//! (`ImageConvertor`, `ImageFactory`, `ImageLoad`, `ImageClone`,
//! `ImageBounds`, `NCB_REGISTER_GDIP_SUBCLASS2(GdipImage, ImageConvertor)`)
//! and `blend2d/LayerExDraw.hpp` (`class GdipImage`). The wrapper holds a
//! decoded bitmap and turns its pixels in place on `RotateFlip`. The port
//! keeps the decoded straight-alpha RGBA and reproduces the accessor results.
//! The pixels come from the engine's decoded-image cache.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::{
    BitmapState, DecodedImageKey, DecodedImageMode, Storage, StorageReadError,
};
use tjs::{DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, Value};

use super::{scene_mut, set_real};
use crate::native_util::{args, report_error, set_int, set_void};
use crate::natives::visual::COLOR_KEY_NONE;
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError};

/// `PixelFormatGDI` / `PixelFormatAlpha` / `PixelFormatCanonical`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:14-19`).
const PIXEL_FORMAT_GDI: i64 = 0x0002_0000;
const PIXEL_FORMAT_ALPHA: i64 = 0x0004_0000;
const PIXEL_FORMAT_CANONICAL: i64 = 0x0020_0000;

/// `PixelFormat32bppRGB` / `PixelFormat32bppARGB` (`reference/cpp/plugins/
/// layerex_draw/blend2d/LayerExDraw.hpp:33-35`).
const PIXEL_FORMAT_32BPP_RGB: i64 = 9 | (32 << 8) | PIXEL_FORMAT_GDI;
const PIXEL_FORMAT_32BPP_ARGB: i64 =
    10 | (32 << 8) | PIXEL_FORMAT_ALPHA | PIXEL_FORMAT_GDI | PIXEL_FORMAT_CANONICAL;

/// `ImageFlagsHasAlpha` / `ImageFlagsColorSpaceRGB` / `ImageFlagsReadOnly`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:54,57,60`).
const IMAGE_FLAGS_HAS_ALPHA: i64 = 0x0002;
const IMAGE_FLAGS_COLOR_SPACE_RGB: i64 = 0x0010;
const IMAGE_FLAGS_READONLY: i64 = 0x0001_0000;

/// `ImageTypeUnknown` / `ImageTypeBitmap` / `ImageTypeMetafile`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:213-216`).
const IMAGE_TYPE_UNKNOWN: i64 = 0;
const IMAGE_TYPE_BITMAP: i64 = 1;
const IMAGE_TYPE_METAFILE: i64 = 2;

/// `InvalidParameter` in the `GdiPlus.Status` table.
const INVALID_PARAMETER: i64 = 2;

/// Common image extensions probed when the requested storage name does not
/// resolve, mirroring the reference `TVPGetPlacedPath` extension search.
const IMAGE_EXTENSION_PROBE: [&str; 21] = [
    ".webp", ".bmp", ".dib", ".jpeg", ".jpg", ".jif", ".png", ".tlg", ".tlg5", ".tlg6", ".gif",
    ".tif", ".tiff", ".tga", ".dds", ".pnm", ".ico", ".qoi", ".hdr", ".exr", ".ff",
];

/// The reference `GdipImage` payload (`reference/cpp/plugins/layerex_draw/
/// blend2d/LayerExDraw.hpp:630-773`): a bitmap image plus the rotate/flip
/// operation. A freshly loaded image is always a bitmap (`type == 0`).
#[derive(Clone, Debug, Default)]
pub(super) struct ImageData {
    /// 0 = bitmap, 1 = vector metafile.
    kind: i32,
    width: i32,
    height: i32,
    /// Straight-alpha RGBA8, `width * height * 4` bytes.
    rgba: Vec<u8>,
    /// Whether the encoded image carried an alpha channel.
    has_alpha: bool,
}

impl ImageData {
    /// Reference `_core.empty()`: a vector image and a bitmap with no data
    /// are empty.
    fn is_empty(&self) -> bool {
        self.kind != 0 || self.rgba.is_empty()
    }

    /// The raster dimensions, after checking that the pixel buffer holds four
    /// straight-alpha RGBA8 bytes per pixel.
    ///
    /// # Errors
    ///
    /// Returns an error when a stored dimension is negative, the size is not
    /// representable, or the buffer length disagrees with it.
    fn checked_raster_size(&self) -> Result<(u32, u32), String> {
        let width = u32::try_from(self.width)
            .map_err(|_| format!("the image width {} is out of range", self.width))?;
        let height = u32::try_from(self.height)
            .map_err(|_| format!("the image height {} is out of range", self.height))?;
        let expected = usize::try_from(width)
            .ok()
            .zip(usize::try_from(height).ok())
            .and_then(|(width, height)| width.checked_mul(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| format!("the image size {width}x{height} is not representable"))?;
        if self.rgba.len() != expected {
            return Err(format!(
                "the image pixel buffer holds {} bytes, {expected} expected",
                self.rgba.len()
            ));
        }
        Ok((width, height))
    }

    /// The pixels a drawing operation blits.
    ///
    /// `Ok(None)` when the image has nothing to place — an empty image, or the
    /// vector kind, which has no raster.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload's dimensions and pixel buffer do not
    /// describe the same raster.
    pub(super) fn blit_snapshot(&self) -> Result<Option<BitmapState>, String> {
        if self.is_empty() {
            return Ok(None);
        }
        let (width, height) = self.checked_raster_size()?;
        Ok(Some(BitmapState::new(width, height, self.rgba.clone())))
    }

    /// Apply `transform` to the raster in place: the pixels are rotated or
    /// mirrored and a quarter turn swaps the stored width and height, so the
    /// size accessors and a later draw see the transformed image.
    ///
    /// An image with no raster has nothing to transform.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload's dimensions and pixel buffer do not
    /// describe the same raster.
    fn apply_rotate_flip(&mut self, transform: RotateFlip) -> Result<(), String> {
        if transform == RotateFlip::Keep || self.is_empty() {
            return Ok(());
        }
        let (width, height) = self.checked_raster_size()?;
        let (out_width, out_height) = transform.size(width, height);
        let (src_width, src_height) = (width as usize, height as usize);
        let (dest_width, dest_height) = (out_width as usize, out_height as usize);
        let src_stride = src_width * 4;
        let dest_stride = dest_width * 4;
        let mut rgba = vec![0u8; self.rgba.len()];
        for y in 0..dest_height {
            for x in 0..dest_width {
                let (sx, sy) = transform.source(src_width, src_height, x, y);
                let src = sy * src_stride + sx * 4;
                let dest = y * dest_stride + x * 4;
                rgba[dest..dest + 4].copy_from_slice(&self.rgba[src..src + 4]);
            }
        }
        self.width = i32::try_from(out_width)
            .map_err(|_| format!("the rotated image width {out_width} is out of range"))?;
        self.height = i32::try_from(out_height)
            .map_err(|_| format!("the rotated image height {out_height} is out of range"))?;
        self.rgba = rgba;
        Ok(())
    }
}

/// The rotate/flip transforms `RotateFlip` applies to an image's pixels.
///
/// A quarter turn exchanges the width and the height; every other transform
/// keeps them.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RotateFlip {
    /// `RotateNoneFlipNone`: the image stays as it is.
    Keep,
    /// `Rotate90FlipNone`: a quarter turn clockwise.
    Clockwise90,
    /// `Rotate180FlipNone`: a half turn.
    HalfTurn,
    /// `Rotate270FlipNone`: a quarter turn counter-clockwise.
    CounterClockwise90,
    /// `RotateNoneFlipX`: a horizontal mirror.
    MirrorX,
    /// `Rotate90FlipX`: a quarter turn clockwise, then a horizontal mirror.
    Clockwise90MirrorX,
    /// `RotateNoneFlipY`: a vertical mirror.
    MirrorY,
    /// `Rotate270FlipX`: a quarter turn counter-clockwise, then a horizontal
    /// mirror.
    CounterClockwise90MirrorX,
}

impl RotateFlip {
    /// The transform `flip_type` names, or `InvalidParameter` for a value
    /// outside the `RotateFlipType` range.
    fn from_type(flip_type: i64) -> Result<Self, i64> {
        Ok(match flip_type {
            0 => Self::Keep,
            1 => Self::Clockwise90,
            2 => Self::HalfTurn,
            3 => Self::CounterClockwise90,
            4 => Self::MirrorX,
            5 => Self::Clockwise90MirrorX,
            6 => Self::MirrorY,
            7 => Self::CounterClockwise90MirrorX,
            _ => return Err(INVALID_PARAMETER),
        })
    }

    /// The size of the raster after the transform.
    fn size(self, width: u32, height: u32) -> (u32, u32) {
        match self {
            Self::Clockwise90
            | Self::CounterClockwise90
            | Self::Clockwise90MirrorX
            | Self::CounterClockwise90MirrorX => (height, width),
            Self::Keep | Self::HalfTurn | Self::MirrorX | Self::MirrorY => (width, height),
        }
    }

    /// The source pixel the destination pixel `(x, y)` reads. `width` and
    /// `height` are the source dimensions, and non-empty images are at least
    /// one pixel wide and high.
    fn source(self, width: usize, height: usize, x: usize, y: usize) -> (usize, usize) {
        match self {
            Self::Keep => (x, y),
            Self::Clockwise90 => (y, height - 1 - x),
            Self::HalfTurn => (width - 1 - x, height - 1 - y),
            Self::CounterClockwise90 => (width - 1 - y, x),
            Self::MirrorX => (width - 1 - x, y),
            Self::Clockwise90MirrorX => (y, x),
            Self::MirrorY => (x, height - 1 - y),
            Self::CounterClockwise90MirrorX => (width - 1 - y, height - 1 - x),
        }
    }
}

/// Read `name` through the attached storage, probing the common image
/// extensions when the name itself does not resolve.
///
/// The plain name is tried first and each extension is composed only after the
/// earlier candidates miss, so a name that resolves as given composes none.
/// Only a missing entry advances to the next candidate; any other read failure
/// is returned as itself.
fn read_image_candidate(
    storage: &Storage,
    name: &str,
) -> Result<Option<(String, Vec<u8>)>, StorageReadError> {
    // The media prefix is L3 syntax; the L2 namespace resolves the bare name.
    let lookup = strip_file_media(name);
    match storage.read(lookup) {
        Ok(bytes) => return Ok(Some((name.to_string(), bytes))),
        Err(StorageReadError::NotFound(_)) => {}
        Err(error) => return Err(error),
    }
    for extension in IMAGE_EXTENSION_PROBE {
        let candidate = format!("{lookup}{extension}");
        match storage.read(&candidate) {
            Ok(bytes) => return Ok(Some((candidate, bytes))),
            Err(StorageReadError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Load `name` through the attached storage and the installed host decoder.
///
/// The reference `loadImage` returns an empty image for a missing placed path
/// or a decode failure, so `Ok(None)` here is the empty image. A storage read
/// failure that is not "not found" is returned as itself instead of being
/// retried as the next candidate.
///
/// The pixels come from the engine's decoded-image cache, keyed by the storage
/// name the candidate search resolved: a second load of that entry reuses the
/// pixels instead of decoding them again.
fn load_image_data(ctx: &CompatCtx, name: &str) -> Result<Option<ImageData>, String> {
    // SAFETY: `ctx` was resolved from the installed binding.
    let services = unsafe { ctx.services() };
    let Some(storage) = services.storage() else {
        return Ok(None);
    };
    // SAFETY: `attach_storage` guarantees the pointer stays valid.
    let storage = unsafe { &*storage };
    let Some((resolved, bytes)) =
        read_image_candidate(storage, name).map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let key = DecodedImageKey::new(
        strip_file_media(&resolved),
        COLOR_KEY_NONE,
        DecodedImageMode::Full,
        0,
        0,
    );
    let decoded = {
        let scene = scene_mut(ctx)?;
        scene.cached_decoded_image(key, || {
            let resolver = crate::resource::resource_state(ctx);
            let decoded = resolver.decode(&bytes, Some(&resolved))?;
            if !decoded.is_consistent() {
                return Err("the decoded image is malformed".to_string());
            }
            Ok(decoded)
        })
    };
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(error) => {
            log::warn!("image: cannot decode {resolved:?}: {error}");
            return Ok(None);
        }
    };
    let has_alpha = {
        let resolver = crate::resource::resource_state(ctx);
        match resolver.probe(&bytes, Some(&resolved)) {
            Ok(info) => info.has_alpha,
            Err(error) => {
                log::warn!("image: cannot probe {resolved:?}: {error}");
                decoded.rgba.chunks_exact(4).any(|pixel| pixel[3] < 255)
            }
        }
    };
    Ok(Some(ImageData {
        kind: 0,
        width: decoded.width as i32,
        height: decoded.height as i32,
        rgba: decoded.rgba,
        has_alpha,
    }))
}

/// Build a fresh `GdiPlus.Image` and install `data` as its payload.
fn new_image_instance(
    host: &Tjs2Engine,
    data: ImageData,
    what: &str,
) -> Result<DetachedValue, String> {
    let object = super::eval_object(host, "new GdiPlus.Image()", what)?;
    let payload = host
        .native_instance_payload(object.raw_id(), "Image")
        .map_err(|error| format!("{what}: {error}"))?
        .ok_or_else(|| format!("{what}: cannot allocate the image"))?;
    // SAFETY: the object is an `Image` instance just constructed by this
    // module's `create`, so its native payload is an `ImageData`.
    unsafe {
        *(payload.as_ptr() as *mut ImageData) = data;
    }
    Ok(object)
}

extern "C" fn create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(ImageData::default())) as *mut c_void
}

extern "C" fn destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: every payload came from `create`'s `Box::into_raw`.
    drop(unsafe { Box::from_raw(instance as *mut ImageData) });
}

/// `Image(...)` — the reference `ImageFactory` (`reference/cpp/plugins/
/// layerex_draw/blend2d/LayerExDraw.cpp:3338-3354`): no arguments is an
/// empty image, a string loads it (`cannot open:%1` when the load yields
/// nothing), and anything else returns `TJS_E_INVALIDPARAM`.
extern "C" fn ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let Ok((_resolved, ctx)) = super::bound(engine) else {
        return report_error(out_error, "GdiPlus.Image: compat context is not bound");
    };
    // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; `ctor` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut ImageData) };
    if a.is_empty() {
        *inst = ImageData::default();
        set_void(out);
        return 0;
    }
    if a.len() == 1 && a[0].ty == tjs::VAL_STRING {
        let name = crate::native_util::value_as_string(&a[0]);
        match load_image_data(&ctx, &name) {
            Ok(Some(data)) => {
                *inst = data;
                set_void(out);
                0
            }
            Ok(None) => report_error(out_error, &format!("cannot open:{name}")),
            Err(error) => report_error(out_error, &error),
        }
    } else {
        report_error(out_error, "GdiPlus.Image: invalid parameter")
    }
}

/// `load(filename)` — reference `ImageLoad` (`reference/cpp/plugins/
/// layerex_draw/blend2d/LayerExDraw.cpp:3356-3363`): the loaded image
/// replaces the current one. A missing or undecodable file leaves an empty
/// image; a storage read failure raises the underlying error.
extern "C" fn load(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "GdiPlus.Image.load requires 1 argument");
    }
    let Ok((_resolved, ctx)) = super::bound(engine) else {
        return report_error(out_error, "GdiPlus.Image.load: compat context is not bound");
    };
    let name = crate::native_util::value_as_string(&a[0]);
    // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; `load` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut ImageData) };
    *inst = match load_image_data(&ctx, &name) {
        Ok(Some(data)) => data,
        Ok(None) => ImageData::default(),
        Err(error) => return report_error(out_error, &error),
    };
    set_void(out);
    0
}

/// `Clone()` — a fresh image with the same pixels and transform.
extern "C" fn clone(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, _ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "GdiPlus.Image.Clone: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; no `&mut` to it is live in `clone`.
    let inst = unsafe { &*(instance as *const ImageData) };
    match new_image_instance(resolved.engine(), inst.clone(), "GdiPlus.Image.Clone") {
        Ok(object) => {
            super::set_retained_out(out, object.raw_id());
            std::mem::forget(object);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `GetBounds()` — `(0, 0, width, height)` for a bitmap, `(0, 0, 0, 0)`
/// when empty or vector (reference `GdipImage::GetBounds`,
/// `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:668-694`).
extern "C" fn get_bounds(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, _ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "GdiPlus.Image.GetBounds: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; no `&mut` to it is live in `get_bounds`.
    let inst = unsafe { &*(instance as *const ImageData) };
    let (x, y, w, h) = if inst.kind == 0 && !inst.is_empty() {
        (0.0, 0.0, f64::from(inst.width), f64::from(inst.height))
    } else {
        (0.0, 0.0, 0.0, 0.0)
    };
    match super::new_rectf(resolved.engine(), x, y, w, h) {
        Ok(object) => {
            super::set_retained_out(out, object.raw_id());
            std::mem::forget(object);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `GetFlags()` — reference `GdipImage::GetFlags` (`reference/cpp/plugins/
/// layerex_draw/blend2d/LayerExDraw.hpp:696-706`).
extern "C" fn get_flags(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; no `&mut` to it is live in `get_flags`.
    let inst = unsafe { &*(instance as *const ImageData) };
    let flags = if inst.kind == 0 {
        if inst.is_empty() {
            0
        } else {
            IMAGE_FLAGS_HAS_ALPHA | IMAGE_FLAGS_COLOR_SPACE_RGB
        }
    } else {
        IMAGE_FLAGS_READONLY | IMAGE_FLAGS_COLOR_SPACE_RGB
    };
    set_int(out, flags);
    0
}

/// `GetHeight()` / `GetWidth()`.
macro_rules! dimension_getter {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
            let inst = unsafe { &*(instance as *const ImageData) };
            set_int(out, i64::from(inst.$field));
            0
        }
    };
}

dimension_getter!(get_height, height);
dimension_getter!(get_width, width);

/// `GetHorizontalResolution()` / `GetVerticalResolution()` — the reference
/// default DPI (`reference/cpp/plugins/layerex_draw/blend2d/
/// LayerExDraw.hpp:716,733`).
macro_rules! resolution_getter {
    ($name:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            _instance: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_real(out, 96.0);
            0
        }
    };
}

resolution_getter!(get_horizontal_resolution);
resolution_getter!(get_vertical_resolution);

/// `GetLastStatus()` — `GenericError` (1) for an empty image, `Ok` (0)
/// otherwise (reference `GdipImage::GetLastStatus`,
/// `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:708-710`).
extern "C" fn get_last_status(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; no `&mut` to it is live in `get_last_status`.
    let inst = unsafe { &*(instance as *const ImageData) };
    set_int(out, i64::from(inst.is_empty()));
    0
}

/// `GetPixelFormat()` — reference `GdipImage::GetPixelFormat`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:712-723`).
/// The port's decoder always yields 32-bit RGBA, so a non-empty bitmap maps
/// to `PixelFormat32bppARGB` when the source had an alpha channel and
/// `PixelFormat32bppRGB` otherwise.
extern "C" fn get_pixel_format(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; no `&mut` to it is live in `get_pixel_format`.
    let inst = unsafe { &*(instance as *const ImageData) };
    let format = if inst.kind != 0 || inst.is_empty() {
        0
    } else if inst.has_alpha {
        PIXEL_FORMAT_32BPP_ARGB
    } else {
        PIXEL_FORMAT_32BPP_RGB
    };
    set_int(out, format);
    0
}

/// `GetType()` — reference `GdipImage::GetType` (`reference/cpp/plugins/
/// layerex_draw/blend2d/LayerExDraw.hpp:725-731`).
extern "C" fn get_type(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; no `&mut` to it is live in `get_type`.
    let inst = unsafe { &*(instance as *const ImageData) };
    let kind = if inst.kind == 0 {
        if inst.is_empty() {
            IMAGE_TYPE_UNKNOWN
        } else {
            IMAGE_TYPE_BITMAP
        }
    } else if inst.kind == 1 {
        IMAGE_TYPE_METAFILE
    } else {
        IMAGE_TYPE_UNKNOWN
    };
    set_int(out, kind);
    0
}

/// `RotateFlip(type)` — reference `GdipImage::RotateFlip` (`reference/cpp/
/// plugins/layerex_draw/blend2d/LayerExDraw.hpp:736-773`): `Ok` (0) for the
/// handled transforms, `InvalidParameter` (2) outside the enum range.
extern "C" fn rotate_flip(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "GdiPlus.Image.RotateFlip requires 1 argument");
    }
    let flip_type = match crate::natives::system::try_value_as_i64(&a[0]) {
        Ok(value) => value,
        Err(error) => {
            return report_error(out_error, &format!("GdiPlus.Image.RotateFlip: {error}"));
        }
    };
    // SAFETY: `instance` is the live `ImageData` payload the VM allocated for this script object; `rotate_flip` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut ImageData) };
    let transform = match RotateFlip::from_type(flip_type) {
        Ok(transform) => transform,
        Err(status) => {
            set_int(out, status);
            return 0;
        }
    };
    match inst.apply_rotate_flip(transform) {
        Ok(()) => {
            set_int(out, 0);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// The method table.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        NativeInstanceMethodDef {
            name: "load",
            f: load,
        },
        NativeInstanceMethodDef {
            name: "Clone",
            f: clone,
        },
        NativeInstanceMethodDef {
            name: "GetBounds",
            f: get_bounds,
        },
        NativeInstanceMethodDef {
            name: "GetFlags",
            f: get_flags,
        },
        NativeInstanceMethodDef {
            name: "GetHeight",
            f: get_height,
        },
        NativeInstanceMethodDef {
            name: "GetHorizontalResolution",
            f: get_horizontal_resolution,
        },
        NativeInstanceMethodDef {
            name: "GetLastStatus",
            f: get_last_status,
        },
        NativeInstanceMethodDef {
            name: "GetPixelFormat",
            f: get_pixel_format,
        },
        NativeInstanceMethodDef {
            name: "GetType",
            f: get_type,
        },
        NativeInstanceMethodDef {
            name: "GetVerticalResolution",
            f: get_vertical_resolution,
        },
        NativeInstanceMethodDef {
            name: "GetWidth",
            f: get_width,
        },
        NativeInstanceMethodDef {
            name: "RotateFlip",
            f: rotate_flip,
        },
    ]
}

/// Register the `Image` class.
pub(super) fn register_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let mut methods = vec![NativeInstanceMethodDef {
        name: "Image",
        f: ctor,
    }];
    methods.extend(method_defs());
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Image",
        create,
        destroy,
        invalidate: None,
        methods,
        properties: Vec::new(),
    })
}
