//! Disc-format sound classes `CDDASoundBuffer` and `MIDISoundBuffer`.
//!
//! The reference guards each class's playback surface behind a feature
//! macro that is defined nowhere in the tree — `ENABLE_CDDA` for the CD-DA
//! class and `TVP_ENABLE_MIDI` for the MIDI class. The playback methods
//! therefore fall through to `TJS_S_OK` after their argument-count check, and
//! every property reports the disabled build's value. The event members
//! (`onStatusChanged`, `onFadeCompleted`) are **outside** those guards: the
//! reference still forwards them to the constructor's action owner, and only
//! the disabled playback build never posts them from a timer.
//!
//! The two classes register the same member set and differ only in the
//! `volume` getter: the MIDI native calls the base `GetVolume()` (which the
//! disabled implementation overrides to return `0`), while the CD-DA getter
//! is compiled out and leaves the result untouched (script `void`). `volume2`
//! is `0` for both, and `status` is the base `GetStatusString()` of a buffer
//! that never left `ssUnload`, i.e. `"unload"`.
//!
//! No object owns a mixer channel, decoder or playback device: the classes
//! are constructed and queried, never played.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    DetachedValue, NativeCreateInstanceFn, NativeInstanceBuilder, NativeInstanceMethodDef,
    NativeInstancePropertyDef, NativeMethodDef, NativeStaticMembers, RetainedValue, Tjs2Engine,
    TjsValue, VAL_INTEGER, VAL_REAL, VAL_STRING, VAL_VOID, Value,
};

use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, set_string_out};
use crate::{CompatCtx, CompatError};

// ---------------------------------------------------------------------------
// Which disc class an instance belongs to
// ---------------------------------------------------------------------------

/// The two classes differ only in source acquisition and their extra static.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Cdda,
    Midi,
}

/// The payload of one disc-sound object: the class it was created for and the
/// reference base class's `ActionOwner` (`SoundBufferBaseIntf.cpp:39-45`).
struct DiscSoundInst {
    kind: Kind,
    /// `ActionOwner = param[0]->AsObjectClosure()`; `None` is the empty closure
    /// that the event members never invoke.
    owner: Option<DetachedValue>,
}

/// Read the payload's class, or `None` when the object was not constructed.
fn instance_kind(instance: *mut c_void) -> Option<Kind> {
    if instance.is_null() {
        return None;
    }
    // SAFETY: `instance` came from `cdda_create`/`midi_create` and stays
    // valid until `disc_destroy`; the VM only invokes members while the
    // object is alive.
    Some(unsafe { (*(instance as *const DiscSoundInst)).kind })
}

// ---------------------------------------------------------------------------
// Constructor / destructor
// ---------------------------------------------------------------------------

extern "C" fn cdda_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(DiscSoundInst {
        kind: Kind::Cdda,
        owner: None,
    })) as *mut c_void
}

extern "C" fn midi_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(DiscSoundInst {
        kind: Kind::Midi,
        owner: None,
    })) as *mut c_void
}

extern "C" fn disc_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `cdda_create`/`midi_create` and is freed
    // exactly once.
    unsafe { drop(Box::from_raw(instance as *mut DiscSoundInst)) };
}

extern "C" fn disc_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, "disc sound: compat context is not bound");
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { (instance as *mut DiscSoundInst).as_mut() }) else {
        return report_error(out_error, "disc sound: missing instance");
    };
    // Reference `tTJSNI_BaseSoundBuffer::Construct` stores
    // `ActionOwner = param[0]->AsObjectClosure()` (`SoundBufferBaseIntf.cpp:44-45`);
    // any non-object first argument yields the empty closure.
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    inst.owner = arguments
        .first()
        .and_then(|value| res.engine().retain_object_arg(value).ok());
    // The reference constructor body is empty and returns `TJS_S_OK`; the
    // object itself is the construction result.
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

extern "C" fn disc_open(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if instance_kind(instance).is_none() {
        return report_error(out_error, "disc sound.open: missing instance");
    }
    // `if(numparams < 1) return TJS_E_BADPARAMCOUNT;`
    // (`CDDAIntf.cpp:62-73`, `MIDIIntf.cpp:62-74`).
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    if unsafe { args(argv, argc) }.is_empty() {
        return report_error(out_error, "open: a storage name or CD track is required");
    }
    set_void(out);
    0
}

extern "C" fn disc_play(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if instance_kind(instance).is_none() {
        return report_error(out_error, "disc sound.play: missing instance");
    }
    set_void(out);
    0
}

extern "C" fn disc_stop(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if instance_kind(instance).is_none() {
        return report_error(out_error, "disc sound.stop: missing instance");
    }
    set_void(out);
    0
}

extern "C" fn disc_fade(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if instance_kind(instance).is_none() {
        return report_error(out_error, "disc sound.fade: missing instance");
    }
    // `if(numparams < 2) return TJS_E_BADPARAMCOUNT;`
    // (`CDDAIntf.cpp:99-119`, `MIDIIntf.cpp:100-120`).
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    if unsafe { args(argv, argc) }.len() < 2 {
        return report_error(out_error, "fade(to, timeMs[, delayMs])");
    }
    set_void(out);
    0
}

extern "C" fn disc_stop_fade(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if instance_kind(instance).is_none() {
        return report_error(out_error, "disc sound.stopFade: missing instance");
    }
    set_void(out);
    0
}

// Event forwarders. The reference fires these through the object's action
// owner (`CDDAIntf.cpp:135-164`, `MIDIIntf.cpp:136-165`). The disabled
// playback build never posts them from a timer, but a script may still call
// them directly, so they forward exactly like the reference.

/// Convert one callback argument into the event-member form (the reference
/// `TVP_ACTION_INVOKE_MEMBER` variant copy, `EventIntf.h:218-223`). Objects
/// are retained so they can cross as [`TjsValue::Retained`].
fn disc_event_arg(host: &Tjs2Engine, value: &Value) -> (TjsValue, Option<DetachedValue>) {
    match value.ty {
        VAL_INTEGER => (TjsValue::Integer(value.integer), None),
        VAL_REAL => (TjsValue::Real(value.real), None),
        VAL_STRING => (TjsValue::String(value_as_string(value)), None),
        VAL_VOID => (TjsValue::Void, None),
        _ => match host.retain_object_arg(value) {
            Ok(retained) => (TjsValue::Retained(retained.raw_id() as u64), Some(retained)),
            Err(_) => (TjsValue::Void, None),
        },
    }
}

/// Build the reference action event dictionary (`TVPCreateEventObject` + one
/// `TVP_ACTION_INVOKE_MEMBER`, `EventIntf.cpp:709-736`, `EventIntf.h:208-225`):
/// a dictionary carrying `type`, `target` and the action's own member.
fn disc_action_event(
    host: &Tjs2Engine,
    objthis: *mut c_void,
    event_type: &str,
    member: Option<(&str, TjsValue)>,
) -> Result<DetachedValue, String> {
    let event = match host.eval_retained("%[]", "disc sound.action") {
        Ok(RetainedValue::Object(event)) => event,
        Ok(RetainedValue::Value(value)) => {
            return Err(format!("event dictionary came back as {value:?}"));
        }
        Err(error) => return Err(format!("event dictionary creation failed: {error}")),
    };
    host.set_member(event.raw_id(), "type", &TjsValue::String(event_type.into()))?;
    if !objthis.is_null()
        && let Ok(target) = host.retain_object_detached(objthis)
    {
        let target_id = target.raw_id() as u64;
        let _ = host.set_member(event.raw_id(), "target", &TjsValue::Retained(target_id));
        drop(target);
    }
    if let Some((name, value)) = member {
        host.set_member(event.raw_id(), name, &value)?;
    }
    Ok(event)
}

/// One reference disc-sound action member.
struct DiscActionSpec {
    /// The member name, which is also the event dictionary's `type` and the
    /// name used in error messages.
    what: &'static str,
    /// The event member taken from the first argument, when the action takes
    /// one.
    arg_member: Option<&'static str>,
    /// The reference `argnum` the call must supply.
    required_args: usize,
}

/// The raw arguments of one native member call, valid for the duration of the
/// call only.
struct CallArgs {
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
}

/// Shared body of `onStatusChanged`/`onFadeCompleted` (`CDDAIntf.cpp:135-164`,
/// `MIDIIntf.cpp:136-165` with `TVP_ACTION_INVOKE_*`, `EventIntf.h:208-234`):
/// without an action owner nothing runs and the argument count is never
/// checked (the check lives inside `if(obj.Object)`); otherwise the count is
/// enforced and `actionOwner.action(event)` runs with its result discarded.
fn disc_forward_action(call: CallArgs, spec: DiscActionSpec) -> c_int {
    let CallArgs {
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        objthis,
    } = call;
    let Ok((resolved, _ctx)) = bound(engine) else {
        return report_error(
            out_error,
            &format!("disc sound.{}: compat context is not bound", spec.what),
        );
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { (instance as *mut DiscSoundInst).as_mut() }) else {
        return report_error(
            out_error,
            &format!("disc sound.{}: missing instance", spec.what),
        );
    };
    // The owner is taken out across the VM call and restored afterwards so a
    // re-entrant native sees consistent state.
    let Some(owner) = inst.owner.take() else {
        set_void(out);
        return 0;
    };
    let host = resolved.engine();
    let result = (|| -> Result<(), String> {
        // SAFETY: the trampoline guarantees `argc` valid entries.
        let raw_args = unsafe { args(argv, argc) };
        if raw_args.len() < spec.required_args {
            return Err(format!(
                "disc sound.{}: needs {} argument(s)",
                spec.what, spec.required_args
            ));
        }
        let mut event_member = None;
        let mut member_retained = None;
        if let Some(name) = spec.arg_member {
            let (value, retained) = disc_event_arg(host, &raw_args[0]);
            event_member = Some((name, value));
            member_retained = retained;
        }
        let event = match disc_action_event(host, objthis, spec.what, event_member) {
            Ok(event) => event,
            Err(error) => {
                drop(member_retained);
                return Err(format!("disc sound.{}: {error}", spec.what));
            }
        };
        let event_id = event.raw_id() as u64;
        if let Err(error) =
            host.call_member(owner.raw_id(), "action", &[TjsValue::Retained(event_id)])
        {
            // The reference never inspects this `FuncCall` result
            // (`EventIntf.h:227-234`).
            log::warn!("disc sound.{}: action dispatch failed: {error}", spec.what);
        }
        drop(event);
        drop(member_retained);
        Ok(())
    })();
    inst.owner = Some(owner);
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn disc_on_status_changed(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // `TVP_ACTION_INVOKE_BEGIN(1, "onStatusChanged", objthis)` +
    // `TVP_ACTION_INVOKE_MEMBER("status")` (`CDDAIntf.cpp:135-147`).
    disc_forward_action(
        CallArgs {
            engine,
            instance,
            argc,
            argv,
            out,
            out_error,
            objthis,
        },
        DiscActionSpec {
            what: "onStatusChanged",
            arg_member: Some("status"),
            required_args: 1,
        },
    )
}

extern "C" fn disc_on_fade_completed(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // `TVP_ACTION_INVOKE_BEGIN(0, "onFadeCompleted", objthis)`
    // (`CDDAIntf.cpp:152-164`).
    disc_forward_action(
        CallArgs {
            engine,
            instance,
            argc,
            argv,
            out,
            out_error,
            objthis,
        },
        DiscActionSpec {
            what: "onFadeCompleted",
            arg_member: None,
            required_args: 0,
        },
    )
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// A getter whose value the reference leaves untouched (script `void`).
fn void_getter(
    name: &'static str,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if instance_kind(instance).is_none() {
        return report_error(out_error, &format!("disc sound.{name}: missing instance"));
    }
    set_void(out);
    0
}

/// A setter whose body the reference compiles out: accept the value, change
/// nothing.
fn inert_setter(
    name: &'static str,
    instance: *mut c_void,
    _value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if instance_kind(instance).is_none() {
        return report_error(out_error, &format!("disc sound.{name}: missing instance"));
    }
    0
}

extern "C" fn disc_position_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    void_getter("position", instance, out, out_error)
}

extern "C" fn disc_paused_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    void_getter("paused", instance, out, out_error)
}

extern "C" fn disc_total_time_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    void_getter("totalTime", instance, out, out_error)
}

extern "C" fn disc_looping_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    void_getter("looping", instance, out, out_error)
}

extern "C" fn disc_volume_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match instance_kind(instance) {
        // `ENABLE_CDDA` undefined: the whole getter body is compiled out.
        Some(Kind::Cdda) => set_void(out),
        // `*result = _this->GetVolume();` — the disabled implementation
        // overrides `GetVolume()` to `0` (`MIDIIntf.cpp:262-282`).
        Some(Kind::Midi) => set_int(out, 0),
        None => return report_error(out_error, "disc sound.volume: missing instance"),
    }
    0
}

extern "C" fn disc_volume2_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if instance_kind(instance).is_none() {
        return report_error(out_error, "disc sound.volume2: missing instance");
    }
    // `#else *result = 0;` (`CDDAIntf.cpp:287-313`, `MIDIIntf.cpp:284-310`).
    set_int(out, 0);
    0
}

extern "C" fn disc_status_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if instance_kind(instance).is_none() {
        return report_error(out_error, "disc sound.status: missing instance");
    }
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "disc sound.status: compat context is not bound");
    };
    // The buffer never leaves `ssUnload` in the disabled build, so the base
    // `GetStatusString()` returns "unload" (`SoundBufferBaseIntf.cpp:58-75`).
    set_string_out(&ctx, out, "unload");
    0
}

extern "C" fn disc_position_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    inert_setter("position", instance, value, out_error)
}

extern "C" fn disc_paused_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    inert_setter("paused", instance, value, out_error)
}

extern "C" fn disc_total_time_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    inert_setter("totalTime", instance, value, out_error)
}

extern "C" fn disc_looping_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    inert_setter("looping", instance, value, out_error)
}

extern "C" fn disc_volume_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    inert_setter("volume", instance, value, out_error)
}

extern "C" fn disc_volume2_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    inert_setter("volume2", instance, value, out_error)
}

// ---------------------------------------------------------------------------
// Static members
// ---------------------------------------------------------------------------

/// `MIDISoundBuffer.midiOut(data)`: push raw MIDI bytes to the output device.
///
/// With `TVP_ENABLE_MIDI` undefined the body is a no-op: after the arity
/// check the call returns void (`MIDIImpl.cpp:978-993`).
extern "C" fn midi_out(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    if unsafe { args(argv, argc) }.is_empty() {
        return report_error(out_error, "MIDISoundBuffer.midiOut(data)");
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Resolve the bound engine for a callback.
fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "disc sound: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// The shared instance surface: the constructor is named per class, so the
/// caller passes the class name; every other member is the same function.
fn instance_builder(
    name: &'static str,
    create: NativeCreateInstanceFn,
) -> NativeInstanceBuilder<'static> {
    NativeInstanceBuilder {
        name,
        create,
        destroy: disc_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef { name, f: disc_ctor },
            NativeInstanceMethodDef {
                name: "open",
                f: disc_open,
            },
            NativeInstanceMethodDef {
                name: "play",
                f: disc_play,
            },
            NativeInstanceMethodDef {
                name: "stop",
                f: disc_stop,
            },
            NativeInstanceMethodDef {
                name: "fade",
                f: disc_fade,
            },
            NativeInstanceMethodDef {
                name: "stopFade",
                f: disc_stop_fade,
            },
            NativeInstanceMethodDef {
                name: "onStatusChanged",
                f: disc_on_status_changed,
            },
            NativeInstanceMethodDef {
                name: "onFadeCompleted",
                f: disc_on_fade_completed,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "position",
                get: Some(disc_position_get),
                set: Some(disc_position_set),
            },
            NativeInstancePropertyDef {
                name: "paused",
                get: Some(disc_paused_get),
                set: Some(disc_paused_set),
            },
            NativeInstancePropertyDef {
                name: "totalTime",
                get: Some(disc_total_time_get),
                set: Some(disc_total_time_set),
            },
            NativeInstancePropertyDef {
                name: "looping",
                get: Some(disc_looping_get),
                set: Some(disc_looping_set),
            },
            NativeInstancePropertyDef {
                name: "volume",
                get: Some(disc_volume_get),
                set: Some(disc_volume_set),
            },
            NativeInstancePropertyDef {
                name: "volume2",
                get: Some(disc_volume2_get),
                set: Some(disc_volume2_set),
            },
            NativeInstancePropertyDef {
                name: "status",
                get: Some(disc_status_get),
                set: None,
            },
        ],
    }
}

/// Register the `CDDASoundBuffer` and `MIDISoundBuffer` native classes.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&instance_builder("CDDASoundBuffer", cdda_create))?;
    ctx.register_instance(&instance_builder("MIDISoundBuffer", midi_create))?;
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "MIDISoundBuffer",
        methods: vec![NativeMethodDef {
            name: "midiOut",
            f: midi_out,
        }],
        properties: Vec::new(),
    })?;
    Ok(())
}
