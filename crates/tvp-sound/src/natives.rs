//! The TVP sound native classes: `SoundBuffer` (a decoded source) and
//! `SoundChannel` (a playable mixer channel).
//!
//! # Class surface (this wave)
//!
//! **`SoundBuffer`** — a fully-decoded audio source:
//!
//! - `new SoundBuffer()` — creates an unloaded buffer. (The reference's
//!   `SoundBuffer(owner)` takes an *owner object* for status events; the
//!   current C ABI cannot pass constructor arguments to the Rust payload,
//!   so the argument is silently dropped by the VM and buffers are loaded
//!   explicitly with `open` — the same load-by-storage-name path the game
//!   uses via `WaveSoundBuffer.open`.)
//! - `open(name)` — read and probe `name` from storage, then decode it on a
//!   background worker (whole-file for short sounds, bounded streaming for
//!   long tracks). Throws a TJS error when the entry is missing; the buffer
//!   becomes "most recently opened" (see [`SoundChannel.play`] object-argument
//!   resolution) and reports `"ready"` once decoding completes.
//! - `getBufferId()` → integer id (0 while unloaded). Buffers are also
//!   addressable by id, because object arguments cannot cross the ABI.
//! - `getBufferInfo()` → string `"(rate,channels,length_seconds)"`, e.g.
//!   `"(44100,2,5.25)"`; void while unloaded. The reference returns a
//!   `"wav,44100,2,16"`-style format string; the task's surface asks for
//!   `(rate,channels,length)`, which is what we return (objects cannot
//!   cross the FFI, so a single string it is).
//! - `getStatus()` → `"unload"` | `"ready"` (reference statuses that make
//!   sense for a pure source; playback states live on the channel).
//!
//! **`SoundChannel`** — one mixer channel:
//!
//! - `new SoundChannel()` — allocates a mixer channel.
//! - `play(bufferOrNameOrId)` — start playback of a buffer, from position
//!   0. Accepts a `SoundBuffer` instance, a storage name (decoded on the
//!   fly), or a buffer id. Restarting with another buffer switches the
//!   source (reference: `SoundChannel.play(buffer)`).
//! - `stop()`, `pause()`, `resume()`.
//! - `getPosition()` / `setPosition(seconds)` — playback position.
//! - `getVolume()` / `setVolume(0..1)` — clamped; `setVolume` cancels an
//!   active fade (reference: volume is `0..=100000`).
//! - `getPan()` / `setPan(-1..1)` — clamped (reference: `-100000..100000`).
//! - `getLoop()` / `setLoop(bool)`.
//! - `isPlaying()`, `isDone()` (a non-looping source that reached its end),
//!   `getStatus()` → `"unload"` | `"play"` | `"pause"` | `"stop"`.
//! - `fade(target, timeMs[, delayMs])` — reference `Fade(to, time,
//!   blanktime)`: linear ramp to `target` over `timeMs`, after `delayMs`.
//! - `fadeIn(timeMs[, target=1])` / `fadeOut(timeMs[, target=0])` — the
//!   task's convenience forms of the same ramp.
//!
//! # ABI limits that shape this surface
//!
//! - The `tjs2_register_native_class_instance` ABI passes **no constructor
//!   arguments** to `create`, and object arguments/returns cross as opaque
//!   handles with no property access. So: loading is an explicit `open`,
//!   and `play` resolves its argument by type (string → storage name,
//!   integer → buffer id, object → the most recently opened buffer).
//! - Instance **properties** are not supported by the instance-class ABI
//!   yet (only methods), so every reference property (`position`, `volume`,
//!   `pan`, `looping`, `status`, `bufferInfo`, ...) is exposed as
//!   `getX`/`setX` methods. This matches the reference, which exposes both
//!   forms (`getVolume`/`setVolume`, `getPan`/`setPan`, ...).

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use engine::Storage;
use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstanceMethodFn, Tjs2Engine, VAL_OBJECT,
    VAL_STRING,
};

use crate::ffi;
use crate::mixer::{Mixer, lock_ok};
use crate::player::{MainThreadOutputGuard, start_output_on_main_thread};
use crate::source::{AudioTrack, open_track};

/// Engine-side context every native method needs: the mounted storage (for
/// decoding) and the mixer (for channels). Set by [`register_sound`] on the
/// VM thread. The VM is single-threaded, so one process-global slot is
/// safe — and **required**: Bevy's parallel scheduler runs `Startup` and
/// `Update` systems on different worker threads, so a thread-local would
/// be invisible to the natives called from `run_vm` on another thread.
#[derive(Clone)]
pub(crate) struct NativeContext {
    pub(crate) storage: Arc<Mutex<Storage>>,
    pub(crate) mixer: Arc<Mutex<Mixer>>,
}

static NATIVE_CTX: std::sync::Mutex<Option<NativeContext>> = std::sync::Mutex::new(None);

pub(crate) fn native_ctx() -> Option<NativeContext> {
    NATIVE_CTX.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Point the native classes at the mounted storage and mixer (called by
/// [`register_sound`]).
fn set_native_ctx(storage: Arc<Mutex<Storage>>, mixer: Arc<Mutex<Mixer>>) {
    *NATIVE_CTX.lock().unwrap_or_else(|p| p.into_inner()) = Some(NativeContext { storage, mixer });
}

// ---------------------------------------------------------------------------
// buffer registry
// ---------------------------------------------------------------------------

/// Decoded/loading audio by buffer id. Buffers are addressable by id
/// because object arguments cannot cross the ABI.
static BUFFER_AUDIO: LazyLock<Mutex<HashMap<u64, Arc<AudioTrack>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The most recently opened buffer's track — the resolution used by
/// `SoundChannel.play(buffer)` for object arguments (the ABI hands us an
/// opaque object handle, not the object's identity; opening a buffer right
/// before playing it is the idiomatic pattern this heuristic covers).
static LAST_OPENED: LazyLock<Mutex<Option<Arc<AudioTrack>>>> = LazyLock::new(|| Mutex::new(None));

static NEXT_BUFFER_ID: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// SoundBuffer
// ---------------------------------------------------------------------------

/// Payload of one `SoundBuffer` TJS object.
struct SoundBufferInst {
    /// Buffer id (0 = unloaded).
    id: u64,
    /// Source track, present after a successful `open` (possibly still
    /// loading or streaming on a background worker).
    track: Option<Arc<AudioTrack>>,
}

extern "C" fn sb_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(SoundBufferInst { id: 0, track: None })) as *mut c_void
}

extern "C" fn sb_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from sb_create (Box::into_raw), exactly once.
    unsafe { drop(Box::from_raw(instance as *mut SoundBufferInst)) };
}

/// `SoundBuffer.open(name)`: read and probe `name` from storage, then
/// decode it on a background worker. Throws synchronously only when the
/// entry is missing or its container cannot be probed; a later packet
/// decode failure surfaces as `getStatus()` staying `"unload"` with a
/// logged warning.
extern "C" fn sb_open(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid SoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<SoundBufferInst>(instance) };
    let args = ffi::args(argv, argc);
    let Some(name) = args.first() else {
        return ffi::report_error(out_error, "SoundBuffer.open requires a storage name");
    };
    let name = ffi::value_as_string(name);
    if name.is_empty() {
        return ffi::report_error(out_error, "SoundBuffer.open requires a storage name");
    }
    let Some(ctx) = native_ctx() else {
        return ffi::report_error(out_error, "sound natives are not registered");
    };
    let track = match open_track(&ctx.storage, &name) {
        Ok(t) => t,
        Err(e) => return ffi::report_error(out_error, &format!("SoundBuffer.open: {e}")),
    };
    let id = NEXT_BUFFER_ID.fetch_add(1, Ordering::SeqCst);
    lock_ok(&BUFFER_AUDIO).insert(id, track.clone());
    *lock_ok(&LAST_OPENED) = Some(track.clone());
    inst.track = Some(track);
    inst.id = id;
    ffi::set_void_out(out);
    0
}

/// `SoundBuffer.getBufferId()` → integer buffer id (0 while unloaded).
extern "C" fn sb_get_buffer_id(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid SoundBufferInst payload for the call.
    let inst = unsafe { &*ffi::instance_ptr::<SoundBufferInst>(instance) };
    ffi::set_int_out(out, inst.id as i64);
    0
}

/// `SoundBuffer.getBufferInfo()` → `"(rate,channels,length)"`; void while
/// unloaded.
extern "C" fn sb_get_buffer_info(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid SoundBufferInst payload for the call.
    let inst = unsafe { &*ffi::instance_ptr::<SoundBufferInst>(instance) };
    match &inst.track {
        Some(t) => {
            let duration = t.known_duration_seconds().unwrap_or(0.0);
            let info = format!("({},{},{:.2})", t.sample_rate(), t.channels(), duration);
            ffi::set_string_out(out, &info);
        }
        None => ffi::set_void_out(out),
    }
    0
}

/// `SoundBuffer.getStatus()` → `"unload"` | `"ready"`.
extern "C" fn sb_get_status(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid SoundBufferInst payload for the call.
    let inst = unsafe { &*ffi::instance_ptr::<SoundBufferInst>(instance) };
    ffi::set_string_out(
        out,
        if inst.track.as_ref().is_some_and(|t| t.is_ready()) {
            "ready"
        } else {
            "unload"
        },
    );
    0
}

// ---------------------------------------------------------------------------
// SoundChannel
// ---------------------------------------------------------------------------

/// Payload of one `SoundChannel` TJS object: a thin handle onto a mixer
/// channel.
struct SoundChannelInst {
    /// Mixer channel id (0 when the natives were not registered, so the
    /// channel could not be spawned — every method errors out then).
    channel_id: u64,
}

extern "C" fn sc_create(_engine: *mut c_void) -> *mut c_void {
    let channel_id = native_ctx().map_or(0, |ctx| lock_ok(&ctx.mixer).spawn_channel());
    Box::into_raw(Box::new(SoundChannelInst { channel_id })) as *mut c_void
}

extern "C" fn sc_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from sc_create (Box::into_raw), exactly once.
    let inst = unsafe { Box::from_raw(instance as *mut SoundChannelInst) };
    if let Some(ctx) = native_ctx() {
        lock_ok(&ctx.mixer).remove_channel(inst.channel_id);
    }
}

/// Run a body with the payload's mixer channel. Errors out when the natives
/// are not registered or the channel vanished. `$ch` is the caller-supplied
/// name the body uses for the channel (a `macro_rules` hygiene requirement:
/// identifiers introduced inside the macro are not visible to the body).
macro_rules! with_channel {
    ($instance:expr, $out_error:expr, $ch:ident, $body:block) => {{
        // SAFETY: instance is a valid SoundChannelInst payload for the call.
        let _inst = unsafe { &mut *ffi::instance_ptr::<SoundChannelInst>($instance) };
        let Some(ctx) = native_ctx() else {
            return ffi::report_error($out_error, "sound natives are not registered");
        };
        let mut mixer = lock_ok(&ctx.mixer);
        let Some($ch) = mixer.channel(_inst.channel_id) else {
            return ffi::report_error(
                $out_error,
                &format!("SoundChannel#{}: mixer channel missing", _inst.channel_id),
            );
        };
        $body
    }};
}

/// `SoundChannel.play(bufferOrNameOrId)`: start (or restart) a source from
/// position 0. See the module docs for argument resolution.
extern "C" fn sc_play(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid SoundChannelInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<SoundChannelInst>(instance) };
    let args = ffi::args(argv, argc);
    let Some(arg) = args.first() else {
        return ffi::report_error(
            out_error,
            "SoundChannel.play requires an argument (storage name, buffer id, or SoundBuffer)",
        );
    };
    let Some(ctx) = native_ctx() else {
        return ffi::report_error(out_error, "sound natives are not registered");
    };

    let source = match arg.ty {
        VAL_STRING => {
            let name = ffi::value_as_string(arg);
            match open_track(&ctx.storage, &name) {
                Ok(t) => t,
                Err(e) => return ffi::report_error(out_error, &format!("SoundChannel.play: {e}")),
            }
        }
        tjs2_sys::VAL_INTEGER => match lock_ok(&BUFFER_AUDIO).get(&(arg.integer as u64)).cloned() {
            Some(a) => a,
            None => {
                return ffi::report_error(
                    out_error,
                    &format!("SoundChannel.play: no buffer with id {}", arg.integer),
                );
            }
        },
        VAL_OBJECT => {
            // Object arguments cross the ABI as opaque handles; resolve the
            // most recently opened buffer (see module docs).
            match lock_ok(&LAST_OPENED).clone() {
                Some(a) => a,
                None => {
                    return ffi::report_error(
                        out_error,
                        "SoundChannel.play: cannot resolve the buffer object (none has been \
                         opened yet); pass the storage name or buffer id instead",
                    );
                }
            }
        }
        _ => return ffi::report_error(out_error, "SoundChannel.play: unsupported argument type"),
    };

    {
        let mut mixer = lock_ok(&ctx.mixer);
        let Some(ch) = mixer.channel(inst.channel_id) else {
            return ffi::report_error(
                out_error,
                &format!("SoundChannel#{}: mixer channel missing", inst.channel_id),
            );
        };
        ch.play_track(source);
    }
    ffi::set_void_out(out);
    0
}

/// `SoundChannel.stop()` — stop playback, keep the position.
extern "C" fn sc_stop(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        ch.stop();
        ffi::set_void_out(out);
        0
    })
}

/// `SoundChannel.pause()`.
extern "C" fn sc_pause(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        ch.pause();
        ffi::set_void_out(out);
        0
    })
}

/// `SoundChannel.resume()`.
extern "C" fn sc_resume(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        ch.resume();
        ffi::set_void_out(out);
        0
    })
}

/// `SoundChannel.getPosition()` → seconds.
extern "C" fn sc_get_position(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        ffi::set_real_out(out, ch.position_seconds);
        0
    })
}

/// `SoundChannel.setPosition(seconds)` — seek.
extern "C" fn sc_set_position(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        let args = ffi::args(argv, argc);
        let Some(v) = args.first() else {
            return ffi::report_error(out_error, "SoundChannel.setPosition requires a number");
        };
        ch.set_position(ffi::value_as_f64(v));
        ffi::set_void_out(out);
        0
    })
}

/// `SoundChannel.getVolume()` → `0..1` (the effective volume: while a fade
/// is active, the reference's `volume` reads the ramped value).
extern "C" fn sc_get_volume(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        ffi::set_real_out(out, f64::from(ch.effective_volume()));
        0
    })
}

/// `SoundChannel.setVolume(0..1)` — clamped; cancels an active fade.
extern "C" fn sc_set_volume(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        let args = ffi::args(argv, argc);
        let Some(v) = args.first() else {
            return ffi::report_error(out_error, "SoundChannel.setVolume requires a number");
        };
        ch.set_volume(ffi::value_as_f64(v) as f32);
        ffi::set_void_out(out);
        0
    })
}

/// `SoundChannel.getPan()` → `-1..1`.
extern "C" fn sc_get_pan(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        ffi::set_real_out(out, f64::from(ch.pan));
        0
    })
}

/// `SoundChannel.setPan(-1..1)` — clamped (`-1` left, `+1` right).
extern "C" fn sc_set_pan(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        let args = ffi::args(argv, argc);
        let Some(v) = args.first() else {
            return ffi::report_error(out_error, "SoundChannel.setPan requires a number");
        };
        ch.set_pan(ffi::value_as_f64(v) as f32);
        ffi::set_void_out(out);
        0
    })
}

/// `SoundChannel.getLoop()` → bool (0/1).
extern "C" fn sc_get_loop(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        ffi::set_int_out(out, i64::from(ch.looping));
        0
    })
}

/// `SoundChannel.setLoop(bool)`.
extern "C" fn sc_set_loop(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        let args = ffi::args(argv, argc);
        let Some(v) = args.first() else {
            return ffi::report_error(out_error, "SoundChannel.setLoop requires a boolean");
        };
        ch.looping = ffi::value_as_bool(v);
        ffi::set_void_out(out);
        0
    })
}

/// `SoundChannel.isPlaying()` → bool.
extern "C" fn sc_is_playing(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        ffi::set_int_out(out, i64::from(ch.is_playing()));
        0
    })
}

/// `SoundChannel.isDone()` → bool (a non-looping source reached its end).
extern "C" fn sc_is_done(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        ffi::set_int_out(out, i64::from(ch.done));
        0
    })
}

/// `SoundChannel.getStatus()` → `"unload"` | `"play"` | `"pause"` |
/// `"stop"`.
extern "C" fn sc_get_status(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        let status = match (&ch.source, ch.playing, ch.paused) {
            (None, _, _) => "unload",
            (Some(s), _, _) if s.is_failed() => "stop",
            (Some(s), _, _) if !s.is_ready() => "unload",
            (_, true, false) => "play",
            (_, true, true) => "pause",
            (_, false, _) => "stop",
        };
        ffi::set_string_out(out, status);
        0
    })
}

/// `SoundChannel.fade(target, timeMs[, delayMs])` — reference
/// `Fade(to, time, blanktime)`: linear ramp from the current volume to
/// `target` over `timeMs`, after `delayMs`.
extern "C" fn sc_fade(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        let args = ffi::args(argv, argc);
        if args.len() < 2 {
            return ffi::report_error(out_error, "SoundChannel.fade requires target and timeMs");
        }
        let to = ffi::value_as_f64(&args[0]) as f32;
        let time_ms = ffi::value_as_f64(&args[1]);
        let delay_ms = if args.len() > 2 {
            ffi::value_as_f64(&args[2])
        } else {
            0.0
        };
        if time_ms < 0.0 || delay_ms < 0.0 {
            return ffi::report_error(out_error, "SoundChannel.fade: negative time");
        }
        ch.fade(to, time_ms / 1000.0, delay_ms / 1000.0);
        ffi::set_void_out(out);
        0
    })
}

/// `SoundChannel.fadeIn(timeMs[, target=1])` — ramp to `target`.
extern "C" fn sc_fade_in(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        let args = ffi::args(argv, argc);
        let Some(v) = args.first() else {
            return ffi::report_error(out_error, "SoundChannel.fadeIn requires timeMs");
        };
        let time_ms = ffi::value_as_f64(v);
        let target = if args.len() > 1 {
            ffi::value_as_f64(&args[1]) as f32
        } else {
            1.0
        };
        if time_ms < 0.0 {
            return ffi::report_error(out_error, "SoundChannel.fadeIn: negative time");
        }
        ch.fade(target, time_ms / 1000.0, 0.0);
        ffi::set_void_out(out);
        0
    })
}

/// `SoundChannel.fadeOut(timeMs[, target=0])` — ramp to `target` (default
/// silence).
extern "C" fn sc_fade_out(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_channel!(instance, out_error, ch, {
        let args = ffi::args(argv, argc);
        let Some(v) = args.first() else {
            return ffi::report_error(out_error, "SoundChannel.fadeOut requires timeMs");
        };
        let time_ms = ffi::value_as_f64(v);
        let target = if args.len() > 1 {
            ffi::value_as_f64(&args[1]) as f32
        } else {
            0.0
        };
        if time_ms < 0.0 {
            return ffi::report_error(out_error, "SoundChannel.fadeOut: negative time");
        }
        ch.fade(target, time_ms / 1000.0, 0.0);
        ffi::set_void_out(out);
        0
    })
}

// ---------------------------------------------------------------------------
// registration
// ---------------------------------------------------------------------------

fn method(name: &'static str, f: NativeInstanceMethodFn) -> NativeInstanceMethodDef {
    NativeInstanceMethodDef { name, f }
}

/// Set the rodio output to start (or stop) streaming the global mixer to
/// the real audio device.
///
/// The stream is opened **on the calling thread** (rodio/cpal streams are
/// `!Send`/`!Sync`), so call this from the main thread once at startup —
/// e.g. in the app's `Startup` after `register_sound`, or in a dedicated
/// audio-startup system. On machines with no audio device this logs and
/// falls back to silent (headless) advancement; it never panics.
///
/// Returns the guard that keeps the stream alive; it must be **held** (in
/// a `Resource` on the main thread) and dropped on the same thread it was
/// created on.
pub fn set_sound_output_enabled(
    main_thread_token: std::thread::ThreadId,
    enabled: bool,
) -> Result<Option<MainThreadOutputGuard>, crate::player::OutputError> {
    if !enabled {
        return Ok(None);
    }
    let mixer = crate::global_mixer().ok_or_else(|| {
        crate::player::OutputError::NoDevice("no global mixer (register_sound not called)".into())
    })?;
    start_output_on_main_thread(main_thread_token, mixer).map(Some)
}

/// Register the `SoundBuffer` and `SoundChannel` native classes on
/// `engine`, pointing them at `storage` (and creating the process-wide
/// mixer used by [`crate::advance`]).
///
/// Must be called on the VM thread before any script uses the classes.
///
/// # Audio output wiring
///
/// Registration does **not** open an audio device by itself (the mixer is
/// pure computation and must keep working headless). The app is
/// responsible for pulling the mixer to the speakers; the supported way is
/// to call [`set_sound_output_enabled`] from the main thread right after
/// this returns (see the function docs for the `!Send`/`!Sync` caveat).
pub fn register_sound(engine: &Tjs2Engine, storage: Arc<Mutex<Storage>>) -> Result<(), String> {
    let mixer = Arc::new(Mutex::new(Mixer::new()));
    set_native_ctx(storage, mixer.clone());
    crate::set_global_mixer(mixer);

    // The real `WaveSoundBuffer` (the game's `SoundBuffer` derives from it).
    crate::wavesound::register_wavesound(engine)?;

    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "SoundBuffer",
        create: sb_create,
        destroy: sb_destroy,
        methods: vec![
            method("open", sb_open),
            method("getBufferId", sb_get_buffer_id),
            method("getBufferInfo", sb_get_buffer_info),
            method("getStatus", sb_get_status),
        ],
        properties: vec![],
    })?;

    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "SoundChannel",
        create: sc_create,
        destroy: sc_destroy,
        methods: vec![
            method("play", sc_play),
            method("stop", sc_stop),
            method("pause", sc_pause),
            method("resume", sc_resume),
            method("getPosition", sc_get_position),
            method("setPosition", sc_set_position),
            method("getVolume", sc_get_volume),
            method("setVolume", sc_set_volume),
            method("getPan", sc_get_pan),
            method("setPan", sc_set_pan),
            method("getLoop", sc_get_loop),
            method("setLoop", sc_set_loop),
            method("isPlaying", sc_is_playing),
            method("isDone", sc_is_done),
            method("getStatus", sc_get_status),
            method("fade", sc_fade),
            method("fadeIn", sc_fade_in),
            method("fadeOut", sc_fade_out),
        ],

        properties: vec![],
    })?;

    Ok(())
}
