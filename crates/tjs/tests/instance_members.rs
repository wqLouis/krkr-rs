//! Integration tests for `tjs2_register_native_instance_members` —
//! attaching instance members (without a native payload) to an
//! already-existing class, including TJS2 builtins such as `Array`
//! (reference `NCB_ATTACH_CLASS` / `NCB_ATTACH_FUNCTION`).
//!
//! The callbacks receive only `objthis` (the object the member was invoked
//! on), so the tests prove the object is threaded through by comparing the
//! pointer the member observes with the raw object handle the VM marshals for
//! the same object as a native method argument.

use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard};

use tjs::{
    NativeClassBuilder, NativeInstanceBuilder, NativeInstanceMembers, NativeInstanceMethodDef,
    NativeMethodDef, NativeObjThisMethodDef, NativeObjThisPropertyDef, Tjs2Engine, TjsValue,
    VAL_INTEGER, VAL_VOID, Value,
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
unsafe fn set_integer(out: *mut Value, v: i64) {
    // SAFETY: out is a valid return slot for the duration of the callback.
    unsafe {
        (*out).ty = VAL_INTEGER;
        (*out).integer = v;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

// ---------------------------------------------------------------------------
// A tiny native instance class to attach the objthis-only member to.
// ---------------------------------------------------------------------------

/// `Widget` payload: a single i32. Destroyed exactly once.
extern "C" fn widget_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(0i32)) as *mut c_void
}

extern "C" fn widget_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from widget_create (Box::into_raw).
    unsafe { drop(Box::from_raw(instance as *mut i32)) };
}

/// `Widget()` constructor member, needed so `new Widget()` works.
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

fn widget_builder() -> NativeInstanceBuilder<'static> {
    NativeInstanceBuilder {
        name: "Widget",
        create: widget_create,
        destroy: widget_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "Widget",
            f: widget_ctor,
        }],
        properties: vec![],
    }
}

// ---------------------------------------------------------------------------
// A static `Probe` class: returns the raw object handle of its argument, so a
// test can prove the `objthis` seen by an objthis-only member is the same
// object.
// ---------------------------------------------------------------------------

extern "C" fn probe_handle(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        unsafe { *out_error = ptr::null_mut() };
        return 1;
    }
    // SAFETY: argv points at argc entries for the duration of the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    // SAFETY: out is a valid return slot.
    unsafe { set_integer(out, args[0].object_handle() as usize as i64) };
    0
}

fn probe_builder() -> NativeClassBuilder<'static> {
    NativeClassBuilder {
        name: "Probe",
        methods: vec![NativeMethodDef {
            name: "handle",
            f: probe_handle,
        }],
        properties: vec![],
    }
}

// ---------------------------------------------------------------------------
// objthis-only members
// ---------------------------------------------------------------------------

/// Returns the raw `objthis` pointer as an integer.
extern "C" fn self_ptr(
    _engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: out is a valid return slot.
    unsafe { set_integer(out, objthis as usize as i64) };
    0
}

/// `stamp` property backing store (setter records, getter returns it).
static STAMP: AtomicI64 = AtomicI64::new(0);

extern "C" fn stamp_get(
    _engine: *mut c_void,
    _objthis: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: out is a valid return slot.
    unsafe { set_integer(out, STAMP.load(Ordering::SeqCst)) };
    0
}

extern "C" fn stamp_set(
    _engine: *mut c_void,
    _objthis: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: value is valid for the duration of the call.
    let v = unsafe { &*value };
    STAMP.store(v.integer, Ordering::SeqCst);
    0
}

// ---------------------------------------------------------------------------
// (a) attach to our own native instance class
// ---------------------------------------------------------------------------

#[test]
fn instance_member_attaches_to_a_native_instance_class_with_objthis() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class(&probe_builder()).unwrap();
    e.register_native_class_instance(&widget_builder()).unwrap();

    // The `selfPtr` member receives objthis but no native payload.
    e.register_native_instance_members(&NativeInstanceMembers {
        class_name: "Widget",
        methods: vec![NativeObjThisMethodDef {
            name: "selfPtr",
            f: self_ptr,
        }],
        properties: vec![],
    })
    .unwrap();

    // The object seen through objthis is the same object the VM marshals for
    // the same value as a method argument.
    e.exec_script(
        "var w = new Widget(); var viaObjthis = w.selfPtr(); \
         var viaArg = Probe.handle(w); var same = (viaObjthis == viaArg);",
        "test",
    )
    .unwrap();
    assert_ne!(
        e.eval("viaObjthis", "test").unwrap(),
        TjsValue::Integer(0),
        "objthis must be non-null"
    );
    assert_eq!(e.eval("same", "test").unwrap(), TjsValue::Integer(1));

    // Two distinct instances see distinct objthis values.
    e.exec_script(
        "var a = new Widget(); var b = new Widget(); \
         var distinct = (a.selfPtr() != b.selfPtr());",
        "test",
    )
    .unwrap();
    assert_eq!(e.eval("distinct", "test").unwrap(), TjsValue::Integer(1));
}

// ---------------------------------------------------------------------------
// (b) attach to the TJS2 builtin `Array`
// ---------------------------------------------------------------------------

#[test]
fn instance_member_attaches_to_builtin_array_and_is_callable() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class(&probe_builder()).unwrap();

    e.register_native_instance_members(&NativeInstanceMembers {
        class_name: "Array",
        methods: vec![NativeObjThisMethodDef {
            name: "selfPtr",
            f: self_ptr,
        }],
        properties: vec![NativeObjThisPropertyDef {
            name: "stamp",
            get: Some(stamp_get),
            set: Some(stamp_set),
        }],
    })
    .unwrap();

    // The member is callable on an array instance, and objthis is that array.
    e.exec_script(
        "var a = [1, 2, 3]; var viaObjthis = a.selfPtr(); \
         var viaArg = Probe.handle(a); var same = (viaObjthis == viaArg);",
        "test",
    )
    .unwrap();
    assert_ne!(
        e.eval("viaObjthis", "test").unwrap(),
        TjsValue::Integer(0),
        "objthis must be non-null"
    );
    assert_eq!(e.eval("same", "test").unwrap(), TjsValue::Integer(1));

    // Properties attached the same way work on array instances too.
    STAMP.store(0, Ordering::SeqCst);
    e.exec_script("var z = [9]; z.stamp = 77; var s = z.stamp;", "test")
        .unwrap();
    assert_eq!(e.eval("s", "test").unwrap(), TjsValue::Integer(77));

    // A different builtin (Dictionary) is untouched by the Array attach.
    let err = e.eval("%[].selfPtr()", "test").unwrap_err();
    assert!(!err.to_string().is_empty(), "expected an error: {err}");
}

#[test]
fn instance_members_reject_an_unknown_class() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    let err = e
        .register_native_instance_members(&NativeInstanceMembers {
            class_name: "NoSuchClass",
            methods: vec![NativeObjThisMethodDef {
                name: "m",
                f: self_ptr,
            }],
            properties: vec![],
        })
        .unwrap_err();
    assert!(err.contains("NoSuchClass"), "unexpected error: {err}");
}
