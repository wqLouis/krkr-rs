//! Integration test for `Tjs2Engine::enum_members` — the ABI that walks
//! `iTJSDispatch2::EnumMembers` (reference `Scripts.getObjectKeys`).
//!
//! A native class registers its members on the class object; this proves the
//! binding lists those registered names on both the class object and a
//! constructed instance.

use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::{Mutex, MutexGuard};

use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, RetainedValue,
    Tjs2Engine, TjsValue, VAL_INTEGER, VAL_VOID, Value,
};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Writes a void return and reports success.
unsafe fn set_void(out: *mut Value) {
    // SAFETY: out is a valid return slot for the duration of the callback.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Writes an integer return and reports success.
unsafe fn set_integer(out: *mut Value, value: i64) {
    // SAFETY: out is a valid return slot for the duration of the callback.
    unsafe {
        (*out).ty = VAL_INTEGER;
        (*out).integer = value;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

// ---------------------------------------------------------------------------
// A small native class with two methods and one property.
// ---------------------------------------------------------------------------

extern "C" fn widget_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(0i32)) as *mut c_void
}

extern "C" fn widget_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from widget_create (Box::into_raw).
    unsafe { drop(Box::from_raw(instance as *mut i32)) };
}

extern "C" fn widget_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: out is a valid return slot.
    unsafe { set_void(out) };
    0
}

extern "C" fn widget_ping(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: out is a valid return slot.
    unsafe { set_integer(out, 1) };
    0
}

extern "C" fn widget_get_size(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: out is a valid return slot.
    unsafe { set_integer(out, 42) };
    0
}

fn widget_builder() -> NativeInstanceBuilder<'static> {
    NativeInstanceBuilder {
        name: "EnumWidget",
        create: widget_create,
        destroy: widget_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "EnumWidget",
                f: widget_ctor,
            },
            NativeInstanceMethodDef {
                name: "ping",
                f: widget_ping,
            },
        ],
        properties: vec![NativeInstancePropertyDef {
            name: "size",
            get: Some(widget_get_size),
            set: None,
        }],
    }
}

#[test]
fn enum_members_lists_a_native_class_surface() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class_instance(&widget_builder()).unwrap();

    // The class object as seen from the global object.
    let class = match e.eval_retained("EnumWidget", "test").unwrap() {
        RetainedValue::Object(class) => class,
        RetainedValue::Value(other) => panic!("EnumWidget is not an object: {other:?}"),
    };
    let class_members = e.enum_members(class.raw_id()).unwrap();
    assert!(
        class_members.iter().any(|m| m == "ping"),
        "class members must include `ping`: {class_members:?}"
    );
    assert!(
        class_members.iter().any(|m| m == "size"),
        "class members must include `size`: {class_members:?}"
    );
    // The class-name constructor is registered as a member too.
    assert!(
        class_members.iter().any(|m| m == "EnumWidget"),
        "class members must include the constructor: {class_members:?}"
    );

    // A constructed instance carries the (non-static) members.
    let instance = match e.eval_retained("new EnumWidget()", "test").unwrap() {
        RetainedValue::Object(instance) => instance,
        RetainedValue::Value(other) => panic!("new EnumWidget() is not an object: {other:?}"),
    };
    let instance_members = e.enum_members(instance.raw_id()).unwrap();
    assert!(
        instance_members.iter().any(|m| m == "ping"),
        "instance members must include `ping`: {instance_members:?}"
    );
    assert!(
        instance_members.iter().any(|m| m == "size"),
        "instance members must include `size`: {instance_members:?}"
    );

    // The members really resolve at runtime, so the enumeration is not just
    // a name list. (`new X().m` parses as `new (X().m)`, so bind first.)
    e.exec_script("var w = new EnumWidget();", "test").unwrap();
    assert_eq!(e.eval("w.ping()", "test").unwrap(), TjsValue::Integer(1));
    assert_eq!(e.eval("w.size", "test").unwrap(), TjsValue::Integer(42));
}

#[test]
fn enum_members_rejects_a_non_object_retained_value() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    let value = e.eval("41", "test").unwrap();
    let id = e.retain_value_detached(&value).unwrap();
    assert!(e.enum_members(id.raw_id()).is_err());
}
