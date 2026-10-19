//! `Layer` focus / capture members (group 4).
//!
//! Reference: `tTJSNI_BaseLayer::FocusNext` / `FocusPrev` /
//! `SetAttentionPoint` / `SetCursorPos` / `ReleaseCapture` /
//! `ReleaseTouchCapture` in `reference/cpp/core/visual/LayerIntf.cpp`.
//!
//! Focus is per **domain** (`engine_api::LayerState::domain`): the reference
//! keeps one `tTVPLayerManager` per parentless-root tree and every focus
//! operation stays inside one manager
//! (`reference/cpp/core/visual/LayerIntf.cpp:449-473`, `LayerManager.h:483-490`).
//! The focus search runs over the L2 `engine_api::Scene` domain chain
//! ([`Scene::next_focusable`] / [`Scene::prev_focusable`]). Every focus
//! transition goes through [`focus_layer`] / [`clear_layer_focus`], the port
//! of `tTVPLayerManager::SetFocusTo`'s guard, state move and synchronous
//! `onBlur`-then-`onFocus` pair; [`blur_tree`] is the port of
//! `tTVPLayerManager::BlurTree` for the setters and teardown that make the
//! focused subtree unusable. `setCursorPos` records a pointer warp on the
//! attached L2 `engine_api::InputState`. The two capture-release
//! methods have no L2 capture model yet, so they queue a request in
//! [`LayerExtras`](super::LayerExtras) for the frame body to drain —
//! explicit state, never a silent no-op.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::Scene;
use tjs::{NativeInstanceMethodDef, Tjs2Engine, TjsValue, Value};

use super::super::common::{ObjectKind, duplicate_object, scene_mut, scene_ref, set_object_out};
use super::util::input_mut;
use super::{LayerExtras, LayerInst, bound};
use crate::CompatCtx;
use crate::native_util::{args, report_error, set_void};
use crate::natives::system::value_as_i64;

/// Give `id` keyboard focus, in its own domain, and fire the focus events,
/// or return without any effect when the layer is not node-focusable.
///
/// Reference `tTJSNI_BaseLayer::SetFocus` → `tTVPLayerManager::SetFocusTo`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3561-3564`,
/// `LayerManager.cpp:760-817`): the target's own manager moves, and
/// `GetNodeFocusable()` is rejected before any state changes (`:766-767`)
/// — `Focusable && Visible && Enabled` plus every ancestor visible and
/// enabled (`reference/cpp/core/visual/LayerIntf.h:663-667`), modelled by
/// [`Scene::node_focusable`]. Returns whether focus moved.
pub(crate) fn focus_layer(ctx: &CompatCtx, id: u32, direction: bool) -> bool {
    let (window, domain) = {
        let scene = match scene_ref(ctx) {
            Ok(scene) => scene,
            Err(error) => {
                log::warn!("focus: no scene attached ({error})");
                return false;
            }
        };
        let Some(layer) = scene.layer(id) else {
            return false;
        };
        (layer.window, layer.domain)
    };
    set_focus_to(ctx, window, domain, Some(id), direction)
}

/// Clear `window`'s **input-domain** focus and fire `onBlur` on the
/// outgoing layer, with `direction` as in [`focus_layer`]. This is the
/// `Window.focusedLayer = null` / tree-owner shape, which the reference
/// routes to the window's first registered manager
/// (`reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:385-389`); a window
/// without an input root is a no-op, like the reference's null
/// `GetLayerManagerAt(0)`. Returns whether focus moved.
pub(crate) fn clear_layer_focus(ctx: &CompatCtx, window: u32, direction: bool) -> bool {
    let Some(domain) = scene_ref(ctx)
        .ok()
        .and_then(|scene| scene.window_input_root(window))
    else {
        return false;
    };
    set_focus_to(ctx, window, domain, None, direction)
}

/// Clear `window`'s `domain` focus and fire `onBlur` on the outgoing
/// layer. The `SetFocusTo(nullptr, direction)` tail
/// (`reference/cpp/core/visual/LayerManager.cpp:704-708`, `:876-883`) for
/// callers that operate on a layer's own domain, not the window-bound one.
pub(crate) fn clear_domain_focus(
    ctx: &CompatCtx,
    window: u32,
    domain: u32,
    direction: bool,
) -> bool {
    set_focus_to(ctx, window, domain, None, direction)
}

/// `Window.focusedLayer = layer` / the tree-owner setter shape: run the
/// `SetFocusTo(layer, direction)` sequence in `window`'s **input domain**,
/// regardless of the layer's own domain — the reference setter moves the
/// first registered manager's `FocusedLayer` and never checks membership
/// (`reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:385-389`,
/// `LayerManager.h:488-490`, `LayerManager.cpp:760-767`). A window without
/// an input root is a no-op. Returns whether focus moved.
pub(crate) fn focus_layer_in_input_domain(
    ctx: &CompatCtx,
    window: u32,
    layer: u32,
    direction: bool,
) -> bool {
    let Some(domain) = scene_ref(ctx)
        .ok()
        .and_then(|scene| scene.window_input_root(window))
    else {
        return false;
    };
    set_focus_to(ctx, window, domain, Some(layer), direction)
}

/// Port of the `SetFocusTo` body (`LayerManager.cpp:760-790`): reject a
/// non-node-focusable target (`:766-767`), move the focus state of
/// `window`'s `domain` first, then fire `onBlur` on the outgoing layer
/// with the newly focused layer (or `null`) and then `onFocus` on the
/// incoming layer with the previously focused layer (or `null`) and the
/// direction. Either event is skipped while its layer is `Shutdown`
/// (`LayerManager.cpp:786-790`), so an invalidated layer that still holds
/// focus is cleared silently. The event arguments are the layers'
/// registered script objects, the `prevfocused->Owner` the reference posts
/// (`reference/cpp/core/visual/LayerIntf.cpp:3702-3727`); both handlers
/// are delivered synchronously, the reference `TVP_EPT_IMMEDIATE` shape
/// (`reference/cpp/core/base/EventIntf.cpp:215-233`).
fn set_focus_to(
    ctx: &CompatCtx,
    window: u32,
    domain: u32,
    target: Option<u32>,
    direction: bool,
) -> bool {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let host = unsafe { ctx.engine() };
    let prev = {
        let scene = match scene_ref(ctx) {
            Ok(scene) => scene,
            Err(error) => {
                log::warn!("focus: no scene attached ({error})");
                return false;
            }
        };
        // `if(layer && !layer->GetNodeFocusable()) return false;`
        // (`LayerManager.cpp:766-767`).
        if let Some(target) = target
            && !scene.node_focusable(target)
        {
            return false;
        }
        let prev = scene
            .focused_in(window, domain)
            .filter(|&id| scene.layer(id).is_some());
        // `if(FocusedLayer == layer) return false;` — no state change and
        // no events (`LayerManager.cpp:779-780`).
        if prev == target {
            return false;
        }
        prev
    };
    {
        let scene = match scene_mut(ctx) {
            Ok(scene) => scene,
            Err(error) => {
                log::warn!("focus: no scene attached ({error})");
                return false;
            }
        };
        scene.set_domain_focus(window, domain, target);
    }
    // `FocusedLayer = layer` precedes the events (`LayerManager.cpp:783-784`),
    // so an `onBlur` handler already reads the new focus; `org->FireBlur(layer)`
    // runs before `FocusedLayer->FireFocus(org, direction)` (`:786-790`) —
    // `onBlur` always precedes `onFocus`, and each fires only while its layer
    // still has an owner (`reference/cpp/core/visual/LayerIntf.cpp:3702`,
    // `:3715`). Both ends are re-checked for `Shutdown` at their own fire
    // site (`LayerManager.cpp:786-790`): a handler that invalidates the
    // other end silences that end's event.
    if let Some(prev) = prev
        && fires_focus_events(ctx, prev)
    {
        fire_focus_event(ctx, host, prev, "onBlur", target);
    }
    if let Some(target) = target
        && fires_focus_events(ctx, target)
    {
        fire_on_focus(ctx, host, target, prev, direction);
    }
    true
}

/// `!layer->Shutdown` evaluated where the event fires
/// (`reference/cpp/core/visual/LayerManager.cpp:786-790`, the re-guards in
/// `FireBlur`/`FireFocus` at `reference/cpp/core/visual/LayerIntf.cpp:3703`,
/// `:3717`): a layer marked by `Invalidate()` (`:516`) fires no focus event
/// in either direction. A record that is already gone was invalidated too.
fn fires_focus_events(ctx: &CompatCtx, id: u32) -> bool {
    match scene_ref(ctx) {
        Ok(scene) => scene.layer(id).is_some_and(|l| !l.shutdown),
        Err(_) => false,
    }
}

/// `FireBlur(newly_focused)`: one argument, the layer that is now focused
/// or `null` (`reference/cpp/core/visual/LayerIntf.cpp:3702-3713`; the
/// native member is named `"focused"` at `:10199-10200`).
fn fire_focus_event(
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    layer: u32,
    name: &'static str,
    focused: Option<u32>,
) {
    let Some(arg) = layer_arg(ctx, host, focused) else {
        return;
    };
    deliver(ctx, host, layer, name, &[arg]);
}

/// `FireFocus(previously_focused, direction)`: the previously focused
/// layer or `null`, then the direction as 0/1
/// (`reference/cpp/core/visual/LayerIntf.cpp:3715-3727`; the native
/// members are named `"blurred"`/`"direction"` at `:10215-10216`).
fn fire_on_focus(
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    layer: u32,
    blurred: Option<u32>,
    direction: bool,
) {
    let Some(arg) = layer_arg(ctx, host, blurred) else {
        return;
    };
    deliver(
        ctx,
        host,
        layer,
        "onFocus",
        &[arg, TjsValue::Integer(i64::from(direction))],
    );
}

/// The event argument for a neighbouring layer: its registered script
/// object as a consumed retention (the `TjsValue::Retained` argument
/// path), or TJS `null` evaluated and retained the same way the `null`
/// result path does it (`set_null_out` in
/// `crates/compat/src/natives/visual/common.rs`). `None` on failure, after
/// logging — the reference fires these events only while the layer has an
/// owner (`reference/cpp/core/visual/LayerIntf.cpp:3702`, `:3715`).
fn layer_arg(ctx: &CompatCtx, host: &Tjs2Engine, layer: Option<u32>) -> Option<TjsValue> {
    match layer {
        Some(id) => {
            let Some(object) = duplicate_object(ctx, host, ObjectKind::Layer, id) else {
                log::warn!("focus: layer {id} has no script object for the event argument");
                return None;
            };
            let raw = object.raw_id().as_ptr() as u64;
            // The C++ conversion consumes the retention; the
            // `DetachedValue` drop is a safe no-op.
            std::mem::forget(object);
            Some(TjsValue::Retained(raw))
        }
        None => match host.eval_retained("null", "focus.null") {
            Ok(tjs::RetainedValue::Object(object)) => {
                let raw = object.raw_id().as_ptr() as u64;
                // The C++ conversion consumes the retention; the
                // `DetachedValue` drop is a safe no-op.
                std::mem::forget(object);
                Some(TjsValue::Retained(raw))
            }
            Ok(tjs::RetainedValue::Value(_)) => {
                log::warn!("focus: the null event argument did not retain as an object");
                None
            }
            Err(error) => {
                log::warn!("focus: the null event argument failed ({error})");
                None
            }
        },
    }
}

/// Call `name` on the layer's script object with `args` — synchronously,
/// like the reference's `TVP_EPT_IMMEDIATE` posts
/// (`reference/cpp/core/base/EventIntf.cpp:215-233`). A missing script
/// override lands on the declared native no-op (`events.rs`); a throwing
/// handler is logged and never aborts the caller, the event adapter's
/// convention (`crates/compat/src/event.rs`).
fn deliver(ctx: &CompatCtx, host: &Tjs2Engine, layer: u32, name: &str, args: &[TjsValue]) {
    let Some(object) = duplicate_object(ctx, host, ObjectKind::Layer, layer) else {
        // The reference fires only while the layer has an owner
        // (`reference/cpp/core/visual/LayerIntf.cpp:3702`, `:3715`).
        log::warn!("focus: layer {layer} has no script object for {name}");
        return;
    };
    if let Err(error) = host.call_member(object.raw_id(), name, args) {
        log::warn!("focus: {name} handler failed ({error})");
    }
}

/// Port of `tTVPLayerManager::BlurTree`
/// (`reference/cpp/core/visual/LayerManager.cpp:690-711`): when the
/// focused layer is `root` or below it in the parent chain, move focus to
/// the next focusable layer of the chain after `root` (direction forward)
/// or clear it. A `engine_api::LayerState::shutdown` `root` (reference
/// `tTJSNI_BaseLayer::Shutdown`) always takes the clear: the reference's
/// `GetNextFocusable` search walks `GetNeighborBelow`, which yields
/// nothing once the layer's manager is gone
/// (`reference/cpp/core/visual/LayerIntf.cpp:1261-1262`), and `Invalidate`
/// releases the manager before the blur reaches it
/// (`reference/cpp/core/visual/LayerIntf.cpp:515-516`, `:538-544`). The
/// other callers blur after a state change on a live layer
/// (`SetVisible(false)` `reference/cpp/core/visual/LayerIntf.cpp:1183-1206`,
/// `SetEnabled(false)` `:3801-3832`, `SeverChild` `:657-665`, `NotifyPart`
/// via `LayerManager.cpp:197-201`).
pub(crate) fn blur_tree(ctx: &CompatCtx, root: u32) {
    let (window, domain, focused) = {
        let scene = match scene_ref(ctx) {
            Ok(scene) => scene,
            Err(error) => {
                log::warn!("focus: no scene attached ({error})");
                return;
            }
        };
        let Some(layer) = scene.layer(root) else {
            return;
        };
        // The manager whose `FocusedLayer` can sit at or below `root` is
        // the one `root` belongs to — `NotifyPart`/`SeverChild`/the state
        // setters all reach the layer's own manager
        // (`reference/cpp/core/visual/LayerIntf.cpp:657-665`,
        // `LayerManager.cpp:197-201`).
        let domain = layer.domain;
        // `if(!FocusedLayer) return false;` (`LayerManager.cpp:695-696`).
        let Some(focused) = scene.focused_in(layer.window, domain) else {
            return;
        };
        // `if(!FocusedLayer->IsAncestorOrSelf(root)) return false;`
        // (`LayerManager.cpp:698-700`).
        if !is_ancestor_or_self(scene, root, focused) {
            return;
        }
        (layer.window, domain, focused)
    };
    // `next = root->GetNextFocusable()` (`LayerManager.cpp:702`): the focus
    // chain search after `root` (`reference/cpp/core/visual/LayerIntf.cpp:3631-3647`),
    // modelled by [`Scene::next_focusable`]. The reference search starts at
    // `root` through `GetNeighborBelow`, which returns null without the
    // layer's manager (`reference/cpp/core/visual/LayerIntf.cpp:1261-1262`)
    // — `Invalidate` released it (`:538-541`) — so a shutdown `root` can
    // only produce the clear.
    let next = {
        let scene = match scene_ref(ctx) {
            Ok(scene) => scene,
            Err(error) => {
                log::warn!("focus: no scene attached ({error})");
                return;
            }
        };
        if scene.layer(root).is_some_and(|l| l.shutdown) {
            None
        } else {
            scene.next_focusable(root)
        }
    };
    // `if(next != FocusedLayer) SetFocusTo(next, true); else SetFocusTo(nullptr, true);`
    // (`LayerManager.cpp:704-708`).
    match next {
        Some(next) if next != focused => {
            set_focus_to(ctx, window, domain, Some(next), true);
        }
        _ => {
            set_focus_to(ctx, window, domain, None, true);
        }
    }
}

/// `FocusedLayer->IsAncestorOrSelf(root)` (`LayerManager.cpp:697`): the
/// parent-chain walk from `focused` reaches `root` or starts at it. The
/// walk is bounded by the layer count, like the other parent-chain walks
/// on [`Scene`] (`node_visible`).
fn is_ancestor_or_self(scene: &Scene, root: u32, focused: u32) -> bool {
    let mut current = Some(focused);
    let mut steps = 0usize;
    while let Some(id) = current {
        if id == root {
            return true;
        }
        steps += 1;
        if steps > scene.layers.len() {
            return false;
        }
        current = scene.layer(id).and_then(|l| l.parent);
    }
    false
}

/// The first focusable layer of `window`'s `domain` in paint order — the
/// port of the reference `tTJSNI_BaseLayer::SearchFirstFocusable` walk
/// (`reference/cpp/core/visual/LayerIntf.cpp:3574-3593`): test the node
/// itself (`GetNodeFocusable() && JoinFocusChain`), then recurse into the
/// children in order. `Scene::window_layer_order` is the same DFS pre-order
/// the reference walk produces (started at the domain's primary,
/// `LayerManager.cpp:749-757`), so one pass over the domain's nodes finds
/// the identical node.
fn first_focusable(scene: &Scene, window: u32, domain: u32) -> Option<u32> {
    scene.window_layer_order(window).into_iter().find(|&id| {
        scene
            .layer(id)
            .is_some_and(|l| l.domain == domain && l.join_focus_chain)
            && scene.node_focusable(id)
    })
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `focus_neighbor`.
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
        let domain = layer.domain;
        // Reference `tTVPLayerManager::FocusNext` / `FocusPrev`
        // (`reference/cpp/core/visual/LayerManager.cpp:723-747`): the
        // search starts from the *domain manager's* focused layer — never
        // from the receiver — and when nothing is focused it starts from
        // the first focusable layer of the domain's primary chain
        // (`SearchFirstFocusable`, `LayerManager.cpp:749-757`), which the
        // receiver-independent path below reproduces. The whole search
        // stays inside the receiver's domain: the neighbor walk uses the
        // manager's node list (`LayerIntf.cpp:1260-1283`).
        let focused = scene.focused_in(window, domain);
        match focused {
            Some(focused) if scene.layer(focused).is_some() => {
                if forward {
                    scene.next_focusable(focused)
                } else {
                    scene.prev_focusable(focused)
                }
            }
            _ => first_focusable(scene, window, domain),
        }
    };
    if let Some(target) = target {
        // `SetFocusTo(l, forward)`: `focusNext` is direction `true`,
        // `focusPrev` `false` (`reference/cpp/core/visual/LayerManager.cpp:723-747`).
        // The move fires `onBlur`/`onFocus` synchronously; the search
        // above already yields a node-focusable layer, so the
        // `SetFocusTo` guard passes.
        focus_layer(&ctx, target, forward);
    }
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_attention_pos`.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_cursor_pos`.
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_release_capture`.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_release_touch_capture`.
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
/// `frame_tail` step calls this). Returns `(mouse_releases,
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
