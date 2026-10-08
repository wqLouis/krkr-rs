//! The visual `Layer` native class.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use engine::input::InputState;
use engine::{LayerState, Rect, Scene};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, TjsValue, VAL_INTEGER, VAL_OBJECT, VAL_REAL, VAL_VOID, Value,
};

use super::common::{
    DEFAULT_FONT_COLOR, DEFAULT_FONT_FACE, DEFAULT_FONT_HEIGHT, ObjectKind, VisualState,
    argb_to_rgba, bound, duplicate_object, input_ref, reparent_layer, resolve_object_id, scene_mut,
    scene_ref, set_null_out, set_object_out, set_retained_out, set_visual_string_out,
};
use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

mod blend;
mod events;
mod focus;
mod order;
mod pixels;
mod transitions;
mod util;

/// Per-layer L3 state that has no home in the L2 [`engine::Scene`] yet: the
/// active transition, the modal-layer stack and the pending capture-release
/// requests the layer-tree owner drains.
///
/// Keyed by [`std::any::TypeId`] in [`CompatServices::state`](crate::CompatServices::state),
/// never a process global.
#[derive(Default)]
struct LayerExtras {
    /// Active `beginTransition` state per layer id.
    transitions: HashMap<u32, LayerTransition>,
    /// Reference `tTVPLayerManager::ModalLayerVector`: modal layer ids.
    modes: Vec<u32>,
    /// `releaseCapture` requests the tree owner drains.
    release_capture: Vec<u32>,
    /// `releaseTouchCapture` requests (`None` = all contacts).
    release_touch_capture: Vec<(u32, Option<u64>)>,
}

/// One active layer transition (`beginTransition`).
#[allow(dead_code)]
struct LayerTransition {
    trans: engine::trans::Trans,
    source: u32,
    with_children: bool,
}

#[derive(Default)]
struct LayerInst {
    id: Option<u32>,
    font_object: Option<DetachedValue>,
}

/// The window pointer position, preferring an active touch contact.
///
/// L2 updates the pointer only from `PointerMoved`; touch events land in the
/// contact map. To honour the reference's "last pointer/touch position" the
/// lowest active contact id wins when any contact exists, otherwise the
/// pointer is used.
fn pointer_or_touch(input: &InputState) -> (f32, f32) {
    if input.touch_point_count() > 0 {
        let mut ids: Vec<u64> = input.touch_contacts().map(|c| c.point.id).collect();
        ids.sort_unstable();
        if let Some(id) = ids.first().copied()
            && let Some(point) = input.touch_point(id)
        {
            return point.pos;
        }
    }
    input.pointer()
}

/// See `register_window`.
fn register_layer(ctx: &CompatCtx, host: &Tjs2Engine, id: u32, objthis: *mut c_void) {
    if objthis.is_null() {
        return;
    }
    match host.retain_object_detached(objthis) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        Ok(object) => {
            unsafe { ctx.services() }
                .state::<VisualState>()
                .objects
                .layers
                .insert(id, object);
        }
        Err(error) => log::warn!("Layer: could not retain the script object: {error}"),
    }
}

/// Allocate (or resize) a layer's MainImage to at least `width` x `height`,
/// preserving content and filling expansion with the layer's `neutral_color`
/// (reference `AllocateImage` / `ChangeImageSize`).
fn ensure_layer_image(scene: &mut Scene, layer_id: u32, width: u32, height: u32) {
    let width = width.max(1);
    let height = height.max(1);
    let (bitmap, fill) = {
        let Some(layer) = scene.layer(layer_id) else {
            return;
        };
        (layer.bitmap, argb_to_rgba(layer.neutral_color))
    };
    match bitmap {
        Some(bitmap_id) => {
            scene.resize_bitmap(bitmap_id, width, height, fill);
        }
        None => {
            let Some(pixels) = (width as usize)
                .checked_mul(height as usize)
                .and_then(|count| count.checked_mul(4))
            else {
                return;
            };
            let mut rgba = vec![0u8; pixels];
            for chunk in rgba.chunks_exact_mut(4) {
                chunk.copy_from_slice(&fill);
            }
            let bitmap_id = scene.add_bitmap(width, height, rgba);
            if let Some(layer) = scene.layer_mut(layer_id) {
                layer.bitmap = Some(bitmap_id);
                layer.image_left = 0;
                layer.image_top = 0;
            }
        }
    }
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.image_width = width;
        layer.image_height = height;
        layer.clip = None;
        layer.image_modified = true;
    }
    if scene.layer(layer_id).is_some_and(|l| l.province.is_some()) {
        scene.resize_province_image(layer_id, width, height);
    }
}

/// Mark a layer's image modified and refresh the tracked image window.
fn mark_layer_image_changed(scene: &mut Scene, layer_id: u32) {
    let dims = scene
        .layer(layer_id)
        .and_then(|l| l.bitmap)
        .and_then(|id| scene.bitmap(id))
        .map(|b| (b.width, b.height));
    if let Some((width, height)) = dims
        && let Some(layer) = scene.layer_mut(layer_id)
    {
        layer.image_width = width;
        layer.image_height = height;
        layer.image_modified = true;
    }
}

/// Build a TJS array of retained Layer objects (used by `Layer.children`).
fn layer_array(
    host: &Tjs2Engine,
    ctx: &CompatCtx,
    ids: &[u32],
) -> Result<Option<DetachedValue>, String> {
    let array = match host
        .eval_retained("[]", "visual.array")
        .map_err(|e| e.to_string())?
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(_) => return Ok(None),
    };
    for (index, id) in ids.iter().enumerate() {
        let Some(object) = duplicate_object(ctx, host, ObjectKind::Layer, *id) else {
            continue;
        };
        host.set_member(
            array.raw_id(),
            &index.to_string(),
            &TjsValue::Retained(object.raw_id() as u64),
        )?;
        std::mem::forget(object);
    }
    Ok(Some(array))
}

// ---------------------------------------------------------------------------
// Layer
// ---------------------------------------------------------------------------

extern "C" fn layer_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(LayerInst::default())) as *mut c_void
}

extern "C" fn layer_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .objects
            .layers
            .remove(&id);
    }
    // Dropping the payload releases the cached `font` object.
    // SAFETY: instance came from `layer_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut LayerInst) });
}

/// `Layer(window, parent)` — add a layer to the scene and return its id.
///
/// `window` may be a `Window` object (or an integer id); `parent` a `Layer`
/// object/id or null. A missing window attaches to the scene's first window
/// (the game has one window).
extern "C" fn layer_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Layer: this layer is already constructed");
    }
    // Window: explicit object/id, else the scene's first window.
    let window = match a.first() {
        Some(value)
            if value.ty == VAL_OBJECT || value.ty == VAL_INTEGER || value.ty == VAL_REAL =>
        {
            resolve_object_id(resolved.engine(), value)
                .ok()
                .filter(|&id| id >= 0)
        }
        _ => None,
    };
    // Parent: a `Layer` object/id, else the window root. Resolved before the
    // scene borrow so the getter cannot re-enter the VM while `&mut Scene` is
    // held.
    let parent = match a.get(1) {
        Some(value) if value.ty == VAL_OBJECT => resolve_object_id(resolved.engine(), value)
            .ok()
            .filter(|&id| id >= 0)
            .map(|id| id as u32),
        Some(value) if value.ty == VAL_INTEGER && value.integer >= 0 => Some(value.integer as u32),
        _ => None,
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let window = match window {
        Some(id) if scene.window(id as u32).is_some() => id as u32,
        _ => match scene.windows.first() {
            Some(window) => window.id,
            None => {
                return report_error(
                    out_error,
                    "Layer: no window in the scene — create a Window first",
                );
            }
        },
    };
    let id = scene.add_layer(window, parent);
    inst.id = Some(id);
    register_layer(&ctx, resolved.engine(), id, objthis);
    set_int(out, i64::from(id));
    0
}

/// `Layer.setPos(left, top [, width, height])`.
extern "C" fn layer_set_pos(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setPos requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let size = match (a.get(2), a.get(3)) {
        (Some(w), Some(h)) => Some((value_as_i64(w).max(0) as u32, value_as_i64(h).max(0) as u32)),
        _ => None,
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setPos: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setPos: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.rect.x = x;
        layer.rect.y = y;
        if let Some((w, h)) = size {
            layer.rect.w = w;
            layer.rect.h = h;
        }
    }
    if size.is_some() {
        mark_layer_image_changed(scene, id);
    }
    set_void(out);
    0
}

/// `Layer.setSize(width, height)`.
extern "C" fn layer_set_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setSize requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setSize: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setSize: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.rect.w = width;
        layer.rect.h = height;
    }
    mark_layer_image_changed(scene, id);
    set_void(out);
    0
}

/// `Layer.setSizeToImageSize()` — size the layer to its MainImage.
extern "C" fn layer_set_size_to_image_size(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.setSizeToImageSize: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setSizeToImageSize: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some((width, height)) = scene
        .layer(id)
        .and_then(|l| l.bitmap)
        .and_then(|bitmap| scene.bitmap(bitmap))
        .map(|b| (b.width, b.height))
        && let Some(layer) = scene.layer_mut(id)
    {
        layer.rect.w = width;
        layer.rect.h = height;
    }
    set_void(out);
    0
}

/// `Layer.setImagePos(left, top)`.
extern "C" fn layer_set_image_pos(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setImagePos requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let left = value_as_i64(&a[0]) as i32;
    let top = value_as_i64(&a[1]) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setImagePos: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setImagePos: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.image_left = left;
        layer.image_top = top;
    }
    set_void(out);
    0
}

/// `Layer.setImageSize(width, height)`.
extern "C" fn layer_set_image_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setImageSize requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setImageSize: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setImageSize: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    ensure_layer_image(scene, id, width, height);
    set_void(out);
    0
}

/// Shared body of `Layer.assignImages`.
fn layer_attach_bitmap(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    method: &str,
) -> c_int {
    if argc < 1 {
        return report_error(
            out_error,
            &format!("Layer.{method} requires a Bitmap argument"),
        );
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            &format!("Layer.{method}: compat context is not bound"),
        );
    };
    let bitmap_id = match resolve_object_id(resolved.engine(), &unsafe { args(argv, argc) }[0]) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, &format!("Layer.{method}: not a Bitmap object")),
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            &format!("Layer.{method}: this layer is not constructed"),
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((width, height)) = scene.bitmap(bitmap_id).map(|b| (b.width, b.height)) else {
        return report_error(
            out_error,
            &format!("Layer.{method}: the bitmap does not exist"),
        );
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.bitmap = Some(bitmap_id);
        layer.image_left = 0;
        layer.image_top = 0;
        layer.image_width = width;
        layer.image_height = height;
        layer.image_modified = true;
    }
    set_void(out);
    0
}

extern "C" fn layer_assign_images(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_attach_bitmap(engine, instance, argc, argv, out, out_error, "assignImages")
}

/// `Layer.copyFromBitmapToMainImage(bitmap)` — copy the source pixels into
/// the layer's MainImage (allocating/resizing it to match).
extern "C" fn layer_copy_from_bitmap_to_main_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage requires a Bitmap argument",
        );
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: compat context is not bound",
        );
    };
    let bitmap_id = match resolve_object_id(resolved.engine(), &unsafe { args(argv, argc) }[0]) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, "Layer.copyFromBitmapToMainImage: not a Bitmap"),
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((width, height, rgba)) = scene
        .bitmap(bitmap_id)
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: the bitmap does not exist",
        );
    };
    ensure_layer_image(scene, id, width, height);
    if let Some(layer_bitmap) = scene.layer(id).and_then(|l| l.bitmap)
        && let Some(bitmap) = scene.bitmap_mut(layer_bitmap)
    {
        bitmap.rgba = rgba;
        bitmap.dirty = true;
    }
    set_void(out);
    0
}

/// `Layer.independMainImage()` — give the layer a private copy of its
/// MainImage.
extern "C" fn layer_independ_main_image(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.independMainImage: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.independMainImage: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((width, height, rgba)) = scene
        .layer(id)
        .and_then(|l| l.bitmap)
        .and_then(|bitmap| scene.bitmap(bitmap))
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        set_void(out);
        return 0;
    };
    let new_bitmap = scene.add_bitmap(width, height, rgba);
    if let Some(layer) = scene.layer_mut(id) {
        layer.bitmap = Some(new_bitmap);
        layer.image_width = width;
        layer.image_height = height;
        layer.image_modified = true;
    }
    set_void(out);
    0
}

/// `Layer.bringToFront()`.
extern "C" fn layer_bring_to_front(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.bringToFront: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.bringToFront: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.layer_move_to_front(id);
    set_void(out);
    0
}

/// Re-parent a layer from a script argument (`Layer.parent` setter; the
/// reference exposes no `setParentId` method).
extern "C" fn layer_set_parent_id(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.parent: compat context is not bound");
    };
    let parent = if argc >= 1 {
        let value = &unsafe { args(argv, argc) }[0];
        if value.ty == VAL_VOID {
            None
        } else {
            resolve_object_id(resolved.engine(), value)
                .ok()
                .filter(|&id| id >= 0)
                .map(|id| id as u32)
        }
    } else {
        None
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.parent: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    reparent_layer(scene, id, parent);
    set_void(out);
    0
}

/// `Layer.focus()` — give the layer keyboard focus.
extern "C" fn layer_focus(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.focus: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.focus: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.set_focus(id);
    set_void(out);
    0
}

/// `Layer.update()` — request the layer's `onPaint` and a repaint.
extern "C" fn layer_update(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.update: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.update: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.pending_paint = true;
    }
    set_void(out);
    0
}

/// `Layer.setClip(left, top, width, height)`.
extern "C" fn layer_set_clip(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 4 {
        return report_error(out_error, "Layer.setClip requires 4 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let w = value_as_i64(&a[2]).max(0) as u32;
    let h = value_as_i64(&a[3]).max(0) as u32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setClip: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setClip: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.clip = Some(Rect::new(x, y, w, h));
    }
    set_void(out);
    0
}

/// `Layer.getProvincePixel(x, y)`.
extern "C" fn layer_get_province_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.getProvincePixel requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.getProvincePixel: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.getProvincePixel: this layer is not constructed",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, i64::from(scene.get_province_pixel(id, x, y)));
    0
}

/// `Layer.setProvincePixel(x, y, n)`.
extern "C" fn layer_set_province_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Layer.setProvincePixel requires 3 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let n = value_as_i64(&a[2]) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.setProvincePixel: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setProvincePixel: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.set_province_pixel(id, x, y, n);
    set_void(out);
    0
}

/// `Layer.getLayerAt(x, y [, excludeSelf [, getDisabled]])` — the frontmost
/// layer at the point, as a Layer object (or `null`).
extern "C" fn layer_get_layer_at(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.getLayerAt requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let exclude_self = a.get(2).is_some_and(value_as_bool);
    let get_disabled = a.get(3).is_some_and(value_as_bool);
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.getLayerAt: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.getLayerAt: this layer is not constructed");
    };
    let hit = {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        scene.layer_at(id, x, y, exclude_self, get_disabled)
    };
    let object =
        hit.and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `Layer.fillRect(left, top, width, height, color)`.
extern "C" fn layer_fill_rect(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 5 {
        return report_error(out_error, "Layer.fillRect requires 5 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let w = value_as_i64(&a[2]).max(0) as u32;
    let h = value_as_i64(&a[3]).max(0) as u32;
    let color = argb_to_rgba(value_as_i64(&a[4]));
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.fillRect: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.fillRect: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let bitmap_id = scene.layer(id).and_then(|layer| layer.bitmap);
    match bitmap_id {
        Some(bitmap_id) => {
            if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
                for py in y.max(0)..y.saturating_add(h as i32) {
                    for px in x.max(0)..x.saturating_add(w as i32) {
                        if let Some(offset) = bitmap.pixel_offset(px as u32, py as u32) {
                            bitmap.rgba[offset..offset + 4].copy_from_slice(&color);
                        }
                    }
                }
                bitmap.mark_dirty();
            }
        }
        None => {
            if let Some(layer) = scene.layer_mut(id) {
                layer.fill_color = Some(color);
                layer.rect.w = layer.rect.w.max(x.max(0) as u32 + w);
                layer.rect.h = layer.rect.h.max(y.max(0) as u32 + h);
            }
        }
    }
    set_void(out);
    0
}

// --- Layer property helpers ------------------------------------------------

/// Read a layer through the bound scene.
fn with_layer_read<R>(
    engine: *mut c_void,
    instance: *mut c_void,
    f: impl FnOnce(&LayerState) -> R,
) -> Result<R, String> {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let id = inst
        .id
        .ok_or_else(|| "Layer: this layer is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_ref(&ctx)?;
    scene
        .layer(id)
        .map(f)
        .ok_or_else(|| "Layer: layer no longer exists".to_string())
}

/// Mutate a layer through the bound scene.
fn with_layer_write(
    engine: *mut c_void,
    instance: *mut c_void,
    value: &Value,
    f: impl FnOnce(&mut LayerState, &Value),
) -> Result<(), String> {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let id = inst
        .id
        .ok_or_else(|| "Layer: this layer is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_mut(&ctx)?;
    let layer = scene
        .layer_mut(id)
        .ok_or_else(|| "Layer: layer no longer exists".to_string())?;
    f(layer, value);
    Ok(())
}

/// Define a read/write layer property over an `i64`/bool field.
macro_rules! layer_int_prop {
    ($get:ident, $set:ident, $getter:expr, $setter:expr) => {
        #[allow(dead_code)]
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            match with_layer_read(engine, instance, $getter) {
                Ok(value) => {
                    set_int(out, value);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
        #[allow(dead_code)]
        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: the trampoline passes a valid value slot.
            let value = unsafe { &*value };
            match with_layer_write(engine, instance, value, $setter) {
                Ok(()) => 0,
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

layer_int_prop!(
    layer_visible_get,
    layer_visible_set,
    |l: &LayerState| i64::from(l.visible),
    |l: &mut LayerState, v: &Value| l.visible = value_as_bool(v)
);
layer_int_prop!(
    layer_type_get,
    layer_type_set,
    |l: &LayerState| blend::to_native(l.blend),
    |l: &mut LayerState, v: &Value| l.blend = blend::from_native(value_as_i64(v))
);
layer_int_prop!(
    layer_left_get,
    layer_left_set,
    |l: &LayerState| i64::from(l.rect.x),
    |l: &mut LayerState, v: &Value| l.rect.x = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_top_get,
    layer_top_set,
    |l: &LayerState| i64::from(l.rect.y),
    |l: &mut LayerState, v: &Value| l.rect.y = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_width_get,
    layer_width_set,
    |l: &LayerState| i64::from(l.rect.w),
    |l: &mut LayerState, v: &Value| l.rect.w = value_as_i64(v).max(0) as u32
);
layer_int_prop!(
    layer_height_get,
    layer_height_set,
    |l: &LayerState| i64::from(l.rect.h),
    |l: &mut LayerState, v: &Value| l.rect.h = value_as_i64(v).max(0) as u32
);
layer_int_prop!(
    layer_id_get,
    layer_id_set,
    |l: &LayerState| i64::from(l.id),
    |_l: &mut LayerState, _v: &Value| {}
);
layer_int_prop!(
    layer_absolute_get,
    layer_absolute_set,
    |l: &LayerState| i64::from(l.z_order),
    |l: &mut LayerState, v: &Value| l.z_order = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_hit_threshold_get,
    layer_hit_threshold_set,
    |l: &LayerState| i64::from(l.hit_threshold),
    |l: &mut LayerState, v: &Value| l.hit_threshold = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_hit_type_get,
    layer_hit_type_set,
    |l: &LayerState| i64::from(l.hit_type),
    |l: &mut LayerState, v: &Value| l.hit_type = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_cursor_get,
    layer_cursor_set,
    |l: &LayerState| i64::from(l.cursor),
    |l: &mut LayerState, v: &Value| l.cursor = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_face_get,
    layer_face_set,
    |l: &LayerState| i64::from(l.face),
    |l: &mut LayerState, v: &Value| l.face = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_hold_alpha_get,
    layer_hold_alpha_set,
    |l: &LayerState| i64::from(l.hold_alpha),
    |l: &mut LayerState, v: &Value| l.hold_alpha = value_as_bool(v)
);
layer_int_prop!(
    layer_image_left_get,
    layer_image_left_set,
    |l: &LayerState| i64::from(l.image_left),
    |l: &mut LayerState, v: &Value| l.image_left = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_image_top_get,
    layer_image_top_set,
    |l: &LayerState| i64::from(l.image_top),
    |l: &mut LayerState, v: &Value| l.image_top = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_image_width_get,
    layer_image_width_set,
    |l: &LayerState| i64::from(l.image_width),
    |_l: &mut LayerState, _v: &Value| {}
);
layer_int_prop!(
    layer_image_height_get,
    layer_image_height_set,
    |l: &LayerState| i64::from(l.image_height),
    |_l: &mut LayerState, _v: &Value| {}
);
layer_int_prop!(
    layer_focusable_get,
    layer_focusable_set,
    |l: &LayerState| i64::from(l.focusable),
    |l: &mut LayerState, v: &Value| l.focusable = value_as_bool(v)
);
layer_int_prop!(
    layer_join_focus_chain_get,
    layer_join_focus_chain_set,
    |l: &LayerState| i64::from(l.join_focus_chain),
    |l: &mut LayerState, v: &Value| l.join_focus_chain = value_as_bool(v)
);
layer_int_prop!(
    layer_enabled_get,
    layer_enabled_set,
    |l: &LayerState| i64::from(l.enabled),
    |l: &mut LayerState, v: &Value| l.enabled = value_as_bool(v)
);
layer_int_prop!(
    layer_cached_get,
    layer_cached_set,
    |l: &LayerState| i64::from(l.cached),
    |l: &mut LayerState, v: &Value| l.cached = value_as_bool(v)
);
layer_int_prop!(
    layer_image_modified_get,
    layer_image_modified_set,
    |l: &LayerState| i64::from(l.image_modified),
    |l: &mut LayerState, v: &Value| l.image_modified = value_as_bool(v)
);
layer_int_prop!(
    layer_attention_left_get,
    layer_attention_left_set,
    |l: &LayerState| i64::from(l.attention_left),
    |l: &mut LayerState, v: &Value| l.attention_left = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_attention_top_get,
    layer_attention_top_set,
    |l: &LayerState| i64::from(l.attention_top),
    |l: &mut LayerState, v: &Value| l.attention_top = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_use_attention_get,
    layer_use_attention_set,
    |l: &LayerState| i64::from(l.use_attention),
    |l: &mut LayerState, v: &Value| l.use_attention = value_as_bool(v)
);
layer_int_prop!(
    layer_show_parent_hint_get,
    layer_show_parent_hint_set,
    |l: &LayerState| i64::from(l.show_parent_hint),
    |l: &mut LayerState, v: &Value| l.show_parent_hint = value_as_bool(v)
);
layer_int_prop!(
    layer_ignore_hint_sensing_get,
    layer_ignore_hint_sensing_set,
    |l: &LayerState| i64::from(l.ignore_hint_sensing),
    |l: &mut LayerState, v: &Value| l.ignore_hint_sensing = value_as_bool(v)
);
layer_int_prop!(
    layer_ime_mode_get,
    layer_ime_mode_set,
    |l: &LayerState| i64::from(l.ime_mode),
    |l: &mut LayerState, v: &Value| l.ime_mode = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_neutral_color_get,
    layer_neutral_color_set,
    |l: &LayerState| l.neutral_color,
    |l: &mut LayerState, v: &Value| l.neutral_color = value_as_i64(v)
);
layer_int_prop!(
    layer_absolute_order_mode_get,
    layer_absolute_order_mode_set,
    |l: &LayerState| i64::from(l.absolute_order_mode),
    |l: &mut LayerState, v: &Value| l.absolute_order_mode = value_as_bool(v)
);
layer_int_prop!(
    layer_call_on_paint_get,
    layer_call_on_paint_set,
    |l: &LayerState| i64::from(l.call_on_paint),
    |l: &mut LayerState, v: &Value| l.call_on_paint = value_as_bool(v)
);
layer_int_prop!(
    layer_clip_left_get,
    layer_clip_left_set,
    |l: &LayerState| i64::from(l.clip.map_or(0, |c| c.x)),
    |l: &mut LayerState, v: &Value| {
        let rect = l.clip.get_or_insert(Rect::default());
        rect.x = value_as_i64(v) as i32;
    }
);
layer_int_prop!(
    layer_clip_top_get,
    layer_clip_top_set,
    |l: &LayerState| i64::from(l.clip.map_or(0, |c| c.y)),
    |l: &mut LayerState, v: &Value| {
        let rect = l.clip.get_or_insert(Rect::default());
        rect.y = value_as_i64(v) as i32;
    }
);
layer_int_prop!(
    layer_clip_width_get,
    layer_clip_width_set,
    |l: &LayerState| i64::from(l.clip.map_or(0, |c| c.w)),
    |l: &mut LayerState, v: &Value| {
        let rect = l.clip.get_or_insert(Rect::default());
        rect.w = value_as_i64(v).max(0) as u32;
    }
);
layer_int_prop!(
    layer_clip_height_get,
    layer_clip_height_set,
    |l: &LayerState| i64::from(l.clip.map_or(0, |c| c.h)),
    |l: &mut LayerState, v: &Value| {
        let rect = l.clip.get_or_insert(Rect::default());
        rect.h = value_as_i64(v).max(0) as u32;
    }
);

/// The window pointer position in `layer_id`'s coordinate space: subtract
/// each layer's rect offset while walking up the parent chain, matching the
/// reference `GetCursorX`/`GetCursorY`.
fn layer_cursor_axis(
    engine: *mut c_void,
    instance: *mut c_void,
    axis: &str,
    x_axis: bool,
) -> Result<i32, String> {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let id = inst
        .id
        .ok_or_else(|| format!("Layer.{axis}: this layer is not constructed"))?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_ref(&ctx)?;
    let input = input_ref(&ctx).map_err(|error| format!("Layer.{axis}: {error}"))?;
    let (px, py) = pointer_or_touch(input);
    let mut value = if x_axis { px } else { py };
    let mut current = scene.layer(id);
    while let Some(layer) = current {
        if layer.parent.is_none() {
            break;
        }
        value -= if x_axis {
            layer.rect.x as f32
        } else {
            layer.rect.y as f32
        };
        current = layer.parent.and_then(|parent| scene.layer(parent));
    }
    Ok(value.round() as i32)
}

/// `Layer.cursorX` — the window pointer X in the layer's coordinate space.
extern "C" fn layer_cursor_x_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match layer_cursor_axis(engine, instance, "cursorX", true) {
        Ok(value) => {
            set_int(out, i64::from(value));
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `Layer.cursorY` — the window pointer Y in the layer's coordinate space.
extern "C" fn layer_cursor_y_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match layer_cursor_axis(engine, instance, "cursorY", false) {
        Ok(value) => {
            set_int(out, i64::from(value));
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `layer.opacity` (0..255 scale, reference `GetOpacity`/`SetOpacity`).
extern "C" fn layer_opacity_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match with_layer_read(engine, instance, |l| {
        (f64::from(l.opacity) * 255.0).round() as i64
    }) {
        Ok(value) => {
            set_int(out, value);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn layer_opacity_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let value = unsafe { &*value };
    match with_layer_write(engine, instance, value, |l, v| {
        l.opacity = (value_as_i64(v) as f64 / 255.0).clamp(0.0, 1.0) as f32;
    }) {
        Ok(()) => 0,
        Err(error) => report_error(out_error, &error),
    }
}

/// `layer.hasImage` — whether the layer has a MainImage.
extern "C" fn layer_has_image_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match with_layer_read(engine, instance, |l| l.bitmap.is_some()) {
        Ok(value) => {
            set_bool(out, value);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn layer_has_image_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let wanted = value_as_bool(unsafe { &*value });
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.hasImage: this layer is not constructed");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.hasImage: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if wanted {
        let (w, h) = scene.layer(id).map_or((1, 1), |l| (l.rect.w, l.rect.h));
        ensure_layer_image(scene, id, w, h);
    } else {
        let removed = scene.layer_mut(id).and_then(|layer| layer.bitmap.take());
        if let Some(bitmap) = removed {
            scene.release_bitmap(bitmap);
        }
        scene.deallocate_province_image(id);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
    }
    0
}

/// `layer.mainImageBuffer` — the MainImage pixel address (0 when absent).
extern "C" fn layer_main_image_buffer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.mainImageBuffer: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.main_image_pixel_buffer(id) as i64);
    0
}

/// `layer.mainImageBufferForWrite` — the MainImage pixel address, marking the
/// image modified.
extern "C" fn layer_main_image_buffer_for_write_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.mainImageBufferForWrite: compat context is not bound",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.main_image_pixel_buffer_for_write(id) as i64);
    0
}

/// `layer.mainImageBufferPitch` — the MainImage row pitch in bytes.
extern "C" fn layer_main_image_buffer_pitch_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.mainImageBufferPitch: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, i64::from(scene.main_image_pitch(id)));
    0
}

/// `layer.provinceImageBuffer` — the province plane address (0 when absent).
extern "C" fn layer_province_image_buffer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.provinceImageBuffer: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.province_image_pixel_buffer(id) as i64);
    0
}

/// `layer.provinceImageBufferForWrite` — allocate the province plane and
/// return its address.
extern "C" fn layer_province_image_buffer_for_write_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.provinceImageBufferForWrite: compat context is not bound",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.province_image_pixel_buffer_for_write(id) as i64);
    0
}

/// `layer.provinceImageBufferPitch` — the province plane row pitch.
extern "C" fn layer_province_image_buffer_pitch_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.provinceImageBufferPitch: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, i64::from(scene.province_image_pitch(id)));
    0
}

/// `layer.nodeVisible` / `nodeEnabled` / `nodeFocusable`.
extern "C" fn layer_node_visible_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_node_flag(engine, instance, out, out_error, Scene::node_visible)
}

extern "C" fn layer_node_enabled_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_node_flag(engine, instance, out, out_error, Scene::node_enabled)
}

extern "C" fn layer_node_focusable_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_node_flag(engine, instance, out, out_error, Scene::node_focusable)
}

fn layer_node_flag(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl Fn(&Scene, u32) -> bool,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_bool(out, f(scene, id));
    0
}

/// `layer.focused` — whether the layer holds its window's focus.
extern "C" fn layer_focused_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.focused: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let focused = scene
        .layer(id)
        .and_then(|layer| scene.window(layer.window))
        .and_then(|window| window.focused_layer)
        == Some(id);
    set_bool(out, focused);
    0
}

/// `layer.isPrimary`.
extern "C" fn layer_is_primary_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.isPrimary: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_bool(out, scene.is_primary_layer(id));
    0
}

/// `layer.name`.
extern "C" fn layer_name_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.name: compat context is not bound");
    };
    let name = match with_layer_read(engine, instance, |l| l.name.clone()) {
        Ok(name) => name,
        Err(error) => return report_error(out_error, &error),
    };
    set_visual_string_out(&ctx, out, &name);
    0
}

extern "C" fn layer_name_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let text = value_as_string(unsafe { &*value });
    match with_layer_write(engine, instance, unsafe { &*value }, |l, _| l.name = text) {
        Ok(()) => 0,
        Err(error) => report_error(out_error, &error),
    }
}

/// `layer.hint`.
extern "C" fn layer_hint_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.hint: compat context is not bound");
    };
    let hint = match with_layer_read(engine, instance, |l| l.hint.clone()) {
        Ok(hint) => hint,
        Err(error) => return report_error(out_error, &error),
    };
    set_visual_string_out(&ctx, out, &hint);
    0
}

extern "C" fn layer_hint_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let text = value_as_string(unsafe { &*value });
    match with_layer_write(engine, instance, unsafe { &*value }, |l, _| l.hint = text) {
        Ok(()) => 0,
        Err(error) => report_error(out_error, &error),
    }
}

/// `layer.window` — the owning window object.
extern "C" fn layer_window_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.window: compat context is not bound");
    };
    let window = match with_layer_read(engine, instance, |l| l.window) {
        Ok(window) => window,
        Err(error) => return report_error(out_error, &error),
    };
    let object = duplicate_object(&ctx, resolved.engine(), ObjectKind::Window, window);
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `layer.parent` — the parent layer object, or `null`.
extern "C" fn layer_parent_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.parent: compat context is not bound");
    };
    let parent = match with_layer_read(engine, instance, |l| l.parent) {
        Ok(parent) => parent,
        Err(error) => return report_error(out_error, &error),
    };
    let object = parent
        .and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `layer.parent = layer` — re-parent (null/void detaches to the window
/// root).
extern "C" fn layer_parent_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_set_parent_id(
        engine,
        instance,
        1,
        value,
        std::ptr::null_mut(),
        out_error,
        std::ptr::null_mut(),
    )
}

/// `layer.children` — an array of the direct child Layer objects.
extern "C" fn layer_children_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.children: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.children: this layer is not constructed");
    };
    let children = {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        scene.ordered_children(id)
    };
    match layer_array(resolved.engine(), &ctx, &children) {
        Ok(Some(array)) => {
            set_retained_out(resolved.engine(), out, array.raw_id());
            std::mem::forget(array);
        }
        Ok(None) => set_null_out(resolved.engine(), out),
        Err(error) => return report_error(out_error, &error),
    }
    0
}

/// `layer.nextFocusable` / `prevFocusable`.
extern "C" fn layer_next_focusable_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_focusable_neighbor(engine, instance, out, out_error, Scene::next_focusable)
}

extern "C" fn layer_prev_focusable_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_focusable_neighbor(engine, instance, out, out_error, Scene::prev_focusable)
}

fn layer_focusable_neighbor(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl Fn(&Scene, u32) -> Option<u32>,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer: this layer is not constructed");
    };
    let neighbor = {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        f(scene, id)
    };
    let object = neighbor
        .and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `layer.font` — the layer's `Font` object (created lazily and cached).
extern "C" fn layer_font_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.font: compat context is not bound");
    };
    let host = resolved.engine();
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.font: this layer is not constructed");
    };
    boxed_font_object(host, &ctx, inst, id, out, out_error)
}

/// Look up (or build) the layer's font wrapper.
fn boxed_font_object(
    host: &Tjs2Engine,
    ctx: &CompatCtx,
    inst: &mut LayerInst,
    layer_id: u32,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if let Some(cached) = inst.font_object.as_ref() {
        set_object_out(host, out, Some(cached));
        return 0;
    }
    let font_id = {
        let scene = match scene_mut(ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        let Some(layer) = scene.layer(layer_id) else {
            return report_error(out_error, "Layer: layer no longer exists");
        };
        match layer.font_id.filter(|id| scene.font(*id).is_some()) {
            Some(id) => id,
            None => {
                let id = scene.add_font(
                    DEFAULT_FONT_FACE.to_string(),
                    DEFAULT_FONT_HEIGHT,
                    argb_to_rgba(DEFAULT_FONT_COLOR),
                );
                if let Some(layer) = scene.layer_mut(layer_id) {
                    layer.font_id = Some(id);
                }
                id
            }
        }
    };
    let wrapper = match host.eval_retained("new Font()", "layer.font") {
        Ok(RetainedValue::Object(object)) => object,
        Ok(RetainedValue::Value(_)) => {
            set_void(out);
            return 0;
        }
        Err(error) => return report_error(out_error, &format!("Layer.font: {error}")),
    };
    if let Err(error) = host.call_member(
        wrapper.raw_id(),
        "__bind",
        &[TjsValue::Integer(i64::from(font_id))],
    ) {
        return report_error(out_error, &format!("Layer.font: {error}"));
    }
    inst.font_object = Some(wrapper);
    set_object_out(host, out, inst.font_object.as_ref());
    0
}

/// `layer.font = font` — copy the source Font's state into the layer's font.
extern "C" fn layer_font_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.font: compat context is not bound");
    };
    let source_id = match resolve_object_id(resolved.engine(), unsafe { &*value }) {
        Ok(id) if id >= 0 => id as u32,
        _ => return 0,
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(layer_id) = inst.id else {
        return report_error(out_error, "Layer.font: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some(source) = scene.font(source_id).cloned() else {
        return 0;
    };
    let destination = match scene
        .layer(layer_id)
        .and_then(|layer| layer.font_id)
        .filter(|id| scene.font(*id).is_some())
    {
        Some(id) => id,
        None => {
            let id = scene.add_font(source.face.clone(), source.height, source.color);
            if let Some(layer) = scene.layer_mut(layer_id) {
                layer.font_id = Some(id);
            }
            id
        }
    };
    if let Some(font) = scene.fonts.iter_mut().find(|font| font.id == destination) {
        *font = source;
        font.id = destination;
    }
    0
}

pub(super) fn register_layer_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let mut methods = vec![
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
            name: "setSizeToImageSize",
            f: layer_set_size_to_image_size,
        },
        NativeInstanceMethodDef {
            name: "setImagePos",
            f: layer_set_image_pos,
        },
        NativeInstanceMethodDef {
            name: "setImageSize",
            f: layer_set_image_size,
        },
        NativeInstanceMethodDef {
            name: "assignImages",
            f: layer_assign_images,
        },
        NativeInstanceMethodDef {
            name: "copyFromBitmapToMainImage",
            f: layer_copy_from_bitmap_to_main_image,
        },
        NativeInstanceMethodDef {
            name: "independMainImage",
            f: layer_independ_main_image,
        },
        NativeInstanceMethodDef {
            name: "bringToFront",
            f: layer_bring_to_front,
        },
        NativeInstanceMethodDef {
            name: "focus",
            f: layer_focus,
        },
        NativeInstanceMethodDef {
            name: "update",
            f: layer_update,
        },
        NativeInstanceMethodDef {
            name: "setClip",
            f: layer_set_clip,
        },
        NativeInstanceMethodDef {
            name: "getProvincePixel",
            f: layer_get_province_pixel,
        },
        NativeInstanceMethodDef {
            name: "setProvincePixel",
            f: layer_set_province_pixel,
        },
        NativeInstanceMethodDef {
            name: "getLayerAt",
            f: layer_get_layer_at,
        },
        NativeInstanceMethodDef {
            name: "fillRect",
            f: layer_fill_rect,
        },
    ];
    methods.extend(pixels::method_defs());
    methods.extend(events::method_defs());
    methods.extend(focus::method_defs());
    methods.extend(order::method_defs());
    methods.extend(transitions::method_defs());
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Layer",
        create: layer_create,
        destroy: layer_destroy,
        invalidate: None,
        methods,
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(layer_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "nativeId",
                get: Some(layer_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "font",
                get: Some(layer_font_get),
                set: Some(layer_font_set),
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
            NativeInstancePropertyDef {
                name: "type",
                get: Some(layer_type_get),
                set: Some(layer_type_set),
            },
            NativeInstancePropertyDef {
                name: "hasImage",
                get: Some(layer_has_image_get),
                set: Some(layer_has_image_set),
            },
            NativeInstancePropertyDef {
                name: "mainImageBuffer",
                get: Some(layer_main_image_buffer_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "mainImageBufferForWrite",
                get: Some(layer_main_image_buffer_for_write_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "mainImageBufferPitch",
                get: Some(layer_main_image_buffer_pitch_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "provinceImageBuffer",
                get: Some(layer_province_image_buffer_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "provinceImageBufferForWrite",
                get: Some(layer_province_image_buffer_for_write_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "provinceImageBufferPitch",
                get: Some(layer_province_image_buffer_pitch_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "absolute",
                get: Some(layer_absolute_get),
                set: Some(layer_absolute_set),
            },
            NativeInstancePropertyDef {
                name: "order",
                get: Some(layer_absolute_get),
                set: Some(layer_absolute_set),
            },
            NativeInstancePropertyDef {
                name: "hitThreshold",
                get: Some(layer_hit_threshold_get),
                set: Some(layer_hit_threshold_set),
            },
            NativeInstancePropertyDef {
                name: "hitType",
                get: Some(layer_hit_type_get),
                set: Some(layer_hit_type_set),
            },
            NativeInstancePropertyDef {
                name: "cursor",
                get: Some(layer_cursor_get),
                set: Some(layer_cursor_set),
            },
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
            NativeInstancePropertyDef {
                name: "face",
                get: Some(layer_face_get),
                set: Some(layer_face_set),
            },
            NativeInstancePropertyDef {
                name: "holdAlpha",
                get: Some(layer_hold_alpha_get),
                set: Some(layer_hold_alpha_set),
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
            NativeInstancePropertyDef {
                name: "window",
                get: Some(layer_window_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "parent",
                get: Some(layer_parent_get),
                set: Some(layer_parent_set),
            },
            NativeInstancePropertyDef {
                name: "focusable",
                get: Some(layer_focusable_get),
                set: Some(layer_focusable_set),
            },
            NativeInstancePropertyDef {
                name: "focused",
                get: Some(layer_focused_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "nodeVisible",
                get: Some(layer_node_visible_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "nodeEnabled",
                get: Some(layer_node_enabled_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "nodeFocusable",
                get: Some(layer_node_focusable_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "joinFocusChain",
                get: Some(layer_join_focus_chain_get),
                set: Some(layer_join_focus_chain_set),
            },
            NativeInstancePropertyDef {
                name: "nextFocusable",
                get: Some(layer_next_focusable_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "prevFocusable",
                get: Some(layer_prev_focusable_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "children",
                get: Some(layer_children_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "enabled",
                get: Some(layer_enabled_get),
                set: Some(layer_enabled_set),
            },
            NativeInstancePropertyDef {
                name: "isPrimary",
                get: Some(layer_is_primary_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "cached",
                get: Some(layer_cached_get),
                set: Some(layer_cached_set),
            },
            NativeInstancePropertyDef {
                name: "imageModified",
                get: Some(layer_image_modified_get),
                set: Some(layer_image_modified_set),
            },
            NativeInstancePropertyDef {
                name: "name",
                get: Some(layer_name_get),
                set: Some(layer_name_set),
            },
            NativeInstancePropertyDef {
                name: "attentionLeft",
                get: Some(layer_attention_left_get),
                set: Some(layer_attention_left_set),
            },
            NativeInstancePropertyDef {
                name: "attentionTop",
                get: Some(layer_attention_top_get),
                set: Some(layer_attention_top_set),
            },
            NativeInstancePropertyDef {
                name: "useAttention",
                get: Some(layer_use_attention_get),
                set: Some(layer_use_attention_set),
            },
            NativeInstancePropertyDef {
                name: "clipLeft",
                get: Some(layer_clip_left_get),
                set: Some(layer_clip_left_set),
            },
            NativeInstancePropertyDef {
                name: "clipTop",
                get: Some(layer_clip_top_get),
                set: Some(layer_clip_top_set),
            },
            NativeInstancePropertyDef {
                name: "clipWidth",
                get: Some(layer_clip_width_get),
                set: Some(layer_clip_width_set),
            },
            NativeInstancePropertyDef {
                name: "clipHeight",
                get: Some(layer_clip_height_get),
                set: Some(layer_clip_height_set),
            },
            NativeInstancePropertyDef {
                name: "hint",
                get: Some(layer_hint_get),
                set: Some(layer_hint_set),
            },
            NativeInstancePropertyDef {
                name: "showParentHint",
                get: Some(layer_show_parent_hint_get),
                set: Some(layer_show_parent_hint_set),
            },
            NativeInstancePropertyDef {
                name: "ignoreHintSensing",
                get: Some(layer_ignore_hint_sensing_get),
                set: Some(layer_ignore_hint_sensing_set),
            },
            NativeInstancePropertyDef {
                name: "imeMode",
                get: Some(layer_ime_mode_get),
                set: Some(layer_ime_mode_set),
            },
            NativeInstancePropertyDef {
                name: "neutralColor",
                get: Some(layer_neutral_color_get),
                set: Some(layer_neutral_color_set),
            },
            NativeInstancePropertyDef {
                name: "absoluteOrderMode",
                get: Some(layer_absolute_order_mode_get),
                set: Some(layer_absolute_order_mode_set),
            },
            NativeInstancePropertyDef {
                name: "callOnPaint",
                get: Some(layer_call_on_paint_get),
                set: Some(layer_call_on_paint_set),
            },
        ],
    })
}
