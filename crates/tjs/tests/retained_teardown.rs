//! Integration test for the retained-value teardown.
//!
//! A native instance's payload can own a retained script value: the compat
//! layer keeps the TJS objects a native object refers to in the payload,
//! which is what the reference stores as `tTJSVariant` members (e.g.
//! `EmotePlayer::_module` and `ResourceManager::State::loadedModules`,
//! `reference/cpp/plugins/motionplayer/EmotePlayer.h:127-129` and
//! `ResourceManager.h:44-50`). Releasing such a value drops the TJS object's
//! reference, and destroying that object runs the native instance's
//! `destroy` callback, which drops the payload and releases the value
//! again.
//!
//! The engine teardown releases every retained value, so that release can
//! happen while the retained-value registry is being emptied. The registry
//! must survive it: the entry that is being released leaves the registry
//! before its value is destroyed, so the nested release finds nothing to
//! remove and the node is not freed twice.

use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use tjs::{
    DetachedValue, Engine, NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, VAL_OBJECT,
    VAL_VOID, Value, retain_object_arg_raw,
};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Set once the native instance's payload has been released.
static PAYLOAD_RELEASED: AtomicBool = AtomicBool::new(false);

/// Write a void result into an out slot.
unsafe fn set_void(out: *mut Value) {
    // SAFETY: out is a valid return slot for the duration of the callback.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// The payload owns the retained value the instance holds.
extern "C" fn holder_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(None::<DetachedValue>)) as *mut c_void
}

extern "C" fn holder_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from holder_create (Box::into_raw).
    unsafe { drop(Box::from_raw(instance as *mut Option<DetachedValue>)) };
    PAYLOAD_RELEASED.store(true, Ordering::SeqCst);
}

/// `Holder()` constructor member, needed so `new Holder()` works.
extern "C" fn holder_ctor(
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

/// Retain the receiver and keep the retention in the payload.
extern "C" fn holder_hold_self(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let value = Value {
        ty: VAL_OBJECT,
        integer: 0,
        real: 0.0,
        string: ptr::null(),
        array: ptr::null(),
        array_count: 0,
        retained: objthis as usize,
    };
    // SAFETY: engine is the live engine this callback was registered on and
    // `value` carries the receiver's live object handle.
    let retained = unsafe { retain_object_arg_raw(engine as *mut Engine, &value) };
    let Ok(retained) = retained else {
        return 1;
    };
    // SAFETY: instance is the payload Box holder_create allocated.
    let payload = unsafe { &mut *(instance as *mut Option<DetachedValue>) };
    *payload = Some(retained);
    // SAFETY: out is a valid return slot.
    unsafe { set_void(out) };
    0
}

/// A native instance that owns a retained value must not corrupt the engine
/// teardown.
///
/// The teardown releases the retained value, which destroys the native
/// instance, whose payload releases the same retention again. The registry
/// entry is gone by then, so the nested release is a no-op and the node is
/// freed exactly once.
#[test]
fn a_native_instance_that_owns_a_retained_value_survives_the_engine_teardown() {
    let _lock = vm_lock();
    PAYLOAD_RELEASED.store(false, Ordering::SeqCst);
    {
        let engine = Tjs2Engine::new().unwrap();
        engine
            .register_native_class_instance(&NativeInstanceBuilder {
                name: "Holder",
                create: holder_create,
                destroy: holder_destroy,
                invalidate: None,
                methods: vec![
                    NativeInstanceMethodDef {
                        name: "Holder",
                        f: holder_ctor,
                    },
                    NativeInstanceMethodDef {
                        name: "holdSelf",
                        f: holder_hold_self,
                    },
                ],
                properties: vec![],
            })
            .unwrap();
        engine
            .exec_script("var holder = new Holder(); holder.holdSelf();", "test")
            .unwrap();
        // The engine keeps its most recent object-valued result alive; point
        // it at an unrelated object so the retained value is the instance's
        // last reference and the teardown destroys it.
        engine.eval("%[]", "test").unwrap();
        assert!(
            engine.retained_count() >= 1,
            "the instance's retention must be registered"
        );
        // Dropping the engine tears the retained-value registry down.
    }
    assert!(
        PAYLOAD_RELEASED.load(Ordering::SeqCst),
        "the native instance must be destroyed by the engine teardown"
    );
}
