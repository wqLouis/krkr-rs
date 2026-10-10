//! Disc-format sound classes `CDDASoundBuffer` and `MIDISoundBuffer`.
//!
//! The two classes expose the reference `SoundBuffer` surface — `open`,
//! `play`, `stop`, `fade`, `stopFade`, `position`, `volume`, `volume2`,
//! `looping`, `paused`, `totalTime`, `status` and the
//! `onStatusChanged`/`onFadeCompleted` forwarders. They are one **shared
//! behavior core** ([`DiscSoundObject`] + [`DiscSoundState`] over the L2
//! [`engine::sound`] model) with two **thin adapters**: the class only
//! decides how a source is obtained and whether it owns the `midiOut` static.
//!
//! * `CDDASoundBuffer.open` is the reference's CD track selection, whose body
//!   is compiled out when `ENABLE_CDDA` is undefined; the call is a no-op that
//!   returns void and leaves the buffer unloaded.
//! * `MIDISoundBuffer.open` uses the shared generic file path (the L2
//!   [`Storage`](engine::Storage) plus a host decoder, exactly like
//!   `WaveSoundBuffer`); with no decoder installed it raises a typed
//!   [`DiscSoundError::NoDecoder`].
//! * `MIDISoundBuffer.midiOut` is a static method that would push raw bytes to
//!   the MIDI output device; its body is compiled out when `TVP_ENABLE_MIDI`
//!   is undefined, so it returns void after the arity check.
//!
//! `volume`/`volume2` cross the script boundary on the TVP `0..=100000` scale
//! ([`TVP_VOLUME_SCALE`]); the L2 mixer stays in `0..=1`. Every live object,
//! its mixer channel and the shared [`Mixer`] live in
//! [`CompatServices::state`] — there is no process global.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::sync::Arc;

use engine::sound::{Mixer, SoundBuffer, SoundSource, Status};
use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, NativeMethodDef,
    NativeStaticMembers, VAL_VOID, Value,
};

use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::sound::{AudioDecoder, TVP_VOLUME_SCALE};
use crate::natives::system::{resolve, set_string_out, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError, CompatServices};

// ---------------------------------------------------------------------------
// Which disc class an instance belongs to
// ---------------------------------------------------------------------------

/// The two classes differ only in source acquisition and their extra static.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Cdda,
    Midi,
}

impl Kind {
    fn class_name(self) -> &'static str {
        match self {
            Kind::Cdda => "CDDASoundBuffer",
            Kind::Midi => "MIDISoundBuffer",
        }
    }
}

/// Failures raised while servicing a disc-sound call (always surfaced as a
/// typed error, never a silent default).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum DiscSoundError {
    /// L3 links no codec and no host decoder was installed.
    #[error("no audio decoder is installed")]
    NoDecoder,
    /// No L2 storage is attached to the context.
    #[error("no storage is attached")]
    NoStorage,
    /// The named entry could not be read from storage.
    #[error("cannot read {name:?}: {message}")]
    Read {
        /// Storage name that failed.
        name: String,
        /// Underlying storage error.
        message: String,
    },
    /// The decoder rejected the entry.
    #[error("cannot decode {name:?}: {message}")]
    Decode {
        /// Storage name that failed.
        name: String,
        /// Underlying decoder error.
        message: String,
    },
    /// The object's mixer channel was removed underneath it.
    #[error("the mixer channel is gone")]
    ChannelGone,
    /// An L2 sound operation failed.
    #[error("engine: {0}")]
    Engine(String),
    /// The native instance has no registered object id.
    #[error("not constructed")]
    NotConstructed,
}

/// Qualify a typed error with the class and member that raised it.
fn class_error(kind: Kind, what: &str, error: impl std::fmt::Display) -> String {
    format!("{}.{what}: {error}", kind.class_name())
}

// ---------------------------------------------------------------------------
// Per-engine dispatch state (never a process global)
// ---------------------------------------------------------------------------

/// One live disc object: the L2 script-object half and the mixer channel it
/// plays on. The decoded source is kept beside the buffer for `totalTime` and
/// for the generic install path.
struct DiscSoundObject {
    /// L2 script-object half: loaded source, loop flags, label bookkeeping.
    buffer: SoundBuffer,
    /// The mixer channel this object drives.
    channel_id: u64,
    /// The loaded decoded source, kept beside the buffer.
    source: Option<Arc<SoundSource>>,
    /// `paused` is settable before `play` (kept on the object, not the device).
    paused: bool,
    /// True once `open`/`play` put the source on the channel.
    started: bool,
}

/// Disc-sound dispatch state, keyed by type inside
/// [`CompatServices::state`].
#[derive(Default)]
struct DiscSoundState {
    /// The single shared mixer (all objects play through it).
    mixer: Mixer,
    /// Live objects by id.
    objects: HashMap<u64, DiscSoundObject>,
    /// Next object id (ids start at 1; 0 is the "not constructed" sentinel).
    next_id: u64,
    /// The host decoder (see [`set_disc_audio_decoder`]).
    decoder: Option<AudioDecoder>,
}

/// Payload of one `CDDASoundBuffer`/`MIDISoundBuffer` TJS object.
struct DiscSoundInst {
    /// Registered object id (`None` until the constructor runs).
    id: Option<u64>,
    /// Which class this payload backs.
    kind: Kind,
}

/// Install the host's audio decoder into the per-engine disc sound state.
///
/// The L4 host, which links the L1 `media` crate, calls this once at startup;
/// the `compat` integration tests leave it unset, and `MIDISoundBuffer.open`
/// then reports a typed error. Mirrors
/// [`add_audio_decoder`](crate::add_audio_decoder) for `WaveSoundBuffer`.
pub fn set_disc_audio_decoder(services: &CompatServices, decoder: AudioDecoder) {
    services.state::<DiscSoundState>().decoder = Some(decoder);
}

// ---------------------------------------------------------------------------
// Marshalling helpers
// ---------------------------------------------------------------------------

/// Resolve the bound engine for a callback and return its dispatch context.
fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "disc sound: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// Access an object and the shared mixer together (both are disjoint fields of
/// [`DiscSoundState`], so the closure gets independent mutable borrows).
fn with_object<R>(
    services: &CompatServices,
    id: u64,
    f: impl FnOnce(&mut DiscSoundObject, &mut Mixer) -> R,
) -> Option<R> {
    let mut state = services.state::<DiscSoundState>();
    let DiscSoundState { mixer, objects, .. } = &mut *state;
    objects.get_mut(&id).map(|object| f(object, mixer))
}

/// Volume conversion for the script boundary (TVP `0..=100000` ↔ mixer
/// `0..=1`).
fn to_script_volume(value: f32) -> i64 {
    (f64::from(value) * TVP_VOLUME_SCALE).round() as i64
}

fn from_script_volume(value: i64) -> f32 {
    (value as f64 / TVP_VOLUME_SCALE).clamp(0.0, 1.0) as f32
}

/// Resolve the bound context and the object id for a native callback (the
/// common prelude of every instance member), binding the class `kind` too.
macro_rules! with_disc {
    ($engine:expr, $instance:expr, $what:literal, $out_error:expr,
     |$ctx:ident, $services:ident, $id:ident, $kind:ident| $body:block) => {{
        let Ok((_resolved, $ctx)) = bound($engine) else {
            return report_error(
                $out_error,
                concat!("disc sound.", $what, ": compat context is not bound"),
            );
        };
        let Some(inst) = (unsafe { ($instance as *const DiscSoundInst).as_ref() }) else {
            return report_error(
                $out_error,
                concat!("disc sound.", $what, ": missing instance"),
            );
        };
        let $kind = inst.kind;
        let Some($id) = inst.id else {
            return report_error(
                $out_error,
                &class_error($kind, $what, DiscSoundError::NotConstructed),
            );
        };
        // SAFETY: the caller resolved the context from the installed binding.
        let $services = unsafe { $ctx.services() };
        $body
    }};
}

// ---------------------------------------------------------------------------
// Shared behavior core (the common surface, over `engine::sound`)
// ---------------------------------------------------------------------------

/// Read a storage entry and decode it through the host seam, then install the
/// resulting L2 source on the object (the generic `open` path).
fn load(services: &CompatServices, id: u64, name: &str) -> Result<(), DiscSoundError> {
    let Some(storage) = services.storage() else {
        return Err(DiscSoundError::NoStorage);
    };
    // SAFETY: the attached storage outlives the bound context.
    let audio = unsafe { &*storage }
        .read(name)
        .map_err(|error| DiscSoundError::Read {
            name: name.to_string(),
            message: error.to_string(),
        })?;
    let source = {
        let state = services.state::<DiscSoundState>();
        let Some(decoder) = state.decoder.as_ref() else {
            return Err(DiscSoundError::NoDecoder);
        };
        // A MIDI sequence carries no `.sli` loop side-car.
        decoder(&audio, None, Some(name)).map_err(|message| DiscSoundError::Decode {
            name: name.to_string(),
            message,
        })?
    };
    install_source(services, id, source)
}

/// Put a decoded source on the L2 buffer and its channel, leaving it loaded
/// but unstarted (the reference's post-`open` state).
fn install_source(
    services: &CompatServices,
    id: u64,
    source: Arc<SoundSource>,
) -> Result<(), DiscSoundError> {
    with_object(services, id, |object, mixer| {
        object.source = Some(source.clone());
        object.paused = false;
        object.started = false;
        object.buffer.open(source);
        let Some(channel) = mixer.channel(object.channel_id) else {
            return Err(DiscSoundError::ChannelGone);
        };
        if object.buffer.play_on(channel).is_ok() {
            channel.stop();
            channel.set_position(0.0);
        }
        channel.looping = false;
        Ok(())
    })
    .ok_or(DiscSoundError::NotConstructed)?
}

fn play(services: &CompatServices, id: u64) -> Result<(), DiscSoundError> {
    with_object(services, id, |object, mixer| {
        let Some(channel) = mixer.channel(object.channel_id) else {
            return Err(DiscSoundError::ChannelGone);
        };
        if !object.started {
            if object.source.is_none() {
                // The reference `Play()` returns when no source is open.
                return Ok(());
            }
            object
                .buffer
                .play_on(channel)
                .map_err(|error| DiscSoundError::Engine(error.to_string()))?;
            object.started = true;
        }
        if object.paused {
            channel.pause();
        }
        Ok(())
    })
    .ok_or(DiscSoundError::NotConstructed)?
}

fn stop(services: &CompatServices, id: u64) -> Result<(), DiscSoundError> {
    with_object(services, id, |object, mixer| {
        object.started = false;
        if let Some(channel) = mixer.channel(object.channel_id) {
            channel.stop();
            // The reference `Stop()` rewinds to the start.
            channel.set_position(0.0);
        }
        Ok(())
    })
    .ok_or(DiscSoundError::NotConstructed)?
}

fn set_paused(services: &CompatServices, id: u64, paused: bool) -> Result<(), DiscSoundError> {
    with_object(services, id, |object, mixer| {
        object.paused = paused;
        if let Some(channel) = mixer.channel(object.channel_id) {
            if paused {
                channel.pause();
            } else {
                channel.resume();
            }
        }
        Ok(())
    })
    .ok_or(DiscSoundError::NotConstructed)?
}

fn fade(
    services: &CompatServices,
    id: u64,
    to: f32,
    duration: f64,
    delay: f64,
) -> Result<(), DiscSoundError> {
    with_object(services, id, |object, mixer| {
        let Some(channel) = mixer.channel(object.channel_id) else {
            return Err(DiscSoundError::ChannelGone);
        };
        if !object.started {
            if object.source.is_none() {
                // The reference `Fade()` plays only a loaded buffer.
                return Ok(());
            }
            object
                .buffer
                .play_on(channel)
                .map_err(|error| DiscSoundError::Engine(error.to_string()))?;
            object.started = true;
        }
        object.paused = false;
        channel.fade(to, duration, delay);
        Ok(())
    })
    .ok_or(DiscSoundError::NotConstructed)?
}

fn stop_fade(services: &CompatServices, id: u64) -> Result<(), DiscSoundError> {
    with_object(services, id, |object, mixer| {
        if let Some(channel) = mixer.channel(object.channel_id) {
            // Cancel the fade while keeping the current effective value
            // (`set_volume` clears the fade; reference `StopFade`).
            let current = channel.effective_volume();
            channel.set_volume(current);
        }
        Ok(())
    })
    .ok_or(DiscSoundError::NotConstructed)?
}

// ---------------------------------------------------------------------------
// Constructor / destructor
// ---------------------------------------------------------------------------

extern "C" fn cdda_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(DiscSoundInst {
        id: None,
        kind: Kind::Cdda,
    })) as *mut c_void
}

extern "C" fn midi_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(DiscSoundInst {
        id: None,
        kind: Kind::Midi,
    })) as *mut c_void
}

extern "C" fn disc_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `cdda_create`/`midi_create` and is freed
    // exactly once.
    let inst = unsafe { Box::from_raw(instance as *mut DiscSoundInst) };
    let Some(id) = inst.id else {
        return;
    };
    let Some(resolved) = resolve(engine) else {
        // The binding is removed before the engine is destroyed, in which case
        // the state is dropped with the services anyway.
        return;
    };
    let ctx = resolved.ctx();
    // SAFETY: the resolved context is bound and alive.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<DiscSoundState>();
    if let Some(object) = state.objects.remove(&id) {
        let DiscSoundState { mixer, .. } = &mut *state;
        mixer.remove_channel(object.channel_id);
    }
}

extern "C" fn disc_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "disc sound: compat context is not bound");
    };
    // SAFETY: the VM passes the payload created by `cdda_create`/`midi_create`.
    let inst = unsafe { &mut *(instance as *mut DiscSoundInst) };
    if inst.id.is_some() {
        return report_error(
            out_error,
            &class_error(inst.kind, inst.kind.class_name(), "already constructed"),
        );
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let id = {
        let mut state = services.state::<DiscSoundState>();
        let id = state.next_id.max(1);
        state.next_id = id + 1;
        let channel_id = state.mixer.spawn_channel();
        state.objects.insert(
            id,
            DiscSoundObject {
                buffer: SoundBuffer::new(),
                channel_id,
                source: None,
                paused: false,
                started: false,
            },
        );
        id
    };
    inst.id = Some(id);
    set_int(out, id as i64);
    0
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

extern "C" fn disc_open(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let raw_args = unsafe { args(argv, argc) };
    if raw_args.is_empty() {
        return report_error(out_error, "open: a storage name or CD track is required");
    }
    let name = value_as_string(&raw_args[0]);
    with_disc!(
        engine,
        instance,
        "open",
        out_error,
        |_ctx, services, id, kind| {
            match kind {
                // `ENABLE_CDDA` is undefined: `open` returns void without
                // loading the buffer.
                Kind::Cdda => {
                    set_void(out);
                    0
                }
                Kind::Midi => match load(services, id, &name) {
                    Ok(()) => {
                        set_void(out);
                        0
                    }
                    Err(error) => report_error(out_error, &class_error(kind, "open", error)),
                },
            }
        }
    )
}

extern "C" fn disc_play(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_disc!(
        engine,
        instance,
        "play",
        out_error,
        |_ctx, services, id, kind| {
            match play(services, id) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &class_error(kind, "play", error)),
            }
        }
    )
}

extern "C" fn disc_stop(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_disc!(
        engine,
        instance,
        "stop",
        out_error,
        |_ctx, services, id, kind| {
            match stop(services, id) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &class_error(kind, "stop", error)),
            }
        }
    )
}

extern "C" fn disc_fade(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let raw_args = unsafe { args(argv, argc) };
    if raw_args.len() < 2 {
        return report_error(out_error, "fade(to, timeMs[, delayMs])");
    }
    let to = value_as_i64(&raw_args[0]) as f64 / TVP_VOLUME_SCALE;
    let duration = value_as_i64(&raw_args[1]).max(0) as f64 / 1000.0;
    let delay = if raw_args.len() >= 3 && raw_args[2].ty != VAL_VOID {
        value_as_i64(&raw_args[2]).max(0) as f64 / 1000.0
    } else {
        0.0
    };
    with_disc!(
        engine,
        instance,
        "fade",
        out_error,
        |_ctx, services, id, kind| {
            match fade(services, id, to.clamp(0.0, 1.0) as f32, duration, delay) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &class_error(kind, "fade", error)),
            }
        }
    )
}

extern "C" fn disc_stop_fade(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_disc!(
        engine,
        instance,
        "stopFade",
        out_error,
        |_ctx, services, id, kind| {
            match stop_fade(services, id) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &class_error(kind, "stopFade", error)),
            }
        }
    )
}

// Event forwarders. The reference fires these through the object's action
// owner; this port has no disc-sound frame poll wired, so the native members exist
// so `super.onStatusChanged(...)` still resolves (mirroring `WaveSoundBuffer`).

extern "C" fn disc_on_status_changed(
    engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // Reference `TVP_ACTION_INVOKE_BEGIN(1, "onStatusChanged", objthis)`
    // requires one argument when the member is invoked directly
    // (`CDDAIntf.cpp:137-149`, `MIDIIntf.cpp:138-150`).
    if argc < 1 {
        return report_error(out_error, "onStatusChanged: 1 argument required");
    }
    if bound(engine).is_err() {
        return report_error(
            out_error,
            "disc sound.onStatusChanged: compat context is not bound",
        );
    }
    set_void(out);
    0
}

extern "C" fn disc_on_fade_completed(
    engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if bound(engine).is_err() {
        return report_error(
            out_error,
            "disc sound.onFadeCompleted: compat context is not bound",
        );
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// Get an integer property from the object's channel.
macro_rules! disc_channel_int_getter {
    ($fn_name:ident, $what:literal, |$object:ident, $channel:ident| $body:expr) => {
        extern "C" fn $fn_name(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_disc!(
                engine,
                instance,
                $what,
                out_error,
                |_ctx, services, id, _kind| {
                    let value = with_object(services, id, |$object, mixer| {
                        let $channel = mixer.channel($object.channel_id);
                        $channel.map(|$channel| $body)
                    })
                    .flatten();
                    set_int(out, value.unwrap_or(0));
                    0
                }
            )
        }
    };
}

/// Set an integer property through a closure over the channel.
macro_rules! disc_channel_setter {
    ($fn_name:ident, $what:literal, $value:ident, $out_error:ident, |$channel:ident| $body:block) => {
        extern "C" fn $fn_name(
            engine: *mut c_void,
            instance: *mut c_void,
            $value: *const Value,
            $out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_disc!(
                engine,
                instance,
                $what,
                $out_error,
                |_ctx, services, id, _kind| {
                    with_object(services, id, |object, mixer| {
                        if let Some($channel) = mixer.channel(object.channel_id) {
                            $body
                        }
                    });
                    0
                }
            )
        }
    };
}

disc_channel_int_getter!(disc_position_get, "position", |_object, channel| {
    (channel.position_seconds() * 1000.0).round() as i64
});

disc_channel_int_getter!(disc_total_time_get, "totalTime", |object, _channel| {
    object.source.as_ref().map_or(0, |source| {
        (source.duration_seconds() * 1000.0).round() as i64
    })
});

disc_channel_int_getter!(disc_looping_get, "looping", |_object, channel| {
    i64::from(channel.looping)
});

disc_channel_int_getter!(disc_volume_get, "volume", |_object, channel| {
    to_script_volume(channel.volume())
});

disc_channel_int_getter!(disc_volume2_get, "volume2", |_object, channel| {
    to_script_volume(channel.volume2())
});

extern "C" fn disc_paused_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_disc!(
        engine,
        instance,
        "paused",
        out_error,
        |_ctx, services, id, _kind| {
            let value = with_object(services, id, |object, _mixer| Some(object.paused)).flatten();
            set_int(out, i64::from(value.unwrap_or(false)));
            0
        }
    )
}

extern "C" fn disc_status_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_disc!(
        engine,
        instance,
        "status",
        out_error,
        |ctx, services, id, _kind| {
            let status = with_object(services, id, |object, mixer| {
                mixer
                    .channel(object.channel_id)
                    .map(|channel| channel.status())
            })
            .flatten()
            .unwrap_or(Status::Unload);
            set_string_out(&ctx, out, status.as_str());
            0
        }
    )
}

disc_channel_setter!(disc_position_set, "position", value, out_error, |channel| {
    channel.set_position(value_as_i64(unsafe { &*value }).max(0) as f64 / 1000.0);
});

disc_channel_setter!(disc_looping_set, "looping", value, out_error, |channel| {
    channel.looping = value_as_bool(unsafe { &*value });
});

disc_channel_setter!(disc_volume_set, "volume", value, out_error, |channel| {
    channel.set_volume(from_script_volume(value_as_i64(unsafe { &*value })));
});

disc_channel_setter!(disc_volume2_set, "volume2", value, out_error, |channel| {
    channel.set_volume2(from_script_volume(value_as_i64(unsafe { &*value })));
});

extern "C" fn disc_paused_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let paused = value_as_bool(unsafe { &*value });
    with_disc!(
        engine,
        instance,
        "paused",
        out_error,
        |_ctx, services, id, kind| {
            match set_paused(services, id, paused) {
                Ok(()) => 0,
                Err(error) => report_error(out_error, &class_error(kind, "paused", error)),
            }
        }
    )
}

// ---------------------------------------------------------------------------
// Static members
// ---------------------------------------------------------------------------

/// `MIDISoundBuffer.midiOut(data)`: push raw MIDI bytes to the output device.
///
/// With `TVP_ENABLE_MIDI` undefined the body is a no-op: after the arity
/// check the call returns void.
extern "C" fn midi_out(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if bound(engine).is_err() {
        return report_error(
            out_error,
            "MIDISoundBuffer.midiOut: compat context is not bound",
        );
    }
    if unsafe { args(argv, argc) }.is_empty() {
        return report_error(out_error, "MIDISoundBuffer.midiOut(data)");
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// The shared instance surface: the constructor is named per class, so the
/// caller passes the class name; every other member is the same function.
fn instance_builder(
    name: &'static str,
    create: tjs::NativeCreateInstanceFn,
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
                set: None,
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
