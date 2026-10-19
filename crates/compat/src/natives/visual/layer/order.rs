//! `Layer` sibling-order and modal-mode members (group 5).
//!
//! Reference: `tTJSNI_BaseLayer::MoveBefore` / `MoveBehind` / `BringToBack`
//! (`reference/cpp/core/visual/LayerIntf.cpp:1356`) and the manager's
//! `SetModeTo` / `RemoveModeFrom` (`LayerManager.cpp:834`).
//!
//! The sibling order is the maintained `children`/`layers` vector of the L2
//! `engine_api::Scene`; reordering also renumbers `z_order` so the sort key
//! ([`Scene::window_layer_order`]) reflects the new order. The modal stack
//! lives in [`LayerExtras`](super::LayerExtras) because the L2 node
//! visibility/enabled predicates do not model modes yet.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::Scene;
use tjs::{NativeInstanceMethodDef, Value};

use super::super::common::{resolve_object_id, scene_mut, scene_ref};
use super::{LayerExtras, LayerInst, bound, clear_domain_focus, focus_layer};
use crate::native_util::{args, report_error, set_void};

/// Mutable access to the sibling order vector that holds `id`.
fn siblings_mut(scene: &mut Scene, id: u32) -> Option<&mut Vec<u32>> {
    let layer = scene.layer(id)?;
    let (window, parent) = (layer.window, layer.parent);
    match parent {
        Some(parent) => scene.layer_mut(parent).map(|l| &mut l.children),
        None => scene.window_mut(window).map(|w| &mut w.layers),
    }
}

/// Reorder the sibling vector, then renumber every sibling's `z_order` to its
/// new index so the stable sibling sort keeps the requested order.
fn renumber_siblings(scene: &mut Scene, id: u32) {
    let ids = match siblings_mut(scene, id) {
        Some(list) => list.clone(),
        None => return,
    };
    for (index, sibling) in ids.iter().enumerate() {
        if let Some(layer) = scene.layer_mut(*sibling) {
            layer.z_order = index as i32;
        }
    }
}

/// Move `id` to the back of its sibling list.
fn bring_to_back(scene: &mut Scene, id: u32) -> Result<(), String> {
    {
        let list = siblings_mut(scene, id)
            .ok_or_else(|| "Layer.bringToBack: this layer has no parent".to_string())?;
        list.retain(|&x| x != id);
        list.insert(0, id);
    }
    renumber_siblings(scene, id);
    Ok(())
}

/// Move `id` immediately `before`/`after` the sibling `target`.
///
/// Reference `tTJSNI_BaseLayer::MoveBefore` / `MoveBehind`
/// (`reference/cpp/core/visual/LayerIntf.cpp:1356-1397`):
/// * both start with `CheckZOrderMoveRule`
///   (`LayerIntf.cpp:1282-1291`), which throws
///   `TVPCannotMoveNextToSelfOrNotSiblings` when `lay == this` or the two
///   are not siblings — a self-move is an **error**, never a silent no-op;
/// * `MoveBefore(lay)` places `this` *directly in front of* `lay`
///   (`ChildChangeOrder(this, lay)` / `(this, lay + 1)` ends `this` at the
///   higher, front-side index);
/// * `MoveBehind(lay)` places `id` *directly behind* `lay`
///   (`(this, lay - 1)` / `(this, lay)`).
fn move_relative(scene: &mut Scene, id: u32, target: u32, before: bool) -> Result<(), String> {
    let method = if before { "moveBefore" } else { "moveBehind" };
    if id == target {
        return Err(format!(
            "Layer.{method}: cannot move a layer next to itself or a non-sibling"
        ));
    }
    let (window, parent) = scene
        .layer(id)
        .map(|l| (l.window, l.parent))
        .ok_or_else(|| format!("Layer.{method}: this layer no longer exists"))?;
    let target_ok = scene
        .layer(target)
        .is_some_and(|t| t.window == window && t.parent == parent);
    if !target_ok {
        return Err(format!(
            "Layer.{method}: cannot move a layer next to itself or a non-sibling"
        ));
    }
    {
        let list = siblings_mut(scene, id)
            .ok_or_else(|| format!("Layer.{method}: this layer has no parent"))?;
        list.retain(|&x| x != id);
        let index = list
            .iter()
            .position(|&x| x == target)
            .ok_or_else(|| format!("Layer.{method}: the sibling no longer exists"))?;
        // `moveBefore` → in front of the target (one past it); `moveBehind`
        // → directly behind it (its own slot).
        let index = if before { index + 1 } else { index };
        list.insert(index, id);
    }
    renumber_siblings(scene, id);
    Ok(())
}

/// `bringToBack()`.
extern "C" fn layer_bring_to_back(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_bring_to_back`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.bringToBack: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.bringToBack: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    match bring_to_back(scene, id) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `moveBefore(sibling)`.
extern "C" fn layer_move_before(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    move_from_script(engine, instance, argc, argv, out, out_error, true)
}

/// `moveBehind(sibling)`.
extern "C" fn layer_move_behind(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    move_from_script(engine, instance, argc, argv, out, out_error, false)
}

fn move_from_script(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    before: bool,
) -> c_int {
    let method = if before { "moveBefore" } else { "moveBehind" };
    if argc < 1 {
        return report_error(out_error, &format!("Layer.{method} requires 1 argument"));
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `move_from_script`.
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
    let target = match resolve_object_id(host.engine(), &a[0], &["Layer"]) {
        Ok(value) if value >= 0 => value as u32,
        _ => {
            return report_error(
                out_error,
                &format!("Layer.{method}: the argument is not a Layer"),
            );
        }
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    match move_relative(scene, id, target, before) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Whether `ancestor` is `node` or one of its ancestors.
fn is_ancestor_or_self(scene: &Scene, ancestor: u32, node: u32) -> bool {
    let mut current = Some(node);
    while let Some(id) = current {
        if id == ancestor {
            return true;
        }
        current = scene.layer(id).and_then(|l| l.parent);
    }
    false
}

/// The first focusable node of `root`'s subtree, depth-first — the port of
/// `tTJSNI_BaseLayer::SearchFirstFocusable`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3574-3593`): test the node
/// itself, then recurse into the children in order.
fn first_focusable_subtree(scene: &Scene, root: u32) -> Option<u32> {
    fn walk(scene: &Scene, id: u32, budget: usize) -> Option<u32> {
        if budget == 0 {
            return None;
        }
        let layer = scene.layer(id)?;
        if scene.node_focusable(id) && layer.join_focus_chain {
            return Some(id);
        }
        layer
            .children
            .clone()
            .into_iter()
            .find_map(|child| walk(scene, child, budget - 1))
    }
    walk(scene, root, scene.layers.len() + 1)
}

/// `setMode()`.
///
/// Reference `tTVPLayerManager::SetModeTo`
/// (`reference/cpp/core/visual/LayerManager.cpp:834-867`):
/// 1. the *current* (top) modal layer blocks an ancestor-or-self of itself;
/// 2. an invisible layer is forced visible first;
/// 3. a parent-invisible or disabled layer throws;
/// 4. focus moves to the first focusable node of the new modal's subtree
///    (`SetFocusTo(layer->SearchFirstFocusable(), true)`), clearing focus
///    when the subtree has none;
/// 5. only then is the layer pushed onto the modal stack.
extern "C" fn layer_set_mode(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_mode`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setMode: this layer is not constructed");
    };
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setMode: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        // Validate against the *top* of the modal stack without holding the
        // scene borrow (`LayerManager.cpp:840-843`).
        let current = {
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let extras = unsafe { ctx.services() }.state::<LayerExtras>();
            extras.modes.last().copied()
        };
        let scene = scene_mut(&ctx)?;
        if scene.layer(id).is_none() {
            return Err("Layer.setMode: this layer no longer exists".into());
        }
        if let Some(current) = current
            && scene.layer(current).is_some()
            && is_ancestor_or_self(scene, id, current)
        {
            return Err("Layer.setMode: cannot set mode to an ancestor of a modal layer".into());
        }
        if !scene.node_visible(id)
            && let Some(layer) = scene.layer_mut(id)
        {
            layer.visible = true;
        }
        // After forcing the layer itself visible, `GetNodeVisible` is exactly
        // `GetParentVisible`; the reference throws on a hidden ancestor or on
        // the layer's **own** disabled flag (`LayerManager.cpp:847-850`).
        if !scene.node_visible(id) || !scene.layer(id).is_some_and(|l| l.enabled) {
            return Err(
                "Layer.setMode: cannot set mode to a disabled or parent-hidden layer".into(),
            );
        }
        let (window, domain, target) = {
            let scene = scene_mut(&ctx)?;
            let Some(window) = scene.layer(id).map(|l| l.window) else {
                return Err("Layer.setMode: this layer no longer exists".into());
            };
            let domain = scene.layer(id).map(|l| l.domain);
            (window, domain, first_focusable_subtree(scene, id))
        };
        // Focus moves to the first focusable node of the modal's subtree
        // with direction forward (`LayerManager.cpp:855`), on the modal
        // layer's own domain manager (`:838-856`); the helper fires
        // `onBlur`/`onFocus` synchronously, and the null target clears that
        // domain's focus.
        match target {
            Some(target) => {
                focus_layer(&ctx, target, true);
            }
            None => {
                if let Some(domain) = domain {
                    clear_domain_focus(&ctx, window, domain, true);
                }
            }
        }
        {
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let mut extras = unsafe { ctx.services() }.state::<LayerExtras>();
            extras.modes.push(id);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `removeMode()`.
extern "C" fn layer_remove_mode(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_remove_mode`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.removeMode: this layer is not constructed");
    };
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.removeMode: compat context is not bound");
    };
    let was_modal = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let mut extras = unsafe { ctx.services() }.state::<LayerExtras>();
        let had = extras.modes.contains(&id);
        extras.modes.retain(|&m| m != id);
        had
    };
    if was_modal {
        let (window, domain, next) = {
            let scene = match scene_ref(&ctx) {
                Ok(scene) => scene,
                Err(error) => return report_error(out_error, &error),
            };
            (
                scene.layer(id).map(|l| l.window),
                scene.layer(id).map(|l| l.domain),
                scene.next_focusable(id),
            )
        };
        if let Some(window) = window {
            // `RemoveModeFrom` hands focus to `layer->GetNextFocusable()`
            // with direction forward, on the removed layer's own domain
            // manager (`LayerManager.cpp:869-890`); the helper fires
            // `onBlur`/`onFocus` synchronously, and the null target clears
            // that domain's focus.
            match next {
                Some(next) => {
                    focus_layer(&ctx, next, true);
                }
                None => {
                    if let Some(domain) = domain {
                        clear_domain_focus(&ctx, window, domain, true);
                    }
                }
            }
        }
    }
    set_void(out);
    0
}

fn d(name: &'static str, f: tjs::NativeInstanceMethodFn) -> NativeInstanceMethodDef {
    NativeInstanceMethodDef { name, f }
}

/// Every group-5 member as a `NativeInstanceMethodDef`.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        d("moveBefore", layer_move_before),
        d("moveBehind", layer_move_behind),
        d("bringToBack", layer_bring_to_back),
        d("setMode", layer_set_mode),
        d("removeMode", layer_remove_mode),
    ]
}
