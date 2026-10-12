//! Integration tests for [`Tjs2Engine::native_instance_payload`] — the ABI
//! entry point that resolves a retained script object to the Rust payload
//! of one of our instance-based native classes (the C++
//! `NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)` + `GetNativePtr()`
//! lookup, exposed so natives can map "script object" -> "our payload"
//! without script-visible `id`/`nativeId` helper properties).

use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::{Mutex, MutexGuard};

use tjs::{
    DetachedValue, NativeClassBuilder, NativeInstanceBuilder, NativeInstanceMethodDef,
    RetainedValue, Tjs2Engine, VAL_VOID, Value,
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

// ---------------------------------------------------------------------------
// `Widget`: an instance-based native class whose payload is a magic i32, so
// a test can prove the looked-up pointer *is* the payload create produced.
// ---------------------------------------------------------------------------

const WIDGET_MAGIC: i32 = 0x00C0_FFEE;

extern "C" fn widget_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(WIDGET_MAGIC)) as *mut c_void
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
// `Gadget`: a second instance class, so a lookup can be tested against an
// object that *is* a native instance — just not of the queried class.
// ---------------------------------------------------------------------------

const GADGET_MAGIC: u64 = 0xFEED_FACE_0BAD_F00D;

extern "C" fn gadget_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(GADGET_MAGIC)) as *mut c_void
}

extern "C" fn gadget_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from gadget_create (Box::into_raw).
    unsafe { drop(Box::from_raw(instance as *mut u64)) };
}

extern "C" fn gadget_ctor(
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

fn gadget_builder() -> NativeInstanceBuilder<'static> {
    NativeInstanceBuilder {
        name: "Gadget",
        create: gadget_create,
        destroy: gadget_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "Gadget",
            f: gadget_ctor,
        }],
        properties: vec![],
    }
}

// ---------------------------------------------------------------------------
// `StaticThing`: a static-only class (no payload callback), registered on
// the same engine. Its name must never resolve to a payload.
// ---------------------------------------------------------------------------

fn static_builder() -> NativeClassBuilder<'static> {
    NativeClassBuilder {
        name: "StaticThing",
        methods: vec![],
        properties: vec![],
    }
}

/// Evaluate `expr` and retain the resulting object.
fn retained(e: &Tjs2Engine, expr: &str) -> DetachedValue {
    match e.eval_retained(expr, "lookup-test").unwrap() {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => {
            panic!("expected an object result from {expr:?}: {other:?}")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn instance_lookup_returns_its_payload() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class(&static_builder()).unwrap();
    e.register_native_class_instance(&widget_builder()).unwrap();

    let object = retained(&e, "new Widget()");
    let payload = e
        .native_instance_payload(object.raw_id(), "Widget")
        .expect("lookup must not error for a registered class")
        .expect("a live Widget instance must resolve to its payload");

    // SAFETY: `object` is a live retained instance, so the payload
    // allocated by widget_create is still allocated — the lookup only
    // hands out pointers satisfying exactly that condition (the
    // GetNativePtr()/IsValid() contract).
    let magic = unsafe { *payload.as_ptr().cast::<i32>() };
    assert_eq!(
        magic, WIDGET_MAGIC,
        "the pointer must be the widget payload"
    );

    // A second instance gets its own, distinct payload.
    let other = retained(&e, "new Widget()");
    let other_payload = e
        .native_instance_payload(other.raw_id(), "Widget")
        .expect("lookup must not error")
        .expect("the second instance must resolve too");
    assert_ne!(payload, other_payload, "payloads must be per-instance");

    // A class name no instance class was registered under misses (never
    // errors), even though it could exist as a builtin or script class.
    assert!(
        e.native_instance_payload(object.raw_id(), "NoSuchClass")
            .expect("lookup must not error")
            .is_none(),
        "an unregistered class name must miss"
    );

    // A static-only class carries no Rust payload, so its name must miss
    // even though it is one of our registered native classes.
    assert!(
        e.native_instance_payload(object.raw_id(), "StaticThing")
            .expect("lookup must not error")
            .is_none(),
        "a static-only class must not resolve to a payload"
    );

    // The class name is matched exactly (case-sensitive, no fallbacks).
    assert!(
        e.native_instance_payload(object.raw_id(), "widget")
            .expect("lookup must not error")
            .is_none()
    );
}

#[test]
fn foreign_instance_plain_object_and_class_object_return_null() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class_instance(&widget_builder()).unwrap();
    e.register_native_class_instance(&gadget_builder()).unwrap();

    // An instance of a *different* native class is foreign to "Widget".
    let gadget = retained(&e, "new Gadget()");
    assert!(
        e.native_instance_payload(gadget.raw_id(), "Widget")
            .expect("lookup must not error")
            .is_none(),
        "a Gadget instance is not a Widget"
    );
    // ...but it resolves for its own class (sanity: the miss above is about
    // the class, not the lookup failing wholesale).
    let gadget_payload = e
        .native_instance_payload(gadget.raw_id(), "Gadget")
        .expect("lookup must not error")
        .expect("a live Gadget instance must resolve");
    // SAFETY: `gadget` is a live retained instance, so the u64 payload
    // allocated by gadget_create is still allocated.
    let magic = unsafe { *gadget_payload.as_ptr().cast::<u64>() };
    assert_eq!(magic, GADGET_MAGIC);

    // A plain script object has no native instance at all.
    e.exec_script("class Plain { function Plain() {} }", "lookup-test")
        .unwrap();
    let plain = retained(&e, "new Plain()");
    assert!(
        e.native_instance_payload(plain.raw_id(), "Widget")
            .expect("lookup must not error")
            .is_none(),
        "a plain script object must miss"
    );

    // A TJS2 builtin carries a VM-native instance of its own class — the
    // lookup must never hand it out for one of *our* class names.
    let builtin = retained(&e, "new Dictionary()");
    assert!(
        e.native_instance_payload(builtin.raw_id(), "Widget")
            .expect("lookup must not error")
            .is_none(),
        "a builtin object must miss"
    );

    // The class object itself is not an instance of its own class.
    let class_object = retained(&e, "Widget");
    assert!(
        e.native_instance_payload(class_object.raw_id(), "Widget")
            .expect("lookup must not error")
            .is_none(),
        "the class object must miss"
    );
}

#[test]
fn null_value_returns_null() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class_instance(&widget_builder()).unwrap();

    // TJS `null` retained as a value: not an instance of anything.
    let null = retained(&e, "null");
    assert!(
        e.native_instance_payload(null.raw_id(), "Widget")
            .expect("lookup must not error for TJS null")
            .is_none(),
        "TJS null must miss"
    );
}

#[test]
fn invalidated_instance_returns_null() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class_instance(&widget_builder()).unwrap();

    let object = retained(&e, "new Widget()");
    assert!(
        e.native_instance_payload(object.raw_id(), "Widget")
            .expect("lookup must not error")
            .is_some(),
        "a fresh instance must resolve before invalidation"
    );

    // Reference `invalidate obj;` — the instance is finalized but not yet
    // destroyed, so the payload is still allocated while direct dispatch
    // (and therefore the lookup) must stop.
    e.invalidate_retained(object.raw_id()).unwrap();
    assert!(
        e.native_instance_payload(object.raw_id(), "Widget")
            .expect("lookup must not error for an invalidated object")
            .is_none(),
        "an invalidated instance must miss"
    );
}

#[test]
fn null_id_and_nul_class_name_are_errors() {
    let _lock = vm_lock();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class_instance(&widget_builder()).unwrap();

    // A null id is a caller mistake, not an ordinary miss.
    assert!(
        e.native_instance_payload(ptr::null_mut(), "Widget")
            .is_err(),
        "a null retained id must be rejected"
    );

    let object = retained(&e, "new Widget()");
    // A class name containing a NUL byte can never name a registered
    // class and is rejected as a caller mistake.
    assert!(
        e.native_instance_payload(object.raw_id(), "Wi\0dget")
            .is_err(),
        "a NUL byte in the class name must be rejected"
    );
}
