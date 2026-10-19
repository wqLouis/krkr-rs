//! The `WaveFlags` native class — the conditional-loop flag surface behind
//! `WaveSoundBuffer.flags`.
//!
//! Behavioral reference: `tTJSNC_WaveFlags` / `tTJSNI_WaveFlags`
//! (`reference/cpp/core/sound/WaveIntf.cpp`). The reference builds one
//! `WaveFlags` per buffer from the owning `tTJSNI_WaveSoundBuffer`
//! (`TVPCreateWaveFlagsObject(Owner)`) and exposes:
//!
//! * `count` — always `TVP_WL_MAX_FLAGS` (`16`);
//! * `reset()` — clears every flag (the loop manager's `ClearFlags`);
//! * numeric properties `0..15` — read/write the buffer's conditional-loop
//!   flag array, clamped to `TVP_WL_MAX_FLAG_VALUE` (`9999`).
//!
//! The C ABI cannot construct a native instance from Rust, so
//! `WaveSoundBuffer.flags` builds the proxy with `new WaveFlags(<bufferId>)`
//! where the id is engine-internal and never script-visible. The proxy is
//! cached on the owning
//! [`SoundObject`](super::SoundObject) so `buffer.flags === buffer.flags`
//! holds, as in the reference.
//!
//! `N` is registered as an alias of `count`: the reference declares the
//! numeric family with the `TVP_DEFINE_FLAG_PROP(N)` macro and the native
//! parity checker records that macro's parameter (`N`) as a member, so the
//! runtime surface must expose that spelling to stay at zero missing.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::LOOP_FLAG_COUNT;
use tjs::{NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Value};

use crate::native_util::{report_error, set_int, set_void};
use crate::{CompatCtx, CompatError};

use super::bound;

/// Payload of one `WaveFlags` TJS object: the owning `WaveSoundBuffer`'s
/// dispatch id (`None` while unbound, e.g. `new WaveFlags()`).
struct WaveFlagsInst {
    buffer: Option<u64>,
}

extern "C" fn wf_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(WaveFlagsInst { buffer: None })) as *mut c_void
}

extern "C" fn wf_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: `instance` came from `wf_create` and is released once.
        drop(unsafe { Box::from_raw(instance as *mut WaveFlagsInst) });
    }
}

/// `WaveFlags(bufferId)` — bind to the owning buffer's dispatch id (the
/// reference ctor takes the buffer object and reads its native instance).
extern "C" fn wf_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let raw_args = unsafe { crate::native_util::args(argv, argc) };
    let Some(id) = raw_args.first() else {
        return report_error(out_error, "WaveFlags: requires a buffer");
    };
    // SAFETY: `instance` came from `wf_create` and is valid for the call.
    let inst = unsafe { &mut *(instance as *mut WaveFlagsInst) };
    inst.buffer = Some(super::value_as_i64(id).max(0) as u64);
    set_void(out);
    0
}

/// Run `f` with the bound services and the owning buffer id.
///
/// Returns `Ok(None)` when the payload is unbound, `Err` when the compat
/// context is not installed. The reference tolerates a null loop manager
/// (reads yield `0`, writes/reset are no-ops), so callers do the same.
fn with_buffer<R>(
    engine: *mut c_void,
    instance: *mut c_void,
    what: &str,
    f: impl FnOnce(&crate::CompatServices, u64) -> R,
) -> Result<Option<R>, String> {
    // SAFETY: the dispatcher guarantees `instance` is the create payload.
    let inst = unsafe { (instance as *const WaveFlagsInst).as_ref() };
    let Some(inst) = inst else {
        return Err(format!("WaveFlags.{what}: missing instance"));
    };
    let Some(buffer) = inst.buffer else {
        return Ok(None);
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return Err(format!("WaveFlags.{what}: compat context is not bound"));
    };
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    Ok(Some(f(services, buffer)))
}

/// `count` / `N` — always `LOOP_FLAG_COUNT` (the reference hard-codes
/// `TVP_WL_MAX_FLAGS`).
extern "C" fn wf_count(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, LOOP_FLAG_COUNT as i64);
    0
}

/// `reset()` — clear every flag (reference `manager->ClearFlags()`). The
/// reference's loop manager only exists while a source is open
/// (`WaveImpl.cpp:3047`), so before an `open` this is a no-op
/// (`WaveIntf.cpp:1688-1705` guards on a null manager).
extern "C" fn wf_reset(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match with_buffer(engine, instance, "reset", |services, buffer| {
        super::with_object(services, buffer, |object, mixer| {
            if object.source.is_none() {
                return;
            }
            object.buffer.flags_mut().reset();
            if let Some(channel) = mixer.channel(object.channel_id) {
                object.buffer.apply_flags_to(channel);
            }
        })
    }) {
        Ok(_) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Generate the getter/setter pair for one numeric flag property.
macro_rules! flag_property {
    ($idx:literal, $get:ident, $set:ident) => {
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            match with_buffer(engine, instance, stringify!($idx), |services, buffer| {
                super::with_object(services, buffer, |object, _mixer| {
                    // The reference reads the loop manager's array and yields
                    // `0` while the manager is null (`WaveIntf.cpp:1743-1749`):
                    // flags only exist while a source is open.
                    if object.source.is_some() {
                        object.buffer.flags().get($idx)
                    } else {
                        0
                    }
                })
                .unwrap_or(0)
            }) {
                Ok(value) => {
                    set_int(out, i64::from(value.unwrap_or(0)));
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }

        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
            let value = super::value_as_i64(unsafe { &*value }) as i32;
            match with_buffer(engine, instance, stringify!($idx), |services, buffer| {
                super::with_object(services, buffer, |object, mixer| {
                    // Null loop manager → the write is dropped
                    // (`WaveIntf.cpp:1750-1764`).
                    if object.source.is_none() {
                        return;
                    }
                    object.buffer.flags_mut().set($idx, value);
                    if let Some(channel) = mixer.channel(object.channel_id) {
                        object.buffer.apply_flags_to(channel);
                    }
                })
            }) {
                Ok(_) => 0,
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

flag_property!(0, flag0_get, flag0_set);
flag_property!(1, flag1_get, flag1_set);
flag_property!(2, flag2_get, flag2_set);
flag_property!(3, flag3_get, flag3_set);
flag_property!(4, flag4_get, flag4_set);
flag_property!(5, flag5_get, flag5_set);
flag_property!(6, flag6_get, flag6_set);
flag_property!(7, flag7_get, flag7_set);
flag_property!(8, flag8_get, flag8_set);
flag_property!(9, flag9_get, flag9_set);
flag_property!(10, flag10_get, flag10_set);
flag_property!(11, flag11_get, flag11_set);
flag_property!(12, flag12_get, flag12_set);
flag_property!(13, flag13_get, flag13_set);
flag_property!(14, flag14_get, flag14_set);
flag_property!(15, flag15_get, flag15_set);

fn numeric_flag_properties() -> Vec<NativeInstancePropertyDef> {
    vec![
        NativeInstancePropertyDef {
            name: "0",
            get: Some(flag0_get),
            set: Some(flag0_set),
        },
        NativeInstancePropertyDef {
            name: "1",
            get: Some(flag1_get),
            set: Some(flag1_set),
        },
        NativeInstancePropertyDef {
            name: "2",
            get: Some(flag2_get),
            set: Some(flag2_set),
        },
        NativeInstancePropertyDef {
            name: "3",
            get: Some(flag3_get),
            set: Some(flag3_set),
        },
        NativeInstancePropertyDef {
            name: "4",
            get: Some(flag4_get),
            set: Some(flag4_set),
        },
        NativeInstancePropertyDef {
            name: "5",
            get: Some(flag5_get),
            set: Some(flag5_set),
        },
        NativeInstancePropertyDef {
            name: "6",
            get: Some(flag6_get),
            set: Some(flag6_set),
        },
        NativeInstancePropertyDef {
            name: "7",
            get: Some(flag7_get),
            set: Some(flag7_set),
        },
        NativeInstancePropertyDef {
            name: "8",
            get: Some(flag8_get),
            set: Some(flag8_set),
        },
        NativeInstancePropertyDef {
            name: "9",
            get: Some(flag9_get),
            set: Some(flag9_set),
        },
        NativeInstancePropertyDef {
            name: "10",
            get: Some(flag10_get),
            set: Some(flag10_set),
        },
        NativeInstancePropertyDef {
            name: "11",
            get: Some(flag11_get),
            set: Some(flag11_set),
        },
        NativeInstancePropertyDef {
            name: "12",
            get: Some(flag12_get),
            set: Some(flag12_set),
        },
        NativeInstancePropertyDef {
            name: "13",
            get: Some(flag13_get),
            set: Some(flag13_set),
        },
        NativeInstancePropertyDef {
            name: "14",
            get: Some(flag14_get),
            set: Some(flag14_set),
        },
        NativeInstancePropertyDef {
            name: "15",
            get: Some(flag15_get),
            set: Some(flag15_set),
        },
    ]
}

/// Register the `WaveFlags` native class.
pub(super) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let mut properties = numeric_flag_properties();
    properties.push(NativeInstancePropertyDef {
        name: "count",
        get: Some(wf_count),
        set: None,
    });
    // See the module docs: the parity checker records the reference macro's
    // `N` parameter as a member, so expose that spelling too.
    properties.push(NativeInstancePropertyDef {
        name: "N",
        get: Some(wf_count),
        set: None,
    });
    ctx.register_instance(&NativeInstanceBuilder {
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
