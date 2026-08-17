//! `Layer` native class — the script-visible sprite surface.
//!
//! Mirrors `reference/cpp/core/visual/LayerIntf.cpp` for the subset the
//! game uses. Instances are backed by a [`crate::scene::Scene`] layer; the
//! payload holds the scene layer id.
//!
//! **Object arguments are a milestone simplification**: the FFI cannot
//! resolve object arguments — a Window/Layer object crossing the ABI
//! arrives as an opaque `VAL_OBJECT` with no handle. The constructor
//! therefore treats
//!
//! * `window` as int → the window with that scene id; anything else
//!   (Window object, `null`, `void`) → the **first** window in the scene
//!   (the game creates exactly one window); an error if there is none,
//! * `parent` as int ≥ 0 → the parent layer's scene id; anything else
//!   (Layer object, `null`, `void`) → `None` (attach directly to the
//!   window; `w.primaryLayer` returns an int id the game can pass back).
//!
//! The first layer of a window auto-becomes its primary layer (scene
//! `add_layer` semantics), so `window.primaryLayer` reads the first layer's
//! id.
//!
//! Surface:
//!
//! | member | behavior |
//! |---|---|
//! | `Layer(window, parent)` | create the layer; return its id |
//! | `setPos(x, y[, w, h])` | set rect position (4 args set bounds) |
//! | `setSize(w, h)` | set rect size |
//! | `fillRect(x, y, w, h, color)` | solid fill (0xAARRGGBB → RGBA) |
//! | `loadImages(name)` / `assignImages(name)` | load a bitmap from storage |
//! | `setSizeToImageSize()` | resize to the current bitmap |
//! | `setBitmap(id)` / `setImage(id)` | attach a bitmap by scene id (-1 clears; object args pending) |
//! | `bringToFront()` / `moveToFront()` | z-order to front |
//! | `setParentId(id)` | re-parent by layer id (-1 → window) |
//! | properties `visible`, `opacity` (0..255), `width`, `height`, `left`, `top`, `absolute`, `hitThreshold` | layer state |
//! | properties `imageLeft`, `imageTop`, `imageWidth`, `imageHeight` | attached-image geometry |
//! | properties `window`, `parent` | owning window / parent layer **ids** (objects pending) |
//! | `update()` / `setCursorPos(x,y)` / `focus()` | no-ops (input: later) |
//! | `setCenter(x,y)`, `setAffineOffset(x,y)`, `setImagePos`, `setImageSize` | no-ops (affine: later) |
//! | `drawText` | rasterizes into the attached scene bitmap using `tvp-text`, with a built-in fallback font |
//! | `doBoxBlur` | minimal in-place RGBA box blur over the attached scene bitmap |
//! | `beginTransition` | queues a next-poll completion callback; interpolation remains a stub |
//! | `affineCopy`, `stretchCopy/Pile/Blend`, `pileRect`, `piledCopy`, `operateRect/Stretch/Affine`, `light`, `stopTransition` | no-op stubs (pixel ops: later) |

use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void};
use std::sync::{LazyLock, Mutex};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine, Value,
};

use crate::scene::{BitmapState, LayerState};
use tvp_text::{FontFace, GlyphAtlas, LayoutOptions, layout};

use super::ffi::{
    arg_bool, arg_f64, arg_i64, arg_string, error_out, instance_ref, set_int_out, set_void_out,
};
use super::{context_engine, context_scene_mut, context_scene_read};

/// Native transition requests are completed on the next VM poll. This keeps
/// the script-side `_isTransition` state coherent: `beginTransition` returns
/// first, the script sets its flag, and only then do we invoke
/// `onTransitionCompleted`. Pixel interpolation remains outside this
/// milestone, but scene changes and callbacks no longer stall forever.
static PENDING_TRANSITIONS: LazyLock<Mutex<Vec<tjs2_sys::DetachedValue>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Payload of one script-visible `Layer` object.
#[derive(Default)]
pub(crate) struct LayerInst {
    /// Scene layer id, assigned by the constructor.
    pub id: u32,
    /// Whether the native constructor has run.
    pub constructed: bool,
    /// The blend/affine type (ltAlpha=2 default; enum values per the reference
    /// drawable.h: ltOpaque=1, ltAlpha=2, ltAdditive=3, ltSubtractive=4,
    /// ltAddAlpha=12, ... — see tvp-natives/constants.rs). Kept as a fallback
    /// for the short period before construction; constructed layers also copy
    /// this value into the shared scene contract.
    pub blend_type: i64,
}

/// `new Layer(...)` payload factory.
extern "C" fn layer_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<LayerInst>::default()) as *mut c_void
}

/// Release a `Layer` payload; removes the layer from the scene.
extern "C" fn layer_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: the trampoline passes the payload from layer_create.
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if inst.constructed {
        let mut scene = context_scene_mut();
        scene.remove_layer(inst.id);
    }
    // SAFETY: instance came from Box::into_raw.
    unsafe { drop(Box::from_raw(instance as *mut LayerInst)) };
}

/// TJS color `0xAARRGGBB` → RGBA (straight alpha), matching the reference
/// `argb_to_rgba` convention (see `LayerImpl.cpp` `FillRect` / the
/// `tTVPBaseBitmap::FillRect` argb handling).
fn argb_to_rgba(color: i64) -> [u8; 4] {
    let c = color as u32;
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
        ((c >> 24) & 0xff) as u8,
    ]
}

/// `Layer(window, parent)` — constructor; returns the new layer's scene id.
/// See the module doc for the object-argument simplification.
extern "C" fn layer_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let win_id: u32 = match args.first() {
        Some(a) if a.ty == tjs2_sys::VAL_INTEGER => {
            if a.integer < 0 {
                return error_out(
                    out_error,
                    "Layer: window must be specified (a Window object) — the reference \
                     throws 'Please specify layerTreeOwnerInterface object'",
                );
            }
            a.integer as u32
        }
        _ => {
            // Window object / null / void: attach to the first window in the
            // scene (the game has one window; see the module doc).
            let scene = context_scene_read();
            match scene.windows.first() {
                Some(w) => w.id,
                None => {
                    return error_out(
                        out_error,
                        "Layer: no window in the scene — create a Window first",
                    );
                }
            }
        }
    };
    // Parent as int ≥ 0 → parent layer id; anything else (Layer object /
    // null / void) → attach to the window.
    let parent = match args.get(1) {
        Some(a) if a.ty == tjs2_sys::VAL_INTEGER && a.integer >= 0 => Some(a.integer as u32),
        _ => None,
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if inst.constructed {
        return error_out(out_error, "Layer: this layer is already constructed");
    }
    let mut scene = context_scene_mut();
    if scene.window(win_id).is_none() {
        return error_out(out_error, "Layer: the given window does not exist");
    }
    let id = scene.add_layer(win_id, parent);
    inst.id = id;
    inst.constructed = true;
    // Capture the layer's TJS object so Window.primaryLayer can return it.
    if !_objthis.is_null() {
        super::set_layer_tjs_object(id, _objthis);
    }
    set_int_out(out, i64::from(id));
    0
}

/// `setPos(x, y[, w, h])` — set the rect position; with four arguments the
/// bounds `(x, y, w, h)` are set (matching the reference `setPos` bounds
/// form).
extern "C" fn layer_set_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.setPos requires 2 arguments");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.rect.x = arg_i64(&args[0]) as i32;
    layer.rect.y = arg_i64(&args[1]) as i32;
    if args.len() >= 4 {
        layer.rect.w = arg_i64(&args[2]).max(0) as u32;
        layer.rect.h = arg_i64(&args[3]).max(0) as u32;
    }
    set_void_out(out);
    0
}

/// `setSize(w, h)` — set the rect size (negative values clamp to 0).
extern "C" fn layer_set_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.setSize requires 2 arguments");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.rect.w = arg_i64(&args[0]).max(0) as u32;
    layer.rect.h = arg_i64(&args[1]).max(0) as u32;
    set_void_out(out);
    0
}

/// `fillRect(x, y, w, h, color)` — solid fill; `color` is `0xAARRGGBB`
/// (TJS integer), stored as straight-alpha RGBA in `LayerState::fill_color`.
extern "C" fn layer_fill_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Layer.fillRect requires 5 arguments");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.rect.x = arg_i64(&args[0]) as i32;
    layer.rect.y = arg_i64(&args[1]) as i32;
    layer.rect.w = arg_i64(&args[2]).max(0) as u32;
    layer.rect.h = arg_i64(&args[3]).max(0) as u32;
    layer.fill_color = Some(argb_to_rgba(arg_i64(&args[4])));
    set_void_out(out);
    0
}

/// `loadImages(name)` / `assignImages(name)` — load a bitmap from game
/// storage and attach it to the layer.
extern "C" fn layer_load_images(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&name_arg) = args.first() else {
        return error_out(out_error, "Layer.loadImages requires 1 argument");
    };
    if name_arg.ty != tjs2_sys::VAL_STRING {
        return error_out(out_error, "Layer.loadImages expects a storage name");
    }
    let name = super::ffi::arg_string(&name_arg);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let (mut scene, mut storage) = super::context_scene_storage();
    let bitmap_id = match crate::bitmap::load_bitmap_from_storage(
        &mut scene,
        &mut super::bitmap_cache(),
        &mut storage,
        &name,
        None,
    ) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &format!("Layer.loadImages: {e}")),
    };
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.bitmap = Some(bitmap_id);
    }
    // The reference returns an image-tag dictionary (mode/opacity); a
    // retained script object lets `ret.mode`/`ret.opacity` read undefined
    // and the game's defaults kick in.
    let engine = crate::natives::context_engine();
    let _ = engine.eval("%[]", "layer.loadImages");
    match engine.retain_value_detached(&tjs2_sys::TjsValue::Object) {
        Ok(dv) => {
            // SAFETY: out is a valid result slot; the C++ side consumes the
            // retention before the callback returns.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = std::ptr::null();
                (*out).array_count = 0;
                (*out).retained = dv.raw_id() as usize;
            }
            std::mem::forget(dv);
            0
        }
        Err(_) => {
            set_void_out(out);
            0
        }
    }
}

/// `setSizeToImageSize()` — resize the layer rect to the attached bitmap.
extern "C" fn layer_set_size_to_image_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let dims = {
        let scene = context_scene_read();
        let Some(layer) = scene.layer(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        match layer.bitmap.and_then(|id| scene.bitmap(id)) {
            Some(bmp) => (bmp.width, bmp.height),
            None => (0, 0),
        }
    };
    let mut scene = context_scene_mut();
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.rect.w = dims.0;
        layer.rect.h = dims.1;
    }
    set_void_out(out);
    0
}

/// `setBitmap(id)` / `setImage(id)` — attach a bitmap to the layer by scene
/// id (-1 clears it). The real TVP methods take a Bitmap **object**, which
/// the FFI cannot resolve yet (object arguments are pending); the game's
/// init path uses `loadImages(name)`, and scripts can pass the id read from
/// `bitmap.id` / `window.primaryLayer` / `layer.bitmap`-style int sources.
extern "C" fn layer_set_bitmap(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&id_arg) = args.first() else {
        return error_out(out_error, "Layer.setBitmap requires 1 argument");
    };
    let bitmap_id = arg_i64(&id_arg);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if bitmap_id >= 0 && scene.bitmap(bitmap_id as u32).is_none() {
        return error_out(out_error, "Layer.setBitmap: no bitmap with that id");
    }
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.bitmap = (bitmap_id >= 0).then_some(bitmap_id as u32);
    set_void_out(out);
    0
}

/// `bringToFront()` / `moveToFront()` — move the layer to the front of its
/// siblings.
extern "C" fn layer_bring_to_front(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    scene.layer_move_to_front(inst.id);
    set_void_out(out);
    0
}

/// Generate a read/write integer property over a `LayerState` accessor pair.
macro_rules! layer_int_prop {
    ($get:ident, $set:ident, $getter:expr, $setter:expr) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { instance_ref::<LayerInst>(instance) };
            let scene = context_scene_read();
            let Some(layer) = scene.layer(inst.id) else {
                return error_out(out_error, "Layer: layer no longer exists");
            };
            set_int_out(out, ($getter)(layer));
            0
        }

        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value is valid for the call.
            let v = unsafe { &*value };
            let inst = unsafe { instance_ref::<LayerInst>(instance) };
            let mut scene = context_scene_mut();
            let Some(layer) = scene.layer_mut(inst.id) else {
                return 1;
            };
            ($setter)(layer, v);
            0
        }
    };
}

layer_int_prop!(
    layer_visible_get,
    layer_visible_set,
    |l: &LayerState| i64::from(l.visible),
    |l: &mut LayerState, v: &Value| l.visible = arg_bool(v)
);
layer_int_prop!(
    layer_width_get,
    layer_width_set,
    |l: &LayerState| i64::from(l.rect.w),
    |l: &mut LayerState, v: &Value| l.rect.w = arg_i64(v).max(0) as u32
);
layer_int_prop!(
    layer_height_get,
    layer_height_set,
    |l: &LayerState| i64::from(l.rect.h),
    |l: &mut LayerState, v: &Value| l.rect.h = arg_i64(v).max(0) as u32
);
layer_int_prop!(
    layer_left_get,
    layer_left_set,
    |l: &LayerState| i64::from(l.rect.x),
    |l: &mut LayerState, v: &Value| l.rect.x = arg_i64(v) as i32
);
layer_int_prop!(
    layer_top_get,
    layer_top_set,
    |l: &LayerState| i64::from(l.rect.y),
    |l: &mut LayerState, v: &Value| l.rect.y = arg_i64(v) as i32
);
layer_int_prop!(
    layer_absolute_get,
    layer_absolute_set,
    |l: &LayerState| i64::from(l.z_order),
    |l: &mut LayerState, v: &Value| l.z_order = arg_i64(v) as i32
);
layer_int_prop!(
    layer_hit_threshold_get,
    layer_hit_threshold_set,
    |l: &LayerState| i64::from(l.hit_threshold),
    |l: &mut LayerState, v: &Value| l.hit_threshold = arg_i64(v) as i32
);
// `imageLeft` / `imageTop` currently alias the rect position (a later wave
// adds real image-offset state).
layer_int_prop!(
    layer_image_left_get,
    layer_image_left_set,
    |l: &LayerState| i64::from(l.rect.x),
    |l: &mut LayerState, v: &Value| l.rect.x = arg_i64(v) as i32
);
layer_int_prop!(
    layer_image_top_get,
    layer_image_top_set,
    |l: &LayerState| i64::from(l.rect.y),
    |l: &mut LayerState, v: &Value| l.rect.y = arg_i64(v) as i32
);

/// `opacity` — layer opacity in TJS2's 0..255 scale. The scene stores
/// 0..1; the getter returns `round(opacity * 255)` and the setter clamps
/// `value / 255.0` into 0..1.
extern "C" fn layer_opacity_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, (f64::from(layer.opacity) * 255.0).round() as i64);
    0
}

extern "C" fn layer_opacity_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return 1;
    };
    layer.opacity = (arg_f64(v) / 255.0).clamp(0.0, 1.0) as f32;
    0
}

/// `imageWidth` — the attached bitmap's width (0 when no bitmap).
extern "C" fn layer_image_width_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let w = layer
        .bitmap
        .and_then(|id| scene.bitmap(id))
        .map_or(0, |b| b.width);
    set_int_out(out, i64::from(w));
    0
}

/// `imageHeight` — see `layer_image_width_get`.
extern "C" fn layer_image_height_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let h = layer
        .bitmap
        .and_then(|id| scene.bitmap(id))
        .map_or(0, |b| b.height);
    set_int_out(out, i64::from(h));
    0
}

/// `window` — the owning window's scene id (int; object returns pending).
extern "C" fn layer_window_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    // Return the owning Window's TJS object (retained) so scripts can call
    // members on it (`window.addInputNotify(this)`).
    let win_obj = super::window_tjs_object(layer.window);
    if !win_obj.is_null() {
        let engine = crate::natives::context_engine();
        // SAFETY: engine is the registered engine; win_obj is a live TJS
        // object.
        let rid = unsafe { tjs2_sys::tjs2_retain_object(engine.raw(), win_obj) };
        if !rid.is_null() {
            // SAFETY: out is a valid result slot.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = std::ptr::null();
                (*out).array_count = 0;
                (*out).retained = rid as usize;
            }
            return 0;
        }
    }
    set_int_out(out, i64::from(layer.window));
    0
}

/// `parent` — the parent layer's scene id, or -1 (int; object returns
/// pending).
extern "C" fn layer_parent_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, layer.parent.map_or(-1, i64::from));
    0
}

/// `setParentId(id)` — re-parent the layer (-1 attaches to the window).
extern "C" fn layer_set_parent(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&id_arg) = args.first() else {
        return error_out(out_error, "Layer.setParentId requires a value");
    };
    let parent_id = arg_i64(&id_arg);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    // Read the old parent + window under a short borrow, then drop it so the
    // parent/window mutations below don't conflict.
    let (old_parent, window) = {
        let Some(layer) = scene.layer_mut(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        (layer.parent, layer.window)
    };
    // Detach from the old parent (or window list).
    if let Some(old) = old_parent {
        if let Some(p) = scene.layer_mut(old) {
            p.children.retain(|&c| c != inst.id);
        }
    } else if let Some(w) = scene.window_mut(window) {
        w.layers.retain(|&c| c != inst.id);
    }
    let new_parent = if parent_id < 0 {
        None
    } else {
        let candidate = parent_id as u32;
        let same_window = scene
            .layer(candidate)
            .is_some_and(|parent| parent.window == window);
        // Reject self-parenting and descendants as parents. A malformed FFI
        // call should not create a cycle that defeats flattening/composition.
        let mut cursor = Some(candidate);
        let mut seen = HashSet::new();
        let mut would_cycle = false;
        while let Some(id) = cursor {
            if !seen.insert(id) {
                would_cycle = true;
                break;
            }
            if id == inst.id {
                would_cycle = true;
                break;
            }
            cursor = scene.layer(id).and_then(|layer| layer.parent);
        }
        (same_window && !would_cycle).then_some(candidate)
    };
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.parent = new_parent;
    }
    // Attach to the new parent (or window list).
    match new_parent {
        Some(p) => {
            if let Some(p) = scene.layer_mut(p) {
                p.children.push(inst.id);
            }
        }
        None => {
            if let Some(w) = scene.window_mut(window) {
                w.layers.push(inst.id);
            }
        }
    }
    set_void_out(out);
    0
}

/// `update()` — no-op (the render loop syncs every frame).
extern "C" fn layer_update(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `setCursorPos(x, y)` — no-op (input is beyond milestone 3A).
extern "C" fn layer_set_cursor_pos(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `focus()` — no-op (input is beyond milestone 3A).
extern "C" fn layer_focus(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// Deterministic text metrics used by the script-side UI helpers.  These are
/// deliberately based on the same half/full-width convention as the fallback
/// rasterizer below, so layout still works on machines without a CJK font.
extern "C" fn layer_get_text_width(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(text) = args.first().map(arg_string) else {
        return error_out(out_error, "Layer.getTextWidth requires text");
    };
    let width = fallback_text_width(&text, 16);
    set_int_out(out, i64::from(width));
    0
}

extern "C" fn layer_get_text_height(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let text = args.first().map(arg_string).unwrap_or_default();
    let lines = text.split('\n').count().max(1);
    set_int_out(out, i64::from(lines as u32 * 16));
    0
}

/// Draw text into the layer's bitmap.  TVP layers are image surfaces, so a
/// layer without an attached image gets a transparent bitmap sized from its
/// rectangle (or from the text when the rectangle is still empty).  The
/// renderer already uploads dirty scene bitmaps, making these CPU pixels
/// visible without adding a second scene/render contract.
extern "C" fn layer_draw_text(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 4 {
        return error_out(out_error, "Layer.drawText requires x, y, text and color");
    }
    let x = arg_i64(&args[0]) as i32;
    let y = arg_i64(&args[1]) as i32;
    let text = arg_string(&args[2]);
    let color = argb_to_rgba(arg_i64(&args[3]));
    let opa = args.get(4).map(arg_i64).unwrap_or(255).clamp(0, 255) as u8;
    let aa = args.get(5).map(arg_bool).unwrap_or(true);
    let shadow_level = args.get(6).map(arg_i64).unwrap_or(0).max(0) as u32;
    let shadow_color = argb_to_rgba(args.get(7).map(arg_i64).unwrap_or(0));
    let shadow_width = args.get(8).map(arg_i64).unwrap_or(0).max(0) as u32;
    let shadow_x = args.get(9).map(arg_i64).unwrap_or(0) as i32;
    let shadow_y = args.get(10).map(arg_i64).unwrap_or(0) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };

    // Snapshot scene values before loading a font; font discovery may do file
    // IO and must not hold the scene lock while doing so.
    let (bitmap_id, width, _height, font_height) = {
        let mut scene = context_scene_mut();
        let Some(layer) = scene.layer(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        let font_height = scene
            .fonts
            .last()
            .map(|font| font.height.max(1) as u32)
            .unwrap_or(16);
        let needed_w = (x.max(0) as u32).saturating_add(fallback_text_width(&text, font_height));
        let needed_h = (y.max(0) as u32).saturating_add(fallback_text_height(&text, font_height));
        let (width, height) = match layer.bitmap.and_then(|id| scene.bitmap(id)) {
            Some(bitmap) => (bitmap.width, bitmap.height),
            None => (layer.rect.w.max(needed_w), layer.rect.h.max(needed_h)),
        };
        let bitmap_id = match layer.bitmap {
            Some(id) => id,
            None => {
                let id = scene.add_bitmap(
                    width.max(1),
                    height.max(1),
                    vec![0; width.max(1) as usize * height.max(1) as usize * 4],
                );
                scene
                    .layer_mut(inst.id)
                    .expect("layer checked above")
                    .bitmap = Some(id);
                id
            }
        };
        if let Some(layer) = scene.layer_mut(inst.id) {
            layer.rect.w = layer.rect.w.max(width);
            layer.rect.h = layer.rect.h.max(height);
        }
        (bitmap_id, width.max(1), height.max(1), font_height)
    };

    // Prefer tvp-text's real CJK rasterizer.  A missing system font is a
    // normal deployment situation (minimal Linux containers in particular),
    // so retain a small deterministic bitmap-font fallback rather than
    // silently dropping the game's title text.
    if std::env::var_os("KRKR_RS_SYSTEM_FONT").is_some()
        && let Some(face) = FontFace::discover_system_jp()
    {
        let mut atlas = GlyphAtlas::with_default_width(face, font_height);
        let text_layout = layout(
            &text,
            width as f32,
            font_height as f32,
            &mut atlas,
            &LayoutOptions {
                wrap: false,
                ..Default::default()
            },
        );
        let mut scene = context_scene_mut();
        if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
            if shadow_level != 0 || shadow_width != 0 {
                paint_layout(
                    bitmap,
                    &atlas,
                    &text_layout,
                    shadow_color,
                    opa,
                    aa,
                    x.saturating_add(shadow_x),
                    y.saturating_add(shadow_y),
                    shadow_level.max(shadow_width),
                );
            }
            paint_layout(bitmap, &atlas, &text_layout, color, opa, aa, x, y, 0);
            bitmap.mark_dirty();
        }
    } else {
        let mut scene = context_scene_mut();
        if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
            if shadow_level != 0 || shadow_width != 0 {
                paint_fallback_text(
                    bitmap,
                    &text,
                    shadow_color,
                    opa,
                    x + shadow_x,
                    y + shadow_y,
                    font_height,
                    shadow_level.max(shadow_width),
                );
            }
            paint_fallback_text(bitmap, &text, color, opa, x, y, font_height, 0);
            bitmap.mark_dirty();
        }
    }
    set_void_out(out);
    0
}

/// Minimal separable box blur over an attached bitmap.  It intentionally
/// averages RGBA channels (rather than only RGB), which is useful for the
/// translucent shadow layers used by ADV UI and remains predictable for
/// straight-alpha scene pixels.
extern "C" fn layer_do_box_blur(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let xradius = args.first().map(arg_i64).unwrap_or(1).max(0) as u32;
    let yradius = args.get(1).map(arg_i64).unwrap_or(1).max(0) as u32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let bitmap_id = scene.layer(inst.id).and_then(|layer| layer.bitmap);
    if let Some(bitmap_id) = bitmap_id
        && let Some(bitmap) = scene.bitmap_mut(bitmap_id)
    {
        blur_bitmap(bitmap, xradius, yradius);
    }
    set_void_out(out);
    0
}

extern "C" fn layer_begin_transition(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if !objthis.is_null()
        && let Ok(value) = context_engine().retain_object_detached(objthis)
    {
        PENDING_TRANSITIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(value);
    }
    set_void_out(out);
    0
}

/// Complete all transitions queued by the previous VM calls.
pub(crate) fn transition_poll(engine: &Tjs2Engine) {
    let pending = std::mem::take(
        &mut *PENDING_TRANSITIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner()),
    );
    for value in pending {
        let _ = engine.call_member(value.raw_id(), "onTransitionCompleted", &[]);
        drop(value);
    }
}

/// Generic no-op stub for methods that need real pixel/affine operations
/// (a later wave; documented in the module doc).
extern "C" fn layer_noop(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

fn fallback_text_width(text: &str, height: u32) -> u32 {
    let em = height.max(1);
    text.split('\n')
        .map(|line| {
            line.chars()
                .map(|ch| {
                    if ch.is_ascii() {
                        (em * 55).div_ceil(100)
                    } else {
                        em
                    }
                })
                .sum::<u32>()
        })
        .max()
        .unwrap_or(0)
}

fn fallback_text_height(text: &str, height: u32) -> u32 {
    text.split('\n').count().max(1) as u32 * height.max(1)
}

/// Alpha-composite one straight-alpha pixel.  Keeping this in the visual
/// crate makes the scene's RGBA convention explicit and avoids renderer-only
/// drawing paths.
fn blend_pixel(bitmap: &mut BitmapState, x: i32, y: i32, color: [u8; 4], coverage: u8, opa: u8) {
    if x < 0 || y < 0 || x as u32 >= bitmap.width || y as u32 >= bitmap.height {
        return;
    }
    let Some(i) = bitmap.pixel_offset(x as u32, y as u32) else {
        return;
    };
    let alpha = (u32::from(color[3]) * u32::from(coverage) * u32::from(opa) / (255 * 255)) as u8;
    if alpha == 0 {
        return;
    }
    let inv = 255u32 - u32::from(alpha);
    let old_a = u32::from(bitmap.rgba[i + 3]);
    let out_a = u32::from(alpha) + old_a * inv / 255;
    for (channel, &src_color) in color[..3].iter().enumerate() {
        let src = u32::from(src_color);
        let dst = u32::from(bitmap.rgba[i + channel]);
        // Keep the scene buffer straight-alpha. This matters for antialiased
        // text over transparent pixels: RGB must remain the requested color,
        // not color multiplied by coverage.
        bitmap.rgba[i + channel] =
            ((src * u32::from(alpha) * 255 + dst * old_a * inv) / (out_a * 255)) as u8;
    }
    bitmap.rgba[i + 3] = out_a as u8;
}

#[allow(clippy::too_many_arguments)]
fn paint_layout(
    bitmap: &mut BitmapState,
    atlas: &GlyphAtlas,
    text_layout: &tvp_text::TextLayout,
    color: [u8; 4],
    opa: u8,
    aa: bool,
    offset_x: i32,
    offset_y: i32,
    spread: u32,
) {
    let (atlas_width, _) = atlas.atlas_size();
    let pixels = atlas.atlas_rgba();
    for run in &text_layout.runs {
        for glyph in &run.chars {
            for dy in 0..glyph.size.1 {
                for dx in 0..glyph.size.0 {
                    let source = (((glyph.uv.1 + dy) * atlas_width + glyph.uv.0 + dx) * 4) as usize;
                    let mut coverage = pixels[source + 3];
                    if !aa {
                        coverage = if coverage >= 128 { 255 } else { 0 };
                    }
                    if coverage == 0 {
                        continue;
                    }
                    let gx = glyph.x.round() as i32 + dx as i32 + offset_x;
                    let gy = glyph.y.round() as i32 + dy as i32 + offset_y;
                    if spread == 0 {
                        blend_pixel(bitmap, gx, gy, color, coverage, opa);
                    } else {
                        // A compact square dilation approximates TVP's
                        // shadow width without a second glyph rasterizer.
                        let r = spread.min(8) as i32;
                        for sy in -r..=r {
                            for sx in -r..=r {
                                blend_pixel(bitmap, gx + sx, gy + sy, color, coverage, opa);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A tiny 5x7 fallback font. ASCII uses a stable per-character pattern and
/// non-ASCII characters use a bordered checker glyph; it is intentionally
/// recognizable as text while requiring no bundled font files or system font.
#[allow(clippy::too_many_arguments)]
fn paint_fallback_text(
    bitmap: &mut BitmapState,
    text: &str,
    color: [u8; 4],
    opa: u8,
    x: i32,
    y: i32,
    height: u32,
    spread: u32,
) {
    let height = height.max(7);
    let scale = (height / 8).max(1) as i32;
    let mut pen_y = y;
    for line in text.split('\n') {
        let mut pen_x = x;
        for ch in line.chars() {
            let advance = if ch.is_ascii() {
                (height * 55 / 100).max(1) as i32
            } else {
                height as i32
            };
            if !ch.is_whitespace() {
                for row in 0..7 {
                    for col in 0..5 {
                        let on = if ch.is_ascii() {
                            // Border plus a character-dependent interior
                            // pattern gives useful output for every ASCII
                            // code without a large embedded font table.
                            row == 0
                                || row == 6
                                || col == 0
                                || col == 4
                                || (ch as usize + row * 3 + col).is_multiple_of(11)
                        } else {
                            row == 0
                                || row == 6
                                || col == 0
                                || col == 4
                                || (ch as usize + row + col).is_multiple_of(5)
                        };
                        if !on {
                            continue;
                        }
                        for sy in 0..scale {
                            for sx in 0..scale {
                                let px = pen_x + col as i32 * scale + sx;
                                let py = pen_y + row as i32 * scale + sy;
                                if spread == 0 {
                                    blend_pixel(bitmap, px, py, color, 255, opa);
                                } else {
                                    let r = spread.min(8) as i32;
                                    for oy in -r..=r {
                                        for ox in -r..=r {
                                            blend_pixel(bitmap, px + ox, py + oy, color, 255, opa);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            pen_x += advance;
        }
        pen_y += height as i32;
    }
}

fn blur_bitmap(bitmap: &mut BitmapState, xradius: u32, yradius: u32) {
    if bitmap.width == 0 || bitmap.height == 0 || (xradius == 0 && yradius == 0) {
        return;
    }
    let source = bitmap.rgba.clone();
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            let x0 = x.saturating_sub(xradius);
            let x1 = x.saturating_add(xradius).min(bitmap.width - 1);
            let y0 = y.saturating_sub(yradius);
            let y1 = y.saturating_add(yradius).min(bitmap.height - 1);
            let mut sums = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..=y1 {
                for sx in x0..=x1 {
                    let i = ((sy * bitmap.width + sx) * 4) as usize;
                    for (channel, sum) in sums.iter_mut().enumerate() {
                        *sum += u32::from(source[i + channel]);
                    }
                    count += 1;
                }
            }
            let i = ((y * bitmap.width + x) * 4) as usize;
            for (channel, &sum) in sums.iter().enumerate() {
                bitmap.rgba[i + channel] = (sum / count) as u8;
            }
        }
    }
    bitmap.mark_dirty();
}

/// Register the `Layer` native class.
pub(crate) fn register_layer(engine: &Tjs2Engine) -> Result<(), String> {
    let noop_stubs = [
        "setCenter",
        "setAffineOffset",
        "setImagePos",
        "setImageSize",
        "drawGlyph",
        "drawRectangle",
        "drawRectangles",
        "drawLine",
        "drawLines",
        "drawPolygon",
        "drawArc",
        "drawBezier",
        "drawBeziers",
        "drawClosedCurve",
        "drawClosedCurve2",
        "drawCurve",
        "drawCurve2",
        "drawCurve3",
        "drawPie",
        "drawEllipse",
        "drawPath",
        "drawString",
        "drawImage",
        "drawImageRect",
        "drawImageStretch",
        "drawImageAffine",
        "setDefaultDrawTextParam",
        "resetDrawTextParam",
        "setFontStyle",
        "getDrawWidth",
        "affineCopy",
        "affinePile",
        "affineBlend",
        "stretchCopy",
        "stretchPile",
        "stretchBlend",
        "pileRect",
        "piledCopy",
        "copyRect",
        "blendRect",
        "operateRect",
        "operateStretch",
        "operateAffine",
        "gaussianBlur",
        "adjustGamma",
        "doGrayScale",
        "flipLR",
        "flipUD",
        "convertType",
        "light",
        "stopTransition",
        "saveLayerImage",
        "releaseCapture",
        "releaseTouchCapture",
        "setMode",
        "removeMode",
        "clear",
    ];
    let mut methods: Vec<NativeInstanceMethodDef> = vec![
        NativeInstanceMethodDef {
            name: "Layer",
            f: layer_ctor,
        },
        NativeInstanceMethodDef {
            name: "setPos",
            f: layer_set_pos,
        },
        NativeInstanceMethodDef {
            name: "setSize",
            f: layer_set_size,
        },
        NativeInstanceMethodDef {
            name: "fillRect",
            f: layer_fill_rect,
        },
        NativeInstanceMethodDef {
            name: "loadImages",
            f: layer_load_images,
        },
        NativeInstanceMethodDef {
            name: "assignImages",
            f: layer_load_images,
        },
        NativeInstanceMethodDef {
            name: "setSizeToImageSize",
            f: layer_set_size_to_image_size,
        },
        NativeInstanceMethodDef {
            name: "setBitmap",
            f: layer_set_bitmap,
        },
        NativeInstanceMethodDef {
            name: "setImage",
            f: layer_set_bitmap,
        },
        NativeInstanceMethodDef {
            name: "bringToFront",
            f: layer_bring_to_front,
        },
        NativeInstanceMethodDef {
            name: "moveToFront",
            f: layer_bring_to_front,
        },
        NativeInstanceMethodDef {
            name: "setParentId",
            f: layer_set_parent,
        },
        NativeInstanceMethodDef {
            name: "getTextWidth",
            f: layer_get_text_width,
        },
        NativeInstanceMethodDef {
            name: "getTextHeight",
            f: layer_get_text_height,
        },
        NativeInstanceMethodDef {
            name: "drawText",
            f: layer_draw_text,
        },
        NativeInstanceMethodDef {
            name: "doBoxBlur",
            f: layer_do_box_blur,
        },
        NativeInstanceMethodDef {
            name: "beginTransition",
            f: layer_begin_transition,
        },
        NativeInstanceMethodDef {
            name: "update",
            f: layer_update,
        },
        NativeInstanceMethodDef {
            name: "setCursorPos",
            f: layer_set_cursor_pos,
        },
        NativeInstanceMethodDef {
            name: "focus",
            f: layer_focus,
        },
    ];
    methods.extend(noop_stubs.into_iter().map(|name| NativeInstanceMethodDef {
        name,
        f: layer_noop,
    }));
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "Layer",
        create: layer_create,
        destroy: layer_destroy,
        methods,
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(layer_id_get),
                set: None,
            },
            // The layer's Font object (k2compat writes `this.font.doUserSelect`
            // and `this.font.face`). Returns a fresh script object so the
            // assignments succeed; the font's visual state is not consumed
            // by the headless load path.
            NativeInstancePropertyDef {
                name: "font",
                get: Some(layer_font_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(layer_visible_get),
                set: Some(layer_visible_set),
            },
            NativeInstancePropertyDef {
                name: "opacity",
                get: Some(layer_opacity_get),
                set: Some(layer_opacity_set),
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(layer_width_get),
                set: Some(layer_width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(layer_height_get),
                set: Some(layer_height_set),
            },
            NativeInstancePropertyDef {
                name: "left",
                get: Some(layer_left_get),
                set: Some(layer_left_set),
            },
            NativeInstancePropertyDef {
                name: "top",
                get: Some(layer_top_get),
                set: Some(layer_top_set),
            },
            // Cursor position, fed by the render input bridge from the shared
            // tvp-input state. The game's MainWindow reads these in
            // onMouseDown/onMouseMove (`primaryLayer.cursorX`/`cursorY`).
            NativeInstancePropertyDef {
                name: "cursorX",
                get: Some(layer_cursor_x_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "cursorY",
                get: Some(layer_cursor_y_get),
                set: None,
            },
            // The blend/affine type (ltAlpha=2, ltAdditive=3, ... per the reference
            // drawable.h). It is mirrored into LayerState for render sync.
            NativeInstancePropertyDef {
                name: "type",
                get: Some(layer_type_get),
                set: Some(layer_type_set),
            },
            // Whether the layer has an image attached.
            NativeInstancePropertyDef {
                name: "hasImage",
                get: Some(layer_has_image_get),
                set: Some(layer_has_image_set),
            },
            NativeInstancePropertyDef {
                name: "absolute",
                get: Some(layer_absolute_get),
                set: Some(layer_absolute_set),
            },
            NativeInstancePropertyDef {
                name: "hitThreshold",
                get: Some(layer_hit_threshold_get),
                set: Some(layer_hit_threshold_set),
            },
            NativeInstancePropertyDef {
                name: "imageLeft",
                get: Some(layer_image_left_get),
                set: Some(layer_image_left_set),
            },
            NativeInstancePropertyDef {
                name: "imageTop",
                get: Some(layer_image_top_get),
                set: Some(layer_image_top_set),
            },
            NativeInstancePropertyDef {
                name: "imageWidth",
                get: Some(layer_image_width_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "imageHeight",
                get: Some(layer_image_height_get),
                set: None,
            },
            // `window` / `parent` return scene **ids** (object returns are
            // pending, like `Window.primaryLayer`).
            NativeInstancePropertyDef {
                name: "window",
                get: Some(layer_window_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "parent",
                get: Some(layer_parent_get),
                set: None,
            },
        ],
    })
}

/// `id` — the layer's scene id (read-only; object returns are pending).
extern "C" fn layer_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    set_int_out(out, i64::from(inst.id));
    0
}

/// `cursorX` — the cursor X in primary-layer / window logical coordinates,
/// as fed by the render input bridge (mirrors `Mouse.getCursorX()`). The
/// game reads `primaryLayer.cursorX`/`cursorY` in `onMouseDown`/`onMouseMove`.
extern "C" fn layer_cursor_x_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let (x, _y) = super::shared_cursor_pos();
    set_int_out(out, i64::from(x));
    0
}

/// `cursorY` — the cursor Y in primary-layer / window logical coordinates.
extern "C" fn layer_cursor_y_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let (_x, y) = super::shared_cursor_pos();
    set_int_out(out, i64::from(y));
    0
}

/// `layer.font` — a script object the game assigns font properties on.
extern "C" fn layer_font_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let _ = instance;
    // Return a real native Font object rather than an empty dictionary. This
    // gives the script-side AffineLayer helpers working face/height and
    // getTextWidth members while preserving the retained-object ABI pattern.
    let engine = crate::natives::context_engine();
    let _ = engine.eval("new Font()", "layer.font");
    match engine.retain_value_detached(&tjs2_sys::TjsValue::Object) {
        Ok(dv) => {
            // SAFETY: out is a valid result slot; the C++ side consumes the
            // retention (copies + erases) before the callback returns.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = std::ptr::null();
                (*out).array_count = 0;
                (*out).retained = dv.raw_id() as usize;
            }
            // The C++ conversion consumes the retention; forget the wrapper
            // so its Drop does not release the id first (safe no-op after).
            std::mem::forget(dv);
            0
        }
        Err(_) => {
            set_void_out(out);
            0
        }
    }
}

/// `layer.hasImage` getter.
extern "C" fn layer_has_image_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(_out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, i64::from(layer.bitmap.is_some()));
    0
}

/// `layer.hasImage = true/false` — accepted; the scene derives it from the
/// attached bitmap (the game forces it true during blends).
extern "C" fn layer_has_image_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    _value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let _ = instance;
    0
}

/// `layer.type` getter — the blend/affine type.
extern "C" fn layer_type_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let blend_type = scene
        .layer(inst.id)
        .map_or(inst.blend_type, |layer| layer.blend_type);
    set_int_out(out, blend_type);
    0
}

/// `layer.type` setter.
extern "C" fn layer_type_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    inst.blend_type = v.integer;
    if inst.constructed
        && let Some(layer) = context_scene_mut().layer_mut(inst.id)
    {
        layer.blend_type = v.integer;
    }
    0
}

#[cfg(test)]
mod tests {
    use crate::natives::tests::{TestEnv, write_fixture};

    #[test]
    fn layer_construct_rect_and_fill() {
        let env = TestEnv::new("layer-ctor");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(100, 50); l.fillRect(0, 0, 100, 50, 0xffff0000);")
            .unwrap();
        let scene = env.scene();
        assert_eq!(scene.layers.len(), 1);
        let layer = &scene.layers[0];
        assert_eq!(
            layer.rect,
            crate::scene::Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 50
            }
        );
        assert_eq!(layer.fill_color, Some([255, 0, 0, 255]));
        // the window's primary layer is auto-set to the first layer
        assert_eq!(scene.windows[0].primary_layer, Some(layer.id));
        assert!(layer.is_primary);
        // the script object exposes its scene id
        assert_eq!(env.eval_int("l.id"), i64::from(layer.id));
    }

    #[test]
    fn layer_properties_update_scene() {
        let env = TestEnv::new("layer-props");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.visible = false; l.opacity = 255; l.hitThreshold = 256; l.absolute = 3;",
        )
        .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert!(!layer.visible);
        assert!((layer.opacity - 1.0).abs() < 1e-6, "255 -> 1.0");
        assert_eq!(layer.hit_threshold, 256);
        assert_eq!(layer.z_order, 3);
        // reads round-trip through the script (opacity on the 0..255 scale)
        assert_eq!(env.eval_int("l.visible"), 0);
        assert_eq!(env.eval_int("l.opacity"), 255);
        assert_eq!(env.eval_int("l.hitThreshold"), 256);
        assert_eq!(env.eval_int("l.absolute"), 3);
    }

    #[test]
    fn layer_opacity_255_scale() {
        let env = TestEnv::new("layer-opacity");
        env.run("var w = new Window(); var l = new Layer(w, null); l.opacity = 128;")
            .unwrap();
        let scene = env.scene();
        assert!((scene.layers[0].opacity - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(env.eval_int("l.opacity"), 128);
    }

    #[test]
    fn layer_set_pos_bounds_form() {
        let env = TestEnv::new("layer-pos");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setPos(10, 20);")
            .unwrap();
        // NB: the read guard must be dropped before the next `env.run` — a
        // script call that mutates the scene takes the write lock
        // (`context_scene_mut`), which blocks forever while this thread
        // still holds the read guard (std RwLock is not reentrant).
        {
            let scene = env.scene();
            assert_eq!((scene.layers[0].rect.x, scene.layers[0].rect.y), (10, 20));
        }
        env.run("l.setPos(1, 2, 30, 40);").unwrap();
        let scene = env.scene();
        assert_eq!(
            scene.layers[0].rect,
            crate::scene::Rect {
                x: 1,
                y: 2,
                w: 30,
                h: 40
            }
        );
    }

    #[test]
    fn layer_draw_text_rasterizes_into_scene_bitmap() {
        let env = TestEnv::new("layer-draw-text");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(128, 32); l.drawText(2, 2, 'Title 日本語', 0xffffffff);")
            .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        let bitmap = scene
            .bitmap(layer.bitmap.expect("drawText creates a surface"))
            .expect("surface bitmap");
        assert_eq!((bitmap.width, bitmap.height), (128, 32));
        assert!(bitmap.dirty);
        assert!(bitmap.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
        assert_eq!(env.eval_int("l.getTextHeight('x')"), 16);
        assert!(env.eval_int("l.getTextWidth('日本')") > 0);
    }

    #[test]
    fn layer_box_blur_changes_attached_pixels() {
        let env = TestEnv::new("layer-box-blur");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(32, 16); l.drawText(1, 1, 'A', 0xffffffff); l.doBoxBlur(1, 1);")
            .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(bitmap.dirty);
        assert!(bitmap.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
    }

    #[test]
    fn layer_attach_bitmap() {
        let env = TestEnv::new("layer-bitmap");
        env.run("var w = new Window(); var l = new Layer(w, null); var b = new Bitmap(8, 8); l.setBitmap(b.id);")
            .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        let bitmap = &scene.bitmaps[0];
        assert_eq!(layer.bitmap, Some(bitmap.id));
        assert_eq!((bitmap.width, bitmap.height), (8, 8));
        assert_eq!(bitmap.rgba.len(), 8 * 8 * 4);
    }

    #[test]
    fn layer_load_images_from_storage() {
        let env = TestEnv::new("layer-load");
        let (w, h) = (4u32, 3u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|_| [255u8, 128, 0, 255]).collect();
        write_fixture(&env, "testimg.webp", &rgba, w, h);
        env.run("var w = new Window(); var l = new Layer(w, null); l.loadImages('testimg'); l.setSizeToImageSize();")
            .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert!(layer.bitmap.is_some(), "loadImages must attach a bitmap");
        let bmp = scene
            .bitmaps
            .iter()
            .find(|b| b.id == layer.bitmap.unwrap())
            .expect("attached bitmap registered");
        assert_eq!((bmp.width, bmp.height), (w, h));
        assert_eq!((layer.rect.w, layer.rect.h), (bmp.width, bmp.height));
        // imageWidth/imageHeight report the bitmap size
        assert_eq!(env.eval_int("l.imageWidth"), i64::from(w));
        assert_eq!(env.eval_int("l.imageHeight"), i64::from(h));
    }

    #[test]
    fn layer_null_window_errors() {
        let env = TestEnv::new("layer-null");
        env.run("var got = 'no error'; try { var l = new Layer(null, null); } catch (e) { got = 'error'; }")
            .unwrap();
        assert_eq!(env.eval_string("got"), "error");
    }

    #[test]
    fn layer_destroy_removes_from_scene() {
        let env = TestEnv::new("layer-destroy");
        // One-shot create+null: the VM releases the layer within the
        // script, so its native destroy removes it from the scene. The
        // window stays (still referenced by `w`).
        env.run("var w = new Window(); var l = new Layer(w, null); l = null;")
            .unwrap();
        let scene = env.scene();
        assert_eq!(scene.layers.len(), 0);
        assert_eq!(scene.windows.len(), 1);
    }

    #[test]
    fn layer_parent_and_window_properties() {
        let env = TestEnv::new("layer-tree");
        env.run(
            "var w = new Window(); var p = new Layer(w, null); var c = new Layer(w, p); \
             var pw = c.window; var pp = c.parent;",
        )
        .unwrap();
        // window returns the owning Window's TJS object (retained), so it
        // evaluates as an object
        assert!(matches!(
            env.eval("pw", "test"),
            Ok(tjs2_sys::TjsValue::Object)
        ));
        // a Layer OBJECT parent cannot be resolved by the FFI (milestone
        // simplification): the layer attaches to the window instead
        assert_eq!(env.eval_int("pp"), -1);
        // re-parent by id works
        env.run("c.setParentId(p.id);").unwrap();
        assert_eq!(env.eval_int("c.parent"), 0);
        let scene = env.scene();
        let c = scene
            .layers
            .iter()
            .find(|l| l.id == env.eval_int("c.id") as u32)
            .expect("child layer");
        assert_eq!(c.parent, Some(env.eval_int("p.id") as u32));
    }

    #[test]
    fn argb_to_rgba_known_colors() {
        // TJS color 0xAARRGGBB → straight-alpha RGBA (the reference
        // FillRect convention; matches the real title scene's fills).
        assert_eq!(super::argb_to_rgba(0xff000000), [0, 0, 0, 255]); // opaque black
        assert_eq!(super::argb_to_rgba(0xffffffff), [255, 255, 255, 255]); // white
        assert_eq!(super::argb_to_rgba(0x80ff0000), [255, 0, 0, 128]); // half-alpha red
        assert_eq!(super::argb_to_rgba(0x00000000), [0, 0, 0, 0]); // transparent
        assert_eq!(super::argb_to_rgba(0xff123456), [0x12, 0x34, 0x56, 0xff]);
    }

    #[test]
    fn layer_opacity_bounds() {
        let env = TestEnv::new("layer-opacity-bounds");
        env.run("var w = new Window(); var l = new Layer(w, null); l.opacity = 0;")
            .unwrap();
        assert_eq!(env.scene().layers[0].opacity, 0.0);
        assert_eq!(env.eval_int("l.opacity"), 0);
        env.run("l.opacity = 300;").unwrap();
        assert_eq!(
            env.scene().layers[0].opacity,
            1.0,
            "values above 255 clamp to 1.0"
        );
        assert_eq!(env.eval_int("l.opacity"), 255);
    }

    /// `layer.type` (blend mode: ltAlpha=2, ltAdditive=3, ...) round-trips
    /// on the native instance and is propagated to the shared scene contract.
    #[test]
    fn layer_type_propagates_to_scene() {
        let env = TestEnv::new("layer-type");
        env.run("var w = new Window(); var l = new Layer(w, null); l.type = 3;")
            .unwrap();
        assert_eq!(
            env.eval_int("l.type"),
            3,
            "script round-trip (ltAdditive=3)"
        );
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert_eq!(layer.blend_type, 3, "ltAdditive reaches the scene");
        assert_eq!(layer.fill_color, None);
        assert_eq!(layer.bitmap, None);
        assert_eq!(layer.z_order, 0);
        assert_eq!(layer.opacity, 1.0);
    }

    /// The real title stack z-values assigned via `layer.absolute`: higher
    /// z sorts in front (window_layer_order is back→front).
    #[test]
    fn layer_absolute_sorts_higher_z_in_front() {
        let env = TestEnv::new("layer-absolute");
        env.run(
            "var w = new Window(); \
             var hint = new Layer(w, null); hint.absolute = 210000; \
             var logo = new Layer(w, null); logo.absolute = 110000; \
             var bg = new Layer(w, null); \
             var cover = new Layer(w, null); cover.absolute = 150000;",
        )
        .unwrap();
        let scene = env.scene();
        let ids = scene.window_layer_order(0);
        assert_eq!(ids.len(), 4);
        let pos = |id: u32| ids.iter().position(|&x| x == id).unwrap();
        // Layer ids are assigned in creation order: hint=0, logo=1, bg=2,
        // cover=3 (each `new Layer` bumps the scene's next_layer counter).
        assert!(
            pos(2) < pos(1) && pos(1) < pos(3) && pos(3) < pos(0),
            "z=0 bg, then 110000, 150000, 210000"
        );
    }

    /// `primaryLayer.cursorX`/`cursorY` mirror the cursor position the
    /// render input bridge forwards via [`super::super::set_shared_cursor_pos`]
    /// (which the bridge sources from the shared `tvp-input` mouse position).
    #[test]
    fn layer_cursor_x_y_mirror_shared_cursor_pos() {
        let env = TestEnv::new("layer-cursor");
        // Simulate the bridge forwarding this frame's cursor position.
        super::super::set_shared_cursor_pos(400, 300);
        env.run("var w = new Window(); var l = new Layer(w, null);")
            .unwrap();
        assert_eq!(env.eval_int("l.cursorX"), 400);
        assert_eq!(env.eval_int("l.cursorY"), 300);
        // The game reads them through the primary layer object.
        assert_eq!(env.eval_int("w.primaryLayer.cursorX"), 400);
        assert_eq!(env.eval_int("w.primaryLayer.cursorY"), 300);
        // A later bridge update is reflected too.
        super::super::set_shared_cursor_pos(5, 7);
        assert_eq!(env.eval_int("l.cursorX"), 5);
        assert_eq!(env.eval_int("l.cursorY"), 7);
    }
}
