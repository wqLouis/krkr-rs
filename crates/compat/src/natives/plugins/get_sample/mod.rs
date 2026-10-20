//! `getSample.dll` plugin — a full port of `reference/cpp/plugins/getSample.cpp`.
//!
//! The reference attaches two groups of members to the already-registered
//! `WaveSoundBuffer` class:
//!
//! * the legacy `getSample(n)` function (`NCB_ATTACH_FUNCTION`): it allocates a
//!   buffer of `n` 16-bit samples, calls the object's `getVisBuffer` into it and
//!   returns the mean of the non-negative samples (a negative sample is treated
//!   as "no sample" and skipped). With no argument it uses `100`.
//! * the new-style `WaveSoundBufferAdd` class (`NCB_ATTACH_CLASS_WITH_HOOK`):
//!   the `sampleValue` read-only property and the `sampleCount` /
//!   `sampleAhead` read/write properties, plus the `setDefaultCounts` /
//!   `setDefaultAheads` functions that change the defaults for objects created
//!   afterwards. `sampleValue` zeroes an `sampleCount`-sample buffer, calls
//!   `getVisBuffer(buf, counts, 1, aheads)` and returns the squared peak
//!   amplitude in `0.0..=1.0` (`(max|sample| / 32768)²`).
//!
//! The members are published when a script calls
//! `Plugins.link("getSample.dll")`, not when the engine starts. The
//! shipped `system/voice.tjs` relies on that timing: it probes
//! `typeof WaveSoundBuffer.setDefaultAheads` before linking the module and
//! only then publishes the `WaveSoundBuffer.enableGetSample` flag its
//! `onFlip` action reads.
//!
//! # Instance state
//!
//! The reference keeps the per-object `counts` / `aheads` (and the reusable
//! sample buffer) in a native instance attached to the `WaveSoundBuffer`
//! object by `NCB_GET_INSTANCE_HOOK`. This port attaches the members with
//! [`CompatCtx::register_instance_members`], whose callbacks receive only
//! `objthis` (no native payload), so the equivalent per-object state lives in
//! a per-engine map keyed by the raw `objthis` pointer — the same
//! object-identity keying `natives/sound.rs` uses for its `SoundInst` /
//! `SoundObject` state. The pointer is recorded when a member is first used and
//! removed by the `finalize` member (the reference's destructor), so a
//! destroyed object leaves no entry behind.
//!
//! `getVisBuffer` itself is the L2 playback state's own method: this plugin
//! never reaches into the `sound` module, it invokes the object's script-visible
//! method exactly like the reference `objthis->FuncCall(0, "getVisBuffer", …)`.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use tjs::{
    NativeInstanceMembers, NativeObjThisMethodDef, NativeObjThisPropertyDef, Tjs2Engine, TjsValue,
    VAL_REAL, Value,
};

use crate::native_util::{args, report_error, set_int, set_void};
use crate::natives::sound::VisBufferRegistration;
use crate::natives::system::{resolve, value_as_i64};
use crate::{CompatCtx, CompatError, CompatServices};

/// Default `sampleCount` (reference `WaveSoundBufferAdd::defaultCounts`).
const DEFAULT_COUNTS: i32 = 100;
/// Default `sampleAhead` (reference `WaveSoundBufferAdd::defaultAheads`).
const DEFAULT_AHEADS: i32 = 0;
/// The legacy `getSample()` default sample count (hard-coded in the reference).
const LEGACY_DEFAULT_COUNTS: i32 = 100;
/// Full-scale value of a signed 16-bit sample.
const SAMPLE_SCALE: f64 = 32768.0;

// ---------------------------------------------------------------------------
// Per-engine dispatch state
// ---------------------------------------------------------------------------

/// Per-object parameters, the port of the reference `WaveSoundBufferAdd`
/// `counts` / `aheads` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SampleParams {
    counts: i32,
    aheads: i32,
}

/// `getSample.dll` dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
struct GetSampleState {
    /// Defaults applied to an object the first time a member touches it.
    default_counts: i32,
    default_aheads: i32,
    /// Per-object parameters keyed by the raw `objthis` pointer.
    params: HashMap<usize, SampleParams>,
}

impl Default for GetSampleState {
    fn default() -> Self {
        Self {
            default_counts: DEFAULT_COUNTS,
            default_aheads: DEFAULT_AHEADS,
            params: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Marshalling helpers
// ---------------------------------------------------------------------------

/// Resolve the bound engine for a callback.
fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "getSample: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// Write a real result into `out`.
fn set_real(out: *mut Value, value: f64) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_REAL;
        (*out).integer = 0;
        (*out).real = value;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Read (initializing on first use) the per-object parameters of `objthis`.
///
/// The returned flag is `true` when the entry was just created; the caller
/// then performs the reference constructor's `useVisBuffer = true` side
/// effect. The state borrow is released before returning, so the caller may
/// safely re-enter the VM.
fn object_params(services: &CompatServices, objthis: *mut c_void) -> (SampleParams, bool) {
    let mut state = services.state::<GetSampleState>();
    let key = objthis as usize;
    if let Some(params) = state.params.get(&key) {
        return (*params, false);
    }
    let params = SampleParams {
        counts: state.default_counts,
        aheads: state.default_aheads,
    };
    state.params.insert(key, params);
    (params, true)
}

/// Perform the reference constructor's `objthis.useVisBuffer = 1` write.
///
/// The reference logs a failed write and continues (`getSample.cpp:79-82`):
/// an object that is not a live `WaveSoundBuffer` (for example the class
/// object itself) simply has no visualization buffer to enable.
fn mark_use_vis_buffer(host: &Tjs2Engine, objthis: *mut c_void) {
    if let Ok(object) = host.retain_object_detached(objthis)
        && let Err(error) = host.set_member(object.raw_id(), "useVisBuffer", &TjsValue::Integer(1))
    {
        log::warn!("WaveSoundBuffer.useVisBuffer=1 failed: {error}");
    }
}

/// Invoke the object's own `getVisBuffer(buffer, count, 1[, ahead])` method,
/// passing the sample buffer as a raw integer pointer exactly like the
/// reference `tTJSVariant buffer = (tTVInteger)buf`.
///
/// Returns the sample count the method reported, coerced with TJS
/// `AsInteger` semantics.
fn call_get_vis_buffer(
    host: &Tjs2Engine,
    services: &CompatServices,
    objthis: *mut c_void,
    buffer: *mut i16,
    buffer_len: usize,
    count: i32,
    aheads: Option<i32>,
) -> Result<i64, String> {
    // Register the caller-owned destination for the duration of the call, so
    // `getVisBuffer` accepts this address and clamps its write to `buffer_len`.
    let _registration = VisBufferRegistration::register(services, buffer, buffer_len);
    let object = host.retain_object_detached(objthis)?;
    let mut arguments = vec![
        TjsValue::Integer(buffer as i64),
        TjsValue::Integer(i64::from(count)),
        TjsValue::Integer(1),
    ];
    if let Some(aheads) = aheads {
        arguments.push(TjsValue::Integer(i64::from(aheads)));
    }
    let result = host.call_member(object.raw_id(), "getVisBuffer", &arguments)?;
    Ok(match result {
        TjsValue::Integer(value) => value,
        TjsValue::Real(value) => value as i64,
        TjsValue::String(value) => crate::natives::system::tjs_string_to_integer(&value),
        _ => 0,
    })
}

// ---------------------------------------------------------------------------
// Sample reductions (the reference formulas, factored for unit tests)
// ---------------------------------------------------------------------------

/// Reference legacy `getSample` reduction: the mean of the non-negative
/// samples, or `0` when the buffer has none. Negative samples are ignored.
fn average_non_negative(samples: &[i16]) -> i64 {
    let mut count = 0i64;
    let mut sum = 0i64;
    for &sample in samples {
        if sample >= 0 {
            sum += i64::from(sample);
            count += 1;
        }
    }
    if count > 0 { sum / count } else { 0 }
}

/// Reference `sampleValue` reduction: the squared peak amplitude over the first
/// `count` samples, normalized to `0.0..=1.0`. `count` is clamped to the
/// buffer length so a malformed `getVisBuffer` result cannot read past the end.
fn peak_power(samples: &[i16], count: i64) -> f64 {
    let count = count.clamp(0, samples.len() as i64) as usize;
    let peak = samples[..count]
        .iter()
        .map(|sample| i32::from(sample.unsigned_abs()))
        .max()
        .unwrap_or(0);
    let normalized = f64::from(peak) / SAMPLE_SCALE;
    normalized * normalized
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

/// Legacy `getSample([n])` — mean of the current playback samples.
extern "C" fn get_sample(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "WaveSoundBuffer.getSample: compat context is not bound",
        );
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    // SAFETY: argv/argc follow the native-callback ABI for the call.
    let raw = unsafe { args(argv, argc) };
    let count = if let Some(first) = raw.first() {
        value_as_i64(first) as i32
    } else {
        LEGACY_DEFAULT_COUNTS
    };
    if count <= 0 {
        set_void(out);
        return 0;
    }
    let mut buffer = vec![0i16; count as usize];
    match call_get_vis_buffer(
        resolved.engine(),
        services,
        objthis,
        buffer.as_mut_ptr(),
        buffer.len(),
        count,
        None,
    ) {
        Ok(_reported) => {
            set_int(out, average_non_negative(&buffer));
            0
        }
        Err(error) => report_error(out_error, &format!("WaveSoundBuffer.getSample: {error}")),
    }
}

/// `sampleValue` getter — squared peak amplitude of the current samples.
extern "C" fn sample_value_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "WaveSoundBuffer.sampleValue: compat context is not bound",
        );
    };
    let host = resolved.engine();
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let (params, created) = object_params(services, objthis);
    if created {
        mark_use_vis_buffer(host, objthis);
    }
    let count = params.counts.max(0) as usize;
    let mut buffer = vec![0i16; count];
    // The reference logs a `getVisBuffer` failure and keeps going with the
    // zeroed buffer; reproduce that here.
    let reported = match call_get_vis_buffer(
        host,
        services,
        objthis,
        buffer.as_mut_ptr(),
        buffer.len(),
        params.counts,
        Some(params.aheads),
    ) {
        Ok(count) => count,
        Err(error) => {
            log::warn!("WaveSoundBuffer.sampleValue: getVisBuffer failed: {error}");
            0
        }
    };
    // `cnt = (cnt > counts || cnt < 0) ? counts : cnt;`
    let cnt = if reported > i64::from(params.counts) || reported < 0 {
        i64::from(params.counts)
    } else {
        reported
    };
    set_real(out, peak_power(&buffer, cnt));
    0
}

/// `sampleCount` getter.
extern "C" fn sample_count_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "WaveSoundBuffer.sampleCount: compat context is not bound",
        );
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let (params, created) = object_params(services, objthis);
    if created {
        mark_use_vis_buffer(resolved.engine(), objthis);
    }
    set_int(out, i64::from(params.counts));
    0
}

/// `sampleCount` setter.
extern "C" fn sample_count_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "WaveSoundBuffer.sampleCount: compat context is not bound",
        );
    };
    // SAFETY: `value` is valid for the duration of the call.
    let new_count = value_as_i64(unsafe { &*value }) as i32;
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let created = {
        let mut state = services.state::<GetSampleState>();
        let key = objthis as usize;
        if let Some(params) = state.params.get_mut(&key) {
            params.counts = new_count;
            false
        } else {
            let params = SampleParams {
                counts: new_count,
                aheads: state.default_aheads,
            };
            state.params.insert(key, params);
            true
        }
    };
    if created {
        mark_use_vis_buffer(resolved.engine(), objthis);
    }
    0
}

/// `sampleAhead` getter.
extern "C" fn sample_ahead_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "WaveSoundBuffer.sampleAhead: compat context is not bound",
        );
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let (params, created) = object_params(services, objthis);
    if created {
        mark_use_vis_buffer(resolved.engine(), objthis);
    }
    set_int(out, i64::from(params.aheads));
    0
}

/// `sampleAhead` setter.
extern "C" fn sample_ahead_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "WaveSoundBuffer.sampleAhead: compat context is not bound",
        );
    };
    // SAFETY: `value` is valid for the duration of the call.
    let new_ahead = value_as_i64(unsafe { &*value }) as i32;
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let created = {
        let mut state = services.state::<GetSampleState>();
        let key = objthis as usize;
        if let Some(params) = state.params.get_mut(&key) {
            params.aheads = new_ahead;
            false
        } else {
            let params = SampleParams {
                counts: state.default_counts,
                aheads: new_ahead,
            };
            state.params.insert(key, params);
            true
        }
    };
    if created {
        mark_use_vis_buffer(resolved.engine(), objthis);
    }
    0
}

/// `setDefaultCounts(count)` — the default `sampleCount` for new objects.
///
/// The reference registers this through `NCB_ATTACH_FUNCTION`, which wraps
/// the C++ function in ncbind's `doInvoke` — a call with fewer arguments
/// than the signature has parameters fails with `TJS_E_BADPARAMCOUNT`
/// (`reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn set_default_counts(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "WaveSoundBuffer.setDefaultCounts: compat context is not bound",
        );
    };
    // SAFETY: argv/argc follow the native-callback ABI for the call.
    let Some(first) = unsafe { args(argv, argc) }.first() else {
        return report_error(
            out_error,
            "WaveSoundBuffer.setDefaultCounts requires 1 argument",
        );
    };
    let count = value_as_i64(first) as i32;
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    services.state::<GetSampleState>().default_counts = count;
    set_void(out);
    0
}

/// `setDefaultAheads(ahead)` — the default `sampleAhead` for new objects.
///
/// Arity as in [`setDefaultCounts`] (`ncbind.hpp:1178`).
extern "C" fn set_default_aheads(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "WaveSoundBuffer.setDefaultAheads: compat context is not bound",
        );
    };
    // SAFETY: argv/argc follow the native-callback ABI for the call.
    let Some(first) = unsafe { args(argv, argc) }.first() else {
        return report_error(
            out_error,
            "WaveSoundBuffer.setDefaultAheads requires 1 argument",
        );
    };
    let ahead = value_as_i64(first) as i32;
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    services.state::<GetSampleState>().default_aheads = ahead;
    set_void(out);
    0
}

/// `finalize()` — the reference `~WaveSoundBufferAdd` destructor: drop the
/// per-object parameters. The default `finalize` the `sound` module installs
/// is a no-op, so replacing it is behavior-preserving for scripts.
extern "C" fn finalize(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    if let Ok((_resolved, ctx)) = bound(engine) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        services
            .state::<GetSampleState>()
            .params
            .remove(&(objthis as usize));
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the plugin's native surface on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance_members(&NativeInstanceMembers {
        class_name: "WaveSoundBuffer",
        methods: vec![
            NativeObjThisMethodDef {
                name: "getSample",
                f: get_sample,
            },
            NativeObjThisMethodDef {
                name: "setDefaultCounts",
                f: set_default_counts,
            },
            NativeObjThisMethodDef {
                name: "setDefaultAheads",
                f: set_default_aheads,
            },
            NativeObjThisMethodDef {
                name: "finalize",
                f: finalize,
            },
        ],
        properties: vec![
            NativeObjThisPropertyDef {
                name: "sampleValue",
                get: Some(sample_value_get),
                set: None,
            },
            NativeObjThisPropertyDef {
                name: "sampleCount",
                get: Some(sample_count_get),
                set: Some(sample_count_set),
            },
            NativeObjThisPropertyDef {
                name: "sampleAhead",
                get: Some(sample_ahead_get),
                set: Some(sample_ahead_set),
            },
        ],
    })
}

// ---------------------------------------------------------------------------
// Unit tests (the pure reference formulas)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_average_ignores_negative_samples() {
        // (10 + 20 + 0) / 3 — the -5 is skipped.
        assert_eq!(average_non_negative(&[10, -5, 20, 0]), 10);
        // All negative -> 0.
        assert_eq!(average_non_negative(&[-1, -2]), 0);
        // Empty -> 0.
        assert_eq!(average_non_negative(&[]), 0);
    }

    #[test]
    fn sample_value_is_the_squared_normalized_peak() {
        // Peak 16384 -> (16384/32768)^2 = 0.25.
        let samples = [0, 16384, -16384, 100];
        assert!((peak_power(&samples, 4) - 0.25).abs() < f64::EPSILON);
        // Only the first `count` samples are considered.
        assert_eq!(peak_power(&samples, 0), 0.0);
        // `i16::MIN` has magnitude 32768 -> 1.0.
        assert!((peak_power(&[i16::MIN], 1) - 1.0).abs() < f64::EPSILON);
        // A count past the end cannot read out of bounds.
        assert!((peak_power(&[i16::MAX], 99) - (32767.0_f64 / 32768.0).powi(2)).abs() < 1e-12);
    }
}
