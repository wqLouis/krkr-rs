//! `Layer` focus / capture members (group 4).
//!
//! Reference: `tTJSNI_BaseLayer::FocusNext` / `FocusPrev` /
//! `SetAttentionPoint` / `SetCursorPos` / `ReleaseCapture` /
//! `ReleaseTouchCapture` in `reference/cpp/core/visual/LayerIntf.cpp`.
//!
//! The focus search runs over the L2 [`engine::Scene`] focusable-node chain
//! ([`Scene::next_focusable`] / [`Scene::prev_focusable`]). `setCursorPos`
//! records a pointer warp on the attached L2 [`engine::input::InputState`].
//! The two capture-release methods have no L2 capture model yet, so they queue
//! a request in [`LayerExtras`](super::LayerExtras) for the layer-tree owner
//! to drain — explicit state, never a silent no-op.

use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeInstanceMethodDef, Value};

use super::super::common::{ObjectKind, duplicate_object, scene_mut, scene_ref, set_object_out};
use super::util::input_mut;
use super::{LayerExtras, LayerInst, bound};
use crate::CompatCtx;
use crate::native_util::{args, report_error, set_void};
use crate::natives::system::value_as_i64;

/// Shared body of `focusNext` / `focusPrev`.
fn focus_neighbor(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    method: &str,
    forward: bool,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            &format!("Layer.{method}: this layer is not constructed"),
        );
    };
    let Ok((host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            &format!("Layer.{method}: compat context is not bound"),
        );
    };
    let target = {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        if forward {
            scene.next_focusable(id)
        } else {
            scene.prev_focusable(id)
        }
    };
    let target = match target {
        Some(target) => {
            let scene = match scene_mut(&ctx) {
                Ok(scene) => scene,
                Err(error) => return report_error(out_error, &error),
            };
            scene.set_focus(target);
            Some(target)
        }
        None => None,
    };
    let object =
        target.and_then(|target| duplicate_object(&ctx, host.engine(), ObjectKind::Layer, target));
    set_object_out(host.engine(), out, object.as_ref());
    0
}

/// `focusNext()` → the newly focused `Layer` object (or `null`).
extern "C" fn layer_focus_next(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    focus_neighbor(engine, instance, out, out_error, "focusNext", true)
}

/// `focusPrev()` → the newly focused `Layer` object (or `null`).
extern "C" fn layer_focus_prev(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    focus_neighbor(engine, instance, out, out_error, "focusPrev", false)
}

/// `setAttentionPos(left, top)`.
extern "C" fn layer_set_attention_pos(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setAttentionPos requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setAttentionPos: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.setAttentionPos: compat context is not bound",
        );
    };
    let left = value_as_i64(&a[0]) as i32;
    let top = value_as_i64(&a[1]) as i32;
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.attention_left = left;
        layer.attention_top = top;
    }
    set_void(out);
    0
}

/// `setCursorPos(x, y)` — request a pointer warp on the L2 input state.
extern "C" fn layer_set_cursor_pos(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setCursorPos requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &*(instance as *const LayerInst) };
    if inst.id.is_none() {
        return report_error(
            out_error,
            "Layer.setCursorPos: this layer is not constructed",
        );
    }
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setCursorPos: compat context is not bound");
    };
    let x = value_as_i64(&a[0]) as f32;
    let y = value_as_i64(&a[1]) as f32;
    let input = match input_mut(&ctx) {
        Ok(input) => input,
        Err(error) => return report_error(out_error, &error),
    };
    input.warp_pointer(x, y);
    set_void(out);
    0
}

/// `releaseCapture()` — queue a mouse-capture release for the tree owner.
extern "C" fn layer_release_capture(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.releaseCapture: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.releaseCapture: compat context is not bound",
        );
    };
    push_capture(&ctx, id, None);
    set_void(out);
    0
}

/// `releaseTouchCapture([id])` — queue a touch-capture release.
extern "C" fn layer_release_touch_capture(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.releaseTouchCapture: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.releaseTouchCapture: compat context is not bound",
        );
    };
    let contact =
        (!a.is_empty() && a[0].ty != tjs::VAL_VOID).then(|| value_as_i64(&a[0]).max(0) as u64);
    push_capture(&ctx, id, contact);
    set_void(out);
    0
}

fn push_capture(ctx: &CompatCtx, id: u32, contact: Option<u64>) {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<LayerExtras>();
    match contact {
        Some(contact) => state.release_touch_capture.push((id, Some(contact))),
        None => state.release_touch_capture.push((id, None)),
    }
}

/// Drain the queued capture-release requests (the layer-tree owner calls
/// this). Returns `(mouse_releases, touch_releases)`.
#[allow(dead_code)]
pub(crate) fn take_capture_releases(ctx: &CompatCtx) -> (Vec<u32>, Vec<(u32, Option<u64>)>) {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<LayerExtras>();
    (
        std::mem::take(&mut state.release_capture),
        std::mem::take(&mut state.release_touch_capture),
    )
}

fn d(name: &'static str, f: tjs::NativeInstanceMethodFn) -> NativeInstanceMethodDef {
    NativeInstanceMethodDef { name, f }
}

/// Every group-4 member as a `NativeInstanceMethodDef`.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        d("focusNext", layer_focus_next),
        d("focusPrev", layer_focus_prev),
        d("setAttentionPos", layer_set_attention_pos),
        d("setCursorPos", layer_set_cursor_pos),
        d("releaseCapture", layer_release_capture),
        d("releaseTouchCapture", layer_release_touch_capture),
    ]
}
