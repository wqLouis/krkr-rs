//! `MenuItem` native class plus the shared menu registry the renderer reads.
//!
//! The desktop TVP implementation backs this class with a platform menu. The
//! logical tree here is the authoritative state (see
//! `reference/cpp/core/visual/MenuItemIntf.cpp` and
//! `reference/cpp/core/visual/impl/MenuItemImpl.cpp`): game scripts build menu
//! trees, inspect their state, call `fireClick()`/`popup()`, and the render
//! layer draws the same tree with Bevy UI.
//!
//! Objects are retained the same way as the timer natives, so a parent keeps
//! its children alive and object-valued properties remain real TJS objects
//! rather than integer stand-ins.
//!
//! # Shared registry
//!
//! The render crate (`krkr_render::menu`) must not reach into raw
//! `MenuItemInst` pointers, so this module publishes owned snapshots through
//! the process-global registry:
//!
//! * [`menu_snapshots`] / [`menu_snapshot`] — read the logical tree for a
//!   window (or all windows) as plain data.
//! * [`fire_menu_click`] — activate an item by its stable handle (the value
//!   `HMENU` exposes and the renderer stores on its UI nodes).
//! * [`take_popup_request`] — the pending `popup(flags, x, y)` request, which
//!   the renderer turns into an opened dropdown.
//!
//! `Window.menu` (`crates/tvp-visual/src/natives/window.rs`) creates the root
//! with `new MenuItem(window, window)`; the constructor reads the window's
//! `id` member and registers the tree here. That keeps `tvp-visual` free of a
//! dependency on this crate while still giving the renderer a single source
//! of truth.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void};
use std::sync::{LazyLock, Mutex};

use tjs2_sys::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    NativePropertyDef, NativeStaticMembers, RetainedValue, Tjs2Engine, TjsValue, VAL_OBJECT, Value,
};

use crate::{
    args, context_engine, set_int_out, set_string_out, set_void_out, value_as_bool, value_as_i64,
    value_as_string,
};

/// A child reference is deliberately retained.  In the reference, adding a
/// child adds a TJS reference to it; this gives the same behavior when the
/// only remaining script reference is the parent's `children` list.
struct ChildLink {
    ptr: *mut MenuItemInst,
    keep: DetachedValue,
}

/// State of one logical menu item.  `objthis` is only used for returning the
/// item as a TJS object; it is valid while the item is alive.
pub(crate) struct MenuItemInst {
    caption: String,
    checked: bool,
    enabled: bool,
    radio: bool,
    group: i64,
    visible: bool,
    shortcut: String,
    parent: *mut MenuItemInst,
    children: Vec<ChildLink>,
    objthis: *mut c_void,
    object_key: usize,
    /// The array is retained by the native object and kept in sync with the
    /// native child list, so `children` is a real TJS array of item objects
    /// (reference `GetChildrenArrayNoAddRef`, `MenuItemIntf.cpp:151`).
    children_array: Option<DetachedValue>,
    /// Global name backing `children` (the retained id cannot be re-retained
    /// across the ABI, so the getter re-evaluates the global).
    children_global: Option<String>,
    action_owner: Option<DetachedValue>,
    /// Retention for the native object itself, used to resolve opaque object
    /// arguments through the engine's retained-value identity map.
    object_owner: Option<DetachedValue>,
    /// The window this item was attached to (reference
    /// `tTJSNI_BaseMenuItem::Window`), if any.
    window_owner: Option<DetachedValue>,
    /// Raw handle of `window_owner`, for returning the `window` property.
    window_handle: *mut c_void,
}

impl Default for MenuItemInst {
    fn default() -> Self {
        Self {
            caption: String::new(),
            checked: false,
            enabled: true,
            radio: false,
            group: 0,
            visible: true,
            shortcut: String::new(),
            parent: std::ptr::null_mut(),
            children: Vec::new(),
            objthis: std::ptr::null_mut(),
            object_key: 0,
            children_array: None,
            children_global: None,
            action_owner: None,
            object_owner: None,
            window_owner: None,
            window_handle: std::ptr::null_mut(),
        }
    }
}

/// Owned, render-friendly snapshot of one menu item. No raw pointers cross
/// the crate boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItemSnapshot {
    /// Stable handle (also what `HMENU` returns); pass it back to
    /// [`fire_menu_click`].
    pub handle: i64,
    pub caption: String,
    pub checked: bool,
    pub enabled: bool,
    pub radio: bool,
    pub group: i64,
    pub visible: bool,
    pub shortcut: String,
    /// Position within the parent's child list.
    pub index: usize,
    pub children: Vec<MenuItemSnapshot>,
}

/// Snapshot of one window's root menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuSnapshot {
    /// Scene window id the root belongs to.
    pub window: u32,
    /// The window's root item (its `children` are the top-level bar entries).
    pub root: MenuItemSnapshot,
}

/// A pending `popup(flags, x, y)` request consumed by the renderer.
///
/// Kept as a tuple of publicly reachable types (no new exported name) so the
/// private `menu_item` module does not leak an unreachable type through the
/// crate root: `(handle, x, y, item)`. The item snapshot is captured when
/// `popup()` is called, which is exactly the moment the reference `TrackPopup`
/// would freeze the popup menu. The reference shows the item's submenu as a
/// native OS popup at the requested client coordinates; on this platform the
/// request is handed to the in-engine Bevy UI instead, so the position has to
/// travel with the handle (a context menu that ignored `x`/`y` would always
/// land in a corner).
pub type PopupRequest = (i64, i32, i32, MenuItemSnapshot);

// The VM is single threaded.  The registry uses integer addresses so it does
// not make raw pointers part of its Send/Sync contract.
static ITEMS: LazyLock<Mutex<HashMap<usize, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Raw TJS object handle (`objthis`) -> native instance pointer. The ABI
/// marshals each object argument's handle in `Value::object_handle`, so this
/// resolves `add`/`insert`/`remove` arguments directly instead of relying on
/// the engine's "last object result".
static OBJECT_PTRS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Live native instance pointers (guards snapshot/click against a dangling
/// pointer after destroy).
static LIVE_ITEMS: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// Window id -> root item pointer, populated by the constructor / attach.
static WINDOW_ROOTS: LazyLock<Mutex<HashMap<u32, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Pending `popup(flags, x, y)` request consumed by the renderer.
static POPUP_REQUEST: LazyLock<Mutex<Option<PopupRequest>>> = LazyLock::new(|| Mutex::new(None));

fn item_ptr_from_arg(value: &Value) -> Option<*mut MenuItemInst> {
    if value.ty != VAL_OBJECT {
        return None;
    }
    let handle = value.object_handle();
    if handle.is_null() {
        return None;
    }
    OBJECT_PTRS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(handle as usize))
        .copied()
        .map(|p| p as *mut MenuItemInst)
}

fn set_retained(out: *mut Value, value: DetachedValue) {
    // The C++ trampoline consumes a VAL_RETAINED result.  Forget the Rust
    // handle after handing its id over; the copied TJS value owns the result.
    let id = value.raw_id();
    unsafe {
        (*out).ty = tjs2_sys::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = id as usize;
    }
    std::mem::forget(value);
}

/// Write a TJS `null` result (distinct from `void`) into `out`.
fn set_null_out(out: *mut Value) {
    unsafe {
        (*out).ty = tjs2_sys::VAL_NULL;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

fn return_object(engine: &Tjs2Engine, out: *mut Value, obj: *mut c_void) -> c_int {
    match engine.retain_object_detached(obj) {
        Ok(value) => {
            set_retained(out, value);
            0
        }
        Err(_) => {
            set_void_out(out);
            0
        }
    }
}

fn root_ptr(mut ptr: *mut MenuItemInst) -> *mut MenuItemInst {
    while !unsafe { (*ptr).parent }.is_null() {
        ptr = unsafe { (*ptr).parent };
    }
    ptr
}

fn register_window_root(window: u32, item: *mut MenuItemInst) {
    WINDOW_ROOTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(window, root_ptr(item) as usize);
}

fn object_id_member(engine: &Tjs2Engine, value: &DetachedValue) -> Option<i64> {
    for member in ["id", "nativeId"] {
        match engine.get_member(value.raw_id(), member) {
            Ok(TjsValue::Integer(v)) => return Some(v),
            Ok(TjsValue::Real(v)) => return Some(v as i64),
            _ => {}
        }
    }
    None
}

/// Build the `Window.menu` root when the second argument (`values[1]`,
/// reference `MenuItemIntf.cpp:56`) is an object.
fn attach_window_arg(engine: &Tjs2Engine, item: &mut MenuItemInst, value: &Value) {
    if value.ty != VAL_OBJECT {
        return;
    }
    let handle = value.object_handle();
    if handle.is_null() {
        return;
    }
    let Ok(owner) = engine.retain_object_detached(handle) else {
        return;
    };
    let Some(id) = object_id_member(engine, &owner) else {
        return;
    };
    let window = id.max(0) as u32;
    item.window_owner = Some(owner);
    item.window_handle = handle;
    register_window_root(window, item as *mut MenuItemInst);
}

extern "C" fn menu_item_create(_engine: *mut c_void) -> *mut c_void {
    let ptr = Box::into_raw(Box::<MenuItemInst>::default()) as *mut c_void;
    LIVE_ITEMS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(ptr as usize);
    ptr
}

extern "C" fn menu_item_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let addr = instance as usize;
    // Clear child parent links before dropping the retained child references.
    let item = unsafe { &mut *(instance as *mut MenuItemInst) };
    for child in item.children.drain(..) {
        // SAFETY: the retained link keeps the child alive until after this
        // assignment; the child may be finalized when `keep` is dropped.
        unsafe { (*child.ptr).parent = std::ptr::null_mut() };
        drop(child.keep);
    }
    if item.object_key != 0 {
        ITEMS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&item.object_key);
    }
    OBJECT_PTRS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&addr);
    LIVE_ITEMS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&addr);
    WINDOW_ROOTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|_, root| *root != addr);
    // SAFETY: pointer came from menu_item_create.
    unsafe { drop(Box::from_raw(instance as *mut MenuItemInst)) };
}

/// `new MenuItem(actionOwner[, windowOrCaption])`.  Reference
/// `tTJSNI_BaseMenuItem::Construct` (`MenuItemIntf.cpp:56`): `param[0]` is the
/// action owner, `param[1]` is the window when it is an object, otherwise the
/// caption (`MenuItemImpl.cpp:125`). `TVPCreateMenuItemObject`
/// (`MenuItemIntf.cpp:560`) passes the window twice.
extern "C" fn menu_item_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let engine = context_engine();
    let values = args(argv, argc);
    let item = unsafe { &mut *(instance as *mut MenuItemInst) };
    item.objthis = objthis;

    // Retain the native object so add/remove can resolve opaque object
    // arguments through the object-handle registry.
    match engine.retain_object_detached(objthis) {
        Ok(owner) => {
            item.object_key = owner.raw_id() as usize;
            ITEMS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(item.object_key, instance as usize);
            OBJECT_PTRS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(objthis as usize, instance as usize);
            item.object_owner = Some(owner);
        }
        Err(e) => {
            return crate::report_error(out_error, &format!("MenuItem: {e}"));
        }
    }

    // The first argument is the action owner in TVP.  It is also how a menu
    // item can call the owning script object's onClick handler.
    if let Some(owner_arg) = values.first()
        && owner_arg.ty == VAL_OBJECT
        && !owner_arg.object_handle().is_null()
        && let Ok(owner) = engine.retain_object_detached(owner_arg.object_handle())
    {
        item.action_owner = Some(owner);
    }
    if let Some(second) = values.get(1) {
        if second.ty == VAL_OBJECT {
            attach_window_arg(engine, item, second);
        } else {
            item.caption = value_as_string(second);
        }
    }

    // Build the children array as a named global so it can be returned as a
    // retained object (the ABI cannot re-retain an existing id).
    let name = format!("__krkr_menu_children_{}", item.object_key);
    if engine
        .exec_script(&format!("global.{name} = [];"), "MenuItem.children")
        .is_ok()
        && let Ok(RetainedValue::Object(array)) =
            engine.eval_retained(&format!("global.{name}"), "MenuItem.children")
    {
        item.children_array = Some(array);
        item.children_global = Some(name);
    }
    set_void_out(out);
    0
}

fn can_deliver(item: *const MenuItemInst) -> bool {
    let mut current = item;
    while !current.is_null() {
        // SAFETY: all parent links are maintained by the tree operations.
        let value = unsafe { &*current };
        if !value.enabled {
            return false;
        }
        current = value.parent;
    }
    true
}

/// Rebuild the parent's `children` TJS array from the native child vector
/// (reference `GetChildrenArrayNoAddRef`, `MenuItemIntf.cpp:151`). The array
/// keeps stable object identity because it lives in a global; only its
/// contents are refreshed.
fn sync_children(engine: &Tjs2Engine, parent: &MenuItemInst) {
    let Some(array) = parent.children_array.as_ref() else {
        return;
    };
    if engine.call_member(array.raw_id(), "clear", &[]).is_err() {
        return;
    }
    for link in &parent.children {
        // SAFETY: links are maintained while the parent is alive.
        let child = unsafe { &*link.ptr };
        if child.objthis.is_null() {
            continue;
        }
        if let Ok(dv) = engine.retain_object_detached(child.objthis) {
            let id = dv.raw_id() as u64;
            let _ = engine.call_member(array.raw_id(), "push", &[TjsValue::Retained(id)]);
            // `dv`'s retention was consumed by the argument copy; its drop is
            // a safe no-op.
        }
    }
}

thread_local! {
    static INVOKING_ACTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn invoke_action(item: &MenuItemInst) {
    if !can_deliver(item) || INVOKING_ACTION.with(|active| active.replace(true)) {
        return;
    }
    let engine = context_engine();
    // Prefer a script-assigned `item.onClick`. The native fallback method
    // re-enters this function, and the guard above prevents recursion.
    let mut delivered = false;
    if let Ok(self_value) = engine.retain_object_detached(item.objthis) {
        delivered = engine
            .call_member(self_value.raw_id(), "onClick", &[])
            .is_ok();
    }
    if !delivered && let Some(owner) = item.action_owner.as_ref() {
        delivered = engine.call_member(owner.raw_id(), "onClick", &[]).is_ok();
        if !delivered {
            let _ = engine.call_detached(owner, &[]);
        }
    }
    INVOKING_ACTION.with(|active| active.set(false));
}

extern "C" fn menu_item_add(
    _e: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    err: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    if values.is_empty() {
        return crate::report_error(err, "MenuItem.add requires an item");
    }
    let index = unsafe {
        let mut v = std::mem::zeroed::<Value>();
        v.ty = tjs2_sys::VAL_INTEGER;
        v.integer = (*(instance as *mut MenuItemInst)).children.len() as i64;
        v
    };
    let mut call_args = [values[0], index];
    menu_item_insert(
        std::ptr::null_mut(),
        instance,
        2,
        call_args.as_mut_ptr(),
        out,
        err,
        objthis,
    )
}

fn detach_child(engine: &Tjs2Engine, parent_ptr: *mut MenuItemInst, child_ptr: *mut MenuItemInst) {
    let parent = unsafe { &mut *parent_ptr };
    if let Some(index) = parent
        .children
        .iter()
        .position(|link| link.ptr == child_ptr)
    {
        let link = parent.children.remove(index);
        unsafe { (*child_ptr).parent = std::ptr::null_mut() };
        drop(link.keep);
        sync_children(engine, parent);
    }
}

extern "C" fn menu_item_insert(
    _e: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    if values.len() < 2 {
        return crate::report_error(err, "MenuItem.insert requires item and index");
    }
    let engine = context_engine();
    let Some(child_ptr) = item_ptr_from_arg(&values[0]) else {
        return crate::report_error(err, "MenuItem.insert expects a MenuItem");
    };
    let parent_ptr = instance as *mut MenuItemInst;
    let mut p = parent_ptr;
    while !p.is_null() {
        if p == child_ptr {
            return crate::report_error(err, "MenuItem.insert would create a cycle");
        }
        p = unsafe { (*p).parent };
    }
    if !unsafe { (*child_ptr).parent }.is_null() {
        let old = unsafe { (*child_ptr).parent };
        detach_child(engine, old, child_ptr);
    }
    let keep = match engine.retain_object_detached(unsafe { (*child_ptr).objthis }) {
        Ok(k) => k,
        Err(e) => return crate::report_error(err, &format!("MenuItem.insert: {e}")),
    };
    let parent = unsafe { &mut *parent_ptr };
    let index = value_as_i64(&values[1]).max(0) as usize;
    let index = index.min(parent.children.len());
    parent.children.insert(
        index,
        ChildLink {
            ptr: child_ptr,
            keep,
        },
    );
    unsafe { (*child_ptr).parent = parent_ptr };
    sync_children(engine, parent);
    set_void_out(out);
    0
}

extern "C" fn menu_item_remove(
    _e: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    let Some(value) = values.first() else {
        return crate::report_error(err, "MenuItem.remove requires an item");
    };
    let engine = context_engine();
    let Some(child_ptr) = item_ptr_from_arg(value) else {
        return crate::report_error(err, "MenuItem.remove expects a MenuItem");
    };
    detach_child(engine, instance as *mut MenuItemInst, child_ptr);
    set_void_out(out);
    0
}

/// `_attachWindow(window)` — internal helper used by `Window.menu`'s setter to
/// (re)attach an already-created item as a window's root. Not part of the
/// reference surface.
extern "C" fn menu_item_attach_window(
    _e: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    let Some(window) = values.first() else {
        return crate::report_error(err, "MenuItem._attachWindow requires a window");
    };
    if window.ty != VAL_OBJECT || window.object_handle().is_null() {
        return crate::report_error(err, "MenuItem._attachWindow expects a Window object");
    }
    let engine = context_engine();
    let item = unsafe { &mut *(instance as *mut MenuItemInst) };
    attach_window_arg(engine, item, window);
    set_void_out(out);
    0
}

extern "C" fn menu_item_fire_click(
    _e: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    invoke_action(unsafe { &*(instance as *mut MenuItemInst) });
    set_void_out(out);
    0
}

extern "C" fn menu_item_on_click(
    _e: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    invoke_action(unsafe { &*(instance as *mut MenuItemInst) });
    set_void_out(out);
    0
}

/// `popup(flags, x, y)` (reference `MenuItemImpl.cpp:314`). There is no
/// native OS popup on this platform; instead the request is published for the
/// in-engine Bevy UI, which opens the item's dropdown. Returns `1`, matching
/// the reference's `TrackPopup` success value.
extern "C" fn menu_item_popup(
    _e: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return crate::report_error(err, "MenuItem.popup requires flags, x and y");
    }
    let values = args(_argv, argc);
    let item = unsafe { &*(instance as *const MenuItemInst) };
    let request = (
        instance as i64,
        value_as_i64(&values[1]) as i32,
        value_as_i64(&values[2]) as i32,
        snapshot_item(item as *const MenuItemInst),
    );
    *POPUP_REQUEST.lock().unwrap_or_else(|p| p.into_inner()) = Some(request);
    set_int_out(out, 1);
    0
}

macro_rules! int_prop {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _e: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _err: *mut *mut c_char,
            _obj: *mut c_void,
        ) -> c_int {
            set_int_out(out, unsafe { (*(instance as *mut MenuItemInst)).$field }
                as i64);
            0
        }
        extern "C" fn $set(
            _e: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _err: *mut *mut c_char,
            _obj: *mut c_void,
        ) -> c_int {
            unsafe {
                (*(instance as *mut MenuItemInst)).$field = value_as_i64(&*value);
            }
            0
        }
    };
}
macro_rules! bool_prop {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _e: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _err: *mut *mut c_char,
            _obj: *mut c_void,
        ) -> c_int {
            set_int_out(out, unsafe {
                (*(instance as *mut MenuItemInst)).$field as i64
            });
            0
        }
        extern "C" fn $set(
            _e: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _err: *mut *mut c_char,
            _obj: *mut c_void,
        ) -> c_int {
            unsafe {
                (*(instance as *mut MenuItemInst)).$field = value_as_bool(&*value);
            }
            0
        }
    };
}

extern "C" fn menu_checked_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    set_int_out(out, unsafe {
        (*(instance as *mut MenuItemInst)).checked as i64
    });
    0
}
extern "C" fn menu_checked_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    let item_ptr = instance as *mut MenuItemInst;
    let checked = value_as_bool(unsafe { &*value });
    if checked && unsafe { (*item_ptr).radio } && !unsafe { (*item_ptr).parent }.is_null() {
        let parent = unsafe { &*(*item_ptr).parent };
        for sibling in &parent.children {
            if sibling.ptr != item_ptr
                && unsafe { (*sibling.ptr).radio }
                && unsafe { (*sibling.ptr).group } == unsafe { (*item_ptr).group }
            {
                unsafe { (*sibling.ptr).checked = false };
            }
        }
    }
    unsafe {
        (*item_ptr).checked = checked;
    }
    0
}
bool_prop!(menu_enabled_get, menu_enabled_set, enabled);
bool_prop!(menu_radio_get, menu_radio_set, radio);
bool_prop!(menu_visible_get, menu_visible_set, visible);
int_prop!(menu_group_get, menu_group_set, group);

extern "C" fn menu_caption_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    set_string_out(out, unsafe { &(*(instance as *mut MenuItemInst)).caption });
    0
}
extern "C" fn menu_caption_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    unsafe {
        (*(instance as *mut MenuItemInst)).caption = value_as_string(&*value);
    }
    0
}
extern "C" fn menu_shortcut_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    set_string_out(out, unsafe { &(*(instance as *mut MenuItemInst)).shortcut });
    0
}
extern "C" fn menu_shortcut_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    unsafe {
        (*(instance as *mut MenuItemInst)).shortcut = value_as_string(&*value);
    }
    0
}

fn index_of(item: &MenuItemInst) -> usize {
    if item.parent.is_null() {
        0
    } else {
        unsafe {
            (*item.parent)
                .children
                .iter()
                .position(|c| std::ptr::eq(c.ptr, item))
                .unwrap_or(0)
        }
    }
}

extern "C" fn menu_index_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    let item = unsafe { &*(instance as *mut MenuItemInst) };
    set_int_out(out, index_of(item) as i64);
    0
}

/// `index` setter: move the item to the requested position within its parent
/// (reference `SetIndex` is a platform no-op, `MenuItemImpl.cpp:202`; the
/// logical tree can honor it).
extern "C" fn menu_index_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    let item_ptr = instance as *mut MenuItemInst;
    let item = unsafe { &mut *item_ptr };
    if item.parent.is_null() {
        return 0;
    }
    let engine = context_engine();
    let parent = unsafe { &mut *item.parent };
    let Some(current) = parent
        .children
        .iter()
        .position(|c| std::ptr::eq(c.ptr, item_ptr))
    else {
        return 0;
    };
    let target = (value_as_i64(unsafe { &*value }).max(0) as usize).min(parent.children.len() - 1);
    if current == target {
        return 0;
    }
    let link = parent.children.remove(current);
    parent.children.insert(target, link);
    sync_children(engine, parent);
    0
}

extern "C" fn menu_parent_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    let item = unsafe { &*(instance as *mut MenuItemInst) };
    let engine = context_engine();
    if item.parent.is_null() {
        set_null_out(out);
        0
    } else {
        return_object(engine, out, unsafe { (*item.parent).objthis })
    }
}
extern "C" fn menu_root_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    let ptr = root_ptr(instance as *mut MenuItemInst);
    return_object(context_engine(), out, unsafe { (*ptr).objthis })
}
extern "C" fn menu_children_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    let item = unsafe { &*(instance as *mut MenuItemInst) };
    let Some(name) = item.children_global.as_ref() else {
        set_void_out(out);
        return 0;
    };
    let engine = context_engine();
    match engine.eval_retained(&format!("global.{name}"), "MenuItem.children") {
        Ok(RetainedValue::Object(array)) => {
            set_retained(out, array);
            0
        }
        Ok(RetainedValue::Value(_)) => {
            set_void_out(out);
            0
        }
        Err(e) => crate::report_error(out_error, &format!("MenuItem.children: {e}")),
    }
}
/// `window` property (reference `MenuItemIntf.cpp:514`): the window the item
/// is attached to, or `null`.
extern "C" fn menu_window_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    let item = unsafe { &*(instance as *mut MenuItemInst) };
    if item.window_handle.is_null() {
        set_null_out(out);
        0
    } else {
        return_object(context_engine(), out, item.window_handle)
    }
}

/// `HMENU` getter (reference `MenuItemImpl.cpp:409`): the platform menu handle
/// a plugin would use. `GetMenuItemHandleForPlugin()` returns `nullptr` in the
/// reference, but an in-engine menu still needs a stable, non-null identity so
/// plugins and the Bevy UI can address the item; the native-instance pointer
/// is exactly that (it is stable for the item's lifetime and is what
/// [`fire_menu_click`] accepts). Read-only.
extern "C" fn menu_hmenu_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    set_int_out(out, instance as usize as i64);
    0
}

// ---------------------------------------------------------------------------
// Shortcut tables (`CreateShortCutKeyCodeTable`, `MenuItemImpl.cpp:363`)
// ---------------------------------------------------------------------------

/// `(display name, virtual keycode)` pairs. The reference installs the three
/// unconditional `SetShortCutKeyCode` entries plus the `#if 0` Windows
/// `MapVirtualKey` loop; the explicit table below covers the same common keys
/// so `textToKeycode`/`keycodeToText` are genuinely usable (and round-trip).
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

/// Build the TJS initializer for the two static tables. Keys are lowercased
/// and the display list keeps the original case, exactly like
/// `SetShortCutKeyCode` (`MenuItemImpl.cpp:347`).
fn shortcut_table_script() -> String {
    let mut script = String::from("global.__krkr_menu_text_to_keycode = %[");
    for (display, code) in SHORTCUT_KEYS {
        script.push_str(&format!("'{}'=>{},", display.to_lowercase(), code));
    }
    script.push_str("];global.__krkr_menu_keycode_to_text = (function(){var a=[];");
    for (display, code) in SHORTCUT_KEYS {
        script.push_str(&format!("a[{code}]='{display}';"));
    }
    script.push_str("return a;})();");
    script
}

fn return_global_object(
    out: *mut Value,
    out_error: *mut *mut c_char,
    expr: &str,
    name: &str,
) -> c_int {
    let engine = context_engine();
    match engine.eval_retained(expr, name) {
        Ok(RetainedValue::Object(value)) => {
            set_retained(out, value);
            0
        }
        Ok(RetainedValue::Value(_)) => {
            set_void_out(out);
            0
        }
        Err(e) => crate::report_error(out_error, &format!("{name}: {e}")),
    }
}

/// Static getter for `MenuItem.textToKeycode` (`MenuItemImpl.cpp:422`).
extern "C" fn menu_text_to_keycode_get(
    _engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    return_global_object(
        out,
        out_error,
        "global.__krkr_menu_text_to_keycode",
        "MenuItem.textToKeycode",
    )
}

/// Static getter for `MenuItem.keycodeToText` (`MenuItemImpl.cpp:432`).
extern "C" fn menu_keycode_to_text_get(
    _engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    return_global_object(
        out,
        out_error,
        "global.__krkr_menu_keycode_to_text",
        "MenuItem.keycodeToText",
    )
}

// ---------------------------------------------------------------------------
// Shared registry API (consumed by `krkr_render::menu`)
// ---------------------------------------------------------------------------

fn snapshot_item(ptr: *const MenuItemInst) -> MenuItemSnapshot {
    let item = unsafe { &*ptr };
    MenuItemSnapshot {
        handle: ptr as i64,
        caption: item.caption.clone(),
        checked: item.checked,
        enabled: item.enabled,
        radio: item.radio,
        group: item.group,
        visible: item.visible,
        shortcut: item.shortcut.clone(),
        index: index_of(item),
        children: item
            .children
            .iter()
            .map(|link| snapshot_item(link.ptr))
            .collect(),
    }
}

/// Snapshot of one window's root menu, or `None` when the window has none.
pub fn menu_snapshot(window: u32) -> Option<MenuSnapshot> {
    let root = {
        WINDOW_ROOTS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&window)
            .copied()?
    };
    if root == 0
        || !LIVE_ITEMS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&root)
    {
        return None;
    }
    Some(MenuSnapshot {
        window,
        root: snapshot_item(root as *const MenuItemInst),
    })
}

/// Snapshots of every window's root menu, sorted by window id.
pub fn menu_snapshots() -> Vec<MenuSnapshot> {
    let mut windows: Vec<u32> = WINDOW_ROOTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .keys()
        .copied()
        .collect();
    windows.sort_unstable();
    windows.into_iter().filter_map(menu_snapshot).collect()
}

/// Activate the item addressed by `handle` (the `HMENU` value): runs the same
/// `onClick` path as `fireClick` (`MenuItemImpl.cpp:305`). Returns `false`
/// when the handle is unknown or the item is disabled.
pub fn fire_menu_click(handle: i64) -> bool {
    let addr = handle as usize;
    if !LIVE_ITEMS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&addr)
    {
        return false;
    }
    let item = unsafe { &*(addr as *const MenuItemInst) };
    if !can_deliver(item) {
        return false;
    }
    invoke_action(item);
    true
}

/// Consume the pending `popup(flags, x, y)` request, if any. The renderer
/// opens the captured item's submenu at the requested position when it sees
/// one.
pub fn take_popup_request() -> Option<PopupRequest> {
    POPUP_REQUEST
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
}

/// Register `MenuItem` on the engine global object.
pub fn register_menu_item(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
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
                name: "fireClick",
                f: menu_item_fire_click,
            },
            NativeInstanceMethodDef {
                name: "onClick",
                f: menu_item_on_click,
            },
            NativeInstanceMethodDef {
                name: "popup",
                f: menu_item_popup,
            },
            NativeInstanceMethodDef {
                name: "_attachWindow",
                f: menu_item_attach_window,
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

    // Static (class-level) members, reference
    // `TJS_END_NATIVE_STATIC_PROP_DECL_OUTER(cls, textToKeycode)`.
    engine
        .exec_script(&shortcut_table_script(), "MenuItem.shortcutTables")
        .map_err(|e| format!("MenuItem: failed to install shortcut tables: {e}"))?;
    engine.register_native_static_members(&NativeStaticMembers {
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

    // Helpers used by `Window.menu` (crates/tvp-visual) so that crate does not
    // need a dependency on this one. `__krkr_native_MenuItem` lets the window
    // getter tell the native class apart from its script fallback.
    engine
        .exec_script(
            "global.__krkr_native_MenuItem = 1;\
             global.__krkr_make_window_menu = function(w) {\
                 var m = new MenuItem(w, w);\
                 global['__krkr_window_menu_' + w.id] = m;\
                 return m;\
             };\
             global.__krkr_set_window_menu = function(w, m) {\
                 global['__krkr_window_menu_' + w.id] = m;\
                 m._attachWindow(w);\
                 return m;\
             };",
            "MenuItem.windowHelpers",
        )
        .map_err(|e| format!("MenuItem: failed to install window helpers: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::register_all;
    use super::super::test_lock::vm_lock;
    use super::{fire_menu_click, menu_snapshot, menu_snapshots, take_popup_request};
    use tjs2_sys::{Tjs2Engine, TjsValue};

    #[test]
    fn menu_tree_state_and_radio_group() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        engine.exec_script("var root = new MenuItem(); var a = new MenuItem(null, 'A'); var b = new MenuItem(null, 'B');", "menu").unwrap();
        engine.exec_script("root.add(a);", "menu").unwrap();
        engine.exec_script("root.insert(b, 0); a.radio = true; b.radio = true; a.group = 2; b.group = 2; b.checked = true;", "menu").unwrap();
        assert_eq!(
            engine.eval("b.parent === root", "menu").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            engine.eval("b.root === root", "menu").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            engine.eval("b.index", "menu").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            engine.eval("b.checked", "menu").unwrap(),
            TjsValue::Integer(1)
        );
        // `children` is a live TJS array of item objects.
        assert_eq!(
            engine.eval("root.children.length", "menu").unwrap(),
            TjsValue::Integer(2)
        );
        assert_eq!(
            engine.eval("root.children[0] === b", "menu").unwrap(),
            TjsValue::Integer(1)
        );
        // Radio group: checking one clears its sibling.
        assert_eq!(
            engine.eval("a.checked", "menu").unwrap(),
            TjsValue::Integer(0)
        );
    }

    #[test]
    fn menu_remove_and_index_move() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        engine
            .exec_script(
                "var root = new MenuItem();\
                 var a = new MenuItem(null, 'A'); var b = new MenuItem(null, 'B'); var c = new MenuItem(null, 'C');\
                 root.add(a); root.add(b); root.add(c);",
                "menu",
            )
            .unwrap();
        assert_eq!(
            engine.eval("a.index", "menu").unwrap(),
            TjsValue::Integer(0)
        );
        engine.exec_script("a.index = 2;", "menu").unwrap();
        assert_eq!(
            engine.eval("a.index", "menu").unwrap(),
            TjsValue::Integer(2)
        );
        assert_eq!(
            engine.eval("root.children[2] === a", "menu").unwrap(),
            TjsValue::Integer(1)
        );
        engine.exec_script("root.remove(b);", "menu").unwrap();
        assert_eq!(
            engine.eval("root.children.length", "menu").unwrap(),
            TjsValue::Integer(2)
        );
        assert_eq!(
            engine.eval("b.parent === null", "menu").unwrap(),
            TjsValue::Integer(1)
        );
    }

    #[test]
    fn menu_properties_and_fire_click_are_headless_safe() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        engine.exec_script("var n = 0; var m = new MenuItem(null, 'Open'); m.onClick = function() { n++; }; m.shortcut = 'Ctrl+O'; m.enabled = false; m.fireClick(); m.enabled = true; m.fireClick();", "menu").unwrap();
        assert_eq!(
            engine.eval("m.caption", "menu").unwrap(),
            TjsValue::String("Open".into())
        );
        assert_eq!(
            engine.eval("m.shortcut", "menu").unwrap(),
            TjsValue::String("Ctrl+O".into())
        );
        assert_eq!(engine.eval("n", "menu").unwrap(), TjsValue::Integer(1));
    }

    /// `HMENU` is a stable, non-null synthetic handle; the shortcut tables
    /// round-trip.
    #[test]
    fn hmenu_and_shortcut_tables() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        engine
            .exec_script("var m = new MenuItem();", "menu")
            .unwrap();
        let hmenu = match engine.eval("m.HMENU", "menu").unwrap() {
            TjsValue::Integer(v) => v,
            other => panic!("HMENU must be an integer, got {other:?}"),
        };
        assert_ne!(hmenu, 0, "HMENU must expose the attached item handle");
        assert_eq!(
            engine.eval("m.HMENU", "menu").unwrap(),
            TjsValue::Integer(hmenu),
            "HMENU must be stable"
        );
        // HMENU is read-only.
        assert!(engine.eval("m.HMENU = 1", "menu").is_err());

        // Reference entries survive.
        assert_eq!(
            engine
                .eval("MenuItem.textToKeycode['bksp']", "menu")
                .unwrap(),
            TjsValue::Integer(8)
        );
        assert_eq!(
            engine.eval("MenuItem.keycodeToText[8]", "menu").unwrap(),
            TjsValue::String("BkSp".into())
        );
        // Full round-trip over the table.
        for (key, code) in [
            ("a", 65),
            ("z", 90),
            ("f1", 112),
            ("f12", 123),
            ("enter", 13),
            ("esc", 27),
            ("pgup", 33),
            ("pgdn", 34),
        ] {
            assert_eq!(
                engine
                    .eval(&format!("MenuItem.textToKeycode['{key}']"), "menu")
                    .unwrap(),
                TjsValue::Integer(code),
                "textToKeycode[{key}]"
            );
        }
        assert_eq!(
            engine.eval("MenuItem.keycodeToText[65]", "menu").unwrap(),
            TjsValue::String("A".into())
        );
        assert_eq!(
            engine.eval("MenuItem.keycodeToText[123]", "menu").unwrap(),
            TjsValue::String("F12".into())
        );
        // The tables are read-only static properties.
        assert!(engine.eval("MenuItem.textToKeycode = %[]", "menu").is_err());
    }

    #[test]
    fn window_menu_root_is_registered_for_renderer() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        engine
            .exec_script(
                "var w = %[id: 7];\
                 var root = __krkr_make_window_menu(w);\
                 var open = new MenuItem(null, 'Open');\
                 var quit = new MenuItem(null, 'Quit'); quit.shortcut = 'Ctrl+Q';\
                 root.add(open); root.add(quit);",
                "menu",
            )
            .unwrap();
        let snapshot = menu_snapshot(7).expect("window 7 must have a root menu");
        assert_eq!(snapshot.root.children.len(), 2);
        assert_eq!(snapshot.root.children[0].caption, "Open");
        assert_eq!(snapshot.root.children[1].shortcut, "Ctrl+Q");
        assert_eq!(menu_snapshots().len(), 1);

        // Activation through the renderer-facing handle fires the callback.
        engine
            .exec_script(
                "var fired = 0; open.onClick = function() { fired++; };",
                "menu",
            )
            .unwrap();
        let handle = snapshot.root.children[0].handle;
        assert!(fire_menu_click(handle));
        assert_eq!(engine.eval("fired", "menu").unwrap(), TjsValue::Integer(1));
    }

    #[test]
    fn popup_request_is_published() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        engine
            .exec_script("var m = new MenuItem();", "menu")
            .unwrap();
        // Reference requires flags, x, y.
        assert!(engine.eval("m.popup()", "menu").is_err());
        engine.exec_script("m.popup(0, 10, 20);", "menu").unwrap();
        let handle = match engine.eval("m.HMENU", "menu").unwrap() {
            TjsValue::Integer(v) => v,
            other => panic!("{other:?}"),
        };
        // The request keeps the requested client position so the in-engine
        // UI can open the context menu where `popup(flags, x, y)` asked.
        let (req_handle, x, y, item) = take_popup_request().expect("popup must be pending");
        assert_eq!(req_handle, handle);
        assert_eq!((x, y), (10, 20));
        assert_eq!(item.handle, handle);
        assert_eq!(take_popup_request(), None);
    }

    /// A detached `MenuItem` (never added to a window root) still publishes a
    /// complete snapshot with its children, which is what the renderer needs
    /// to draw a `popup()` context menu without a window tree.
    #[test]
    fn popup_captures_detached_item_tree() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        engine
            .exec_script(
                "var root = new MenuItem(null, 'Context');\
                 root.add(new MenuItem(null, 'Copy'));\
                 var past = new MenuItem(null, 'Paste'); past.enabled = false;\
                 root.add(past);\
                 root.popup(0, 4, 8);",
                "menu",
            )
            .unwrap();
        let (handle, x, y, snapshot) = take_popup_request().expect("detached popup must publish");
        assert_eq!(snapshot.handle, handle);
        assert_eq!(snapshot.caption, "Context");
        assert_eq!(snapshot.children.len(), 2);
        assert_eq!(snapshot.children[0].caption, "Copy");
        assert!(!snapshot.children[1].enabled);
        assert_eq!((x, y), (4, 8));
        assert!(take_popup_request().is_none());
    }
}
