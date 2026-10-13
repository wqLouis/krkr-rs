//! `PSD` native class — the `psd.dll` plugin surface.
//!
//! Behavioral reference: `reference/cpp/plugins/psdfile/`
//! (`main.cpp`, `psdclass.{h,cpp}`, `psdclass_load*.cpp`, `psdparse/`). The
//! plugin parses a Photoshop document (header, colour mode, image resources,
//! layers and the merged composite), expands the RLE / zip channel data and
//! exposes the decoded pixels as L2 bitmaps on a script [`Layer`].
//!
//! The registered surface mirrors `NCB_REGISTER_CLASS(PSD)`:
//!
//! * the constructor and `load`;
//! * read-only `width`, `height`, `channels`, `depth`, `color_mode` and
//!   `layer_count`;
//! * `getLayerType`, `getLayerName`, `getLayerInfo`;
//! * `getLayerData`, `getLayerDataRaw`, `getLayerDataMask`;
//! * `getSlices`, `getGuides`, `getBlend`, `getLayerComp`;
//! * the static `color_mode_*`, `blend_mode_*`, `layer_type_*` constants and
//!   `clearStorageCache`.
//!
//! The reference also registers a `psd://` storage media so a layer image can
//! be opened as `psd://file.psd/root/folder/layer.bmp`; the compat storage
//! namespace has no media-provider seam, so that part is not ported (the PSD
//! class surface is complete). The reference's `loadMemory`/`loadStream` are
//! plain C++ helpers rather than TJS members, so they are not registered.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use api::blend::{BlendMode as SceneBlend, DstBlend};
use api::fs::FsPath;
use engine::{Scene, StorageReadError};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    NativeMethodDef, NativePropertyDef, NativeStaticMembers, RetainedValue, Tjs2Engine,
    Tjs2ValueId, TjsValue, VAL_INTEGER, VAL_OBJECT, VAL_REAL, VAL_RETAINED, Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, set_string_out, value_as_i64};
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError};

mod decode;
mod descriptor;
mod parse;

use parse::{ImageMode, LayerInfo, LayerType, PsdFile};

// ---------------------------------------------------------------------------
// Per-instance payload
// ---------------------------------------------------------------------------

/// The parsed document, if `load` has succeeded.
#[derive(Default)]
struct PsdInst {
    file: Option<PsdFile>,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const COLOR_MODE_BITMAP: i64 = 0;
const COLOR_MODE_GRAYSCALE: i64 = 1;
const COLOR_MODE_INDEXED: i64 = 2;
const COLOR_MODE_RGB: i64 = 3;
const COLOR_MODE_CMYK: i64 = 4;
const COLOR_MODE_MULTICHANNEL: i64 = 7;
const COLOR_MODE_DUOTONE: i64 = 8;
const COLOR_MODE_LAB: i64 = 9;

const BLEND_MODE_NORMAL: i64 = 0;
const BLEND_MODE_DISSOLVE: i64 = 1;
const BLEND_MODE_DARKEN: i64 = 2;
const BLEND_MODE_MULTIPLY: i64 = 3;
const BLEND_MODE_COLOR_BURN: i64 = 4;
const BLEND_MODE_LINEAR_BURN: i64 = 5;
const BLEND_MODE_LIGHTEN: i64 = 6;
const BLEND_MODE_SCREEN: i64 = 7;
const BLEND_MODE_COLOR_DODGE: i64 = 8;
const BLEND_MODE_LINEAR_DODGE: i64 = 9;
const BLEND_MODE_OVERLAY: i64 = 10;
const BLEND_MODE_SOFT_LIGHT: i64 = 11;
const BLEND_MODE_HARD_LIGHT: i64 = 12;
const BLEND_MODE_VIVID_LIGHT: i64 = 13;
const BLEND_MODE_LINEAR_LIGHT: i64 = 14;
const BLEND_MODE_PIN_LIGHT: i64 = 15;
const BLEND_MODE_HARD_MIX: i64 = 16;
const BLEND_MODE_DIFFERENCE: i64 = 17;
const BLEND_MODE_EXCLUSION: i64 = 18;
const BLEND_MODE_HUE: i64 = 19;
const BLEND_MODE_SATURATION: i64 = 20;
const BLEND_MODE_COLOR: i64 = 21;
const BLEND_MODE_LUMINOSITY: i64 = 22;
const BLEND_MODE_PASS_THROUGH: i64 = 23;
const BLEND_MODE_DARKER_COLOR: i64 = 24;
const BLEND_MODE_LIGHTER_COLOR: i64 = 25;
const BLEND_MODE_SUBTRACT: i64 = 26;
const BLEND_MODE_DIVIDE: i64 = 27;

const LAYER_TYPE_NORMAL: i64 = 0;
const LAYER_TYPE_HIDDEN: i64 = 1;
const LAYER_TYPE_FOLDER: i64 = 2;
const LAYER_TYPE_ADJUST: i64 = 3;
const LAYER_TYPE_FILL: i64 = 4;

/// Generate one read-only static property per PSD constant.
macro_rules! psd_constant_properties {
    ($($name:ident = $value:expr;)*) => {
        $(
            extern "C" fn $name(
                _engine: *mut c_void,
                out: *mut Value,
                _out_error: *mut *mut c_char,
            ) -> c_int {
                set_int(out, $value);
                0
            }
        )*

        fn constant_properties() -> Vec<NativePropertyDef> {
            vec![
                $(NativePropertyDef {
                    name: stringify!($name),
                    get: Some($name),
                    set: None,
                },)*
            ]
        }
    };
}

psd_constant_properties! {
    color_mode_bitmap = COLOR_MODE_BITMAP;
    color_mode_grayscale = COLOR_MODE_GRAYSCALE;
    color_mode_indexed = COLOR_MODE_INDEXED;
    color_mode_rgb = COLOR_MODE_RGB;
    color_mode_cmyk = COLOR_MODE_CMYK;
    color_mode_multichannel = COLOR_MODE_MULTICHANNEL;
    color_mode_duotone = COLOR_MODE_DUOTONE;
    color_mode_lab = COLOR_MODE_LAB;

    blend_mode_normal = BLEND_MODE_NORMAL;
    blend_mode_dissolve = BLEND_MODE_DISSOLVE;
    blend_mode_darken = BLEND_MODE_DARKEN;
    blend_mode_multiply = BLEND_MODE_MULTIPLY;
    blend_mode_color_burn = BLEND_MODE_COLOR_BURN;
    blend_mode_linear_burn = BLEND_MODE_LINEAR_BURN;
    blend_mode_lighten = BLEND_MODE_LIGHTEN;
    blend_mode_screen = BLEND_MODE_SCREEN;
    blend_mode_color_dodge = BLEND_MODE_COLOR_DODGE;
    blend_mode_linear_dodge = BLEND_MODE_LINEAR_DODGE;
    blend_mode_overlay = BLEND_MODE_OVERLAY;
    blend_mode_soft_light = BLEND_MODE_SOFT_LIGHT;
    blend_mode_hard_light = BLEND_MODE_HARD_LIGHT;
    blend_mode_vivid_light = BLEND_MODE_VIVID_LIGHT;
    blend_mode_linear_light = BLEND_MODE_LINEAR_LIGHT;
    blend_mode_pin_light = BLEND_MODE_PIN_LIGHT;
    blend_mode_hard_mix = BLEND_MODE_HARD_MIX;
    blend_mode_difference = BLEND_MODE_DIFFERENCE;
    blend_mode_exclusion = BLEND_MODE_EXCLUSION;
    blend_mode_hue = BLEND_MODE_HUE;
    blend_mode_saturation = BLEND_MODE_SATURATION;
    blend_mode_color = BLEND_MODE_COLOR;
    blend_mode_luminosity = BLEND_MODE_LUMINOSITY;
    blend_mode_pass_through = BLEND_MODE_PASS_THROUGH;
    blend_mode_darker_color = BLEND_MODE_DARKER_COLOR;
    blend_mode_lighter_color = BLEND_MODE_LIGHTER_COLOR;
    blend_mode_subtract = BLEND_MODE_SUBTRACT;
    blend_mode_divide = BLEND_MODE_DIVIDE;

    layer_type_normal = LAYER_TYPE_NORMAL;
    layer_type_hidden = LAYER_TYPE_HIDDEN;
    layer_type_folder = LAYER_TYPE_FOLDER;
    layer_type_adjust = LAYER_TYPE_ADJUST;
    layer_type_fill = LAYER_TYPE_FILL;
}

// ---------------------------------------------------------------------------
// TJS object helpers
// ---------------------------------------------------------------------------

/// Evaluate `expression` and keep the resulting object.
fn new_object(host: &Tjs2Engine, expression: &str, tag: &str) -> Result<DetachedValue, String> {
    match host.eval_retained(expression, tag) {
        Ok(RetainedValue::Object(object)) => Ok(object),
        Ok(RetainedValue::Value(value)) => Err(format!("expected an object, got {value:?}")),
        Err(error) => Err(error.to_string()),
    }
}

/// Set an integer member.
fn set_int_member(
    host: &Tjs2Engine,
    object: Tjs2ValueId,
    key: &str,
    value: i64,
) -> Result<(), String> {
    host.set_member(object, key, &TjsValue::Integer(value))
        .map_err(|error| error.to_string())
}

/// Set a string member.
fn set_str_member(
    host: &Tjs2Engine,
    object: Tjs2ValueId,
    key: &str,
    value: &str,
) -> Result<(), String> {
    host.set_member(object, key, &TjsValue::String(value.to_string()))
        .map_err(|error| error.to_string())
}

/// Move a retained object into a member; the retention is consumed by the
/// member copy.
fn set_obj_member(
    host: &Tjs2Engine,
    object: Tjs2ValueId,
    key: &str,
    child: DetachedValue,
) -> Result<(), String> {
    let id = child.raw_id();
    host.set_member(object, key, &TjsValue::Retained(id as u64))
        .map_err(|error| error.to_string())?;
    std::mem::forget(child);
    Ok(())
}

/// Write a retained object into a native callback's out slot.
fn set_retained_out(out: *mut Value, object: DetachedValue) {
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = std::ptr::null();
            (*out).array = std::ptr::null();
            (*out).array_count = 0;
            (*out).retained = object.raw_id() as usize;
        }
    }
    std::mem::forget(object);
}

// ---------------------------------------------------------------------------
// Scene helpers
// ---------------------------------------------------------------------------

/// The attached L2 scene, mutably.
fn scene_mut<'a>(ctx: &CompatCtx) -> Result<&'a mut Scene, String> {
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `scene_mut`.
    let ptr = unsafe { ctx.services() }
        .scene()
        .ok_or_else(|| "PSD: no scene attached".to_string())?;
    // SAFETY: the host keeps the attached scene alive for the binding's
    // lifetime and the VM is single-threaded.
    Ok(unsafe { &mut *ptr })
}

/// ARGB (`0xAARRGGBB`) → straight-alpha RGBA.
fn argb_to_rgba(color: i64) -> [u8; 4] {
    let value = color as u32;
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    ]
}

/// Map a KiriKiri `tTVPLayerType` onto the render pipeline's blend mode
/// (mirrors `visual/layer/blend.rs::from_native`).
fn native_to_blend(value: i32) -> SceneBlend {
    match value {
        1 => SceneBlend::Replace,
        2 | 13 => SceneBlend::SourceOver,
        3 | 14 => SceneBlend::Additive,
        4 | 15 => SceneBlend::Subtractive,
        5 | 16 => SceneBlend::Multiply,
        11 | 17 => SceneBlend::Screen,
        12 => SceneBlend::AddAlpha,
        8 | 21 => SceneBlend::DestinationDependent(DstBlend::ColorDodge),
        22 => SceneBlend::DestinationDependent(DstBlend::ColorDodge5),
        23 => SceneBlend::DestinationDependent(DstBlend::ColorBurn),
        9 | 25 => SceneBlend::DestinationDependent(DstBlend::Darken),
        10 | 24 => SceneBlend::DestinationDependent(DstBlend::Lighten),
        26 => SceneBlend::DestinationDependent(DstBlend::Difference),
        27 => SceneBlend::DestinationDependent(DstBlend::Difference5),
        28 => SceneBlend::DestinationDependent(DstBlend::Exclusion),
        18 => SceneBlend::DestinationDependent(DstBlend::Overlay),
        19 => SceneBlend::DestinationDependent(DstBlend::HardLight),
        20 => SceneBlend::DestinationDependent(DstBlend::SoftLight),
        _ => SceneBlend::SourceOver,
    }
}

/// Allocate (or resize) `layer_id`'s bitmap and return its id, filling new
/// area with the layer's neutral colour.
fn ensure_bitmap(scene: &mut Scene, layer_id: u32, width: u32, height: u32) -> Option<u32> {
    let (fill, existing) = {
        let layer = scene.layer(layer_id)?;
        (argb_to_rgba(layer.neutral_color), layer.bitmap)
    };
    match existing {
        Some(bitmap_id) => {
            scene.resize_bitmap(bitmap_id, width, height, fill);
            Some(bitmap_id)
        }
        None => {
            let count = (width as usize)
                .checked_mul(height as usize)?
                .checked_mul(4)?;
            let mut rgba = vec![0u8; count];
            for chunk in rgba.chunks_exact_mut(4) {
                chunk.copy_from_slice(&fill);
            }
            Some(scene.add_bitmap(width, height, rgba))
        }
    }
}

/// Copy `pixels` into the layer's bitmap, resizing it first. The image is the
/// layer's MainImage: the reference writes it through
/// `mainImageBufferForWrite` (`reference/cpp/plugins/psdfile/psdclass.cpp:380-385`),
/// so the layer owns it and no script `Bitmap` object claims it.
fn write_layer_pixels(scene: &mut Scene, layer_id: u32, width: u32, height: u32, pixels: Vec<u8>) {
    let Some(bitmap_id) = ensure_bitmap(scene, layer_id, width, height) else {
        return;
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        bitmap.width = width;
        bitmap.height = height;
        bitmap.rgba = pixels;
        bitmap.mark_dirty();
    }
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.bitmap = Some(bitmap_id);
        layer.image_modified = true;
    }
}

/// Place a decoded layer image (reference `PSD::_getLayerData`).
fn place_layer_image(
    scene: &mut Scene,
    layer_id: u32,
    image: decode::LayerImage,
    visible: bool,
    name: &str,
) {
    let decode::LayerImage {
        left,
        top,
        width,
        height,
        opacity,
        native_type,
        pixels,
    } = image;
    write_layer_pixels(scene, layer_id, width, height, pixels);
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.rect.x = left;
        layer.rect.y = top;
        layer.rect.w = width;
        layer.rect.h = height;
        layer.image_left = 0;
        layer.image_top = 0;
        layer.image_width = width;
        layer.image_height = height;
        layer.opacity = f32::from(opacity) / 255.0;
        layer.visible = visible;
        layer.blend = native_to_blend(native_type);
        layer.name = name.to_string();
        layer.image_modified = true;
    }
}

// ---------------------------------------------------------------------------
// Layer argument resolution
// ---------------------------------------------------------------------------

/// Read a scene layer id out of a layer object argument (reference
/// `ncbPropAccessor`-based `Layer` downcast): the VM's native-instance
/// lookup for the `Layer` class. A non-`Layer` object is an error.
fn resolve_layer_id(host: &Tjs2Engine, value: &Value) -> Result<u32, String> {
    let id = match value.ty {
        VAL_INTEGER => value.integer,
        VAL_REAL => value.real as i64,
        VAL_OBJECT => {
            let object = host
                .retain_object_arg(value)
                .map_err(|error| error.to_string())?;
            match crate::CompatCtx::native_instance_id(host, object.raw_id(), &["Layer"]) {
                Ok(Some(id)) => i64::from(id),
                Ok(None) => return Err("PSD: not a layer".to_string()),
                Err(error) => return Err(error),
            }
        }
        _ => return Err("PSD: not a layer".to_string()),
    };
    if id < 0 {
        return Err("PSD: not a layer".to_string());
    }
    Ok(id as u32)
}

/// The script-visible layer name, preferring the Unicode name.
fn layer_name(layer: &LayerInfo) -> String {
    if layer.layer_name_unicode.is_empty() {
        layer.layer_name.clone()
    } else {
        layer.layer_name_unicode.clone()
    }
}

/// Look up layer `no`, validating the index.
fn layer_at(file: &PsdFile, no: i64) -> Result<&LayerInfo, String> {
    if no < 0 || no as usize >= file.layers.len() {
        return Err("PSD: not such layer".to_string());
    }
    Ok(&file.layers[no as usize])
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

extern "C" fn psd_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(PsdInst::default())) as *mut c_void
}

extern "C" fn psd_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `psd_create` and is freed exactly once.
    drop(unsafe { Box::from_raw(instance as *mut PsdInst) });
}

extern "C" fn psd_invalidate(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the live payload for the call.
    unsafe { &mut *(instance as *mut PsdInst) }.file = None;
}

/// `new PSD()` — the reference `Factory`.
extern "C" fn psd_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// load
// ---------------------------------------------------------------------------

/// Read a PSD file through the attached L2 storage, falling back to the host
/// file system (the reference's placed-path lookup then local read).
fn read_psd_bytes(ctx: &CompatCtx, filename: &str) -> Result<Vec<u8>, String> {
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `read_psd_bytes`.
    let services = unsafe { ctx.services() };
    // The media prefix is L3 syntax; the L2 namespace resolves the bare name.
    let lookup = strip_file_media(filename);
    if let Some(storage) = services.storage() {
        // SAFETY: the host keeps the attached storage alive for the binding's
        // lifetime.
        match unsafe { &*storage }.read(lookup) {
            Ok(bytes) => return Ok(bytes),
            Err(StorageReadError::NotFound(_)) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let file_system = fs::FileSystem::host("");
    fs::read(&file_system, &FsPath::host(lookup))
        .map_err(|error| format!("cannot read {filename}: {error}"))
}

/// `load()` — read the file, parse it and store the result on the instance.
extern "C" fn psd_load(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(binding) = resolve(engine) else {
        return report_error(out_error, "PSD.load: compat context is not bound");
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(filename) = argv.first() else {
        return report_error(out_error, "PSD.load requires 1 argument");
    };
    let filename = value_as_string(filename);
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &mut *(instance as *mut PsdInst) };

    match read_psd_bytes(&binding.ctx(), &filename) {
        Ok(bytes) => match parse::parse(&bytes) {
            Ok(file) => {
                inst.file = Some(file);
                set_bool(out, true);
                0
            }
            Err(error) => {
                inst.file = None;
                log::warn!("PSD.load({filename:?}): {error}");
                set_bool(out, false);
                0
            }
        },
        Err(error) => {
            inst.file = None;
            log::warn!("PSD.load({filename:?}): {error}");
            set_bool(out, false);
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Basic properties
// ---------------------------------------------------------------------------

/// A read-only integer property over the parsed header.
macro_rules! psd_header_prop {
    ($get:ident, $field:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live payload for the call.
            let inst = unsafe { &*(instance as *const PsdInst) };
            let value = inst
                .file
                .as_ref()
                .map_or(-1, |file| file.header.$field as i64);
            set_int(out, value);
            0
        }
    };
}

psd_header_prop!(psd_width_get, width);
psd_header_prop!(psd_height_get, height);
psd_header_prop!(psd_channels_get, channels);
psd_header_prop!(psd_depth_get, depth);
psd_header_prop!(psd_color_mode_get, mode);

/// `layer_count` — the number of layer records.
extern "C" fn psd_layer_count_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const PsdInst) };
    let value = inst
        .file
        .as_ref()
        .map_or(-1, |file| file.layers.len() as i64);
    set_int(out, value);
    0
}

// ---------------------------------------------------------------------------
// Layer accessors
// ---------------------------------------------------------------------------

/// `getLayerType(no)`.
extern "C" fn psd_get_layer_type(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "PSD.getLayerType: compat context is not bound");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const PsdInst) };
    let Some(file) = inst.file.as_ref() else {
        return report_error(out_error, "PSD: no data");
    };
    let Some(no) = argv.first() else {
        return report_error(out_error, "PSD.getLayerType requires 1 argument");
    };
    match layer_at(file, value_as_i64(no)) {
        Ok(layer) => {
            set_int(out, i64::from(layer.layer_type.as_i32()));
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `getLayerName(no)`.
extern "C" fn psd_get_layer_name(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(binding) = resolve(engine) else {
        return report_error(out_error, "PSD.getLayerName: compat context is not bound");
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const PsdInst) };
    let Some(file) = inst.file.as_ref() else {
        return report_error(out_error, "PSD: no data");
    };
    let Some(no) = argv.first() else {
        return report_error(out_error, "PSD.getLayerName requires 1 argument");
    };
    match layer_at(file, value_as_i64(no)) {
        Ok(layer) => {
            set_string_out(&binding.ctx(), out, &layer_name(layer));
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Build the `getLayerInfo` dictionary for one layer.
fn build_layer_info(
    host: &Tjs2Engine,
    file: &PsdFile,
    index: usize,
) -> Result<DetachedValue, String> {
    let layer = &file.layers[index];
    let dict = new_object(host, "%[]", "PSD.getLayerInfo")?;
    let id = dict.raw_id();
    set_int_member(host, id, "top", i64::from(layer.top))?;
    set_int_member(host, id, "left", i64::from(layer.left))?;
    set_int_member(host, id, "bottom", i64::from(layer.bottom))?;
    set_int_member(host, id, "right", i64::from(layer.right))?;
    set_int_member(host, id, "width", i64::from(layer.width))?;
    set_int_member(host, id, "height", i64::from(layer.height))?;
    set_int_member(host, id, "opacity", i64::from(layer.opacity))?;
    set_int_member(host, id, "fill_opacity", i64::from(layer.fill_opacity))?;
    set_int_member(
        host,
        id,
        "mask",
        i64::from(layer.channels.iter().any(|c| c.is_mask_channel())),
    )?;
    set_int_member(host, id, "type", i64::from(layer.blend_mode.as_tvp()))?;
    set_int_member(host, id, "layer_type", i64::from(layer.layer_type.as_i32()))?;
    set_int_member(host, id, "blend_mode", i64::from(layer.blend_mode.as_i32()))?;
    set_int_member(host, id, "visible", i64::from(layer.is_visible()))?;
    set_str_member(host, id, "name", &layer_name(layer))?;
    set_int_member(host, id, "clipping", i64::from(layer.clipping))?;
    set_int_member(host, id, "layer_id", i64::from(layer.layer_id))?;
    set_int_member(host, id, "obsolete", i64::from(layer.is_obsolete()))?;
    set_int_member(
        host,
        id,
        "transparency_protected",
        i64::from(layer.is_transparency_protected()),
    )?;
    set_int_member(
        host,
        id,
        "pixel_data_irrelevant",
        i64::from(layer.is_pixel_data_irrelevant()),
    )?;
    if let Some(parent) = layer.parent {
        set_int_member(
            host,
            id,
            "group_layer_id",
            i64::from(file.layers[parent].layer_id),
        )?;
    }
    if !layer.layer_comps.is_empty() {
        let comps = new_object(host, "%[]", "PSD.getLayerInfo")?;
        for (comp_id, comp) in &layer.layer_comps {
            let entry = new_object(host, "%[]", "PSD.getLayerInfo")?;
            set_int_member(host, entry.raw_id(), "id", i64::from(comp.id))?;
            set_int_member(host, entry.raw_id(), "offset_x", i64::from(comp.offset_x))?;
            set_int_member(host, entry.raw_id(), "offset_y", i64::from(comp.offset_y))?;
            set_int_member(host, entry.raw_id(), "enable", i64::from(comp.is_enabled))?;
            set_obj_member(host, comps.raw_id(), &comp_id.to_string(), entry)?;
        }
        set_obj_member(host, id, "layer_comp", comps)?;
    }
    Ok(dict)
}

/// `getLayerInfo(no)`.
extern "C" fn psd_get_layer_info(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(binding) = resolve(engine) else {
        return report_error(out_error, "PSD.getLayerInfo: compat context is not bound");
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const PsdInst) };
    let Some(file) = inst.file.as_ref() else {
        return report_error(out_error, "PSD: no data");
    };
    let Some(no) = argv.first() else {
        return report_error(out_error, "PSD.getLayerInfo requires 1 argument");
    };
    let no = value_as_i64(no);
    if no < 0 || no as usize >= file.layers.len() {
        return report_error(out_error, "PSD: not such layer");
    }
    match build_layer_info(binding.engine(), file, no as usize) {
        Ok(dict) => {
            set_retained_out(out, dict);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// Layer data
// ---------------------------------------------------------------------------

/// The shared body of the three `getLayerData*` methods.
#[allow(clippy::too_many_arguments)]
fn get_layer_data_impl(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    mode: ImageMode,
    method: &str,
) -> c_int {
    let Some(binding) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("PSD.{method}: compat context is not bound"),
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    if argv.len() < 2 {
        return report_error(out_error, &format!("PSD.{method} requires 2 arguments"));
    }
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const PsdInst) };
    let Some(file) = inst.file.as_ref() else {
        return report_error(out_error, "PSD: no data");
    };
    let no = value_as_i64(&argv[1]);
    let layer = match layer_at(file, no) {
        Ok(layer) => layer,
        Err(error) => return report_error(out_error, &error),
    };
    if layer.layer_type != LayerType::Normal
        && !(layer.layer_type == LayerType::Folder && mode == ImageMode::Mask)
    {
        return report_error(out_error, "PSD: invalid layer type");
    }
    let layer_id = match resolve_layer_id(binding.engine(), &argv[0]) {
        Ok(id) => id,
        Err(error) => return report_error(out_error, &error),
    };
    let image = match decode::layer_image(file, layer, mode) {
        Ok(Some(image)) => image,
        Ok(None) => {
            set_void(out);
            return 0;
        }
        Err(error) => return report_error(out_error, &format!("PSD: {error}")),
    };
    let scene = match scene_mut(&binding.ctx()) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if scene.layer(layer_id).is_none() {
        return report_error(out_error, "PSD: not a layer");
    }
    let name = layer_name(layer);
    place_layer_image(scene, layer_id, image, layer.is_visible(), &name);
    set_void(out);
    0
}

/// `getLayerData(layer, no)` — image with the mask folded into alpha.
extern "C" fn psd_get_layer_data(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    get_layer_data_impl(
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        ImageMode::MaskedImage,
        "getLayerData",
    )
}

/// `getLayerDataRaw(layer, no)` — the image without its mask.
extern "C" fn psd_get_layer_data_raw(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    get_layer_data_impl(
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        ImageMode::Image,
        "getLayerDataRaw",
    )
}

/// `getLayerDataMask(layer, no)` — the mask only, as grayscale.
extern "C" fn psd_get_layer_data_mask(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    get_layer_data_impl(
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        ImageMode::Mask,
        "getLayerDataMask",
    )
}

/// `getBlend(layer)` — the stored composite image.
extern "C" fn psd_get_blend(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(binding) = resolve(engine) else {
        return report_error(out_error, "PSD.getBlend: compat context is not bound");
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(layer_value) = argv.first() else {
        return report_error(out_error, "PSD.getBlend requires 1 argument");
    };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const PsdInst) };
    let Some(file) = inst.file.as_ref() else {
        return report_error(out_error, "PSD: no data");
    };
    let (width, height, pixels) = match decode::merged_image(file) {
        Ok(Some(image)) => image,
        Ok(None) => {
            set_bool(out, false);
            return 0;
        }
        Err(error) => return report_error(out_error, &format!("PSD.getBlend: {error}")),
    };
    let layer_id = match resolve_layer_id(binding.engine(), layer_value) {
        Ok(id) => id,
        Err(error) => return report_error(out_error, &error),
    };
    let scene = match scene_mut(&binding.ctx()) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if scene.layer(layer_id).is_none() {
        return report_error(out_error, "PSD: not a layer");
    }
    write_layer_pixels(scene, layer_id, width, height, pixels);
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.rect.w = width;
        layer.rect.h = height;
        layer.image_left = 0;
        layer.image_top = 0;
        layer.image_width = width;
        layer.image_height = height;
        layer.image_modified = true;
    }
    set_bool(out, true);
    0
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// `getSlices()`.
extern "C" fn psd_get_slices(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(binding) = resolve(engine) else {
        return report_error(out_error, "PSD.getSlices: compat context is not bound");
    };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const PsdInst) };
    let Some(file) = inst.file.as_ref() else {
        return report_error(out_error, "PSD: no data");
    };
    let Some(slice) = file.slice.as_ref() else {
        set_void(out);
        return 0;
    };
    let host = binding.engine();
    let result = (|| -> Result<DetachedValue, String> {
        let dict = new_object(host, "%[]", "PSD.getSlices")?;
        let id = dict.raw_id();
        set_int_member(host, id, "top", i64::from(slice.bounding_top))?;
        set_int_member(host, id, "left", i64::from(slice.bounding_left))?;
        set_int_member(host, id, "bottom", i64::from(slice.bounding_bottom))?;
        set_int_member(host, id, "right", i64::from(slice.bounding_right))?;
        set_str_member(host, id, "name", &slice.group_name)?;
        let array = new_object(host, "[]", "PSD.getSlices")?;
        for (index, item) in slice.slices.iter().enumerate() {
            let entry = new_object(host, "%[]", "PSD.getSlices")?;
            let entry_id = entry.raw_id();
            set_int_member(host, entry_id, "id", i64::from(item.id))?;
            set_int_member(host, entry_id, "group_id", i64::from(item.group_id))?;
            set_int_member(host, entry_id, "origin", i64::from(item.origin))?;
            set_int_member(host, entry_id, "type", i64::from(item.slice_type))?;
            set_int_member(host, entry_id, "left", i64::from(item.left))?;
            set_int_member(host, entry_id, "top", i64::from(item.top))?;
            set_int_member(host, entry_id, "right", i64::from(item.right))?;
            set_int_member(host, entry_id, "bottom", i64::from(item.bottom))?;
            let color = (i64::from(item.color_a) << 24)
                | (i64::from(item.color_r) << 16)
                | (i64::from(item.color_g) << 8)
                | i64::from(item.color_b);
            set_int_member(host, entry_id, "color", color)?;
            set_int_member(
                host,
                entry_id,
                "cell_text_is_html",
                i64::from(item.cell_text_is_html),
            )?;
            set_int_member(
                host,
                entry_id,
                "horizontal_alignment",
                i64::from(item.horizontal_align),
            )?;
            set_int_member(
                host,
                entry_id,
                "vertical_alignment",
                i64::from(item.vertical_align),
            )?;
            set_int_member(
                host,
                entry_id,
                "associated_layer_id",
                i64::from(item.associated_layer_id),
            )?;
            set_str_member(host, entry_id, "name", &item.name)?;
            set_str_member(host, entry_id, "url", &item.url)?;
            set_str_member(host, entry_id, "target", &item.target)?;
            set_str_member(host, entry_id, "message", &item.message)?;
            set_str_member(host, entry_id, "alt_tag", &item.alt_tag)?;
            set_str_member(host, entry_id, "cell_text", &item.cell_text)?;
            set_obj_member(host, array.raw_id(), &index.to_string(), entry)?;
        }
        set_obj_member(host, id, "slices", array)?;
        Ok(dict)
    })();
    match result {
        Ok(dict) => {
            set_retained_out(out, dict);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `getGuides()`.
extern "C" fn psd_get_guides(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(binding) = resolve(engine) else {
        return report_error(out_error, "PSD.getGuides: compat context is not bound");
    };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const PsdInst) };
    let Some(file) = inst.file.as_ref() else {
        return report_error(out_error, "PSD: no data");
    };
    let Some(grid) = file.grid_guide.as_ref() else {
        set_void(out);
        return 0;
    };
    let host = binding.engine();
    let result = (|| -> Result<DetachedValue, String> {
        let dict = new_object(host, "%[]", "PSD.getGuides")?;
        let id = dict.raw_id();
        set_int_member(host, id, "horz_grid", i64::from(grid.horizontal_grid))?;
        set_int_member(host, id, "vert_grid", i64::from(grid.vertical_grid))?;
        let vertical = new_object(host, "[]", "PSD.getGuides")?;
        let horizontal = new_object(host, "[]", "PSD.getGuides")?;
        let mut v = 0usize;
        let mut h = 0usize;
        for guide in &grid.guides {
            if guide.direction == 0 {
                set_obj_element(host, vertical.raw_id(), v, i64::from(guide.location))?;
                v += 1;
            } else {
                set_obj_element(host, horizontal.raw_id(), h, i64::from(guide.location))?;
                h += 1;
            }
        }
        set_obj_member(host, id, "vertical", vertical)?;
        set_obj_member(host, id, "horizontal", horizontal)?;
        Ok(dict)
    })();
    match result {
        Ok(dict) => {
            set_retained_out(out, dict);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `getLayerComp()`.
extern "C" fn psd_get_layer_comp(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(binding) = resolve(engine) else {
        return report_error(out_error, "PSD.getLayerComp: compat context is not bound");
    };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const PsdInst) };
    let Some(file) = inst.file.as_ref() else {
        return report_error(out_error, "PSD: no data");
    };
    if file.layer_comps.is_empty() {
        set_void(out);
        return 0;
    }
    let host = binding.engine();
    let result = (|| -> Result<DetachedValue, String> {
        let dict = new_object(host, "%[]", "PSD.getLayerComp")?;
        let id = dict.raw_id();
        set_int_member(
            host,
            id,
            "last_applied_id",
            i64::from(file.last_applied_comp_id),
        )?;
        let array = new_object(host, "[]", "PSD.getLayerComp")?;
        for (index, comp) in file.layer_comps.iter().enumerate() {
            let entry = new_object(host, "%[]", "PSD.getLayerComp")?;
            let entry_id = entry.raw_id();
            set_int_member(host, entry_id, "id", i64::from(comp.id))?;
            set_int_member(
                host,
                entry_id,
                "record_visibility",
                i64::from(comp.record_visibility),
            )?;
            set_int_member(
                host,
                entry_id,
                "record_position",
                i64::from(comp.record_position),
            )?;
            set_int_member(
                host,
                entry_id,
                "record_appearance",
                i64::from(comp.record_appearance),
            )?;
            set_str_member(host, entry_id, "name", &comp.name)?;
            set_str_member(host, entry_id, "comment", &comp.comment)?;
            set_obj_member(host, array.raw_id(), &index.to_string(), entry)?;
        }
        set_obj_member(host, id, "comps", array)?;
        Ok(dict)
    })();
    match result {
        Ok(dict) => {
            set_retained_out(out, dict);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Append an integer to a TJS array.
fn set_obj_element(
    host: &Tjs2Engine,
    object: Tjs2ValueId,
    index: usize,
    value: i64,
) -> Result<(), String> {
    set_int_member(host, object, &index.to_string(), value)
}

/// `clearStorageCache()` — the static cache clear (a no-op: the compat port
/// keeps no process-global PSD storage cache).
extern "C" fn psd_clear_storage_cache(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `PSD` native class on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "PSD",
        create: psd_create,
        destroy: psd_destroy,
        invalidate: Some(psd_invalidate),
        methods: vec![
            NativeInstanceMethodDef {
                name: "PSD",
                f: psd_ctor,
            },
            NativeInstanceMethodDef {
                name: "load",
                f: psd_load,
            },
            NativeInstanceMethodDef {
                name: "getLayerType",
                f: psd_get_layer_type,
            },
            NativeInstanceMethodDef {
                name: "getLayerName",
                f: psd_get_layer_name,
            },
            NativeInstanceMethodDef {
                name: "getLayerInfo",
                f: psd_get_layer_info,
            },
            NativeInstanceMethodDef {
                name: "getLayerData",
                f: psd_get_layer_data,
            },
            NativeInstanceMethodDef {
                name: "getLayerDataRaw",
                f: psd_get_layer_data_raw,
            },
            NativeInstanceMethodDef {
                name: "getLayerDataMask",
                f: psd_get_layer_data_mask,
            },
            NativeInstanceMethodDef {
                name: "getSlices",
                f: psd_get_slices,
            },
            NativeInstanceMethodDef {
                name: "getGuides",
                f: psd_get_guides,
            },
            NativeInstanceMethodDef {
                name: "getBlend",
                f: psd_get_blend,
            },
            NativeInstanceMethodDef {
                name: "getLayerComp",
                f: psd_get_layer_comp,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "width",
                get: Some(psd_width_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(psd_height_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "channels",
                get: Some(psd_channels_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "depth",
                get: Some(psd_depth_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "color_mode",
                get: Some(psd_color_mode_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "layer_count",
                get: Some(psd_layer_count_get),
                set: None,
            },
        ],
    })?;
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "PSD",
        methods: vec![NativeMethodDef {
            name: "clearStorageCache",
            f: psd_clear_storage_cache,
        }],
        properties: constant_properties(),
    })
}
