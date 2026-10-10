//! `LayerExBase`: the shared layer-extension base class.
//!
//! Reference: `reference/cpp/plugins/LayerExBase.{h,cpp}` +
//! `layerExBase.hpp`. The C++ class is a native instance attached to a
//! `Layer` object that caches `imageLeft`/`imageTop`/`imageWidth`/
//! `imageHeight`/`mainImageBufferForWrite`/`mainImageBufferPitch`/`update`
//! and the clip rectangle, and drives `Layer.update`. The port registers it
//! as an instance class bound to a layer (optionally at construction) and
//! reads the values live from the L2 scene.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Value};

use super::{BaseInst, bound, read_layer, read_layer_mut};
use crate::native_util::{args, report_error, set_int, set_void};
use crate::natives::system::try_value_as_i64;
use crate::{CompatCtx, CompatError};

// ---------------------------------------------------------------------------
// Native-instance payload lifecycle
// ---------------------------------------------------------------------------

extern "C" fn create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(BaseInst::default())) as *mut c_void
}

extern "C" fn destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: every payload came from `create`'s `Box::into_raw`.
    drop(unsafe { Box::from_raw(instance as *mut BaseInst) });
}

/// `LayerExBase(layer)` — bind the extension to a `Layer` object or id. With
/// no argument the instance is unbound and its members report an error.
extern "C" fn ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, _ctx)) = bound(engine) else {
        return report_error(out_error, "LayerExBase: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut BaseInst) };
    if let Some(value) = a.first() {
        match super::resolve_id(resolved.engine(), value) {
            Ok(id) if id >= 0 => inst.layer = Some(id as u32),
            Ok(_) => {}
            Err(error) => {
                return report_error(out_error, &format!("LayerExBase: {error}"));
            }
        }
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

/// `redraw()` — reference `layerExBase::redraw`
/// (`reference/cpp/plugins/layerExBase.hpp:102-107`) issues `Layer.update`
/// over the clip rectangle (the `NI_LayerExBase` variant over the image
/// geometry, `reference/cpp/plugins/LayerExBase.cpp:115-121`). The L2 model
/// tracks a single dirty flag, so this marks the layer dirty.
extern "C" fn redraw(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let (id, scene) = match read_layer_mut(engine, instance, "LayerExBase.redraw") {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    super::mark_layer_paint(scene, id);
    set_void(out);
    0
}

/// `update([left, top, width, height])` — request a repaint.
///
/// Reference `Layer.update` (`reference/cpp/core/visual/LayerIntf.cpp:
/// 9663-9686`): no argument updates the entire area, four arguments update
/// `(l, t, l+w, t+h)` with each component converted through `AsInteger`,
/// and a parameter count of 1–3 raises `TJS_E_BADPARAMCOUNT` (`:9674-9676`).
/// The reference event sets the layer's `CallOnPaint` flag (`:9666-9670`);
/// the L2 model tracks the same thing as `pending_paint`, so an in-bounds
/// rectangle marks the layer dirty (clipped to the layer's MainImage, the
/// reference `UpdateByScript` region mapping).
extern "C" fn update(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if !a.is_empty() && a.len() < 4 {
        return report_error(out_error, "LayerExBase.update requires 0 or 4 arguments");
    }
    let (id, scene) = match read_layer_mut(engine, instance, "LayerExBase.update") {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    if a.len() >= 4 {
        let mut rect = [0i64; 4];
        for (slot, value) in rect.iter_mut().zip(a.iter()) {
            match try_value_as_i64(value) {
                Ok(number) => *slot = number,
                Err(error) => {
                    return report_error(out_error, &format!("LayerExBase.update: {error}"));
                }
            }
        }
        let (x, y, w, h) = (
            rect[0] as i32,
            rect[1] as i32,
            rect[2] as i32,
            rect[3] as i32,
        );
        let empty = w <= 0 || h <= 0;
        if empty {
            set_void(out);
            return 0;
        }
        let Some(bitmap_id) = scene.layer(id).and_then(|l| l.bitmap) else {
            set_void(out);
            return 0;
        };
        let inside = scene.bitmap(bitmap_id).is_some_and(|b| {
            x < b.width as i32
                && y < b.height as i32
                && x.saturating_add(w) > 0
                && y.saturating_add(h) > 0
        });
        if !inside {
            set_void(out);
            return 0;
        }
    }
    super::mark_layer_paint(scene, id);
    set_void(out);
    0
}

/// `reset()` — reference `layerExBase::reset`
/// (`reference/cpp/plugins/layerExBase.hpp:112-119`,
/// `reference/cpp/plugins/LayerExBase.cpp:103-112`) refreshes the cached
/// layer geometry and buffers. The port reads the values live, so this only
/// validates that the bound layer still exists.
extern "C" fn reset(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if let Err(error) = read_layer(engine, instance, "LayerExBase.reset") {
        return report_error(out_error, &error);
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Geometry / buffer properties
// ---------------------------------------------------------------------------

macro_rules! layer_getter {
    ($name:ident, $method:literal, $field:ident) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            match read_layer(engine, instance, $method) {
                Ok((id, scene)) => {
                    let value = scene.layer(id).map(|l| l.$field).unwrap_or(0);
                    set_int(out, value as i64);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

layer_getter!(image_left_get, "LayerExBase.imageLeft", image_left);
layer_getter!(image_top_get, "LayerExBase.imageTop", image_top);
layer_getter!(image_width_get, "LayerExBase.imageWidth", image_width);
layer_getter!(image_height_get, "LayerExBase.imageHeight", image_height);

/// `mainImageBufferForWrite` — the MainImage pixel address, marking the image
/// modified (reference `GetMainImagePixelBufferForWrite`).
extern "C" fn main_image_buffer_for_write_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let (id, scene) = match read_layer_mut(engine, instance, "LayerExBase.mainImageBufferForWrite")
    {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.main_image_pixel_buffer_for_write(id) as i64);
    0
}

/// `mainImageBufferPitch` — the MainImage row pitch in bytes.
extern "C" fn main_image_buffer_pitch_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let (id, scene) = match read_layer(engine, instance, "LayerExBase.mainImageBufferPitch") {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, i64::from(scene.main_image_pitch(id)));
    0
}

// ---------------------------------------------------------------------------
// Clip properties
// ---------------------------------------------------------------------------

macro_rules! clip_getter {
    ($name:ident, $method:literal, $map:expr) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let map: fn(&engine::LayerState) -> i64 = $map;
            match read_layer(engine, instance, $method) {
                Ok((id, scene)) => {
                    let value = scene.layer(id).map(map).unwrap_or(0);
                    set_int(out, value);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

clip_getter!(clip_left_get, "LayerExBase.clipLeft", |l| i64::from(
    l.clip.map_or(0, |c| c.x)
));
clip_getter!(clip_top_get, "LayerExBase.clipTop", |l| i64::from(
    l.clip.map_or(0, |c| c.y)
));
clip_getter!(clip_width_get, "LayerExBase.clipWidth", |l| i64::from(
    l.clip.map_or(0, |c| c.w)
));
clip_getter!(clip_height_get, "LayerExBase.clipHeight", |l| i64::from(
    l.clip.map_or(0, |c| c.h)
));

/// Which clip component a setter re-derives (reference `SetClipLeft` /
/// `SetClipTop` / `SetClipWidth` / `SetClipHeight`,
/// `reference/cpp/core/visual/LayerIntf.cpp:4062-4079`).
#[derive(Clone, Copy)]
enum ClipComponent {
    Left,
    Top,
    Width,
    Height,
}

/// The clip setters all funnel through the reference `SetClip`
/// (`reference/cpp/core/visual/LayerIntf.cpp:4041-4060`): left/top clamp to
/// 0, right/bottom clamp to the MainImage bounds and never fall below
/// left/top, and a layer without a MainImage raises
/// `TVPNotDrawableLayerType` (`:4043-4044`).
macro_rules! clip_setter {
    ($name:ident, $method:literal, $component:expr) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let component: ClipComponent = $component;
            let (id, scene) = match read_layer_mut(engine, instance, $method) {
                Ok(value) => value,
                Err(error) => return report_error(out_error, &error),
            };
            let raw_value = unsafe { *value };
            let raw = match try_value_as_i64(&raw_value) {
                Ok(raw) => raw,
                Err(error) => return report_error(out_error, &format!("{}: {error}", $method)),
            };
            let Some(layer) = scene.layer(id) else {
                return report_error(
                    out_error,
                    &format!("{}: the layer no longer exists", $method),
                );
            };
            let Some(bitmap_id) = layer.bitmap else {
                return report_error(
                    out_error,
                    &format!("{}: the layer has no main image", $method),
                );
            };
            let Some((image_w, image_h)) = scene
                .bitmap(bitmap_id)
                .map(|bitmap| (i64::from(bitmap.width), i64::from(bitmap.height)))
            else {
                return report_error(
                    out_error,
                    &format!("{}: the main image no longer exists", $method),
                );
            };
            let current = layer.clip.unwrap_or_default();
            let (x, y, w, h) = (
                i64::from(current.x),
                i64::from(current.y),
                i64::from(current.w),
                i64::from(current.h),
            );
            let (x, y, w, h) = match component {
                ClipComponent::Left => (raw, y, w, h),
                ClipComponent::Top => (x, raw, w, h),
                ClipComponent::Width => (x, y, raw, h),
                ClipComponent::Height => (x, y, w, raw),
            };
            // `SetClip` with the raw component values
            // (`reference/cpp/core/visual/LayerIntf.cpp:4046-4059`).
            let left = x.max(0);
            let top = y.max(0);
            let right = x.saturating_add(w).min(image_w).max(left);
            let bottom = y.saturating_add(h).min(image_h).max(top);
            if let Some(layer) = scene.layer_mut(id) {
                let mut clip = engine::Rect::default();
                clip.x = left as i32;
                clip.y = top as i32;
                clip.w = (right - left) as u32;
                clip.h = (bottom - top) as u32;
                layer.clip = Some(clip);
            }
            0
        }
    };
}

clip_setter!(clip_left_set, "LayerExBase.clipLeft", ClipComponent::Left);
clip_setter!(clip_top_set, "LayerExBase.clipTop", ClipComponent::Top);
clip_setter!(
    clip_width_set,
    "LayerExBase.clipWidth",
    ClipComponent::Width
);
clip_setter!(
    clip_height_set,
    "LayerExBase.clipHeight",
    ClipComponent::Height
);

// ---------------------------------------------------------------------------
// Registration surface
// ---------------------------------------------------------------------------

/// The methods every `LayerEx*` class exposes.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        NativeInstanceMethodDef {
            name: "redraw",
            f: redraw,
        },
        NativeInstanceMethodDef {
            name: "update",
            f: update,
        },
        NativeInstanceMethodDef {
            name: "reset",
            f: reset,
        },
    ]
}

/// The properties every `LayerEx*` class exposes.
pub(super) fn property_defs() -> Vec<NativeInstancePropertyDef> {
    vec![
        NativeInstancePropertyDef {
            name: "imageLeft",
            get: Some(image_left_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "imageTop",
            get: Some(image_top_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "imageWidth",
            get: Some(image_width_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "imageHeight",
            get: Some(image_height_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "clipLeft",
            get: Some(clip_left_get),
            set: Some(clip_left_set),
        },
        NativeInstancePropertyDef {
            name: "clipTop",
            get: Some(clip_top_get),
            set: Some(clip_top_set),
        },
        NativeInstancePropertyDef {
            name: "clipWidth",
            get: Some(clip_width_get),
            set: Some(clip_width_set),
        },
        NativeInstancePropertyDef {
            name: "clipHeight",
            get: Some(clip_height_get),
            set: Some(clip_height_set),
        },
        NativeInstancePropertyDef {
            name: "mainImageBufferForWrite",
            get: Some(main_image_buffer_for_write_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "mainImageBufferPitch",
            get: Some(main_image_buffer_pitch_get),
            set: None,
        },
    ]
}

/// Register the `LayerExBase` class.
pub(super) fn register_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let mut methods = vec![NativeInstanceMethodDef {
        name: "LayerExBase",
        f: ctor,
    }];
    methods.extend(method_defs());
    ctx.register_instance(&NativeInstanceBuilder {
        name: "LayerExBase",
        create,
        destroy,
        invalidate: None,
        methods,
        properties: property_defs(),
    })
}
