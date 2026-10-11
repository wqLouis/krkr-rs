//! The `GdiPlus.Image` value type.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp`
//! (`ImageConvertor`, `ImageFactory`, `ImageLoad`, `ImageClone`,
//! `ImageBounds`, `NCB_REGISTER_GDIP_SUBCLASS2(GdipImage, ImageConvertor)`)
//! and `blend2d/LayerExDraw.hpp` (`class GdipImage`). The wrapper holds a
//! decoded bitmap and its rotate/flip transform. The port keeps the decoded
//! straight-alpha RGBA and reproduces the reference's accessor results.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, Value};

use super::draw::Mat;
use super::set_real;
use crate::native_util::{args, report_error, set_int, set_void};
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

/// Common image extensions probed when the requested storage name does not
/// resolve, mirroring the reference `TVPGetPlacedPath` extension search.
const IMAGE_EXTENSION_PROBE: [&str; 21] = [
    ".webp", ".bmp", ".dib", ".jpeg", ".jpg", ".jif", ".png", ".tlg", ".tlg5", ".tlg6", ".gif",
    ".tif", ".tiff", ".tga", ".dds", ".pnm", ".ico", ".qoi", ".hdr", ".exr", ".ff",
];

/// The reference `GdipImage` payload (`reference/cpp/plugins/layerex_draw/
/// blend2d/LayerExDraw.hpp:630-773`): a bitmap image plus the rotate/flip
/// transform. A freshly loaded image is always a bitmap (`type == 0`).
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
    /// The `RotateFlip` transform.
    trans: Mat,
}

impl ImageData {
    /// Reference `_core.empty()`: a vector image and a bitmap with no data
    /// are empty.
    fn is_empty(&self) -> bool {
        self.kind != 0 || self.rgba.is_empty()
    }
}

/// Decode `name` through the attached storage and the installed host decoder.
///
/// The reference `loadImage` returns an empty image for a missing placed path
/// or a decode failure, so `None` here is the empty image, not an error.
fn load_image_data(ctx: &CompatCtx, name: &str) -> Option<ImageData> {
    // SAFETY: `ctx` was resolved from the installed binding.
    let services = unsafe { ctx.services() };
    let storage = services.storage()?;
    let mut candidates = Vec::with_capacity(IMAGE_EXTENSION_PROBE.len() + 1);
    candidates.push(name.to_string());
    candidates.extend(
        IMAGE_EXTENSION_PROBE
            .iter()
            .map(|extension| format!("{name}{extension}")),
    );
    let (resolved, bytes) = candidates.into_iter().find_map(|candidate| {
        // SAFETY: `attach_storage` guarantees the pointer stays valid.
        match unsafe { &*storage }.read(&candidate) {
            Ok(bytes) => Some((candidate, bytes)),
            Err(_) => None,
        }
    })?;
    let decoded = {
        let resolver = crate::resource::resource_state(ctx);
        resolver.decode(&bytes, Some(&resolved)).ok()?
    };
    if !decoded.is_consistent() {
        return None;
    }
    let has_alpha = {
        let resolver = crate::resource::resource_state(ctx);
        match resolver.probe(&bytes, Some(&resolved)) {
            Ok(info) => info.has_alpha,
            Err(_) => decoded.rgba.chunks_exact(4).any(|pixel| pixel[3] < 255),
        }
    };
    Some(ImageData {
        kind: 0,
        width: decoded.width as i32,
        height: decoded.height as i32,
        rgba: decoded.rgba,
        has_alpha,
        trans: Mat::IDENTITY,
    })
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
    let a = unsafe { args(argv, argc) };
    let Ok((_resolved, ctx)) = super::bound(engine) else {
        return report_error(out_error, "GdiPlus.Image: compat context is not bound");
    };
    let inst = unsafe { &mut *(instance as *mut ImageData) };
    if a.is_empty() {
        *inst = ImageData::default();
        set_void(out);
        return 0;
    }
    if a.len() == 1 && a[0].ty == tjs::VAL_STRING {
        let name = crate::native_util::value_as_string(&a[0]);
        match load_image_data(&ctx, &name) {
            Some(data) => {
                *inst = data;
                set_void(out);
                0
            }
            None => report_error(out_error, &format!("cannot open:{name}")),
        }
    } else {
        report_error(out_error, "GdiPlus.Image: invalid parameter")
    }
}

/// `load(filename)` — reference `ImageLoad` (`reference/cpp/plugins/
/// layerex_draw/blend2d/LayerExDraw.cpp:3356-3363`): the loaded image
/// replaces the current one; a missing or undecodable file leaves an empty
/// image and does not raise.
extern "C" fn load(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "GdiPlus.Image.load requires 1 argument");
    }
    let Ok((_resolved, ctx)) = super::bound(engine) else {
        return report_error(out_error, "GdiPlus.Image.load: compat context is not bound");
    };
    let name = crate::native_util::value_as_string(&a[0]);
    let inst = unsafe { &mut *(instance as *mut ImageData) };
    *inst = load_image_data(&ctx, &name).unwrap_or_default();
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
/// handled transforms, `NotImplemented` (6) otherwise.
extern "C" fn rotate_flip(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
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
    let inst = unsafe { &mut *(instance as *mut ImageData) };
    match flip_type {
        0 => {}
        1 => inst.trans = rotation_matrix(std::f64::consts::PI * 0.5),
        2 => inst.trans = rotation_matrix(std::f64::consts::PI),
        3 => inst.trans = rotation_matrix(std::f64::consts::PI * 1.5),
        4 => inst.trans = scaling_matrix(-1.0, 1.0),
        5 => {
            inst.trans = rotation_matrix(std::f64::consts::PI * 0.5);
            inst.trans = inst.trans.mul(&scaling_matrix(-1.0, 1.0));
        }
        6 => inst.trans = scaling_matrix(1.0, -1.0),
        _ => {
            set_int(out, 6);
            return 0;
        }
    }
    set_int(out, 0);
    0
}

/// The reference `BLMatrix2D::makeRotation` affine.
fn rotation_matrix(rad: f64) -> Mat {
    let (s, c) = rad.sin_cos();
    Mat {
        m11: c,
        m12: s,
        m21: -s,
        m22: c,
        dx: 0.0,
        dy: 0.0,
    }
}

/// The reference `BLMatrix2D::makeScaling` affine.
fn scaling_matrix(sx: f64, sy: f64) -> Mat {
    Mat {
        m11: sx,
        m22: sy,
        ..Mat::IDENTITY
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
