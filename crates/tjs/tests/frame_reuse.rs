//! Reusing a VM frame releases the previous frame's `this` reference.
//!
//! `ExecuteAsFunction` stores `objthis` in the frame's dedicated slot
//! (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:700`); the next call reuses
//! the slot and releases the value it held
//! (`reference/cpp/core/tjs2/tjsVariant.cpp:534-545`).

use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use tjs::{NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, VAL_VOID, Value};

static DESTROYED: AtomicUsize = AtomicUsize::new(0);

extern "C" fn widget_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(0i32)) as *mut c_void
}

extern "C" fn widget_destroy(_engine: *mut c_void, instance: *mut c_void) {
    DESTROYED.fetch_add(1, Ordering::SeqCst);
    // SAFETY: instance came from widget_create (Box::into_raw).
    unsafe { drop(Box::from_raw(instance as *mut i32)) };
}

unsafe fn set_void(out: *mut Value) {
    // SAFETY: out is a valid return slot for the duration of the callback.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
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

fn widget_builder() -> NativeInstanceBuilder<'static> {
    NativeInstanceBuilder {
        name: "FrameWidget",
        create: widget_create,
        destroy: widget_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "FrameWidget",
            f: widget_ctor,
        }],
        properties: vec![],
    }
}

#[test]
fn reusing_a_frame_releases_the_previous_this_object() {
    DESTROYED.store(0, Ordering::SeqCst);
    let engine = Tjs2Engine::new().unwrap();
    engine
        .register_native_class_instance(&widget_builder())
        .unwrap();

    // `w` is local to the function. Each `poke()` call reuses the same VM
    // frame; once the function returns the native instance must be destroyed.
    engine
        .exec_script_discard(
            "class FrameProbe extends FrameWidget { \
                 function poke() { return 0; } \
             } \
             function __frame_probe() { \
                 var w = new FrameProbe(); \
                 for (var i = 0; i < 16; i++) w.poke(); \
             } \
             __frame_probe(); \
             delete __frame_probe;",
            "frame_reuse",
        )
        .unwrap();

    assert_eq!(
        DESTROYED.load(Ordering::SeqCst),
        1,
        "the local native instance must be destroyed after its frame returns"
    );
}
