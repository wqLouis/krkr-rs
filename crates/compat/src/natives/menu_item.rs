//! `MenuItem` native class — a logical menu-tree value object.
//!
//! Behavioral reference: `main`'s `crates/tvp-natives/src/menu_item.rs` and the
//! reference C++ `tTJSNC_MenuItem` /
//! `reference/cpp/core/visual/{MenuItemIntf,impl/MenuItemImpl}.cpp`.
//!
//! # What this composes
//!
//! The desktop TVP implementation backs every item with a platform menu and
//! keeps the tree in that OS object. This port has no OS menu, so the **tree
//! itself is the authoritative state** and is held explicitly in
//! [`CompatServices::state`] as a [`MenuState`] (`id` → [`MenuNode`]) — never
//! in a process global. A `MenuItem` TJS object owns only its node id; the
//! node holds the caption/flags, the parent/children links and the retained
//! script objects (`this`, the `onClick` action owner and the attached
//! window) needed to hand the *real* objects back from `parent`, `root`,
//! `children` and `window`.
//!
//! Members that genuinely need a platform menu are **deferred explicitly**
//! (D11), not silently defaulted:
//!
//! | member | behavior |
//! |---|---|
//! | `HMENU` | raises a typed [`MenuError::NoPlatformMenu`]; there is no `HMENU` to return |
//! | `popup(flags,x,y)` | raises the same typed error; there is no OS popup to open |
//!
//! Everything else is pure logical state: the tree operations (`add`,
//! `insert`, `remove`, `index`), the item flags (`caption`, `checked`,
//! `enabled`, `radio`, `group`, `shortcut`, `visible`), traversal
//! (`parent`, `root`, `children`, `window`), click dispatch (`fireClick`,
//! `onClick`) and the static `textToKeycode` / `keycodeToText` tables.
//!
//! # `onClick` / `fireClick`
//!
//! `fireClick()` activates the item: it first walks the ancestor chain and
//! does nothing when any ancestor is disabled (`CanDeliverEvents`,
//! `MenuItemImpl.cpp`), then invokes the object's `onClick` member — a script
//! override when one was assigned, otherwise the native `onClick` method. The
//! native method is the reference's default handler and dispatches the
//! `onClick` event to the constructor's action owner via `owner.action(ev)`
//! (falling back to `owner.onClick()` / calling the owner directly for owners
//! that predate the action protocol).

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    NativePropertyDef, NativeStaticMembers, RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue,
    VAL_OBJECT, VAL_RETAINED, Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, set_string_out, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError, CompatServices};

/// Platform-menu operations that have no backing on this build (D11).
///
/// Raising this instead of fabricating a handle keeps the deferral explicit
/// and testable: a script can tell "this build has no OS menu" apart from
/// "the item has no menu".
#[derive(Debug, thiserror::Error)]
enum MenuError {
    /// `HMENU` / `popup` need a native menu the build cannot provide.
    #[error(
        "MenuItem.{member}: no platform menu host is available on this build; \
         the logical menu tree exists but there is no OS menu to address"
    )]
    NoPlatformMenu { member: &'static str },
}

// ---------------------------------------------------------------------------
// Per-family dispatch state (never a process global)
// ---------------------------------------------------------------------------

/// One logical menu item.
struct MenuNode {
    caption: String,
    checked: bool,
    enabled: bool,
    radio: bool,
    group: i64,
    visible: bool,
    shortcut: String,
    /// Parent node id (`None` for a detached item or a root).
    parent: Option<u64>,
    /// Child node ids, in menu order.
    children: Vec<u64>,
    /// Retained `objthis` so `parent`/`root`/`children` hand back the real
    /// object the script created.
    object: Option<DetachedValue>,
    /// Retained action owner (`MenuItem` constructor argument 0); the default
    /// `onClick` handler dispatches to it.
    action_owner: Option<DetachedValue>,
    /// Retained window object (`MenuItem` constructor argument 1 when it is an
    /// object); returned by the `window` property.
    window: Option<DetachedValue>,
    /// The raw `objthis` pointer, used to remove the instance lookup on
    /// destroy (0 when unavailable).
    self_handle: usize,
    /// Cached `children` TJS array (reference `GetChildrenArrayNoAddRef`); the
    /// same object is handed back so a held reference stays live.
    children_array: Option<DetachedValue>,
    /// Whether `children_array` must be rebuilt from `children`.
    children_dirty: bool,
}

impl Default for MenuNode {
    fn default() -> Self {
        Self {
            caption: String::new(),
            checked: false,
            // Reference `tTJSNI_MenuItem::tTJSNI_MenuItem`.
            enabled: true,
            radio: false,
            group: 0,
            visible: true,
            shortcut: String::new(),
            parent: None,
            children: Vec::new(),
            object: None,
            action_owner: None,
            window: None,
            self_handle: 0,
            children_array: None,
            children_dirty: true,
        }
    }
}

/// The menu family's dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
struct MenuState {
    /// Live nodes by id (ids start at 1; 0 is the "not constructed" sentinel).
    nodes: HashMap<u64, MenuNode>,
    /// Next node id.
    next_id: u64,
    /// Raw `objthis` pointer → node id, so an item passed as a method argument
    /// resolves to its node without exposing an `id` member.
    instances: HashMap<usize, u64>,
    /// Cached `MenuItem.textToKeycode` dictionary.
    text_to_keycode: Option<DetachedValue>,
    /// Cached `MenuItem.keycodeToText` array.
    keycode_to_text: Option<DetachedValue>,
}

/// Payload of one `MenuItem` TJS object: only its node id.
#[derive(Default)]
struct MenuItemInst {
    id: Option<u64>,
}

// ---------------------------------------------------------------------------
// Binding and marshalling helpers
// ---------------------------------------------------------------------------

/// Resolve the bound engine for a callback and return its dispatch context.
fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "menu_item: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// Write a retained id into `out` (the C++ trampoline consumes the retention).
fn set_retained_out(out: *mut Value, id: Tjs2ValueId) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = id as usize;
    }
}

/// Write TJS `null` into `out`. The C ABI has no null literal, so the `null`
/// expression is evaluated and its (empty) object variant retained.
fn set_null_out(host: &Tjs2Engine, out: *mut Value) {
    if out.is_null() {
        return;
    }
    if let Ok(RetainedValue::Object(object)) = host.eval_retained("null", "MenuItem.null") {
        set_retained_out(out, object.raw_id());
        std::mem::forget(object);
        return;
    }
    set_void(out);
}

/// Duplicate a retained object into `out`, or write TJS `null` when there is
/// none. `raw` is the source id while the caller still owns its retention.
fn emit_object_out(host: &Tjs2Engine, out: *mut Value, raw: Option<Tjs2ValueId>) {
    let Some(raw) = raw else {
        set_null_out(host, out);
        return;
    };
    match host.duplicate_retained(raw) {
        Ok(duplicate) => {
            set_retained_out(out, duplicate.raw_id());
            std::mem::forget(duplicate);
        }
        Err(error) => {
            log::warn!("MenuItem: could not duplicate a retained object: {error}");
            set_null_out(host, out);
        }
    }
}

/// Evaluate a TJS expression and retain the resulting object, if any.
fn eval_object(host: &Tjs2Engine, expression: &str, what: &str) -> Option<DetachedValue> {
    match host.eval_retained(expression, what) {
        Ok(RetainedValue::Object(object)) => Some(object),
        Ok(_) => None,
        Err(error) => {
            log::warn!("{what}: evaluation failed: {error}");
            None
        }
    }
}

/// Access a node field without holding the state borrow across later engine
/// calls.
fn with_node<T>(services: &CompatServices, id: u64, f: impl FnOnce(&MenuNode) -> T) -> Option<T> {
    let state = services.state::<MenuState>();
    state.nodes.get(&id).map(f)
}

/// Resolve a `MenuItem` argument to its node id through the raw object handle
/// recorded by the ABI.
fn item_id_from_arg(state: &MenuState, value: &Value) -> Option<u64> {
    if value.ty != VAL_OBJECT {
        return None;
    }
    let handle = value.object_handle();
    if handle.is_null() {
        return None;
    }
    state.instances.get(&(handle as usize)).copied()
}

/// Detach `child_id` from `parent_id`, if it is a child.
fn detach_child(state: &mut MenuState, parent_id: u64, child_id: u64) {
    if let Some(parent) = state.nodes.get_mut(&parent_id)
        && let Some(index) = parent.children.iter().position(|&child| child == child_id)
    {
        parent.children.remove(index);
        parent.children_dirty = true;
    }
    if let Some(child) = state.nodes.get_mut(&child_id) {
        child.parent = None;
    }
}

/// Insert `child_id` into `parent_id` at `index`, detaching it from any old
/// parent first and rejecting cycles.
fn insert_child(
    state: &mut MenuState,
    parent_id: u64,
    child_id: u64,
    index: usize,
) -> Result<(), String> {
    if !state.nodes.contains_key(&parent_id) || !state.nodes.contains_key(&child_id) {
        return Err("MenuItem: the item no longer exists".into());
    }
    // Reject a cycle: `parent` must not already be a descendant of `child`.
    let mut cursor = Some(parent_id);
    while let Some(current) = cursor {
        if current == child_id {
            return Err("MenuItem: inserting here would create a cycle".into());
        }
        cursor = state.nodes.get(&current).and_then(|node| node.parent);
    }
    if let Some(old_parent) = state.nodes.get(&child_id).and_then(|node| node.parent) {
        detach_child(state, old_parent, child_id);
    }
    {
        let parent = state
            .nodes
            .get_mut(&parent_id)
            .ok_or_else(|| "MenuItem: the parent no longer exists".to_string())?;
        let index = index.min(parent.children.len());
        parent.children.insert(index, child_id);
        parent.children_dirty = true;
    }
    if let Some(child) = state.nodes.get_mut(&child_id) {
        child.parent = Some(parent_id);
    }
    Ok(())
}

/// Whether an item and every one of its ancestors is enabled
/// (`CanDeliverEvents`, `MenuItemImpl.cpp`).
fn can_deliver(state: &MenuState, id: u64) -> bool {
    let mut current = Some(id);
    while let Some(id) = current {
        let Some(node) = state.nodes.get(&id) else {
            return false;
        };
        if !node.enabled {
            return false;
        }
        current = node.parent;
    }
    true
}

/// The item's position within its parent's child list (0 for a root).
fn index_of(state: &MenuState, id: u64) -> usize {
    let Some(parent) = state.nodes.get(&id).and_then(|node| node.parent) else {
        return 0;
    };
    state
        .nodes
        .get(&parent)
        .and_then(|parent| parent.children.iter().position(|&child| child == id))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Instance lifecycle
// ---------------------------------------------------------------------------

extern "C" fn menu_item_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<MenuItemInst>::default()) as *mut c_void
}

extern "C" fn menu_item_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        let services = unsafe { ctx.services() };
        // Remove the node and drop its retained values only after the state
        // borrow is released: releasing the last reference to a script object
        // can invoke the native destroy callback and re-enter this function.
        let removed = {
            let mut state = services.state::<MenuState>();
            if let Some(parent) = state.nodes.get(&id).and_then(|node| node.parent)
                && let Some(parent) = state.nodes.get_mut(&parent)
                && let Some(index) = parent.children.iter().position(|&child| child == id)
            {
                parent.children.remove(index);
                parent.children_dirty = true;
            }
            let children: Vec<u64> = state
                .nodes
                .get(&id)
                .map(|node| node.children.clone())
                .unwrap_or_default();
            for child in children {
                if let Some(node) = state.nodes.get_mut(&child) {
                    node.parent = None;
                }
            }
            if let Some(handle) = state.nodes.get(&id).map(|node| node.self_handle)
                && handle != 0
            {
                state.instances.remove(&handle);
            }
            state.nodes.remove(&id)
        };
        drop(removed);
    }
    // SAFETY: instance came from `menu_item_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut MenuItemInst) });
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

/// `new MenuItem([actionOwner[, windowOrCaption]])`.
///
/// Reference `tTJSNI_BaseMenuItem::Construct` (`MenuItemIntf.cpp`): the first
/// argument is the action owner and the second is the window when it is an
/// object, otherwise the initial caption. The constructor is permissive about
/// a missing first argument (the tree root is often built with `new
/// MenuItem()`), matching `main`.
extern "C" fn menu_item_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem: compat context is not bound");
    };
    let host = resolved.engine();
    let inst = unsafe { &mut *(instance as *mut MenuItemInst) };
    if inst.id.is_some() {
        return report_error(out_error, "MenuItem: this item is already constructed");
    }
    let values = unsafe { args(argv, argc) };

    let action_owner = values
        .first()
        .filter(|value| value.ty == VAL_OBJECT && !value.object_handle().is_null())
        .and_then(|value| host.retain_object_arg(value).ok());
    let (window, caption) = match values.get(1) {
        Some(second) if second.ty == VAL_OBJECT && !second.object_handle().is_null() => {
            (host.retain_object_arg(second).ok(), String::new())
        }
        Some(second) => (None, value_as_string(second)),
        None => (None, String::new()),
    };
    let object = host.retain_object_detached(objthis).ok();

    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let id = {
        let mut state = services.state::<MenuState>();
        let id = state.next_id.max(1);
        state.next_id = id + 1;
        state.nodes.insert(
            id,
            MenuNode {
                caption,
                action_owner,
                window,
                object,
                self_handle: objthis as usize,
                ..MenuNode::default()
            },
        );
        if !objthis.is_null() {
            state.instances.insert(objthis as usize, id);
        }
        id
    };
    inst.id = Some(id);
    set_void(out);
    0
}

/// `add(item)` — append an item (reference `tTJSNI_MenuItem::Add`).
extern "C" fn menu_item_add(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.add: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.add: this item is not constructed");
    };
    let values = unsafe { args(argv, argc) };
    let Some(value) = values.first() else {
        return report_error(out_error, "MenuItem.add requires a MenuItem");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<MenuState>();
    let Some(child) = item_id_from_arg(&state, value) else {
        return report_error(out_error, "MenuItem.add expects a MenuItem");
    };
    let index = state.nodes.get(&id).map_or(0, |node| node.children.len());
    if let Err(error) = insert_child(&mut state, id, child, index) {
        return report_error(out_error, &error);
    }
    set_void(out);
    0
}

/// `insert(item, index)` — insert an item at a position (reference
/// `tTJSNI_MenuItem::Insert`); the index is clamped.
extern "C" fn menu_item_insert(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.insert: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.insert: this item is not constructed");
    };
    let values = unsafe { args(argv, argc) };
    if values.len() < 2 {
        return report_error(out_error, "MenuItem.insert requires an item and an index");
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<MenuState>();
    let Some(child) = item_id_from_arg(&state, &values[0]) else {
        return report_error(out_error, "MenuItem.insert expects a MenuItem");
    };
    let index = value_as_i64(&values[1]).max(0) as usize;
    if let Err(error) = insert_child(&mut state, id, child, index) {
        return report_error(out_error, &error);
    }
    set_void(out);
    0
}

/// `remove(item)` — detach a child (reference `tTJSNI_MenuItem::Remove`).
extern "C" fn menu_item_remove(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.remove: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.remove: this item is not constructed");
    };
    let values = unsafe { args(argv, argc) };
    let Some(value) = values.first() else {
        return report_error(out_error, "MenuItem.remove requires a MenuItem");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<MenuState>();
    let Some(child) = item_id_from_arg(&state, value) else {
        return report_error(out_error, "MenuItem.remove expects a MenuItem");
    };
    detach_child(&mut state, id, child);
    set_void(out);
    0
}

/// `popup(flags, x, y)` — the reference opens a native popup (`TrackPopup`,
/// `MenuItemImpl.cpp`). This build has no OS menu, so it raises a typed error
/// (D11) instead of pretending the popup opened.
extern "C" fn menu_item_popup(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "MenuItem.popup requires flags, x and y");
    }
    report_error(
        out_error,
        &MenuError::NoPlatformMenu { member: "popup" }.to_string(),
    )
}

/// `fireClick()` — activate the item. Disabled items (or items under a
/// disabled ancestor) do nothing; otherwise the object's `onClick` member runs
/// (a script override when present, the native default otherwise).
extern "C" fn menu_item_fire_click(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.fireClick: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "MenuItem.fireClick: this item is not constructed",
        );
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let (object, deliver) = {
        let state = services.state::<MenuState>();
        let object = state
            .nodes
            .get(&id)
            .and_then(|node| node.object.as_ref())
            .map(|object| object.raw_id());
        (object, can_deliver(&state, id))
    };
    if deliver {
        let host = resolved.engine();
        if let Some(raw) = object
            && let Ok(self_obj) = host.duplicate_retained(raw)
        {
            // Call the member: a script override runs directly, otherwise the
            // native `onClick` dispatches to the action owner. The state
            // borrow is already released, so the callback may re-enter.
            if let Err(error) = host.call_member(self_obj.raw_id(), "onClick", &[]) {
                log::warn!("MenuItem.fireClick: onClick handler failed: {error}");
            }
        }
    }
    set_void(out);
    0
}

/// `onClick()` — the reference default handler (`MenuItemIntf.cpp`): dispatch
/// an `onClick` event to the constructor's action owner.
extern "C" fn menu_item_on_click(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.onClick: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.onClick: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let owner = with_node(services, id, |node| {
        node.action_owner.as_ref().map(|owner| owner.raw_id())
    })
    .flatten();
    let host = resolved.engine();
    if let Some(raw) = owner
        && let Ok(owner) = host.duplicate_retained(raw)
    {
        dispatch_click_action(host, &owner);
    }
    set_void(out);
    0
}

/// Dispatch the reference `owner.action(event)` invocation, with fallbacks for
/// owners that predate the action protocol.
fn dispatch_click_action(host: &Tjs2Engine, owner: &DetachedValue) {
    if let Some(event) = eval_object(host, "%[]", "MenuItem.onClickEvent") {
        let _ = host.set_member(event.raw_id(), "type", &TjsValue::String("onClick".into()));
        let event_id = event.raw_id();
        match host.call_member(
            owner.raw_id(),
            "action",
            &[TjsValue::Retained(event_id as u64)],
        ) {
            Ok(_) => {
                // The argument copy consumed the retention.
                std::mem::forget(event);
                return;
            }
            Err(_) => {
                // The id may or may not have been consumed; forget it rather
                // than risk a double release.
                std::mem::forget(event);
            }
        }
    }
    if host.call_member(owner.raw_id(), "onClick", &[]).is_ok() {
        return;
    }
    if let Err(error) = host.call_detached(owner, &[]) {
        log::debug!("MenuItem.onClick: action owner is not callable: {error}");
    }
}

// ---------------------------------------------------------------------------
// Item properties
// ---------------------------------------------------------------------------

extern "C" fn menu_caption_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.caption: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.caption: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let caption = with_node(services, id, |node| node.caption.clone()).unwrap_or_default();
    set_string_out(&ctx, out, &caption);
    0
}

extern "C" fn menu_caption_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.caption: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.caption: this item is not constructed");
    };
    let caption = value_as_string(unsafe { &*value });
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<MenuState>();
    if let Some(node) = state.nodes.get_mut(&id) {
        node.caption = caption;
    }
    0
}

extern "C" fn menu_shortcut_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.shortcut: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.shortcut: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let shortcut = with_node(services, id, |node| node.shortcut.clone()).unwrap_or_default();
    set_string_out(&ctx, out, &shortcut);
    0
}

extern "C" fn menu_shortcut_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.shortcut: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.shortcut: this item is not constructed");
    };
    let shortcut = value_as_string(unsafe { &*value });
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<MenuState>();
    if let Some(node) = state.nodes.get_mut(&id) {
        node.shortcut = shortcut;
    }
    0
}

/// Expand a boolean item flag into a getter/setter pair.
macro_rules! bool_property {
    ($get:ident, $set:ident, $field:ident, $member:literal) => {
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let Ok((_resolved, ctx)) = bound(engine) else {
                return report_error(
                    out_error,
                    concat!("MenuItem.", $member, ": compat context is not bound"),
                );
            };
            let inst = unsafe { &*(instance as *const MenuItemInst) };
            let Some(id) = inst.id else {
                return report_error(
                    out_error,
                    concat!("MenuItem.", $member, ": this item is not constructed"),
                );
            };
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let services = unsafe { ctx.services() };
            let value = with_node(services, id, |node| node.$field).unwrap_or(false);
            set_bool(out, value);
            0
        }

        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let Ok((_resolved, ctx)) = bound(engine) else {
                return report_error(
                    out_error,
                    concat!("MenuItem.", $member, ": compat context is not bound"),
                );
            };
            let inst = unsafe { &*(instance as *const MenuItemInst) };
            let Some(id) = inst.id else {
                return report_error(
                    out_error,
                    concat!("MenuItem.", $member, ": this item is not constructed"),
                );
            };
            let value = value_as_bool(unsafe { &*value });
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let mut state = unsafe { ctx.services() }.state::<MenuState>();
            if let Some(node) = state.nodes.get_mut(&id) {
                node.$field = value;
            }
            0
        }
    };
}

bool_property!(menu_enabled_get, menu_enabled_set, enabled, "enabled");
bool_property!(menu_radio_get, menu_radio_set, radio, "radio");
bool_property!(menu_visible_get, menu_visible_set, visible, "visible");

/// `checked`: reading is plain state; writing one radio sibling clears the
/// other members of the same group (reference `SetChecked`).
extern "C" fn menu_checked_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.checked: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.checked: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let checked = with_node(services, id, |node| node.checked).unwrap_or(false);
    set_bool(out, checked);
    0
}

extern "C" fn menu_checked_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.checked: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.checked: this item is not constructed");
    };
    let checked = value_as_bool(unsafe { &*value });
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<MenuState>();
    let (radio, group, parent) = match state.nodes.get(&id) {
        Some(node) => (node.radio, node.group, node.parent),
        None => (false, 0, None),
    };
    if checked
        && radio
        && let Some(parent) = parent
    {
        let siblings: Vec<u64> = state
            .nodes
            .get(&parent)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        for sibling in siblings {
            if sibling == id {
                continue;
            }
            if let Some(node) = state.nodes.get_mut(&sibling)
                && node.radio
                && node.group == group
            {
                node.checked = false;
            }
        }
    }
    if let Some(node) = state.nodes.get_mut(&id) {
        node.checked = checked;
    }
    0
}

extern "C" fn menu_group_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.group: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.group: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let group = with_node(services, id, |node| node.group).unwrap_or(0);
    set_int(out, group);
    0
}

extern "C" fn menu_group_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.group: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.group: this item is not constructed");
    };
    let group = value_as_i64(unsafe { &*value });
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<MenuState>();
    if let Some(node) = state.nodes.get_mut(&id) {
        node.group = group;
    }
    0
}

/// `index`: the position within the parent's child list. The setter moves the
/// item (the reference `SetIndex` is a platform no-op; the logical tree can
/// honor it), clamping to the valid range.
extern "C" fn menu_index_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.index: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.index: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let index = {
        let state = services.state::<MenuState>();
        index_of(&state, id)
    };
    set_int(out, index as i64);
    0
}

extern "C" fn menu_index_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.index: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.index: this item is not constructed");
    };
    let target = value_as_i64(unsafe { &*value }).max(0) as usize;
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<MenuState>();
    let Some(parent) = state.nodes.get(&id).and_then(|node| node.parent) else {
        return 0;
    };
    let Some(current) = state
        .nodes
        .get(&parent)
        .and_then(|node| node.children.iter().position(|&child| child == id))
    else {
        return 0;
    };
    let len = state
        .nodes
        .get(&parent)
        .map_or(0, |node| node.children.len());
    if len == 0 {
        return 0;
    }
    let target = target.min(len - 1);
    if let Some(node) = state.nodes.get_mut(&parent) {
        let child = node.children.remove(current);
        node.children.insert(target, child);
        node.children_dirty = true;
    }
    0
}

// ---------------------------------------------------------------------------
// Traversal properties
// ---------------------------------------------------------------------------

extern "C" fn menu_parent_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.parent: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.parent: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let raw = {
        let state = services.state::<MenuState>();
        state
            .nodes
            .get(&id)
            .and_then(|node| node.parent)
            .and_then(|parent| state.nodes.get(&parent))
            .and_then(|parent| parent.object.as_ref())
            .map(|object| object.raw_id())
    };
    emit_object_out(resolved.engine(), out, raw);
    0
}

extern "C" fn menu_root_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.root: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.root: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let raw = {
        let state = services.state::<MenuState>();
        let mut current = Some(id);
        let mut root = id;
        while let Some(node_id) = current {
            root = node_id;
            current = state.nodes.get(&node_id).and_then(|node| node.parent);
        }
        state
            .nodes
            .get(&root)
            .and_then(|node| node.object.as_ref())
            .map(|object| object.raw_id())
    };
    emit_object_out(resolved.engine(), out, raw);
    0
}

/// `window` — the window passed to the constructor, or `null`.
extern "C" fn menu_window_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.window: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.window: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let raw = with_node(services, id, |node| {
        node.window.as_ref().map(|window| window.raw_id())
    })
    .flatten();
    emit_object_out(resolved.engine(), out, raw);
    0
}

/// `children` — a live TJS array of the item's children. The array object is
/// cached per item and rebuilt from the logical list when it changed
/// (reference `GetChildrenArrayNoAddRef`, `MenuItemIntf.cpp`).
extern "C" fn menu_children_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "MenuItem.children: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const MenuItemInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "MenuItem.children: this item is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    match children_array(resolved.engine(), services, id) {
        Ok(array) => {
            set_retained_out(out, array.raw_id());
            std::mem::forget(array);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// The cached `children` array for `id`, created and refreshed as needed.
fn children_array(
    host: &Tjs2Engine,
    services: &CompatServices,
    id: u64,
) -> Result<DetachedValue, String> {
    let has_array = {
        let state = services.state::<MenuState>();
        state
            .nodes
            .get(&id)
            .is_some_and(|node| node.children_array.is_some())
    };
    if !has_array {
        let array = eval_object(host, "[]", "MenuItem.children")
            .ok_or_else(|| "MenuItem.children: could not create the array".to_string())?;
        let mut state = services.state::<MenuState>();
        if let Some(node) = state.nodes.get_mut(&id)
            && node.children_array.is_none()
        {
            node.children_array = Some(array);
            node.children_dirty = true;
        }
    }
    let dirty = {
        let state = services.state::<MenuState>();
        state.nodes.get(&id).is_none_or(|node| node.children_dirty)
    };
    if dirty {
        refresh_children_array(host, services, id)?;
    }
    let raw = {
        let state = services.state::<MenuState>();
        state
            .nodes
            .get(&id)
            .and_then(|node| node.children_array.as_ref())
            .map(|array| array.raw_id())
    };
    let raw = raw.ok_or_else(|| "MenuItem.children: the array is missing".to_string())?;
    host.duplicate_retained(raw)
        .map_err(|error| format!("MenuItem.children: {error}"))
}

/// Rebuild the cached `children` array from the logical child list.
fn refresh_children_array(
    host: &Tjs2Engine,
    services: &CompatServices,
    id: u64,
) -> Result<(), String> {
    let (array, children) = {
        let state = services.state::<MenuState>();
        let Some(node) = state.nodes.get(&id) else {
            return Ok(());
        };
        let Some(array) = node.children_array.as_ref().map(|array| array.raw_id()) else {
            return Ok(());
        };
        let children: Vec<Tjs2ValueId> = node
            .children
            .iter()
            .filter_map(|child| state.nodes.get(child))
            .filter_map(|child| child.object.as_ref().map(|object| object.raw_id()))
            .collect();
        (array, children)
    };
    host.call_member(array, "clear", &[])
        .map_err(|error| format!("MenuItem.children: clear failed: {error}"))?;
    for raw in children {
        let duplicate = host
            .duplicate_retained(raw)
            .map_err(|error| format!("MenuItem.children: {error}"))?;
        let duplicate_id = duplicate.raw_id();
        host.call_member(array, "push", &[TjsValue::Retained(duplicate_id as u64)])
            .map_err(|error| format!("MenuItem.children: push failed: {error}"))?;
        // The push consumed the retention.
        std::mem::forget(duplicate);
    }
    let mut state = services.state::<MenuState>();
    if let Some(node) = state.nodes.get_mut(&id) {
        node.children_dirty = false;
    }
    Ok(())
}

/// `HMENU` — a native menu handle a plugin could use. This build has no OS
/// menu, so it raises a typed error (D11) instead of inventing a handle.
extern "C" fn menu_hmenu_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    report_error(
        out_error,
        &MenuError::NoPlatformMenu { member: "HMENU" }.to_string(),
    )
}

// ---------------------------------------------------------------------------
// Static shortcut tables
// ---------------------------------------------------------------------------

/// `(display name, virtual keycode)` pairs. The reference installs the three
/// unconditional `SetShortCutKeyCode` entries plus the `#if 0` Windows
/// `MapVirtualKey` loop; this covers the same common keys so
/// `textToKeycode`/`keycodeToText` round-trip.
const SHORTCUT_KEYS: &[(&str, i64)] = &[
    ("BkSp", 8),
    ("Tab", 9),
    ("Enter", 13),
    ("Esc", 27),
    ("Space", 32),
    ("PgUp", 33),
    ("PgDn", 34),
    ("End", 35),
    ("Home", 36),
    ("Left", 37),
    ("Up", 38),
    ("Right", 39),
    ("Down", 40),
    ("Ins", 45),
    ("Del", 46),
    ("0", 48),
    ("1", 49),
    ("2", 50),
    ("3", 51),
    ("4", 52),
    ("5", 53),
    ("6", 54),
    ("7", 55),
    ("8", 56),
    ("9", 57),
    ("A", 65),
    ("B", 66),
    ("C", 67),
    ("D", 68),
    ("E", 69),
    ("F", 70),
    ("G", 71),
    ("H", 72),
    ("I", 73),
    ("J", 74),
    ("K", 75),
    ("L", 76),
    ("M", 77),
    ("N", 78),
    ("O", 79),
    ("P", 80),
    ("Q", 81),
    ("R", 82),
    ("S", 83),
    ("T", 84),
    ("U", 85),
    ("V", 86),
    ("W", 87),
    ("X", 88),
    ("Y", 89),
    ("Z", 90),
    ("F1", 112),
    ("F2", 113),
    ("F3", 114),
    ("F4", 115),
    ("F5", 116),
    ("F6", 117),
    ("F7", 118),
    ("F8", 119),
    ("F9", 120),
    ("F10", 121),
    ("F11", 122),
    ("F12", 123),
];

/// Build the initializer for `MenuItem.textToKeycode` (lowercased keys keep
/// the original-case display strings in the value list).
fn text_to_keycode_expression() -> String {
    let mut script = String::from("%[");
    for (display, code) in SHORTCUT_KEYS {
        script.push_str(&format!("'{}'=>{code},", display.to_lowercase()));
    }
    script.push(']');
    script
}

/// Build the initializer for `MenuItem.keycodeToText` (a sparse array indexed
/// by virtual keycode).
fn keycode_to_text_expression() -> String {
    let mut script = String::from("(function(){var a=[];");
    for (display, code) in SHORTCUT_KEYS {
        script.push_str(&format!("a[{code}]='{display}';"));
    }
    script.push_str("return a;})()");
    script
}

/// `MenuItem.textToKeycode` (static, read-only).
extern "C" fn menu_text_to_keycode_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "MenuItem.textToKeycode: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let raw = unsafe { ctx.services() }
        .state::<MenuState>()
        .text_to_keycode
        .as_ref()
        .map(|table| table.raw_id());
    emit_object_out(resolved.engine(), out, raw);
    0
}

/// `MenuItem.keycodeToText` (static, read-only).
extern "C" fn menu_keycode_to_text_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "MenuItem.keycodeToText: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let raw = unsafe { ctx.services() }
        .state::<MenuState>()
        .keycode_to_text
        .as_ref()
        .map(|table| table.raw_id());
    emit_object_out(resolved.engine(), out, raw);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Build and cache the two static shortcut tables on the VM.
fn install_shortcut_tables(ctx: &CompatCtx) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine, and the
    // host keeps it alive across registration.
    let host = unsafe { ctx.engine() };
    let text = eval_object(
        host,
        &text_to_keycode_expression(),
        "MenuItem.textToKeycode",
    )
    .ok_or_else(|| CompatError::Register("MenuItem: failed to build textToKeycode".into()))?;
    let keys = eval_object(
        host,
        &keycode_to_text_expression(),
        "MenuItem.keycodeToText",
    )
    .ok_or_else(|| CompatError::Register("MenuItem: failed to build keycodeToText".into()))?;
    // SAFETY: see above.
    let mut state = unsafe { ctx.services() }.state::<MenuState>();
    state.text_to_keycode = Some(text);
    state.keycode_to_text = Some(keys);
    Ok(())
}

/// Register the `MenuItem` class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    install_shortcut_tables(ctx)?;

    ctx.register_instance(&NativeInstanceBuilder {
        name: "MenuItem",
        create: menu_item_create,
        destroy: menu_item_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "MenuItem",
                f: menu_item_ctor,
            },
            NativeInstanceMethodDef {
                name: "add",
                f: menu_item_add,
            },
            NativeInstanceMethodDef {
                name: "insert",
                f: menu_item_insert,
            },
            NativeInstanceMethodDef {
                name: "remove",
                f: menu_item_remove,
            },
            NativeInstanceMethodDef {
                name: "popup",
                f: menu_item_popup,
            },
            NativeInstanceMethodDef {
                name: "onClick",
                f: menu_item_on_click,
            },
            NativeInstanceMethodDef {
                name: "fireClick",
                f: menu_item_fire_click,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "caption",
                get: Some(menu_caption_get),
                set: Some(menu_caption_set),
            },
            NativeInstancePropertyDef {
                name: "checked",
                get: Some(menu_checked_get),
                set: Some(menu_checked_set),
            },
            NativeInstancePropertyDef {
                name: "enabled",
                get: Some(menu_enabled_get),
                set: Some(menu_enabled_set),
            },
            NativeInstancePropertyDef {
                name: "radio",
                get: Some(menu_radio_get),
                set: Some(menu_radio_set),
            },
            NativeInstancePropertyDef {
                name: "group",
                get: Some(menu_group_get),
                set: Some(menu_group_set),
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(menu_visible_get),
                set: Some(menu_visible_set),
            },
            NativeInstancePropertyDef {
                name: "shortcut",
                get: Some(menu_shortcut_get),
                set: Some(menu_shortcut_set),
            },
            NativeInstancePropertyDef {
                name: "index",
                get: Some(menu_index_get),
                set: Some(menu_index_set),
            },
            NativeInstancePropertyDef {
                name: "parent",
                get: Some(menu_parent_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "root",
                get: Some(menu_root_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "children",
                get: Some(menu_children_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "window",
                get: Some(menu_window_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "HMENU",
                get: Some(menu_hmenu_get),
                set: None,
            },
        ],
    })?;

    ctx.register_static_members(&NativeStaticMembers {
        class_name: "MenuItem",
        methods: vec![],
        properties: vec![
            NativePropertyDef {
                name: "textToKeycode",
                get: Some(menu_text_to_keycode_get),
                set: None,
            },
            NativePropertyDef {
                name: "keycodeToText",
                get: Some(menu_keycode_to_text_get),
                set: None,
            },
        ],
    })?;

    Ok(())
}
