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
//! a request in [`LayerExtras`](super::LayerExtras) for the frame body to
//! drain — explicit state, never a silent no-op.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use engine::Scene;
use tjs::{NativeInstanceMethodDef, Value};

use super::super::common::{ObjectKind, duplicate_object, scene_mut, scene_ref, set_object_out};
use super::util::input_mut;
use super::{LayerExtras, LayerInst, bound};
use crate::CompatCtx;
use crate::native_util::{args, report_error, set_void};
use crate::natives::system::value_as_i64;

/// The first focusable layer of `window` in paint order — the port of the
/// reference `tTJSNI_BaseLayer::SearchFirstFocusable` walk
/// (`reference/cpp/core/visual/LayerIntf.cpp:3574-3593`): test the node
/// itself (`GetNodeFocusable() && JoinFocusChain`), then recurse into the
/// children in order. `Scene::window_layer_order` is the same DFS pre-order
/// the reference walk produces, so one pass finds the identical node.
fn first_focusable(scene: &Scene, window: u32) -> Option<u32> {
    scene
        .window_layer_order(window)
        .into_iter()
        .find(|&id| scene.node_focusable(id) && scene.layer(id).is_some_and(|l| l.join_focus_chain))
}

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
        let Some(layer) = scene.layer(id) else {
            return report_error(
                out_error,
                &format!("Layer.{method}: this layer no longer exists"),
            );
        };
        let window = layer.window;
        // Reference `tTVPLayerManager::FocusNext` / `FocusPrev`
        // (`reference/cpp/core/visual/LayerManager.cpp:723-747`): the search
        // starts from the *manager's* focused layer — never from the
        // receiver — and when nothing is focused it starts from the first
        // focusable layer of the primary chain (`SearchFirstFocusable`),
        // which the receiver-independent path below reproduces.
        let focused = scene.window(window).and_then(|w| w.focused_layer);
        match focused {
            Some(focused) if scene.layer(focused).is_some() => {
                if forward {
                    scene.next_focusable(focused)
                } else {
                    scene.prev_focusable(focused)
                }
            }
            _ => first_focusable(scene, window),
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
///
/// Reference `tTJSNI_BaseLayer::SetCursorPos`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3150-3163`): the arguments are
/// **layer-local**; the layer's and every *parented* ancestor's offset is
/// accumulated up the parent chain (the chain's root contributes nothing),
/// and only the resulting window coordinate is handed to the manager.
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
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setCursorPos: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setCursorPos: compat context is not bound");
    };
    let mut x = value_as_i64(&a[0]) as i32;
    let mut y = value_as_i64(&a[1]) as i32;
    {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
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
    }
    let input = match input_mut(&ctx) {
        Ok(input) => input,
        Err(error) => return report_error(out_error, &error),
    };
    input.warp_pointer(x as f32, y as f32);
    set_void(out);
    0
}

/// `releaseCapture()` — queue a mouse-capture release for the tree owner.
///
/// Reference `tTJSNI_BaseLayer::ReleaseCapture`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3467-3472`): the manager
/// releases the mouse capture globally, ignoring which layer captured. The
/// port queues one request on the mouse queue (`LayerExtras::release_capture`).
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
    {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let mut state = unsafe { ctx.services() }.state::<LayerExtras>();
        state.release_capture.push(id);
    }
    // The release drops the adapter's mouse-capture state as well as queuing
    // the tree-owner notification.
    crate::event::release_mouse_capture(&ctx);
    set_void(out);
    0
}

/// `releaseTouchCapture([id])` — queue a touch-capture release.
///
/// Reference native (`reference/cpp/core/visual/LayerIntf.cpp:9710-9721`):
/// with **no** argument the manager releases *every* touch capture
/// (`ReleaseTouchCapture(0, true)` → `ReleaseTouchCaptureAll`); with one
/// argument the id is `static_cast<tjs_uint32>(param[0]->AsInteger())` — a
/// present but `void` argument therefore selects contact `0`, and the value
/// truncates to 32 bits. `None` in the queue models "all contacts".
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
    let contact = if argc >= 1 {
        Some(value_as_i64(&a[0]) as u64 & 0xffff_ffff)
    } else {
        None
    };
    push_capture(&ctx, id, contact);
    set_void(out);
    0
}

fn push_capture(ctx: &CompatCtx, id: u32, contact: Option<u64>) {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<LayerExtras>();
    state.release_touch_capture.push((id, contact));
}

/// Drain the queued capture-release requests (the frame body's
/// `project_and_render` step calls this). Returns `(mouse_releases,
/// touch_releases)`.
pub fn take_capture_releases(ctx: &CompatCtx) -> (Vec<u32>, Vec<(u32, Option<u64>)>) {
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
