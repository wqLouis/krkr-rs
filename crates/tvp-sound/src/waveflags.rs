//! The `WaveFlags` native class — the conditional-loop flag surface behind
//! `WaveSoundBuffer.flags`.
//!
//! Ported from the reference `tTJSNC_WaveFlags` / `tTJSNI_WaveFlags`
//! (`reference/cpp/core/sound/WaveIntf.cpp`): a per-buffer object with
//! `count` (always `TVP_WL_MAX_FLAGS` = 16), a `reset()` method, and
//! numeric properties `0..15` that read/write the buffer's loop-manager
//! flag array. `.sli` `Link { Condition=…; CondVar=…; RefValue=… }`
//! conditions are evaluated against these flags when the loop point is
//! reached (see [`crate::sli::LoopLink::matches`] and
//! [`crate::mixer::Channel::loop_flags`]).
//!
//! The reference's `WaveSoundBuffer.flags` returns a `WaveFlags` built from
//! the owning buffer (`TVPCreateWaveFlagsObject(Owner)`). This port builds
//! it from the owning stream id (`new WaveFlags(<id>)`) because the C ABI
//! cannot construct a native instance in Rust; the id is engine-internal
//! and never script-visible.
//!
//! Registered from [`crate::wavesound::register_wavesound`]. The class is
//! kept in its own file so the parity checker's `WaveSoundBuffer` scan
//! (which unions every builder in `wavesound.rs`) does not attribute these
//! members to the buffer.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine,
};

use crate::ffi;
use crate::sli::TVP_WL_MAX_FLAGS;
use crate::wavesound::{reset_stream_flags, set_stream_flag, stream_flag};

/// Payload of one `WaveFlags` TJS object: the owning buffer's stream id.
struct WaveFlagsInst {
    stream_id: u64,
}

extern "C" fn wf_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(WaveFlagsInst { stream_id: 0 })) as *mut c_void
}

extern "C" fn wf_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from wf_create (Box::into_raw), exactly once.
    unsafe { drop(Box::from_raw(instance as *mut WaveFlagsInst)) };
}

/// `WaveFlags(bufferId)` — bind to the owning buffer's stream (the reference
/// ctor takes the buffer object and reads its native instance).
extern "C" fn wf_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveFlagsInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveFlagsInst>(instance) };
    let args = ffi::args(argv, argc);
    let Some(id) = args.first() else {
        return ffi::report_error(out_error, "WaveFlags requires a buffer");
    };
    inst.stream_id = ffi::value_as_f64(id) as u64;
    ffi::set_void_out(out);
    0
}

/// `reset()` — clear every flag (reference `tTJSNI_BaseWaveSoundBuffer`'s
/// `manager->ClearFlags()`).
extern "C" fn wf_reset(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveFlagsInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveFlagsInst>(instance) };
    reset_stream_flags(inst.stream_id);
    ffi::set_void_out(out);
    0
}

/// `count` — always `TVP_WL_MAX_FLAGS` (reference hard-codes this).
extern "C" fn wf_count(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    ffi::set_int_out(out, TVP_WL_MAX_FLAGS as i64);
    0
}

/// Generate the getter/setter pair for each numeric flag property `0..15`.
macro_rules! flag_properties {
    ($($idx:literal => $get:ident, $set:ident),* $(,)?) => {
        $(
            extern "C" fn $get(
                _engine: *mut c_void,
                instance: *mut c_void,
                out: *mut tjs2_sys::Value,
                _out_error: *mut *mut c_char,
                _objthis: *mut c_void,
            ) -> c_int {
                // SAFETY: instance is a valid WaveFlagsInst payload.
                let inst = unsafe { &mut *ffi::instance_ptr::<WaveFlagsInst>(instance) };
                ffi::set_int_out(out, i64::from(stream_flag(inst.stream_id, $idx)));
                0
            }

            extern "C" fn $set(
                _engine: *mut c_void,
                instance: *mut c_void,
                value: *const tjs2_sys::Value,
                _out_error: *mut *mut c_char,
                _objthis: *mut c_void,
            ) -> c_int {
                // SAFETY: instance is a valid WaveFlagsInst payload.
                let inst = unsafe { &mut *ffi::instance_ptr::<WaveFlagsInst>(instance) };
                // SAFETY: value points at the property value for the call.
                let v = unsafe { &*value };
                set_stream_flag(inst.stream_id, $idx, ffi::value_as_f64(v) as i32);
                0
            }
        )*

        fn numeric_flag_properties() -> Vec<NativeInstancePropertyDef> {
            vec![
                $(
                    NativeInstancePropertyDef {
                        name: stringify!($idx),
                        get: Some($get),
                        set: Some($set),
                    },
                )*
            ]
        }
    };
}

flag_properties!(
    0 => flag0_get, flag0_set,
    1 => flag1_get, flag1_set,
    2 => flag2_get, flag2_set,
    3 => flag3_get, flag3_set,
    4 => flag4_get, flag4_set,
    5 => flag5_get, flag5_set,
    6 => flag6_get, flag6_set,
    7 => flag7_get, flag7_set,
    8 => flag8_get, flag8_set,
    9 => flag9_get, flag9_set,
    10 => flag10_get, flag10_set,
    11 => flag11_get, flag11_set,
    12 => flag12_get, flag12_set,
    13 => flag13_get, flag13_set,
    14 => flag14_get, flag14_set,
    15 => flag15_get, flag15_set,
);

/// Register the `WaveFlags` native class.
pub(crate) fn register_waveflags(engine: &Tjs2Engine) -> Result<(), String> {
    let mut properties = numeric_flag_properties();
    properties.push(NativeInstancePropertyDef {
        name: "count",
        get: Some(wf_count),
        set: None,
    });
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "WaveFlags",
        create: wf_create,
        destroy: wf_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "WaveFlags",
                f: wf_ctor,
            },
            NativeInstanceMethodDef {
                name: "reset",
                f: wf_reset,
            },
        ],
        properties,
    })
}
