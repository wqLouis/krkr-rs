//! The visual `Layer` native class.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::InputState;
use crate::engine_api::{LayerState, Rect, Scene};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, TjsValue, VAL_INTEGER, VAL_OBJECT, VAL_REAL, VAL_VOID, Value,
};

use super::common::{
    DEFAULT_FONT_COLOR, DEFAULT_FONT_FACE, DEFAULT_FONT_HEIGHT, ObjectHandle, ObjectKind,
    VisualState, argb_to_rgba, bound, duplicate_object, input_ref, reparent_layer,
    resolve_object_id, scene_mut, scene_ref, set_null_out, set_object_out, set_retained_out,
    set_visual_string_out,
};
use crate::collections::Map;
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

pub(crate) use blend::from_native;
pub use focus::take_capture_releases;
pub(crate) use focus::{
    blur_tree, clear_domain_focus, clear_layer_focus, focus_layer, focus_layer_in_input_domain,
};
pub use transitions::{advance_self_updating_transitions, advance_transitions};

/// Per-layer L3 state that has no home in the L2 `engine_api::Scene` yet: the
/// active transition, the modal-layer stack and the pending capture-release
/// requests the frame body drains.
///
/// Keyed by [`std::any::TypeId`] in [`CompatServices::state`](crate::CompatServices::state),
/// never a process global.
#[derive(Default)]
struct LayerExtras {
    /// Active `beginTransition` state per layer id.
    transitions: Map<u32, LayerTransition>,
    /// The scene's structure and content revisions the last
    /// script-cached-layer pass ran at, so an unchanged frame composites
    /// nothing.
    cache_revisions: Option<(u64, u64)>,
    /// Reference `tTVPLayerManager::ModalLayerVector`: modal layer ids.
    modes: Vec<u32>,
    /// `releaseCapture` requests the tree owner drains.
    release_capture: Vec<u32>,
    /// `releaseTouchCapture` requests (`None` = all contacts).
    release_touch_capture: Vec<(u32, Option<u64>)>,
}

/// One active layer transition (`beginTransition`).
struct LayerTransition {
    trans: crate::engine_api::Trans,
    /// The `transwith` layer the destination is being blended into (the
    /// reference's transition source).
    source: u32,
    /// Reference `TransWithChildren` (`LayerIntf.cpp:7872`): the transition
    /// blends the destination's and source's whole subtrees rather than
    /// their MainImages.
    with_children: bool,
}

/// Composite every script-cached layer's subtree into its cache and hand the
/// result to the projection: the per-frame step of `Layer.cached`.
///
/// The pass runs as a frame-body pump after the transition step, so a cache
/// that contains a transition sees the frame just blended, and before the
/// engine uploads and projects. It does nothing while the scene is unchanged:
/// no layer or bitmap edit, no timer-driven composite and no structural or
/// content revision bump means no cache can be stale.
///
/// A layer owned by an active transition keeps its cache slots for the
/// transition step and is left alone. A cached layer whose subtree contains a
/// whole-subtree transition cannot represent that transition's frame, so its
/// published cache is withdrawn and the layer draws live until the transition
/// ends. A layer that is hidden, or has neither its own image nor a visible
/// child, is skipped because the projection does not draw it.
///
/// The compositor reuses its content bitmap and recomposites only when the
/// subtree's recalc flag or its source images' generations changed, so a
/// stable frame calls no pixel work. Cached layers are visited deepest first,
/// so a parent composites through the cache its children publish in the same
/// pass instead of waiting for the next frame to see it.
///
/// # Errors
///
/// Returns the composite failure of one cached layer as a message; the
/// other layers are still composited.
pub fn refresh_cached_layers(ctx: &crate::CompatCtx) -> Result<(), String> {
    let (structure, content) = {
        let scene = scene_ref(ctx)?;
        (scene.structure_revision(), scene.content_revision())
    };
    let (last_revisions, in_transition, whole_subtree_transitions) = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        let extras = services.state::<LayerExtras>();
        (
            extras.cache_revisions,
            extras.transitions.keys().copied().collect::<Vec<u32>>(),
            extras
                .transitions
                .iter()
                .filter(|(_, entry)| entry.with_children)
                .map(|(&id, _)| id)
                .collect::<Vec<u32>>(),
        )
    };
    if last_revisions == Some((structure, content)) {
        return Ok(());
    }

    let scene = scene_mut(ctx)?;
    // The transition step owns the cache slots of these layers' ancestors too:
    // an ancestor's cache would hold the un-blended image the frame replaces.
    // A frame or an effect draw the projection draws never reaches a bitmap at
    // all, so an ancestor's cache cannot hold it either, and the layer has to
    // draw live for it to appear.
    let mut blocked: Vec<u32> = Vec::new();
    for &id in &in_transition {
        if scene
            .layer(id)
            .is_some_and(|layer| layer.transition_frame.is_some() || layer.effect.is_some())
        {
            blocked.extend(ancestors(scene, id));
        }
    }
    for id in whole_subtree_transitions {
        blocked.extend(ancestors(scene, id));
    }
    // Deepest first: a cached parent draws its cached descendants through the
    // caches this pass publishes, so one pass gives every level the current
    // child cache instead of the previous frame's.
    let mut cached: Vec<(usize, u32)> = Vec::new();
    for layer in &scene.layers {
        if layer.cached {
            cached.push((ancestors(scene, layer.id).len(), layer.id));
        }
    }
    cached.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, id) in cached {
        if in_transition.contains(&id) {
            continue;
        }
        if blocked.contains(&id) {
            scene.set_cache_bitmap(id, None);
            continue;
        }
        if !layer_seen(scene, id) {
            continue;
        }
        if !layer_has_content(scene, id) {
            // Nothing drawable is left: withdraw a cache published by an
            // earlier frame so the projection draws the live subtree.
            scene.set_cache_bitmap(id, None);
            continue;
        }
        let content = crate::engine_api::layer_ops::render_layer_subtree(scene, id)
            .ok_or_else(|| format!("Layer.cached: cannot composite the subtree of layer {id}"))?;
        if scene.layer(id).and_then(|layer| layer.bitmap) != Some(content) {
            scene.set_cache_bitmap(id, Some(content));
        }
    }
    let revisions = (scene.structure_revision(), scene.content_revision());
    // SAFETY: see above.
    let services = unsafe { ctx.services() };
    let mut extras = services.state::<LayerExtras>();
    extras.cache_revisions = Some(revisions);
    Ok(())
}

/// The layer's ancestors, nearest first.
fn ancestors(scene: &Scene, id: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut current = scene.layer(id).and_then(|layer| layer.parent);
    while let Some(next) = current {
        let Some(layer) = scene.layer(next) else {
            break;
        };
        out.push(layer.id);
        if out.len() > scene.layers.len() {
            break;
        }
        current = layer.parent;
    }
    out
}

/// Whether the layer and every ancestor is visible: the projection draws a
/// layer only then, and its cache only in that case.
fn layer_seen(scene: &Scene, id: u32) -> bool {
    let mut current = Some(id);
    let mut steps = 0usize;
    while let Some(next) = current {
        let Some(layer) = scene.layer(next) else {
            return false;
        };
        if !layer.visible {
            return false;
        }
        steps += 1;
        if steps > scene.layers.len() {
            return false;
        }
        current = layer.parent;
    }
    true
}

/// Whether the layer has a subtree worth compositing: its own image or a
/// visible, non-transparent direct child, with no transition self image
/// standing in for its own image.
fn layer_has_content(scene: &Scene, id: u32) -> bool {
    let Some(layer) = scene.layer(id) else {
        return false;
    };
    layer.transition_self.is_none()
        && (layer.bitmap.is_some()
            || layer.children.iter().any(|child| {
                scene
                    .layer(*child)
                    .is_some_and(|c| c.visible && c.opacity != 0.0)
            }))
}

#[derive(Default)]
struct LayerInst {
    id: Option<u32>,
    /// Set by the native constructor (`super.Layer(...)`). Before it runs the
    /// layer exists but is detached from its window and parent; `id` is
    /// already allocated so the reference's pre-`Construct` member writes
    /// land.
    attached: bool,
    font_object: Option<DetachedValue>,
    /// Reference `tTJSNI_BaseLayer::CursorX_Work` (`LayerIntf.cpp:347`): the
    /// pending X `SetCursorX` stores for the next `SetCursorY`.
    cursor_x_work: i32,
}

/// The scene id a `Layer` payload carries — the reader behind the VM's
/// native-instance lookup (`native_instance_payload("Layer")`), replacing
/// the removed `id`/`nativeId` script members for engine call sites.
pub(super) fn layer_payload_id(payload: *mut c_void) -> Option<u32> {
    // SAFETY: `native_instance_payload("Layer")` only hands out Layer payloads.
    unsafe { &*(payload as *const LayerInst) }.id
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
///
/// The layer is registered as a non-owning [`ObjectHandle`]: the script
/// object must be able to reach reference count zero so its native
/// `Invalidate`/destroy callbacks run and the scene record is released
/// (reference `tTJSNI_BaseLayer::Owner`, `LayerIntf.h:173`).
fn register_layer(ctx: &CompatCtx, id: u32, objthis: *mut c_void) {
    if objthis.is_null() {
        return;
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    unsafe { ctx.services() }
        .state::<VisualState>()
        .objects
        .layers
        .insert(id, ObjectHandle::new(objthis));
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
            &TjsValue::Retained(object.raw_id().as_ptr() as u64),
        )?;
        std::mem::forget(object);
    }
    Ok(Some(array))
}

// ---------------------------------------------------------------------------
// Layer
// ---------------------------------------------------------------------------

extern "C" fn layer_create(engine: *mut c_void) -> *mut c_void {
    // The reference's `CreateNativeInstance` returns a layer that already
    // carries its default rect and main image; only the window/parent join is
    // deferred to `Construct`. A script subclass touches members from its
    // class prelude or before `super.Layer(...)`, so the scene record must
    // exist from creation, detached until the join.
    let mut inst = LayerInst::default();
    if let Ok((_resolved, ctx)) = bound(engine)
        && let Ok(scene) = scene_mut(&ctx)
    {
        let id = scene.add_detached_layer();
        if let Some(layer) = scene.layer_mut(id) {
            layer.rect = Rect::new(0, 0, 32, 32);
        }
        if scene.ensure_layer_image(id, 32, 32) {
            inst.id = Some(id);
        } else {
            scene.remove_layer(id);
        }
    }
    Box::into_raw(Box::new(inst)) as *mut c_void
}

extern "C" fn layer_invalidate(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `layer_create` allocated the payload and the VM keeps it
    // alive for the duration of the callback.
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    let Some(id) = inst.id else {
        return;
    };
    // An invalidated layer can no longer hand out its cached `Font` object,
    // so release it now rather than when the script object drops.
    drop(inst.font_object.take());
    let Ok((_resolved, ctx)) = bound(engine) else {
        return;
    };
    // Reference `Invalidate()` flips `Shutdown` before any detach or blur
    // (`reference/cpp/core/visual/LayerIntf.cpp:515-516`), so the blur the
    // teardown triggers (`:538-544` → `Part()` `:624-631` → `SeverChild`
    // `:657-665` via `LayerManager.cpp:197-201`, `:690-711`) sees an inert
    // layer: the search origin has lost the manager and can only clear
    // focus, and an invalidated focus holder fires no `onBlur`
    // (`LayerManager.cpp:786-787`).
    if let Ok(scene) = scene_mut(&ctx)
        && let Some(layer) = scene.layer_mut(id)
    {
        layer.shutdown = true;
    }
    blur_tree(&ctx, id);
    // Finalize the direct child script objects while their Scene records
    // still exist, since a child finalizer expects its own layer record.
    let children: Vec<u32> = scene_ref(&ctx)
        .ok()
        .and_then(|scene| scene.layer(id).map(|layer| layer.children.clone()))
        .unwrap_or_default();
    for child in children {
        let raw = {
            // SAFETY: the caller resolved a live compat context.
            let services = unsafe { ctx.services() };
            let state = services.state::<VisualState>();
            state
                .objects
                .layers
                .get(&child)
                .map(crate::natives::visual::common::ObjectHandle::as_ptr)
        };
        let Some(raw) = raw else {
            continue;
        };
        let Ok(object) = _resolved.engine().retain_object_detached(raw) else {
            continue;
        };
        if let Err(error) = _resolved.engine().invalidate_retained(object.raw_id()) {
            log::debug!("Layer: child invalidate failed for #{child}: {error}");
        }
    }
    if let Ok(scene) = scene_mut(&ctx) {
        // Detach the layer from the scene so hit-testing and event dispatch
        // no longer resolve it once its script object is invalidated.
        scene.invalidate_layer(id);
    }
    // The plugin families keep their per-layer state keyed by layer id; the
    // reference frees it with the layer object
    // (`reference/cpp/plugins/layerExMovie.cpp:318-327`).
    crate::natives::plugins::layer_ex::forget_layer(&ctx, id);
    release_orphaned_layer_state(&ctx, _resolved.engine());
}

/// Release the cached L3 state of layers whose Scene record is already gone.
///
/// Their `LayerInst.font_object` would otherwise keep a retained `Font`
/// forever; the script object itself is finalized by its own lifetime.
fn release_orphaned_layer_state(ctx: &CompatCtx, host: &Tjs2Engine) {
    let Some(scene) = scene_ref(ctx).ok() else {
        return;
    };
    let candidates: Vec<(u32, *mut c_void)> = {
        // SAFETY: the caller resolved a live compat context.
        let services = unsafe { ctx.services() };
        let mut state = services.state::<VisualState>();
        let candidates = state
            .objects
            .layers
            .iter()
            .filter(|(id, _)| scene.layer(**id).is_none())
            .map(|(id, handle)| (*id, handle.as_ptr()))
            .collect();
        // The Scene record is gone, so drop the registry entry before
        // touching the script objects; a re-entrant `layer_invalidate` then
        // sees no stale candidates.
        state
            .objects
            .layers
            .retain(|id, _| scene.layer(*id).is_some());
        candidates
    };
    for (id, raw) in candidates {
        let Ok(object) = host.retain_object_detached(raw) else {
            continue;
        };
        if let Ok(Some(payload)) = host.native_instance_payload(object.raw_id(), "Layer") {
            // SAFETY: `native_instance_payload("Layer")` only returns Layer
            // payloads, and `object` keeps the payload alive for this call.
            let inst = unsafe { &mut *(payload.as_ptr() as *mut LayerInst) };
            drop(inst.font_object.take());
        }
        crate::natives::plugins::layer_ex::forget_layer(ctx, id);
        // The script object keeps its remaining members and is finalized when
        // its own references drop; invalidating it here would run a finalizer
        // against the already removed Scene record.
    }
}

extern "C" fn layer_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; `layer_destroy` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .objects
            .layers
            .remove(&id);
        // A payload destroyed without the invalidate hook still owns a
        // detached record; remove it so no scene entry is left behind.
        if let Ok(scene) = scene_mut(&ctx) {
            scene.remove_layer(id);
        }
        // Idempotent with the invalidate hook, which always runs first; a
        // payload destroyed without it still drops its per-layer plugin
        // state.
        crate::natives::plugins::layer_ex::forget_layer(&ctx, id);
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; `layer_ctor` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    if inst.attached {
        return report_error(out_error, "Layer: this layer is already constructed");
    }
    // Window: explicit object/id, else the scene's first window.
    let window = match a.first() {
        Some(value)
            if value.ty == VAL_OBJECT || value.ty == VAL_INTEGER || value.ty == VAL_REAL =>
        {
            match resolve_object_id(resolved.engine(), value, &["Window"]) {
                Ok(id) if id >= 0 => Some(id),
                Ok(_) => None,
                Err(error) => return report_error(out_error, &format!("Layer: {error}")),
            }
        }
        _ => None,
    };
    // Parent: a `Layer` object/id, else the window root. Resolved before the
    // scene borrow so the getter cannot re-enter the VM while `&mut Scene` is
    // held.
    let parent = match a.get(1) {
        Some(value) if value.ty == VAL_OBJECT => {
            match resolve_object_id(resolved.engine(), value, &["Layer"]) {
                Ok(id) if id >= 0 => Some(id as u32),
                Ok(_) => None,
                Err(error) => return report_error(out_error, &format!("Layer: {error}")),
            }
        }
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
    let id = match inst.id {
        // `layer_create` already allocated the detached scene record; join it
        // to `window`/`parent` (the reference `Construct`).
        Some(id) => {
            scene.attach_layer(id, window, parent);
            id
        }
        // No scene was available at creation time (e.g. before any window);
        // allocate and join now.
        None => {
            let id = scene.add_layer(window, parent);
            if let Some(layer) = scene.layer_mut(id) {
                layer.rect = Rect::new(0, 0, 32, 32);
            }
            if !scene.ensure_layer_image(id, 32, 32) {
                scene.remove_layer(id);
                return report_error(out_error, "Layer: could not allocate the default image");
            }
            inst.id = Some(id);
            id
        }
    };
    inst.attached = true;
    log::trace!("Layer created: id={id} parent={parent:?}");
    register_layer(&ctx, id, objthis);
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    // A bounds request is exactly four arguments whose third and fourth are
    // not void; otherwise the call is a position-only move.
    let size = if argc == 4 && a[2].ty != VAL_VOID && a[3].ty != VAL_VOID {
        Some((value_as_i64(&a[2]) as i32, value_as_i64(&a[3]) as i32))
    } else {
        None
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setPos: compat context is not bound");
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_pos`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setPos: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // A negative extent is rejected before the move is considered.
    if let Some((width, height)) = size
        && (width < 0 || height < 0)
    {
        return report_error(out_error, "Layer.setPos: Invalid argument");
    }
    let Some((current_x, current_y)) = scene.layer(id).map(|layer| (layer.rect.x, layer.rect.y))
    else {
        return report_error(out_error, "Layer.setPos: this layer is not constructed");
    };
    // Only a position change is refused on the primary layer; a size-only
    // change, or writing the position it already holds, is allowed.
    if (current_x != x || current_y != y) && scene.is_primary_layer(id) && (x != 0 || y != 0) {
        return report_error(out_error, "Layer.setPos: Cannot move primary layer");
    }
    // The position guard and the size guard are independent: a repeated
    // position write does not run `layer_size_changed`, and a repeated size
    // write does not re-run the position half.
    let mut size_changed = false;
    let _ = scene.update_layer(id, |layer| {
        let mut changed = false;
        if layer.rect.x != x || layer.rect.y != y {
            layer.rect.x = x;
            layer.rect.y = y;
            changed = true;
        }
        if let Some((w, h)) = size {
            let (w, h) = (w as u32, h as u32);
            if layer.rect.w != w || layer.rect.h != h {
                layer.rect.w = w;
                layer.rect.h = h;
                size_changed = true;
                changed = true;
            }
        }
        changed
    });
    if size_changed {
        scene.layer_size_changed(id);
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setSize: compat context is not bound");
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_size`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setSize: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if scene.update_layer(id, |layer| {
        if layer.rect.w == width && layer.rect.h == height {
            return false;
        }
        layer.rect.w = width;
        layer.rect.h = height;
        true
    }) == Some(true)
    {
        scene.layer_size_changed(id);
    }
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_size_to_image_size`.
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
    // A layer with no MainImage raises instead of leaving the rect unchanged.
    let Some((width, height)) = scene
        .layer(id)
        .and_then(|l| l.bitmap)
        .and_then(|bitmap| scene.bitmap(bitmap))
        .map(|b| (b.width, b.height))
    else {
        return report_error(
            out_error,
            "Layer.setSizeToImageSize: Not drawable layer type",
        );
    };
    if scene.update_layer(id, |layer| {
        if layer.rect.w == width && layer.rect.h == height {
            return false;
        }
        layer.rect.w = width;
        layer.rect.h = height;
        true
    }) == Some(true)
    {
        scene.layer_size_changed(id);
    }
    set_void(out);
    0
}

/// The image-offset guard the `imageLeft`/`imageTop` setters and
/// `setImagePos` share: an offset is allowed only when it is not positive and
/// the image (`image_extent` wide or tall) still covers the layer rectangle
/// (`rect_extent`) after the shift.
fn image_offset_allowed(offset: i32, image_extent: u32, rect_extent: u32) -> bool {
    offset <= 0 && i64::from(image_extent) + i64::from(offset) >= i64::from(rect_extent)
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let left = value_as_i64(&a[0]) as i32;
    let top = value_as_i64(&a[1]) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setImagePos: compat context is not bound");
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_image_pos`.
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
    let (has_image, image_width, image_height, rect_w, rect_h, current_left, current_top) =
        match scene.layer(id) {
            Some(layer) => (
                layer.bitmap.is_some(),
                layer.image_width,
                layer.image_height,
                layer.rect.w,
                layer.rect.h,
                layer.image_left,
                layer.image_top,
            ),
            None => {
                return report_error(
                    out_error,
                    "Layer.setImagePos: this layer is not constructed",
                );
            }
        };
    if !has_image {
        return report_error(out_error, "Layer.setImagePos: this layer has no main image");
    }
    if current_left != left || current_top != top {
        if !image_offset_allowed(left, image_width, rect_w)
            || !image_offset_allowed(top, image_height, rect_h)
        {
            return report_error(out_error, "Layer.setImagePos: invalid image position");
        }
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_left = left;
            layer.image_top = top;
        }
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setImageSize: compat context is not bound");
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_image_size`.
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
    if width == 0 || height == 0 {
        return report_error(
            out_error,
            "Layer.setImageSize: cannot create an empty layer image",
        );
    }
    let (has_image, current_width, current_height) = match scene.layer(id) {
        Some(layer) => (
            layer.bitmap.is_some(),
            layer.image_width,
            layer.image_height,
        ),
        None => {
            return report_error(
                out_error,
                "Layer.setImageSize: this layer is not constructed",
            );
        }
    };
    if !has_image {
        return report_error(
            out_error,
            "Layer.setImageSize: this layer has no main image",
        );
    }
    if width == current_width && height == current_height {
        set_void(out);
        return 0;
    }
    // The size change defers to the engine's own rectangle/offset adjustment.
    if !scene.internal_set_image_size(id, width, height) {
        return report_error(
            out_error,
            "Layer.setImageSize: could not resize the layer image",
        );
    }
    set_void(out);
    0
}

/// The pixels of a scene bitmap, with a still-decoding image resolved first.
///
/// A bitmap whose decode is still running holds no pixels until
/// [`Scene::resolve_bitmap`] installs them. `operation` names the caller and
/// the bitmap id goes into the error a bitmap raises when it still holds no
/// pixel buffer after the wait, so no caller clones or indexes an empty
/// image.
pub(super) fn resolved_bitmap_pixels(
    scene: &mut Scene,
    bitmap: u32,
    operation: &str,
) -> Result<(u32, u32, Vec<u8>), String> {
    // The decode has not landed yet: wait for it on the mutable scene borrow
    // the caller already holds.
    scene.resolve_bitmap(bitmap);
    let Some(state) = scene.bitmap(bitmap) else {
        return Err(format!("{operation}: the bitmap {bitmap} does not exist"));
    };
    let expected = usize::try_from(state.width)
        .ok()
        .zip(usize::try_from(state.height).ok())
        .and_then(|(width, height)| width.checked_mul(height))
        .and_then(|pixels| pixels.checked_mul(4));
    if expected != Some(state.rgba.len()) {
        return Err(format!(
            "{operation}: the bitmap {bitmap} holds {} pixel bytes instead of its {}x{} image",
            state.rgba.len(),
            state.width,
            state.height
        ));
    }
    Ok((state.width, state.height, state.rgba.clone()))
}

/// Copy `source`'s MainImage and ProvinceImage into `dest`, following the
/// reference `tTJSNI_BaseLayer::AssignImages` (`LayerIntf.cpp:2394-2432`):
/// a present source image is assigned into ours, an absent one deallocates
/// ours (`DeallocateImage`/`DeallocateProvinceImage`, `:2349-2378`); only the
/// images are copied, so the destination rectangle adjusts to the assigned
/// image size with the clip reset, leaving the destination's own image offsets
/// in place (`InternalSetImageSize`/`ResetClip`, `:2419-2430`); `ImageModified` is
/// set. The reference's trailing `Update(false)` is a plain repaint, which
/// the projection already performs every frame; `pending_paint` is reserved
/// for the `onPaint` dispatch (`CallOnPaint`, set by `UpdateByScript`), so
/// this member must not re-arm it. An assigned image that cannot take its
/// size is an error rather than stale geometry.
fn assign_images(scene: &mut Scene, dest: u32, source: u32) -> Result<(), String> {
    // An invalidated source still resolves as a Layer — its native instance
    // stays registered until the object is destructed — but invalidation has
    // already released its MainImage and ProvinceImage, so it carries no
    // images and the destination's images are deallocated. A missing scene
    // record therefore means an empty source, not a rejected argument.
    // Snapshot the source before touching the destination: the two may share
    // a bitmap id, and the copy must reflect the pre-write source. The source
    // may still be decoding, so the snapshot waits for its pixels.
    let src_main = match scene.layer(source).and_then(|layer| layer.bitmap) {
        Some(bitmap) => Some(resolved_bitmap_pixels(scene, bitmap, "Layer.assignImages")?),
        None => None,
    };
    let src_province = scene.layer(source).map(|layer| {
        (
            layer.province.clone(),
            layer.province_width,
            layer.province_height,
        )
    });

    // MainImage (`LayerIntf.cpp:2399-2408`). Reuse the destination's own
    // bitmap, but never overwrite one owned by a script `Bitmap` object
    // (`script_owned`, e.g. a PSD-placed image): the reference's MainImage is
    // layer-private, so adopt a fresh copy instead of corrupting the script's.
    if let Some((width, height, rgba)) = src_main {
        let reusable = scene
            .layer(dest)
            .and_then(|layer| layer.bitmap)
            .filter(|id| scene.bitmap(*id).is_some_and(|bitmap| !bitmap.script_owned));
        match reusable {
            Some(bitmap_id) => {
                // The assigned image replaces the destination's pixels: a
                // decode the destination still carries is cancelled, so the
                // assigned pixels are not overwritten by the stale image.
                if !scene.replace_bitmap_image(bitmap_id, width, height, rgba) {
                    return Err(
                        "Layer.assignImages: the layer image could not be replaced".to_string()
                    );
                }
            }
            None => {
                let bitmap_id = scene.add_bitmap(width, height, rgba);
                if let Some(layer) = scene.layer_mut(dest) {
                    layer.bitmap = Some(bitmap_id);
                }
            }
        }
    } else {
        // `DeallocateImage` (`LayerIntf.cpp:2349-2356`); release the old
        // bitmap like the `hasImage = false` setter, which keeps a shared or
        // script-owned bitmap alive when another holder remains.
        let removed = scene.layer_mut(dest).and_then(|layer| layer.bitmap.take());
        if let Some(bitmap) = removed {
            scene.release_bitmap(bitmap);
        }
    }

    // ProvinceImage (`LayerIntf.cpp:2410-2417`).
    match src_province {
        Some((plane, width, height)) => {
            if let Some(layer) = scene.layer_mut(dest) {
                layer.province = plane;
                layer.province_width = width;
                layer.province_height = height;
            }
        }
        None => scene.deallocate_province_image(dest),
    }

    // `InternalSetImageSize` + `ImageModified` + `ResetClip`
    // (`LayerIntf.cpp:2419-2430`). The assigned image is the destination's
    // MainImage, which is absent when the source had none; the rectangle
    // adjusts to it and the destination keeps its own image offsets.
    let assigned = scene
        .layer(dest)
        .and_then(|layer| layer.bitmap)
        .and_then(|id| scene.bitmap(id))
        .map(|bitmap| (bitmap.width, bitmap.height));
    match assigned {
        Some((width, height)) => {
            if !scene.internal_set_image_size(dest, width, height) {
                return Err("Layer.assignImages: the layer image could not be sized".to_string());
            }
        }
        None => {
            if let Some(layer) = scene.layer_mut(dest) {
                layer.image_modified = true;
            }
        }
    }
    Ok(())
}

/// `Layer.assignImages(srcLayer)` — copy the source layer's images into this
/// layer (reference `tTJSNI_BaseLayer::AssignImages`,
/// `LayerIntf.cpp:2394-2432`). The argument is resolved only through the
/// Layer native instance, so a non-object, `null`, or a non-Layer object
/// raises `TVPSpecifyLayer` (`LayerIntf.cpp:9860-9882`); a missing argument
/// is `TJS_E_BADPARAMCOUNT` (`:9860-9861`). An invalidated Layer is still a
/// Layer for the lookup — its native instance stays registered until the
/// object is destructed — and contributes no images, so the destination's
/// images are deallocated.
extern "C" fn layer_assign_images(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.assignImages requires 1 argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.assignImages: compat context is not bound");
    };
    // Reference `param[0]->AsObjectClosureNoAddRef()` throws for a
    // non-object, then `NativeInstanceSupport(TJS_NIS_GETINSTANCE,
    // Layer::ClassID)` and `TVPSpecifyLayer` reject the rest
    // (`LayerIntf.cpp:9863-9875`).
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let value = &unsafe { args(argv, argc) }[0];
    if value.ty != VAL_OBJECT {
        return report_error(out_error, "Layer.assignImages: not a Layer object");
    }
    let source = match resolve_object_id(resolved.engine(), value, &["Layer"]) {
        Ok(id) if id >= 0 => id as u32,
        _ => return report_error(out_error, "Layer.assignImages: not a Layer object"),
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_assign_images`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.assignImages: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Err(error) = assign_images(scene, id, source) {
        return report_error(out_error, &error);
    }
    set_void(out);
    0
}

/// `Layer.copyFromBitmapToMainImage(bitmap)` — copy the source pixels into
/// the layer's MainImage (allocating/resizing it to match) and adjust the
/// layer rectangle and image offsets to the assigned image; the clip resets
/// and `ImageModified` is set.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let value = &unsafe { args(argv, argc) }[0];
    // A `null` argument is an object value with no object: the copy is skipped
    // and the call succeeds.
    if value.ty == tjs::VAL_NULL {
        set_void(out);
        return 0;
    }
    if value.ty != tjs::VAL_OBJECT {
        return report_error(out_error, "Layer.copyFromBitmapToMainImage: not a Bitmap");
    }
    let bitmap_id = match resolve_object_id(resolved.engine(), value, &["Bitmap"]) {
        Ok(id) if id >= 0 => id as u32,
        _ => {
            return report_error(out_error, "Layer.copyFromBitmapToMainImage: not a Bitmap");
        }
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_copy_from_bitmap_to_main_image`.
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
    let (width, height, rgba) =
        match resolved_bitmap_pixels(scene, bitmap_id, "Layer.copyFromBitmapToMainImage") {
            Ok(pixels) => pixels,
            Err(error) => return report_error(out_error, &error),
        };
    if !scene.set_layer_image_size(id, width, height) {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: could not resize the layer image",
        );
    }
    let Some(layer_bitmap) = scene.layer(id).and_then(|l| l.bitmap) else {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: the layer has no main image",
        );
    };
    // The copy replaces the layer's image: a decode the layer still carries is
    // cancelled, so the copied pixels are not overwritten by the stale image.
    if !scene.replace_bitmap_image(layer_bitmap, width, height, rgba) {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: the layer image could not be replaced",
        );
    }
    if !scene.internal_set_image_size(id, width, height) {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: could not size the layer image",
        );
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_independ_main_image`.
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
    let Some(bitmap_id) = scene.layer(id).and_then(|l| l.bitmap) else {
        set_void(out);
        return 0;
    };
    // The image may still be decoding: the copy is of the resolved pixels.
    let (width, height, rgba) =
        match resolved_bitmap_pixels(scene, bitmap_id, "Layer.independMainImage") {
            Ok(pixels) => pixels,
            Err(error) => return report_error(out_error, &error),
        };
    // Install the fresh copy, then release the layer's reference to the
    // replaced image.
    let replaced = scene.layer_mut(id).and_then(|layer| layer.bitmap.take());
    let new_bitmap = scene.add_bitmap(width, height, rgba);
    if let Some(layer) = scene.layer_mut(id) {
        layer.bitmap = Some(new_bitmap);
        layer.image_width = width;
        layer.image_height = height;
        layer.image_modified = true;
    }
    if let Some(replaced) = replaced {
        scene.release_bitmap(replaced);
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_bring_to_front`.
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
        // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
        let value = &unsafe { args(argv, argc) }[0];
        if value.ty == VAL_VOID {
            None
        } else {
            match resolve_object_id(resolved.engine(), value, &["Layer"]) {
                Ok(id) if id >= 0 => Some(id as u32),
                Ok(_) => None,
                Err(error) => return report_error(out_error, &format!("Layer.parent: {error}")),
            }
        }
    } else {
        None
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_parent_id`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.parent: this layer is not constructed");
    };
    let old_parent = {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        scene.layer(id).and_then(|l| l.parent)
    };
    // `Join` refuses a self-parent (`reference/cpp/core/visual/LayerIntf.cpp:612-613`)
    // and a move under a parent whose manager differs (`:614-615`) before
    // any part or blur; every parentless layer owns its own manager, so
    // "different manager" is "different focus domain". A missing record on
    // either side is a released manager, which also fails the comparison.
    if parent == Some(id) {
        return report_error(out_error, "Layer.parent: Cannot set parent self");
    }
    if let Some(parent) = parent {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        let same_domain = scene
            .layer(id)
            .zip(scene.layer(parent))
            .is_some_and(|(layer, target)| layer.domain == target.domain);
        if !same_domain {
            return report_error(
                out_error,
                "Layer.parent: Cannot move to under Other primary layer",
            );
        }
    }
    // Reference `Join` calls `Part()` while the layer is still attached to
    // its old parent and only then re-attaches it
    // (`reference/cpp/core/visual/LayerIntf.cpp:619-621`); `Part()` fires
    // `NotifyPart` -> `BlurTree` before the sever, so the blur sees the
    // pre-move tree (`LayerIntf.cpp:624-631`, `LayerManager.cpp:197-201`,
    // `:690-711`).
    if old_parent.is_some() {
        blur_tree(&ctx, id);
    }
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    reparent_layer(scene, id, parent);
    set_void(out);
    0
}

/// `Layer.focus([direction])` — give the layer keyboard focus.
///
/// The optional argument is the focus direction: the reference native reads
/// `param[0]->operator bool()`, defaulting to `true`
/// (`reference/cpp/core/visual/LayerIntf.cpp:9726-9742`).
extern "C" fn layer_focus(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.focus: compat context is not bound");
    };
    let direction = if argc >= 1 {
        // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
        value_as_bool(&unsafe { args(argv, argc) }[0])
    } else {
        true
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_focus`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.focus: this layer is not constructed");
    };
    focus_layer(&ctx, id, direction);
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_update`.
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

/// Fire `onPaint` for every layer whose `pending_paint` flag is set — the
/// per-draw `tTJSNI_BaseLayer::BeforeCompletion` step
/// (`reference/cpp/core/visual/LayerIntf.cpp:6401-6402`): the flag is cleared
/// and the event is posted to the layer object, so a script override runs.
///
/// This is what makes `AffineLayer` render: its `onPaint` copies the hidden
/// inner `_image` layer onto the visible outer layer and syncs the outer
/// size (`system/affinelayer.tjs:_image`/`super.assignImages(_image)`), and
/// nothing else composites it. The frame body calls this before the draw, so
/// a layer whose `update()` (or `setSize*`/`loadImages`, which call it) armed
/// the flag gets its composed bitmap rendered in the same frame.
///
/// The clear happens **before** the call, like the reference, so a re-entrant
/// `update()` from inside `onPaint` persists to the next frame's dispatch.
///
/// # Errors
///
/// Returns the failure of one `onPaint` handler as a message; the other
/// layers still receive their event.
pub fn paint_poll(ctx: &CompatCtx) -> Result<(), String> {
    let ids: Vec<u32> = scene_ref(ctx)?
        .layers
        .iter()
        .filter(|layer| layer.pending_paint)
        .map(|layer| layer.id)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let host = unsafe { ctx.engine() };
    for id in ids {
        if let Some(layer) = scene_mut(ctx)?.layer_mut(id) {
            layer.pending_paint = false;
        }
        let Some(target) = duplicate_object(ctx, host, ObjectKind::Layer, id) else {
            continue;
        };
        if let Err(error) = host.call_member(target.raw_id(), "onPaint", &[]) {
            log::warn!("Layer {id}: onPaint handler failed: {error}");
        }
    }
    Ok(())
}

/// `Layer.setClip(left, top, width, height)`.
///
/// A call with no arguments resets the clip rectangle to the whole
/// MainImage; otherwise the four edges are required (reference `setClip`,
/// `reference/cpp/core/visual/LayerIntf.cpp:8635-8658`).
extern "C" fn layer_set_clip(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc != 0 && argc < 4 {
        return report_error(out_error, "Layer.setClip requires 4 arguments");
    }
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setClip: compat context is not bound");
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_clip`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setClip: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // A layer with no MainImage raises rather than storing or resetting the
    // rectangle.
    let result = if argc == 0 {
        reset_clip(scene, id)
    } else {
        // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
        let a = unsafe { args(argv, argc) };
        let left = value_as_i64(&a[0]) as i32;
        let top = value_as_i64(&a[1]) as i32;
        let width = value_as_i64(&a[2]) as i32;
        let height = value_as_i64(&a[3]) as i32;
        set_clip_rect(
            scene,
            id,
            i64::from(left),
            i64::from(top),
            i64::from(width),
            i64::from(height),
        )
    };
    if let Err(error) = result {
        return report_error(out_error, &format!("Layer.setClip: {error}"));
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.getProvincePixel: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_get_province_pixel`.
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
    set_int(out, i64::from(scene.province_pixel(id, x, y)));
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_province_pixel`.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let exclude_self = a.get(2).is_some_and(value_as_bool);
    let get_disabled = a.get(3).is_some_and(value_as_bool);
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.getLayerAt: compat context is not bound");
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_get_layer_at`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.getLayerAt: this layer is not constructed");
    };
    let hit = {
        let scene = match scene_mut(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        // The hit test reads a layer's main image: an image whose decode has
        // not landed yet is waited for first.
        if let Some(window) = scene.layer(id).map(|layer| layer.window) {
            crate::event::resolve_layer_images(scene, window);
        }
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let w = value_as_i64(&a[2]) as i32;
    let h = value_as_i64(&a[3]) as i32;
    let color = argb_to_rgba(value_as_i64(&a[4]));
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.fillRect: compat context is not bound");
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_fill_rect`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.fillRect: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let clip = util::layer_clip(scene, id);
    let Some(dest) = crate::engine_api::layer_ops::intersect_rect(
        (x, y, x.saturating_add(w), y.saturating_add(h)),
        clip,
    ) else {
        set_void(out);
        return 0;
    };
    if scene.layer(id).and_then(|layer| layer.bitmap).is_none() {
        return report_error(out_error, "Layer.fillRect: this layer has no main image");
    }
    let face = pixels::draw_face(scene, id);
    if face == pixels::DF_PROVINCE {
        pixels::fill_province_region(scene, id, dest, color[2]);
        set_void(out);
        return 0;
    }
    let hold_alpha = scene.layer(id).is_some_and(|layer| layer.hold_alpha);
    // The image may still be decoding: the fill lands on the decoded pixels,
    // and an image that resolved without its pixels is an error rather than a
    // silently dropped fill.
    let bitmap_id = match util::main_bitmap(scene, id, "fillRect") {
        Ok(bitmap_id) => bitmap_id,
        Err(error) => return report_error(out_error, &error),
    };
    let Some(bitmap) = scene.bitmap_mut(bitmap_id) else {
        return report_error(
            out_error,
            "Layer.fillRect: the layer image no longer exists",
        );
    };
    let width = (dest.2 - dest.0) as u32;
    let height = (dest.3 - dest.1) as u32;
    let modified = match face {
        pixels::DF_ALPHA | pixels::DF_ADD_ALPHA => {
            crate::engine_api::raster::fill_rect_replace(
                bitmap, dest.0, dest.1, width, height, color,
            );
            true
        }
        pixels::DF_OPAQUE if !hold_alpha => {
            crate::engine_api::raster::fill_rect_replace(
                bitmap, dest.0, dest.1, width, height, color,
            );
            true
        }
        pixels::DF_OPAQUE => {
            crate::engine_api::layer_ops::fill_color_hold_alpha(bitmap, dest, color, 255);
            true
        }
        pixels::DF_MASK => {
            crate::engine_api::layer_ops::fill_mask(bitmap, dest, color[2]);
            true
        }
        _ => false,
    };
    if modified {
        bitmap.mark_dirty();
        pixels::mark_image_modified(scene, id);
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `with_layer_read`.
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

/// Mutate a layer through the bound scene. `f` returns true when it changed
/// the layer; an unchanged write marks no subtree cache stale and bumps no
/// revision, so a setter that receives the stored value is free.
fn with_layer_write(
    engine: *mut c_void,
    instance: *mut c_void,
    value: &Value,
    f: impl FnOnce(&mut LayerState, &Value) -> bool,
) -> Result<(), String> {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `with_layer_write`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let id = inst
        .id
        .ok_or_else(|| "Layer: this layer is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_mut(&ctx)?;
    match scene.update_layer(id, |layer| f(layer, value)) {
        Some(_) => Ok(()),
        None => Err("Layer: layer no longer exists".to_string()),
    }
}

/// [`with_layer_write`], then [`blur_tree`] on the layer when the write
/// changed it and the flag now reads off — the reference
/// `SetVisible(false)` / `SetEnabled(false)` / `SetFocusable(false)` each
/// blur the focused subtree the change made unusable
/// (`reference/cpp/core/visual/LayerIntf.cpp:1183-1206`, `:3801-3832`,
/// `:3683-3696`).
fn with_layer_write_blur(
    engine: *mut c_void,
    instance: *mut c_void,
    value: &Value,
    f: impl FnOnce(&mut LayerState, &Value) -> bool,
    off: impl FnOnce(&LayerState) -> bool,
) -> Result<(), String> {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `with_layer_write_blur`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let id = inst
        .id
        .ok_or_else(|| "Layer: this layer is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_mut(&ctx)?;
    let changed = match scene.update_layer(id, |layer| f(layer, value)) {
        Some(changed) => changed,
        None => return Err("Layer: layer no longer exists".to_string()),
    };
    if changed && scene.layer(id).is_some_and(off) {
        blur_tree(&ctx, id);
    }
    Ok(())
}

/// Mutate the layer through the bound scene with the whole scene handed to
/// the setter: a property write that must refuse before it stores (the
/// position setters) or run scene-level follow-up after it (the size
/// setters) needs the scene the value lives in, not just the layer. The
/// setter owns change detection — it decides whether the write invalidates
/// and whether its follow-up runs.
fn with_layer_scene(
    engine: *mut c_void,
    instance: *mut c_void,
    value: &Value,
    f: impl FnOnce(&mut Scene, u32, &Value) -> Result<(), String>,
) -> Result<(), String> {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `with_layer_scene`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let id = inst
        .id
        .ok_or_else(|| "Layer: this layer is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_mut(&ctx)?;
    f(scene, id, value)
}

/// The clip rectangle the `clip*` getters report: the stored clip, or the
/// whole image while no clip has been stored.
fn effective_clip(layer: &LayerState) -> Rect {
    layer
        .clip
        .unwrap_or_else(|| Rect::new(0, 0, layer.image_width, layer.image_height))
}

/// The clip a `clip*` property setter writes its own edge into — the same
/// read the getters do, so the stored value the script last read is the
/// value the next write intersects from.
fn current_clip(scene: &Scene, id: u32) -> Result<Rect, String> {
    scene
        .layer(id)
        .map(effective_clip)
        .ok_or_else(|| "Layer: layer no longer exists".to_string())
}

/// Store `left`/`top`/`width`/`height` intersected with the layer's
/// MainImage — the rectangle both the `setClip` method and the `clip*`
/// property setters store. Without an image nothing is stored and
/// `Not drawable layer type` is returned. The near edges clamp at zero, the
/// far edges at the image bounds measured from the unclamped near edge, and
/// a far edge never precedes its near edge. `left`/`top` arrive bounded by
/// `tjs_int`; `width`/`height` only reach the stored rectangle through the
/// image bounds.
fn set_clip_rect(
    scene: &mut Scene,
    id: u32,
    left: i64,
    top: i64,
    width: i64,
    height: i64,
) -> Result<(), String> {
    let Some((image_w, image_h)) = scene
        .layer(id)
        .and_then(|layer| layer.bitmap)
        .and_then(|bitmap| scene.bitmap(bitmap))
        .map(|bitmap| (i64::from(bitmap.width), i64::from(bitmap.height)))
    else {
        return Err("Not drawable layer type".to_string());
    };
    let left_clamped = left.max(0);
    let top_clamped = top.max(0);
    let right = (width + left).min(image_w).max(left_clamped);
    let bottom = (height + top).min(image_h).max(top_clamped);
    let Some(layer) = scene.layer_mut(id) else {
        return Err("layer no longer exists".to_string());
    };
    layer.clip = Some(Rect::new(
        left_clamped as i32,
        top_clamped as i32,
        (right - left_clamped) as u32,
        (bottom - top_clamped) as u32,
    ));
    Ok(())
}

/// Reset the clip rectangle to the whole MainImage — the no-argument
/// `setClip()` (reference `ResetClip`, `LayerIntf.cpp:4032-4039`). Without an
/// image nothing is stored and `Not drawable layer type` is returned.
fn reset_clip(scene: &mut Scene, id: u32) -> Result<(), String> {
    if scene
        .layer(id)
        .and_then(|layer| layer.bitmap)
        .and_then(|bitmap| scene.bitmap(bitmap))
        .is_none()
    {
        return Err("Not drawable layer type".to_string());
    }
    let Some(layer) = scene.layer_mut(id) else {
        return Err("layer no longer exists".to_string());
    };
    layer.clip = None;
    Ok(())
}

/// Define a read-only `Layer` integer property (getter only, no setter).
macro_rules! layer_int_get {
    ($get:ident, $getter:expr) => {
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
    };
}

/// Define a read/write layer property over an `i64`/bool field.
macro_rules! layer_int_prop {
    ($get:ident, $set:ident, $getter:expr, $setter:expr) => {
        layer_int_get!($get, $getter);
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

/// Define a boolean layer property whose setter blurs the focused subtree
/// when the flag turns off; the citations live on
/// [`with_layer_write_blur`].
macro_rules! layer_blur_off_prop {
    ($get:ident, $set:ident, $getter:expr, $setter:expr, $off:expr) => {
        layer_int_get!($get, $getter);
        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: the trampoline passes a valid value slot.
            let value = unsafe { &*value };
            match with_layer_write_blur(engine, instance, value, $setter, $off) {
                Ok(()) => 0,
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

/// Define a read/write layer property whose setter needs the whole scene:
/// it can raise before it stores and run scene-level follow-up after it.
/// The setter receives the scene, the layer id and the script value, and
/// returns the error to raise, if any.
macro_rules! layer_int_prop_scene {
    ($get:ident, $set:ident, $getter:expr, $setter:expr) => {
        layer_int_get!($get, $getter);
        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: the trampoline passes a valid value slot.
            let value = unsafe { &*value };
            match with_layer_scene(engine, instance, value, $setter) {
                Ok(()) => 0,
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

/// Define a layer property setter whose writes are validated by the whole
/// scene; the getter is written separately when it must raise. See
/// [`with_layer_scene`].
macro_rules! layer_scene_set {
    ($set:ident, $setter:expr) => {
        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: the trampoline passes a valid value slot.
            let value = unsafe { &*value };
            match with_layer_scene(engine, instance, value, $setter) {
                Ok(()) => 0,
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

layer_blur_off_prop!(
    layer_visible_get,
    layer_visible_set,
    |l: &LayerState| i64::from(l.visible),
    |l: &mut LayerState, v: &Value| {
        let visible = value_as_bool(v);
        log::trace!("Layer {} .visible -> {visible}", l.id);
        if l.visible == visible {
            return false;
        }
        l.visible = visible;
        true
    },
    |l: &LayerState| !l.visible
);
layer_int_prop!(
    layer_type_get,
    layer_type_set,
    |l: &LayerState| blend::to_native(l.blend),
    |l: &mut LayerState, v: &Value| {
        let new_blend = blend::from_native(value_as_i64(v));
        if l.blend == new_blend {
            return false;
        }
        l.blend = new_blend;
        true
    }
);
layer_int_prop_scene!(
    layer_left_get,
    layer_left_set,
    |l: &LayerState| i64::from(l.rect.x),
    |scene: &mut Scene, id: u32, v: &Value| {
        let left = value_as_i64(v) as i32;
        let current = scene
            .layer(id)
            .map(|layer| layer.rect.x)
            .ok_or_else(|| "Layer: layer no longer exists".to_string())?;
        // A primary layer refuses a real move to a nonzero left; writing
        // the value the layer already holds stores nothing and raises
        // nothing.
        if current != left && scene.is_primary_layer(id) && left != 0 {
            return Err("Layer: Cannot move primary layer".to_string());
        }
        match scene.update_layer(id, |layer| {
            if layer.rect.x == left {
                return false;
            }
            layer.rect.x = left;
            true
        }) {
            Some(_) => Ok(()),
            None => Err("Layer: layer no longer exists".to_string()),
        }
    }
);
layer_int_prop_scene!(
    layer_top_get,
    layer_top_set,
    |l: &LayerState| i64::from(l.rect.y),
    |scene: &mut Scene, id: u32, v: &Value| {
        let top = value_as_i64(v) as i32;
        let current = scene
            .layer(id)
            .map(|layer| layer.rect.y)
            .ok_or_else(|| "Layer: layer no longer exists".to_string())?;
        // A primary layer refuses a real move to a nonzero top; writing
        // the value the layer already holds stores nothing and raises
        // nothing.
        if current != top && scene.is_primary_layer(id) && top != 0 {
            return Err("Layer: Cannot move primary layer".to_string());
        }
        match scene.update_layer(id, |layer| {
            if layer.rect.y == top {
                return false;
            }
            layer.rect.y = top;
            true
        }) {
            Some(_) => Ok(()),
            None => Err("Layer: layer no longer exists".to_string()),
        }
    }
);
layer_int_prop_scene!(
    layer_width_get,
    layer_width_set,
    |l: &LayerState| i64::from(l.rect.w),
    |scene: &mut Scene, id: u32, v: &Value| {
        let width = value_as_i64(v).max(0) as u32;
        match scene.update_layer(id, |layer| {
            if layer.rect.w == width {
                return false;
            }
            layer.rect.w = width;
            true
        }) {
            Some(true) => {
                // A changed extent grows the MainImage when it no longer
                // covers the rectangle and shifts the image offset so the
                // image still covers it; an imageless layer runs nothing.
                scene.layer_size_changed(id);
                Ok(())
            }
            Some(false) => Ok(()),
            None => Err("Layer: layer no longer exists".to_string()),
        }
    }
);
layer_int_prop_scene!(
    layer_height_get,
    layer_height_set,
    |l: &LayerState| i64::from(l.rect.h),
    |scene: &mut Scene, id: u32, v: &Value| {
        let height = value_as_i64(v).max(0) as u32;
        match scene.update_layer(id, |layer| {
            if layer.rect.h == height {
                return false;
            }
            layer.rect.h = height;
            true
        }) {
            Some(true) => {
                // A changed extent grows the MainImage when it no longer
                // covers the rectangle and shifts the image offset so the
                // image still covers it; an imageless layer runs nothing.
                scene.layer_size_changed(id);
                Ok(())
            }
            Some(false) => Ok(()),
            None => Err("Layer: layer no longer exists".to_string()),
        }
    }
);
/// Read a layer property that needs its parent/sibling context.
fn with_layer_scene_read<R>(
    engine: *mut c_void,
    instance: *mut c_void,
    f: impl FnOnce(&Scene, u32) -> R,
) -> Result<R, String> {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `with_layer_scene_read`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let id = inst
        .id
        .ok_or_else(|| "Layer: this layer is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_ref(&ctx)?;
    if scene.layer(id).is_none() {
        return Err("Layer: layer no longer exists".to_string());
    }
    Ok(f(scene, id))
}

/// `Layer.absolute` getter: the stored absolute index while the parent is in
/// absolute mode, otherwise the sibling position.
extern "C" fn layer_absolute_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match with_layer_scene_read(engine, instance, |scene, id| {
        scene.absolute_order_index(id).unwrap_or(0)
    }) {
        Ok(value) => {
            set_int(out, i64::from(value));
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// `Layer.absolute` setter: a parent is required, the parent enters absolute
// order mode, the sibling list is reordered, and the requested index is
// stored.
layer_scene_set!(layer_absolute_set, |scene, id, value| {
    scene.set_absolute_order_index(id, value_as_i64(value) as i32)
});

/// `Layer.order` getter: the sibling position.
extern "C" fn layer_order_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match with_layer_scene_read(engine, instance, |scene, id| {
        scene.order_index(id).unwrap_or(0)
    }) {
        Ok(value) => {
            set_int(out, i64::from(value));
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// `Layer.order` setter: a parent is required, the parent leaves absolute
// order mode, the index is clamped to the sibling range and the layer is
// moved there.
layer_scene_set!(layer_order_set, |scene, id, value| {
    scene.set_order_index(id, value_as_i64(value) as i32)
});
layer_int_prop!(
    layer_hit_threshold_get,
    layer_hit_threshold_set,
    |l: &LayerState| i64::from(l.hit_threshold),
    |l: &mut LayerState, v: &Value| {
        l.hit_threshold = value_as_i64(v) as i32;
        true
    }
);
layer_int_prop!(
    layer_hit_type_get,
    layer_hit_type_set,
    |l: &LayerState| i64::from(l.hit_type),
    |l: &mut LayerState, v: &Value| {
        l.hit_type = value_as_i64(v) as i32;
        true
    }
);
layer_int_prop!(
    layer_cursor_get,
    layer_cursor_set,
    |l: &LayerState| i64::from(l.cursor),
    |l: &mut LayerState, v: &Value| {
        l.cursor = value_as_i64(v) as i32;
        true
    }
);
layer_int_prop!(
    layer_face_get,
    layer_face_set,
    |l: &LayerState| i64::from(l.face),
    |l: &mut LayerState, v: &Value| {
        l.face = value_as_i64(v) as i32;
        true
    }
);
layer_int_prop!(
    layer_hold_alpha_get,
    layer_hold_alpha_set,
    |l: &LayerState| i64::from(l.hold_alpha),
    |l: &mut LayerState, v: &Value| {
        l.hold_alpha = value_as_bool(v);
        true
    }
);

/// `Layer.imageLeft` — the stored image offset. An imageless layer is not
/// drawable and raises instead.
extern "C" fn layer_image_left_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_image_value_get(
        engine,
        instance,
        out,
        out_error,
        "Layer.imageLeft",
        |layer| i64::from(layer.image_left),
    )
}
layer_scene_set!(
    layer_image_left_set,
    |scene: &mut Scene, id: u32, v: &Value| {
        let left = value_as_i64(v) as i32;
        store_image_offset(
            scene,
            id,
            left,
            "Layer.imageLeft",
            |layer| {
                (
                    layer.bitmap.is_some(),
                    layer.image_left,
                    layer.image_width,
                    layer.rect.w,
                )
            },
            |layer, value| layer.image_left = value,
        )
    }
);

/// `Layer.imageTop`; the vertical counterpart of [`layer_image_left_get`].
extern "C" fn layer_image_top_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_image_value_get(
        engine,
        instance,
        out,
        out_error,
        "Layer.imageTop",
        |layer| i64::from(layer.image_top),
    )
}
layer_scene_set!(
    layer_image_top_set,
    |scene: &mut Scene, id: u32, v: &Value| {
        let top = value_as_i64(v) as i32;
        store_image_offset(
            scene,
            id,
            top,
            "Layer.imageTop",
            |layer| {
                (
                    layer.bitmap.is_some(),
                    layer.image_top,
                    layer.image_height,
                    layer.rect.h,
                )
            },
            |layer, value| layer.image_top = value,
        )
    }
);

/// Store one image offset under the image-offset guard: an imageless layer is
/// not drawable, a positive offset or one that would leave the image short of
/// the layer rectangle is an invalid image position, and an unchanged offset
/// stores nothing. `read` yields the layer's (has image, current offset, image
/// extent, rectangle extent) for the axis, `write` stores the new offset.
fn store_image_offset(
    scene: &mut Scene,
    id: u32,
    offset: i32,
    member: &str,
    read: impl FnOnce(&LayerState) -> (bool, i32, u32, u32),
    write: impl FnOnce(&mut LayerState, i32),
) -> Result<(), String> {
    let Some((has_image, current, image_extent, rect_extent)) = scene.layer(id).map(read) else {
        return Err("Layer: layer no longer exists".to_string());
    };
    if !has_image {
        return Err(format!("{member}: Not drawable layer type"));
    }
    if current == offset {
        return Ok(());
    }
    if !image_offset_allowed(offset, image_extent, rect_extent) {
        return Err(format!("{member}: invalid image position"));
    }
    if let Some(layer) = scene.layer_mut(id) {
        write(layer, offset);
    }
    Ok(())
}

/// Read one main-image value for a `Layer` getter. An imageless layer is not
/// drawable and raises instead; the width, the height and both image offsets
/// behave that way.
fn layer_image_value_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    name: &str,
    value: impl FnOnce(&LayerState) -> i64,
) -> c_int {
    match with_layer_read(engine, instance, |layer| {
        if layer.bitmap.is_none() {
            Err(format!("{name}: Not drawable layer type"))
        } else {
            Ok(value(layer))
        }
    }) {
        Ok(Ok(value)) => {
            set_int(out, value);
            0
        }
        Ok(Err(error)) | Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn layer_image_width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_image_value_get(
        engine,
        instance,
        out,
        out_error,
        "Layer.imageWidth",
        |layer| i64::from(layer.image_width),
    )
}

extern "C" fn layer_image_height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_image_value_get(
        engine,
        instance,
        out,
        out_error,
        "Layer.imageHeight",
        |layer| i64::from(layer.image_height),
    )
}

/// `Layer.imageWidth = width` — the reference `SetImageWidth`
/// (`LayerIntf.cpp:2565-2585`): raise on an imageless layer, treat an
/// unchanged width as a no-op, shrink the layer rect and clamp the image
/// offset, then resize the MainImage to the new width keeping its height.
extern "C" fn layer_image_width_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline passes a valid value slot.
    let value = unsafe { &*value };
    match with_layer_scene(engine, instance, value, |scene, id, v| {
        let width = value_as_i64(v) as u32;
        let Some((has_image, image_width)) = scene
            .layer(id)
            .map(|layer| (layer.bitmap.is_some(), layer.image_width))
        else {
            return Err("Layer: layer no longer exists".to_string());
        };
        if !has_image {
            return Err("Layer.imageWidth: Not drawable layer type".to_string());
        }
        if width == image_width {
            return Ok(());
        }
        // `SetWidth` shrinks the rect and clears the offset, then
        // `ImageLayerSizeChanged` grows the image back to cover the rect.
        let mut rect_changed = false;
        if let Some(layer) = scene.layer_mut(id)
            && (width as i32) < layer.rect.w as i32
        {
            layer.image_left = 0;
            layer.rect.w = width;
            rect_changed = true;
        }
        if rect_changed {
            scene.layer_size_changed(id);
        }
        // The clamp reads the rect width the shrink above left behind.
        let Some(rect_w) = scene.layer(id).map(|layer| layer.rect.w) else {
            return Err("Layer: layer no longer exists".to_string());
        };
        if let Some(layer) = scene.layer_mut(id)
            && (width.wrapping_add(layer.image_left as u32) as i32) < rect_w as i32
        {
            layer.image_left = rect_w.wrapping_sub(width) as i32;
        }
        let Some(image_height) = scene.layer(id).map(|layer| layer.image_height) else {
            return Err("Layer: layer no longer exists".to_string());
        };
        if width == 0 {
            return Err("Layer.imageWidth: Cannot create empty layer image".to_string());
        }
        if !scene.set_layer_image_size(id, width, image_height) {
            return Err("Layer.imageWidth: could not resize the layer image".to_string());
        }
        Ok(())
    }) {
        Ok(()) => 0,
        Err(error) => report_error(out_error, &error),
    }
}

/// `Layer.imageHeight = height` — the reference `SetImageHeight`
/// (`LayerIntf.cpp:2594-2614`): raise on an imageless layer, treat an
/// unchanged height as a no-op, shrink the layer rect and clamp the image
/// offset, then resize the MainImage to the new height keeping its width.
extern "C" fn layer_image_height_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline passes a valid value slot.
    let value = unsafe { &*value };
    match with_layer_scene(engine, instance, value, |scene, id, v| {
        let height = value_as_i64(v) as u32;
        let Some((has_image, image_height)) = scene
            .layer(id)
            .map(|layer| (layer.bitmap.is_some(), layer.image_height))
        else {
            return Err("Layer: layer no longer exists".to_string());
        };
        if !has_image {
            return Err("Layer.imageHeight: Not drawable layer type".to_string());
        }
        if height == image_height {
            return Ok(());
        }
        // `SetHeight` shrinks the rect and clears the offset, then
        // `ImageLayerSizeChanged` grows the image back to cover the rect.
        let mut rect_changed = false;
        if let Some(layer) = scene.layer_mut(id)
            && (height as i32) < layer.rect.h as i32
        {
            layer.image_top = 0;
            layer.rect.h = height;
            rect_changed = true;
        }
        if rect_changed {
            scene.layer_size_changed(id);
        }
        // The clamp reads the rect height the shrink above left behind.
        let Some(rect_h) = scene.layer(id).map(|layer| layer.rect.h) else {
            return Err("Layer: layer no longer exists".to_string());
        };
        if let Some(layer) = scene.layer_mut(id)
            && (height.wrapping_add(layer.image_top as u32) as i32) < rect_h as i32
        {
            layer.image_top = rect_h.wrapping_sub(height) as i32;
        }
        let Some(image_width) = scene.layer(id).map(|layer| layer.image_width) else {
            return Err("Layer: layer no longer exists".to_string());
        };
        if height == 0 {
            return Err("Layer.imageHeight: Cannot create empty layer image".to_string());
        }
        if !scene.set_layer_image_size(id, image_width, height) {
            return Err("Layer.imageHeight: could not resize the layer image".to_string());
        }
        Ok(())
    }) {
        Ok(()) => 0,
        Err(error) => report_error(out_error, &error),
    }
}
layer_blur_off_prop!(
    layer_focusable_get,
    layer_focusable_set,
    |l: &LayerState| i64::from(l.focusable),
    |l: &mut LayerState, v: &Value| {
        let focusable = value_as_bool(v);
        if l.focusable == focusable {
            return false;
        }
        l.focusable = focusable;
        true
    },
    |l: &LayerState| !l.focusable
);
layer_int_prop!(
    layer_join_focus_chain_get,
    layer_join_focus_chain_set,
    |l: &LayerState| i64::from(l.join_focus_chain),
    |l: &mut LayerState, v: &Value| {
        l.join_focus_chain = value_as_bool(v);
        true
    }
);
layer_blur_off_prop!(
    layer_enabled_get,
    layer_enabled_set,
    |l: &LayerState| i64::from(l.enabled),
    |l: &mut LayerState, v: &Value| {
        let enabled = value_as_bool(v);
        if l.enabled == enabled {
            return false;
        }
        l.enabled = enabled;
        true
    },
    |l: &LayerState| !l.enabled
);
layer_int_prop_scene!(
    layer_cached_get,
    layer_cached_set,
    |l: &LayerState| i64::from(l.cached),
    |scene: &mut Scene, id: u32, v: &Value| {
        let cached = value_as_bool(v);
        let current = scene
            .layer(id)
            .map(|layer| layer.cached)
            .ok_or_else(|| "Layer: layer no longer exists".to_string())?;
        if current == cached {
            return Ok(());
        }
        if let Some(layer) = scene.layer_mut(id) {
            layer.cached = cached;
        }
        // A cache consumer is registered while the flag is on and released
        // when it goes off; the last release frees the rendered cache.
        if cached {
            scene.inc_cache_enabled_count(id);
        } else {
            scene.dec_cache_enabled_count(id);
        }
        Ok(())
    }
);
layer_int_prop!(
    layer_image_modified_get,
    layer_image_modified_set,
    |l: &LayerState| i64::from(l.image_modified),
    |l: &mut LayerState, v: &Value| {
        l.image_modified = value_as_bool(v);
        true
    }
);
layer_int_prop!(
    layer_attention_left_get,
    layer_attention_left_set,
    |l: &LayerState| i64::from(l.attention_left),
    |l: &mut LayerState, v: &Value| {
        l.attention_left = value_as_i64(v) as i32;
        true
    }
);
layer_int_prop!(
    layer_attention_top_get,
    layer_attention_top_set,
    |l: &LayerState| i64::from(l.attention_top),
    |l: &mut LayerState, v: &Value| {
        l.attention_top = value_as_i64(v) as i32;
        true
    }
);
layer_int_prop!(
    layer_use_attention_get,
    layer_use_attention_set,
    |l: &LayerState| i64::from(l.use_attention),
    |l: &mut LayerState, v: &Value| {
        l.use_attention = value_as_bool(v);
        true
    }
);
layer_int_prop!(
    layer_show_parent_hint_get,
    layer_show_parent_hint_set,
    |l: &LayerState| i64::from(l.show_parent_hint),
    |l: &mut LayerState, v: &Value| {
        l.show_parent_hint = value_as_bool(v);
        true
    }
);
layer_int_prop!(
    layer_ignore_hint_sensing_get,
    layer_ignore_hint_sensing_set,
    |l: &LayerState| i64::from(l.ignore_hint_sensing),
    |l: &mut LayerState, v: &Value| {
        l.ignore_hint_sensing = value_as_bool(v);
        true
    }
);
layer_int_prop!(
    layer_ime_mode_get,
    layer_ime_mode_set,
    |l: &LayerState| i64::from(l.ime_mode),
    |l: &mut LayerState, v: &Value| {
        l.ime_mode = value_as_i64(v) as i32;
        true
    }
);
layer_int_prop!(
    layer_neutral_color_get,
    layer_neutral_color_set,
    |l: &LayerState| l.neutral_color,
    |l: &mut LayerState, v: &Value| {
        l.neutral_color = value_as_i64(v);
        true
    }
);
layer_int_get!(layer_absolute_order_mode_get, |l: &LayerState| i64::from(
    l.absolute_order_mode
));
// `Layer.absoluteOrderMode` setter: enabling the mode seeds each child's
// absolute index from its current sibling position.
layer_scene_set!(layer_absolute_order_mode_set, |scene, id, value| {
    scene.set_absolute_order_mode(id, value_as_bool(value));
    Ok(())
});
layer_int_prop!(
    layer_call_on_paint_get,
    layer_call_on_paint_set,
    |l: &LayerState| i64::from(l.call_on_paint),
    |l: &mut LayerState, v: &Value| {
        l.call_on_paint = value_as_bool(v);
        true
    }
);
layer_int_prop_scene!(
    layer_clip_left_get,
    layer_clip_left_set,
    |l: &LayerState| i64::from(effective_clip(l).x),
    |scene: &mut Scene, id: u32, v: &Value| {
        let left = value_as_i64(v) as i32;
        let clip = current_clip(scene, id)?;
        set_clip_rect(
            scene,
            id,
            i64::from(left),
            i64::from(clip.y),
            i64::from(clip.w),
            i64::from(clip.h),
        )
        .map_err(|error| format!("Layer: {error}"))
    }
);
layer_int_prop_scene!(
    layer_clip_top_get,
    layer_clip_top_set,
    |l: &LayerState| i64::from(effective_clip(l).y),
    |scene: &mut Scene, id: u32, v: &Value| {
        let top = value_as_i64(v) as i32;
        let clip = current_clip(scene, id)?;
        set_clip_rect(
            scene,
            id,
            i64::from(clip.x),
            i64::from(top),
            i64::from(clip.w),
            i64::from(clip.h),
        )
        .map_err(|error| format!("Layer: {error}"))
    }
);
layer_int_prop_scene!(
    layer_clip_width_get,
    layer_clip_width_set,
    |l: &LayerState| i64::from(effective_clip(l).w),
    |scene: &mut Scene, id: u32, v: &Value| {
        let width = value_as_i64(v) as i32;
        let clip = current_clip(scene, id)?;
        set_clip_rect(
            scene,
            id,
            i64::from(clip.x),
            i64::from(clip.y),
            i64::from(width),
            i64::from(clip.h),
        )
        .map_err(|error| format!("Layer: {error}"))
    }
);
layer_int_prop_scene!(
    layer_clip_height_get,
    layer_clip_height_set,
    |l: &LayerState| i64::from(effective_clip(l).h),
    |scene: &mut Scene, id: u32, v: &Value| {
        let height = value_as_i64(v) as i32;
        let clip = current_clip(scene, id)?;
        set_clip_rect(
            scene,
            id,
            i64::from(clip.x),
            i64::from(clip.y),
            i64::from(clip.w),
            i64::from(height),
        )
        .map_err(|error| format!("Layer: {error}"))
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_cursor_axis`.
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

/// `layer.cursorX = x` — the reference `SetCursorX` stores `CursorX_Work` for
/// the next `SetCursorY` (`reference/cpp/core/visual/LayerIntf.cpp:3108-3112`).
extern "C" fn layer_cursor_x_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline passes a valid value slot.
    let x = value_as_i64(unsafe { &*value }) as i32;
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_cursor_x_set`.
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    inst.cursor_x_work = x;
    0
}

/// `layer.cursorY = y` — the reference `SetCursorY` warps the pointer to the
/// pending `cursorX` and `y`, adding this layer's and every *parented*
/// ancestor's rect offset and stopping before the parentless root
/// (`reference/cpp/core/visual/LayerIntf.cpp:3132-3145`).
extern "C" fn layer_cursor_y_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline passes a valid value slot.
    let y = value_as_i64(unsafe { &*value }) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.cursorY: compat context is not bound");
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_cursor_y_set`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.cursorY: this layer is not constructed");
    };
    let x = inst.cursor_x_work;
    let (x, y) = {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        let mut x = x;
        let mut y = y;
        let mut current = Some(id);
        while let Some(layer_id) = current {
            let Some(layer) = scene.layer(layer_id) else {
                break;
            };
            // The reference adds `p->Rect` only when `p->Parent` exists and
            // stops before the root (`LayerIntf.cpp:3154-3160`).
            let Some(parent) = layer.parent else {
                break;
            };
            x = x.wrapping_add(layer.rect.x);
            y = y.wrapping_add(layer.rect.y);
            current = Some(parent);
        }
        (x, y)
    };
    let input = match util::input_mut(&ctx) {
        Ok(input) => input,
        Err(error) => return report_error(out_error, &format!("Layer.cursorY: {error}")),
    };
    input.warp_pointer(x as f32, y as f32);
    0
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
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let value = unsafe { &*value };
    match with_layer_write(engine, instance, value, |l, v| {
        let opacity = (value_as_i64(v) as f64 / 255.0).clamp(0.0, 1.0) as f32;
        if l.opacity == opacity {
            return false;
        }
        l.opacity = opacity;
        true
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
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let wanted = value_as_bool(unsafe { &*value });
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_has_image_set`.
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
        if !scene.ensure_layer_image(id, w, h) {
            return report_error(
                out_error,
                "Layer.hasImage: could not allocate the main image",
            );
        }
        if let Some(layer) = scene.layer_mut(id) {
            layer.clip = None;
        }
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_main_image_buffer_get`.
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
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(bitmap) = scene.layer(id).and_then(|layer| layer.bitmap) {
        scene.resolve_bitmap(bitmap);
    }
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_main_image_buffer_for_write_get`.
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
    if let Some(bitmap) = scene.layer(id).and_then(|layer| layer.bitmap) {
        scene.resolve_bitmap(bitmap);
    }
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_main_image_buffer_pitch_get`.
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_province_image_buffer_get`.
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_province_image_buffer_for_write_get`.
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_province_image_buffer_pitch_get`.
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_node_flag`.
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

/// `layer.focused` — whether the layer holds the focus of its own focus
/// domain.
extern "C" fn layer_focused_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_focused_get`.
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
    // Reference `GetFocused` compares the layer against its **own
    // manager's** focus (`reference/cpp/core/visual/LayerIntf.cpp:3569-3572`),
    // not the window-bound one.
    let focused = scene
        .layer(id)
        .and_then(|layer| scene.focused_in(layer.window, layer.domain))
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_is_primary_get`.
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
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let text = value_as_string(unsafe { &*value });
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    match with_layer_write(engine, instance, unsafe { &*value }, |l, _| {
        l.name = text;
        true
    }) {
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
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let text = value_as_string(unsafe { &*value });
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    match with_layer_write(engine, instance, unsafe { &*value }, |l, _| {
        l.hint = text;
        true
    }) {
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_children_get`.
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_focusable_neighbor`.
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; `layer_font_get` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    // Reference `tTJSNI_BaseLayer::GetFontObjectNoAddRef` raises until the
    // native constructor has joined the layer; the id alone is no longer
    // enough once it is allocated at creation.
    if !inst.attached {
        return report_error(out_error, "Layer.font: this layer is not constructed");
    }
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
    if let Err(error) = super::font::bind_font_object(host, ctx, wrapper.raw_id(), font_id) {
        return report_error(out_error, &format!("Layer.font: {error}"));
    }
    inst.font_object = Some(wrapper);
    set_object_out(host, out, inst.font_object.as_ref());
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
        invalidate: Some(layer_invalidate),
        methods,
        properties: vec![
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
                get: Some(layer_order_get),
                set: Some(layer_order_set),
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
                set: Some(layer_cursor_x_set),
            },
            NativeInstancePropertyDef {
                name: "cursorY",
                get: Some(layer_cursor_y_get),
                set: Some(layer_cursor_y_set),
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
                set: Some(layer_image_width_set),
            },
            NativeInstancePropertyDef {
                name: "imageHeight",
                get: Some(layer_image_height_get),
                set: Some(layer_image_height_set),
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
