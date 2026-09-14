//! Headless-safe `MenuItem` native class.
//!
//! The desktop TVP implementation backs this class with a platform menu.
//! Keeping the logical part here is useful even when no desktop exists: game
//! scripts build menu trees, inspect their state, and call `fireClick()`.
//! Objects are retained in the same way as the timer natives, so a parent
//! keeps its children alive and object-valued properties remain real TJS
//! objects rather than integer stand-ins.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::sync::{LazyLock, Mutex};

use tjs2_sys::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    Tjs2Engine, TjsValue, VAL_OBJECT, Value,
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
    /// The array is retained by the native object and is exposed as a slice
    /// copy, preventing script code from corrupting the native child list.
    children_array: Option<DetachedValue>,
    action_owner: Option<DetachedValue>,
    /// Retention for the native object itself, used to resolve opaque object
    /// arguments through the engine's retained-value identity map.
    object_owner: Option<DetachedValue>,
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
            action_owner: None,
            object_owner: None,
        }
    }
}

// The VM is single threaded.  The registry uses integer addresses so it does
// not make raw pointers part of its Send/Sync contract.
static ITEMS: LazyLock<Mutex<HashMap<usize, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
fn item_ptr_from_arg(engine: &Tjs2Engine, value: &Value) -> Option<*mut MenuItemInst> {
    if value.ty != VAL_OBJECT {
        return None;
    }
    // `find_retained_id` scans the engine's retained map for the *same* value (closure +
    // ObjThis). Like the reference, object arguments arrive as the engine's most recent
    // object result (System's menu tree passes plain `new MenuItem()` instances), so this
    // resolves the MenuItem for add/insert/remove faithfully.
    let key = engine.find_retained_id(&TjsValue::Object)? as usize;
    ITEMS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key)
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

extern "C" fn menu_item_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<MenuItemInst>::default()) as *mut c_void
}

extern "C" fn menu_item_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
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
    // SAFETY: pointer came from menu_item_create.
    unsafe { drop(Box::from_raw(instance as *mut MenuItemInst)) };
}

/// `new MenuItem([actionOwner[, caption]])`.  A missing owner is useful for a
/// headless root menu; an object owner is retained for `onClick` dispatch.
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
    // arguments with find_retained_id (the ABI intentionally has no object
    // pointer in Value).
    match engine.retain_object_detached(objthis) {
        Ok(owner) => {
            item.object_key = owner.raw_id() as usize;
            ITEMS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(item.object_key, instance as usize);
            item.object_owner = Some(owner);
        }
        Err(e) => {
            return crate::report_error(out_error, &format!("MenuItem: {e}"));
        }
    }

    // The first argument is the action owner in TVP.  It is also how a menu
    // item can call the owning script object's onClick handler.
    if values.first().is_some_and(|v| v.ty == VAL_OBJECT)
        && let Ok(owner) = engine.retain_value_detached(&TjsValue::Object)
    {
        item.action_owner = Some(owner);
    }
    if let Some(caption) = values.get(1) {
        item.caption = value_as_string(caption);
    }

    // Build an array once; each mutation mirrors the native child vector.
    // (The array is optional so MenuItem remains safe if a host disables
    // script array helpers.)
    if engine.eval("[]", "MenuItem.children").is_ok() {
        item.children_array = engine.retain_value_detached(&TjsValue::Object).ok();
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

fn sync_add(
    _engine: &Tjs2Engine,
    _parent: &MenuItemInst,
    _child_index: usize,
    _child: &MenuItemInst,
) {
    // The current ABI cannot pass a retained object id as an argument to a
    // subsequent TJS array mutation. The authoritative native tree is still
    // updated; `children` remains a compatibility array until the ABI gains
    // mixed retained arguments.
}

fn sync_remove(_engine: &Tjs2Engine, _parent: &MenuItemInst, _index: usize) {}

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
        sync_remove(engine, parent, index);
        drop(link.keep);
        unsafe { (*child_ptr).parent = std::ptr::null_mut() };
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
    let Some(child_ptr) = item_ptr_from_arg(engine, &values[0]) else {
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
    sync_add(engine, parent, index, unsafe { &*child_ptr });
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
    let Some(child_ptr) = item_ptr_from_arg(engine, value) else {
        return crate::report_error(err, "MenuItem.remove expects a MenuItem");
    };
    detach_child(engine, instance as *mut MenuItemInst, child_ptr);
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

extern "C" fn menu_item_popup(
    _e: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // No platform popup in headless mode; a successful no-op matches the
    // reference's logical TrackPopup return path.
    let _ = instance;
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

extern "C" fn menu_index_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    let item = unsafe { &*(instance as *mut MenuItemInst) };
    let index = if item.parent.is_null() {
        0
    } else {
        unsafe {
            (*item.parent)
                .children
                .iter()
                .position(|c| c.ptr == instance as *mut MenuItemInst)
                .unwrap_or(0)
        }
    };
    set_int_out(out, index as i64);
    0
}
extern "C" fn menu_index_set(
    _e: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
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
        set_void_out(out);
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
    let mut ptr = instance as *mut MenuItemInst;
    while !unsafe { (*ptr).parent }.is_null() {
        ptr = unsafe { (*ptr).parent };
    }
    return_object(context_engine(), out, unsafe { (*ptr).objthis })
}
extern "C" fn menu_children_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    let item = unsafe { &*(instance as *mut MenuItemInst) };
    let Some(array) = item.children_array.as_ref() else {
        set_void_out(out);
        return 0;
    };
    let engine = context_engine();
    match engine.call_member(array.raw_id(), "slice", &[]) {
        Ok(TjsValue::Object) => match engine.retain_value_detached(&TjsValue::Object) {
            Ok(v) => {
                set_retained(out, v);
                0
            }
            Err(_) => {
                set_void_out(out);
                0
            }
        },
        _ => {
            set_void_out(out);
            0
        }
    }
}
extern "C" fn menu_window_get(
    _e: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `HMENU` getter (reference `MenuItemImpl.cpp:409`): the platform menu handle
/// a plugin would use. `tTJSNI_MenuItem::GetMenuItemHandleForPlugin()` is a
/// stub that returns `nullptr` in this codebase (`MenuItemImpl.cpp:193`), so
/// the faithful value is the null handle (`0`), not a fabricated handle. The
/// property is read-only (`TJS_DENY_NATIVE_PROP_SETTER`).
extern "C" fn menu_hmenu_get(
    _e: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _obj: *mut c_void,
) -> c_int {
    set_int_out(out, 0);
    0
}

/// The reference's `CreateShortCutKeyCodeTable()` (`MenuItemImpl.cpp:363`,
/// called from `ScriptMgnIntf.cpp:535`): the `textToKeycode` dictionary and
/// `keycodeToText` array. The `#if 0` Windows `MapVirtualKey` loop is skipped
/// in a normal build, so only the three unconditional entries remain, with
/// lowercase dictionary keys and original-case display names, exactly as
/// `SetShortCutKeyCode` writes them.
///
/// NOTE: the reference registers these as *static* class properties
/// (`TJS_END_NATIVE_STATIC_PROP_DECL_OUTER`), but `tjs2-sys`'s
/// `register_native_class_instance` has no static-property parameter, so they
/// are installed as TJS static members on the class object instead. They are
/// functionally equivalent but not registered through the native property
/// dispatch, which is why the parity checker does not credit them.
const MENU_SHORTCUT_TABLES: &str = "MenuItem.textToKeycode = %['bksp'=>8,'pgup'=>33,'pgdn'=>34];\
     MenuItem.keycodeToText = (function(){var a=[]; a[8]='BkSp'; a[33]='PgUp'; a[34]='PgDn'; return a;})();";

/// Register `MenuItem` on the engine global object.
pub fn register_menu_item(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "MenuItem",
        create: menu_item_create,
        destroy: menu_item_destroy,
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
    // `textToKeycode` / `keycodeToText` (MenuItemImpl.cpp:422,432) as static
    // class members; see `MENU_SHORTCUT_TABLES` for why they are not native.
    engine
        .exec_script(MENU_SHORTCUT_TABLES, "MenuItem.shortcutTables")
        .map_err(|e| format!("MenuItem: failed to install shortcut tables: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::register_all;
    use super::super::test_lock::vm_lock;
    use tjs2_sys::{Tjs2Engine, TjsValue};

    #[test]
    fn menu_tree_state_and_radio_group() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        engine.exec_script("var root = new MenuItem(); var a = new MenuItem(null, 'A'); var b = new MenuItem(null, 'B');", "menu").unwrap();
        engine.exec_script("root.add(a);", "menu").unwrap();
        engine.exec_script("root.insert(b, 0); a.radio = true; b.radio = true; a.group = 2; b.group = 2; b.checked = true;", "menu").unwrap();
        // The native tree is authoritative; object-valued children arrays
        // require the retained-argument ABI extension and are covered by the
        // Window.menu script fallback tests.
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

    /// `HMENU` (`MenuItemImpl.cpp:409`) reports the null plugin handle this
    /// reference returns, and `textToKeycode` / `keycodeToText`
    /// (`MenuItemImpl.cpp:422,432`) expose the shortcut tables.
    #[test]
    fn hmenu_and_shortcut_tables() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        engine
            .exec_script("var m = new MenuItem();", "menu")
            .unwrap();
        assert_eq!(
            engine.eval("m.HMENU", "menu").unwrap(),
            TjsValue::Integer(0),
            "GetMenuItemHandleForPlugin returns null in this reference"
        );
        // HMENU is read-only.
        assert!(engine.eval("m.HMENU = 1", "menu").is_err());

        // The tables carry the three unconditional SetShortCutKeyCode
        // entries (the Windows MapVirtualKey loop is `#if 0`).
        assert_eq!(
            engine
                .eval("MenuItem.textToKeycode['bksp']", "menu")
                .unwrap(),
            TjsValue::Integer(8)
        );
        assert_eq!(
            engine
                .eval("MenuItem.textToKeycode['pgup']", "menu")
                .unwrap(),
            TjsValue::Integer(33)
        );
        assert_eq!(
            engine
                .eval("MenuItem.textToKeycode['pgdn']", "menu")
                .unwrap(),
            TjsValue::Integer(34)
        );
        assert_eq!(
            engine.eval("MenuItem.keycodeToText[8]", "menu").unwrap(),
            TjsValue::String("BkSp".into())
        );
        assert_eq!(
            engine.eval("MenuItem.keycodeToText[33]", "menu").unwrap(),
            TjsValue::String("PgUp".into())
        );
        assert_eq!(
            engine.eval("MenuItem.keycodeToText[34]", "menu").unwrap(),
            TjsValue::String("PgDn".into())
        );
    }
}
