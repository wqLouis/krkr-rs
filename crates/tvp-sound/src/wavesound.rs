//! The real `WaveSoundBuffer` native class — the class the game's
//! `system/sound.tjs` derives from (`class SoundBuffer extends
//! WaveSoundBuffer`), ported from the reference
//! `reference/cpp/core/sound/WaveIntf.cpp` + `SoundBufferBaseIntf.cpp`.
//!
//! # Class surface
//!
//! **Methods** (the game calls these via `super.xxx` from its script
//! subclass):
//!
//! - `WaveSoundBuffer(owner)` — constructor. Retains `objthis` (the event
//!   target: the script object, possibly the derived `SoundBuffer`
//!   instance, since the derived ctor calls `WaveSoundBuffer(owner)` with
//!   `this` bound) and `owner` (the action owner that receives
//!   `action(ev)`), mirroring the reference's `Owner`/`ActionOwner`.
//! - `open(name)` — decode `name` from storage; the buffer becomes
//!   "stopped" (TVP: status is "unload" until opened).
//! - `play(pos = 0)` — start playback from `pos` seconds (or the current
//!   position when `pos` is 0 and the channel is already playing, matching
//!   the reference's play-from-current-position).
//! - `stop()` / `pause()` / `resume()`.
//! - `fade(to, timeMs[, delayMs])` — reference `Fade(to, time, blanktime)`:
//!   linear ramp to `to` (0..=100000 scale) over `timeMs`, after `delayMs`.
//! - `fadeIn(timeMs)` / `fadeOut(timeMs[, target])`.
//! - `stopFade()` — cancel the active fade, keeping the fade's target.
//! - `getStatus()` — `"unload" | "play" | "pause" | "stop"`.
//! - `onStatusChanged(st)` / `onFadeCompleted()` — native handlers that
//!   forward a KiriKiri event dictionary to the action owner's `action(ev)`
//!   (reference `WaveIntf.cpp` / `TVP_ACTION_INVOKE`). The game's
//!   `SoundBuffer` override calls `super.onStatusChanged(...)`, so this is
//!   where BGM-playlist chaining reaches the action owner.
//!
//! **Properties** (the game's `SoundLayer` reads/writes these directly on
//! its `SoundBuffer` instances):
//!
//! - `volume` (rw, 0..=100000 — divided by 100000 for the mixer).
//! - `pan` (rw, 0..=100000).
//! - `position` (rw, **seconds**).
//! - `status` (ro) — `"unload"|"play"|"pause"|"stop"`.
//! - `looping` (rw, bool), `paused` (rw, bool).
//! - `speed` (rw) — **stored only**: PhaseVocoder playback is out of scope
//!   (documented limitation; the value round-trips but playback rate is
//!   unaffected).
//! - `filters` (ro) — a fresh empty array per access. The game's BGM path
//!   (`createFilter:1`) calls `.filters.clear()` / `.filters.add(...)` and
//!   reads `filters[0]`; an empty array keeps that path from crashing while
//!   filter processing stays unimplemented (PhaseVocoder out of scope).
//!
//! # Status events
//!
//! [`sound_poll`] runs once per frame from the app's update loop: it
//! advances the global mixer, derives each stream's status from its channel
//! state (`unload` until a source is loaded, `stop` when not playing,
//! `pause` when paused, `play` otherwise) and, on a *transition*, fires the
//! retained object's `onStatusChanged(status)` member through its class
//! chain — so the game's `SoundBuffer.onStatusChanged` override runs, which
//! is what advances the BGM playlist on `"stop"`. Fade completions fire
//! `onFadeCompleted` the same way. Events are delivered synchronously from
//! the poll with all locks released (never while holding the mixer/stream
//! locks), so the script handlers can call back into the natives freely.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use tjs2_sys::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    Tjs2Engine, TjsValue,
};

use crate::ffi;
use crate::mixer::{Channel, lock_ok};
use crate::natives::native_ctx;

/// The script volume/pan scale (the reference's `tjs_int` 0..=100000); the
/// mixer works in 0..=1, so every boundary divides/multiplies by this.
const TVP_VOLUME_SCALE: f64 = 100_000.0;

/// The engine used by the constructor (retention) and the poll (events).
/// Set by [`register_wavesound`]; the app keeps the VM alive for the whole
/// process, so the pointer stays valid for every native object's lifetime.
static ENGINE: OnceLock<usize> = OnceLock::new();

fn context_engine() -> Option<&'static Tjs2Engine> {
    ENGINE
        .get()
        .map(|ptr| unsafe { &*(*ptr as *const Tjs2Engine) })
}

/// Script-visible playback status (the reference's `tTVPSoundStatus`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    /// No data loaded yet.
    Unload,
    /// Loaded but not playing.
    Stop,
    /// Playing (or paused).
    Play,
    /// Paused.
    Pause,
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Status::Unload => "unload",
            Status::Stop => "stop",
            Status::Play => "play",
            Status::Pause => "pause",
        }
    }
}

/// One live `WaveSoundBuffer` object: the two retained script objects (so
/// events dispatch with `this` bound to the right instance) and the mixer
/// channel it plays on.
///
/// The pair mirrors the reference `SoundBufferBaseIntf.cpp`: `self_obj` is
/// `Owner` — the `WaveSoundBuffer`/script-subclass instance that
/// `SetStatus` posts `onStatusChanged`/`onFadeCompleted` to — and
/// `action_owner` is `ActionOwner` — constructor argument 0, which the
/// native handlers forward the KiriKiri event dictionary to via
/// `action(ev)` (the game's `AttentionVoice` implements `action`).
struct Stream {
    /// Retained `objthis` (the `WaveSoundBuffer` or a script subclass like
    /// the game's `SoundBuffer`). Events are delivered here through its
    /// class chain, so a subclass `onStatusChanged` override runs first and
    /// may call `super.onStatusChanged(...)` to reach the native handler.
    self_obj: DetachedValue,
    /// Retained constructor argument 0; receives `action(ev)`.
    action_owner: DetachedValue,
    /// Mixer channel id (0 until `open`/`play` spawns one).
    channel_id: u64,
    /// Last status the poll observed (the script-visible `status`).
    status: Status,
    /// True once `play()` was called on a buffer whose open failed (no
    /// channel/source). The reference still reports `"stop"` for such a
    /// buffer and delivers `onStatusChanged("stop")` exactly once so
    /// script sequences that wait on a voice/effect finishing (e.g. the
    /// game's `AttentionVoice` wait entries like `"1000"`) advance.
    emitted_failed_stop: bool,
}

static STREAMS: LazyLock<Mutex<HashMap<u64, Stream>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);

/// Payload of one `WaveSoundBuffer` TJS object: a handle into [`STREAMS`]
/// plus script-visible scalars with no mixer counterpart.
struct WaveSoundBufferInst {
    /// Stream id (0 = the constructor has not run / not registered).
    stream_id: u64,
    /// `speed` property value (stored only; see the module docs).
    speed: f64,
}

extern "C" fn ws_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(WaveSoundBufferInst {
        stream_id: 0,
        speed: 1.0,
    })) as *mut c_void
}

extern "C" fn ws_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from ws_create (Box::into_raw), exactly once.
    let inst = unsafe { Box::from_raw(instance as *mut WaveSoundBufferInst) };
    if inst.stream_id != 0 {
        let mut streams = lock_ok(&STREAMS);
        if let Some(st) = streams.remove(&inst.stream_id) {
            // Drop the mixer channel and release the retained owner.
            if let Some(ctx) = native_ctx() {
                lock_ok(&ctx.mixer).remove_channel(st.channel_id);
            }
            drop(st);
        }
    }
}

/// `WaveSoundBuffer(owner)` — the reference constructor
/// (`SoundBufferBaseIntf.cpp` `Construct`): retain `objthis` as `Owner`
/// (the event target) and the **first constructor argument** as
/// `ActionOwner`. The reference keeps `Owner` as a raw pointer and
/// `ActionOwner` as a strong closure; we retain both detached so
/// [`sound_poll`] can deliver `onStatusChanged`/`onFadeCompleted` to the
/// instance's class chain, whose native handler forwards the event
/// dictionary to `ActionOwner.action(ev)`.
///
/// The game calls `new WaveSoundBuffer(this)` (AttentionVoice, MovieScene)
/// and `class SoundBuffer extends WaveSoundBuffer` + `new SoundBuffer(owner)`
/// (BGM/SE). For the former `objthis` is a plain `WaveSoundBuffer` whose
/// native handler runs directly; for the latter `objthis` is the
/// `SoundBuffer` and its `onStatusChanged` override runs before calling
/// `super.onStatusChanged(...)`.
extern "C" fn ws_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    if inst.stream_id != 0 {
        return ffi::report_error(out_error, "WaveSoundBuffer: already constructed");
    }
    // The action owner is the first constructor argument (a script object
    // like `this`). Retain it detached so status events can reach it from
    // `sound_poll` even after the call returns.
    let args = ffi::args(argv, argc);
    if args.is_empty() {
        return ffi::report_error(out_error, "WaveSoundBuffer: constructor requires an owner");
    }
    // Object arguments arrive as VAL_OBJECT resolved against the most
    // recent object-valued script result — the constructor argument.
    let Some(engine) = context_engine() else {
        return ffi::report_error(
            out_error,
            "WaveSoundBuffer: engine context not set (register_wavesound not called)",
        );
    };
    let action_owner = match engine.retain_value_detached(&tjs2_sys::TjsValue::Object) {
        Ok(dv) => dv,
        Err(e) => return ffi::report_error(out_error, &format!("WaveSoundBuffer: {e}")),
    };
    // `objthis` is the object status events are posted to (reference
    // `Owner`); sound_poll dispatches its onStatusChanged/onFadeCompleted.
    let self_obj = match engine.retain_object_detached(objthis) {
        Ok(dv) => dv,
        Err(e) => return ffi::report_error(out_error, &format!("WaveSoundBuffer: {e}")),
    };
    let id = NEXT_STREAM.fetch_add(1, Ordering::SeqCst);
    // sound_poll never holds STREAMS while delivering events (see below),
    // so a blocking lock here cannot deadlock against a re-entrant
    // `new WaveSoundBuffer` from an action handler.
    lock_ok(&STREAMS).insert(
        id,
        Stream {
            self_obj,
            action_owner,
            channel_id: 0,
            status: Status::Unload,
            emitted_failed_stop: false,
        },
    );
    inst.stream_id = id;
    ffi::set_void_out(out);
    0
}

/// Run `$body` with the stream's mixer channel (`$ch`), spawning the
/// channel on first use. `$inst` is read-only (the stream id); errors out
/// when the natives are not registered or the stream is gone.
macro_rules! with_channel {
    ($inst:expr, $out_error:expr, $ch:ident, $body:block) => {{
        let _inst = $inst;
        let Some(_ctx) = native_ctx() else {
            return ffi::report_error($out_error, "sound natives are not registered");
        };
        let mut _streams = lock_ok(&STREAMS);
        let Some(_st) = _streams.get_mut(&_inst.stream_id) else {
            return ffi::report_error($out_error, "WaveSoundBuffer: not constructed");
        };
        let mut _mixer = lock_ok(&_ctx.mixer);
        if _st.channel_id == 0 {
            _st.channel_id = _mixer.spawn_channel();
        }
        let Some($ch) = _mixer.channel(_st.channel_id) else {
            return ffi::report_error(
                $out_error,
                &format!("WaveSoundBuffer#{}: mixer channel missing", _st.channel_id),
            );
        };
        $body
    }};
}

/// `open(name)` — decode `name` from storage. Throws on failure. The
/// buffer becomes "stopped" (status "unload" until the first successful
/// open, matching the reference).
extern "C" fn ws_open(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let args = ffi::args(argv, argc);
    let Some(name) = args.first() else {
        return ffi::report_error(out_error, "WaveSoundBuffer.open requires a storage name");
    };
    let name = ffi::value_as_string(name);
    if name.is_empty() {
        return ffi::report_error(out_error, "WaveSoundBuffer.open requires a storage name");
    }
    let Some(ctx) = native_ctx() else {
        return ffi::report_error(out_error, "sound natives are not registered");
    };
    let audio = match crate::decode::decode_audio(&ctx.storage, &name) {
        Ok(a) => Arc::new(a),
        Err(e) => return ffi::report_error(out_error, &format!("WaveSoundBuffer.open: {e}")),
    };

    let mut streams = lock_ok(&STREAMS);
    let Some(st) = streams.get_mut(&inst.stream_id) else {
        return ffi::report_error(out_error, "WaveSoundBuffer: not constructed");
    };
    let mut mixer = lock_ok(&ctx.mixer);
    if st.channel_id == 0 {
        st.channel_id = mixer.spawn_channel();
    }
    let Some(ch) = mixer.channel(st.channel_id) else {
        return ffi::report_error(
            out_error,
            &format!("WaveSoundBuffer#{}: mixer channel missing", st.channel_id),
        );
    };
    // Load the source without starting playback (the poll derives "stop").
    ch.source = Some(audio);
    ch.playing = false;
    ch.paused = false;
    ch.done = false;
    ffi::set_void_out(out);
    0
}

/// `play(pos = 0)` — play from `pos` seconds. The status flip to "play"
/// happens in the poll (derived from the channel state), so the
/// `onStatusChanged("play")` event fires on the next frame like the
/// reference's posted events.
extern "C" fn ws_play(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let pos = ffi::args(argv, argc).first().map_or(0.0, ffi::value_as_f64);
    // A failed open leaves no channel; the reference still reports
    // "stop" (and the poll emits onStatusChanged("stop") once) so
    // script sequences waiting on completion advance.
    if inst.stream_id != 0 {
        let mut streams = lock_ok(&STREAMS);
        let ch_id = streams
            .get(&inst.stream_id)
            .map(|s| s.channel_id)
            .unwrap_or(0);
        if ch_id == 0 {
            if let Some(st) = streams.get_mut(&inst.stream_id) {
                st.emitted_failed_stop = true;
            }
            ffi::set_void_out(out);
            return 0;
        }
    }
    with_channel!(inst, out_error, ch, {
        let Some(audio) = ch.source.clone() else {
            // Nothing loaded (open failed): the reference's Play simply
            // does nothing when there is no decoder.
            ffi::set_void_out(out);
            return 0;
        };
        ch.play(audio);
        if pos > 0.0 {
            ch.set_position(pos);
        }
        ffi::set_void_out(out);
        0
    })
}

/// `stop()` — stop playback (the position is kept, matching the mixer's
/// `Channel::stop`; a later `play()` restarts from 0).
extern "C" fn ws_stop(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        ch.stop();
        ffi::set_void_out(out);
        0
    })
}

/// `pause()`.
extern "C" fn ws_pause(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        if ch.playing {
            ch.pause();
        }
        ffi::set_void_out(out);
        0
    })
}

/// `resume()`.
extern "C" fn ws_resume(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        if ch.paused {
            ch.resume();
        }
        ffi::set_void_out(out);
        0
    })
}

/// `fade(to, timeMs[, delayMs])` — reference `Fade(to, time, blanktime)`:
/// linear ramp to `to` (0..=100000) over `timeMs`, after `delayMs`.
extern "C" fn ws_fade(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let args = ffi::args(argv, argc);
    if args.len() < 2 {
        return ffi::report_error(out_error, "WaveSoundBuffer.fade requires target and timeMs");
    }
    let to = ffi::value_as_f64(&args[0]) / TVP_VOLUME_SCALE;
    let time_ms = ffi::value_as_f64(&args[1]);
    let delay_ms = if args.len() > 2 {
        ffi::value_as_f64(&args[2])
    } else {
        0.0
    };
    if time_ms < 0.0 || delay_ms < 0.0 {
        return ffi::report_error(out_error, "WaveSoundBuffer.fade: negative time");
    }
    with_channel!(inst, out_error, ch, {
        ch.fade(to as f32, time_ms / 1000.0, delay_ms / 1000.0);
        ffi::set_void_out(out);
        0
    })
}

/// `fadeIn(timeMs)` — ramp from the current volume to full (100000).
extern "C" fn ws_fade_in(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let args = ffi::args(argv, argc);
    let Some(v) = args.first() else {
        return ffi::report_error(out_error, "WaveSoundBuffer.fadeIn requires timeMs");
    };
    let time_ms = ffi::value_as_f64(v);
    if time_ms < 0.0 {
        return ffi::report_error(out_error, "WaveSoundBuffer.fadeIn: negative time");
    }
    with_channel!(inst, out_error, ch, {
        ch.fade(1.0, time_ms / 1000.0, 0.0);
        ffi::set_void_out(out);
        0
    })
}

/// `fadeOut(timeMs[, target=0])` — ramp to silence (or `target`).
extern "C" fn ws_fade_out(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let args = ffi::args(argv, argc);
    let Some(v) = args.first() else {
        return ffi::report_error(out_error, "WaveSoundBuffer.fadeOut requires timeMs");
    };
    let time_ms = ffi::value_as_f64(v);
    let target = if args.len() > 1 {
        ffi::value_as_f64(&args[1]) / TVP_VOLUME_SCALE
    } else {
        0.0
    };
    if time_ms < 0.0 {
        return ffi::report_error(out_error, "WaveSoundBuffer.fadeOut: negative time");
    }
    with_channel!(inst, out_error, ch, {
        ch.fade(target as f32, time_ms / 1000.0, 0.0);
        ffi::set_void_out(out);
        0
    })
}

/// `stopFade()` — cancel the active fade, keeping its target volume.
extern "C" fn ws_stop_fade(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        let target = ch.fade.as_ref().map(|f| f.to);
        if let Some(to) = target {
            ch.set_volume(to);
        }
        ffi::set_void_out(out);
        0
    })
}

/// `getStatus()` — `"unload"|"play"|"pause"|"stop"`.
extern "C" fn ws_get_status(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let status = lock_ok(&STREAMS)
        .get(&inst.stream_id)
        .map_or(Status::Unload, |s| s.status);
    ffi::set_string_out(out, status.as_str());
    0
}

/// The native `onStatusChanged(st)` handler (reference `WaveIntf.cpp`
/// `onStatusChanged`): the reference's base handler forwards a KiriKiri
/// event dictionary `%[type:"onStatusChanged", target:this, status:st]` to
/// the retained action owner's `action(ev)`. A plain
/// `new WaveSoundBuffer(owner)` reaches this directly; the game's
/// `SoundBuffer.onStatusChanged(st)` override calls
/// `super.onStatusChanged(...)`, so this is where BGM-playlist chaining
/// reaches the action owner.
extern "C" fn ws_on_status_changed(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    ffi::set_void_out(out);
    let Some(engine) = context_engine() else {
        return 0;
    };
    // `super.onStatusChanged(st)` forwards the status; a bare call falls
    // back to the stream's current status.
    let args = ffi::args(argv, argc);
    let status = args
        .first()
        .map(ffi::value_as_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            lock_ok(&STREAMS)
                .get(&inst.stream_id)
                .map_or_else(|| "unload".to_string(), |s| s.status.as_str().to_string())
        });
    dispatch_action(engine, inst.stream_id, "onStatusChanged", Some(&status));
    0
}

/// The native `onFadeCompleted()` handler (reference `WaveIntf.cpp`):
/// forwards `%[type:"onFadeCompleted", target:this]` to the action owner.
/// The game's `SoundBuffer.onFadeCompleted` override does not call
/// `super`, so this only runs for a plain buffer or a subclass that
/// forwards.
extern "C" fn ws_on_fade_completed(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    ffi::set_void_out(out);
    if let Some(engine) = context_engine() {
        dispatch_action(engine, inst.stream_id, "onFadeCompleted", None);
    }
    0
}

/// Build the KiriKiri event dictionary and call the stream's action
/// owner's `action(ev)` (reference `TVP_ACTION_INVOKE_BEGIN` / `_MEMBER` /
/// `_END`). `status` is the extra `%[status:...]` member the
/// `onStatusChanged` event carries.
///
/// The reference macro ignores the `FuncCall` return code, so a missing
/// `action` member is silently tolerated (the action owner only implements
/// `action` when it wants the events; e.g. the game's `SoundBuffer` passes
/// a manager that may not). Only a genuine script error is logged.
fn dispatch_action(engine: &Tjs2Engine, stream_id: u64, event_type: &str, status: Option<&str>) {
    let Some(owner) = lock_ok(&STREAMS)
        .get(&stream_id)
        .map(|s| s.action_owner.raw_id())
    else {
        return;
    };
    // Build `%[type:ty, status:status]` imperatively (this TJS2 build
    // rejects quoted keys in `%[...]` literals).
    let mut expr = format!(
        "(function(){{ var d = %[]; d.type = '{}';",
        escape_js(event_type)
    );
    if let Some(status) = status {
        expr.push_str(&format!(" d.status = '{}';", escape_js(status)));
    }
    expr.push_str(" return d; })()");
    let retained = engine
        .eval(&expr, "krkr_rs_sound_event")
        .ok()
        .and_then(|_| engine.retain_value_detached(&TjsValue::Object).ok());
    let Some(dv) = retained else {
        return;
    };
    if let Err(e) = engine.call_member(owner, "action", &[TjsValue::Retained(dv.raw_id() as u64)]) {
        // `Member "" does not exist` is the reference-tolerated missing
        // `action`; keep it quiet. Anything else is a real handler error.
        if !e.contains("does not exist") {
            log::warn!("WaveSoundBuffer action({event_type}) dispatch failed: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// properties
// ---------------------------------------------------------------------------

fn property(
    name: &'static str,
    get: tjs2_sys::NativeInstancePropertyGetFn,
    set: Option<tjs2_sys::NativeInstancePropertySetFn>,
) -> NativeInstancePropertyDef {
    NativeInstancePropertyDef {
        name,
        get: Some(get),
        set,
    }
}

/// `volume` getter — 0..=100000 (the mixer's 0..=1 times 100000; the
/// reference's `volume` reads back the ramped value during a fade, so we
/// use the channel's effective volume).
extern "C" fn ws_volume_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        ffi::set_int_out(
            out,
            (f64::from(ch.effective_volume()) * TVP_VOLUME_SCALE) as i64,
        );
        0
    })
}

/// `volume` setter — 100000-scale, divided by 100000 for the mixer.
extern "C" fn ws_volume_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload; value is valid for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    // SAFETY: value points at the property value for the duration of the call.
    let v = unsafe { &*value };
    with_channel!(inst, out_error, ch, {
        ch.set_volume((ffi::value_as_f64(v) / TVP_VOLUME_SCALE) as f32);
        0
    })
}

/// `pan` getter — 0..=100000.
extern "C" fn ws_pan_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        ffi::set_int_out(out, (f64::from(ch.pan) * TVP_VOLUME_SCALE) as i64);
        0
    })
}

/// `pan` setter — 100000-scale.
extern "C" fn ws_pan_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload; value is valid for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    // SAFETY: value points at the property value for the duration of the call.
    let v = unsafe { &*value };
    with_channel!(inst, out_error, ch, {
        ch.set_pan((ffi::value_as_f64(v) / TVP_VOLUME_SCALE) as f32);
        0
    })
}

/// `position` getter — seconds.
extern "C" fn ws_position_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        ffi::set_real_out(out, ch.position_seconds);
        0
    })
}

/// `position` setter — seconds (seek).
extern "C" fn ws_position_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload; value is valid for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    // SAFETY: value points at the property value for the duration of the call.
    let v = unsafe { &*value };
    with_channel!(inst, out_error, ch, {
        ch.set_position(ffi::value_as_f64(v));
        0
    })
}

/// `status` getter.
extern "C" fn ws_status_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let status = lock_ok(&STREAMS)
        .get(&inst.stream_id)
        .map_or(Status::Unload, |s| s.status);
    ffi::set_string_out(out, status.as_str());
    0
}

/// `looping` getter.
extern "C" fn ws_looping_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        ffi::set_int_out(out, i64::from(ch.looping));
        0
    })
}

/// `looping` setter.
extern "C" fn ws_looping_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload; value is valid for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    // SAFETY: value points at the property value for the duration of the call.
    let v = unsafe { &*value };
    with_channel!(inst, out_error, ch, {
        ch.looping = ffi::value_as_bool(v);
        0
    })
}

/// `paused` getter.
extern "C" fn ws_paused_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        ffi::set_int_out(out, i64::from(ch.paused));
        0
    })
}

/// `paused` setter (pause/resume).
extern "C" fn ws_paused_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload; value is valid for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    // SAFETY: value points at the property value for the duration of the call.
    let v = unsafe { &*value };
    with_channel!(inst, out_error, ch, {
        if ffi::value_as_bool(v) {
            if ch.playing {
                ch.pause();
            }
        } else if ch.paused {
            ch.resume();
        }
        0
    })
}

/// `speed` getter — stored only (see the module docs).
extern "C" fn ws_speed_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &*ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    ffi::set_real_out(out, inst.speed);
    0
}

/// `speed` setter — stored only (PhaseVocoder playback is out of scope).
extern "C" fn ws_speed_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload; value is valid for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    // SAFETY: value points at the property value for the duration of the call.
    let v = unsafe { &*value };
    inst.speed = ffi::value_as_f64(v);
    0
}

/// `filters` getter — a fresh empty array per access. The game's BGM path
/// (`createFilter:1`) calls `.filters.clear()` / `.filters.add(...)` and
/// reads `filters[0]`; an empty array keeps that path from crashing while
/// filter processing stays unimplemented (PhaseVocoder out of scope).
extern "C" fn ws_filters_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let _inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    ffi::set_empty_array_out(out);
    0
}

// ---------------------------------------------------------------------------
// registration + the per-frame poll
// ---------------------------------------------------------------------------

fn method(name: &'static str, f: tjs2_sys::NativeInstanceMethodFn) -> NativeInstanceMethodDef {
    NativeInstanceMethodDef { name, f }
}

/// Register the `WaveSoundBuffer` native class on `engine`. Must be called
/// on the VM thread; the engine pointer is kept for the poll's event
/// delivery.
pub(crate) fn register_wavesound(engine: &Tjs2Engine) -> Result<(), String> {
    let _ = ENGINE.set(engine as *const Tjs2Engine as usize);
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "WaveSoundBuffer",
        create: ws_create,
        destroy: ws_destroy,
        methods: vec![
            method("WaveSoundBuffer", ws_ctor),
            method("open", ws_open),
            method("play", ws_play),
            method("stop", ws_stop),
            method("pause", ws_pause),
            method("resume", ws_resume),
            method("fade", ws_fade),
            method("fadeIn", ws_fade_in),
            method("fadeOut", ws_fade_out),
            method("stopFade", ws_stop_fade),
            method("getStatus", ws_get_status),
            method("onStatusChanged", ws_on_status_changed),
            method("onFadeCompleted", ws_on_fade_completed),
        ],
        properties: vec![
            property("volume", ws_volume_get, Some(ws_volume_set)),
            property("pan", ws_pan_get, Some(ws_pan_set)),
            property("position", ws_position_get, Some(ws_position_set)),
            property("status", ws_status_get, None),
            property("looping", ws_looping_get, Some(ws_looping_set)),
            property("paused", ws_paused_get, Some(ws_paused_set)),
            property("speed", ws_speed_get, Some(ws_speed_set)),
            property("filters", ws_filters_get, None),
        ],
    })?;
    // The reference's `WaveSoundBuffer` exposes nested filter classes; the
    // game's `SoundLayer` does `new WaveSoundBuffer.PhaseVocoder()` when a
    // BGM is played with `createFilter:1` (the title/the OP). Filter
    // processing is out of scope, but the class must exist and construct
    // so the playlist setup does not throw.
    engine
        .exec_script(
            r#"
            if(typeof WaveSoundBuffer.PhaseVocoder == "undefined"){
                class _PhaseVocoder {
                    function _PhaseVocoder(){}
                }
                WaveSoundBuffer.PhaseVocoder = _PhaseVocoder;
            }
            "#,
            "WaveSoundBuffer_PhaseVocoder",
        )
        .map_err(|e| format!("register WaveSoundBuffer.PhaseVocoder: {e}"))?;
    Ok(())
}

/// Derive a stream's script-visible status from its mixer channel state.
fn derive_status(ch: &Channel) -> Status {
    if ch.source.is_none() {
        Status::Unload
    } else if !ch.playing {
        Status::Stop
    } else if ch.paused {
        Status::Pause
    } else {
        Status::Play
    }
}

/// One event queued by [`sound_poll`] for delivery after the locks drop.
enum PollEvent {
    StatusChanged(&'static str),
    FadeCompleted,
}

/// Drive the sound pipeline once per frame: advance the global mixer to
/// `now_seconds` (monotonic seconds), then deliver `onStatusChanged` /
/// `onFadeCompleted` events to every live `WaveSoundBuffer` whose state
/// changed.
///
/// Events are fired with `this` bound to the retained script object, so a
/// script subclass override (the game's `SoundBuffer.onStatusChanged`,
/// which advances the BGM playlist on `"stop"`) runs. Firing happens with
/// all locks released, so the handlers can call back into the natives.
pub fn sound_poll(engine: &Tjs2Engine, now_seconds: f64) {
    // 1. Advance the mixer (no-op before register_sound set one).
    crate::advance(now_seconds);

    // 2. Walk the live streams under the locks and queue events. Events
    // target the retained instance (`self_obj`), so the script class chain
    // (a `SoundBuffer.onStatusChanged` override) resolves the handler.
    let mut events: Vec<(tjs2_sys::Tjs2ValueId, PollEvent)> = Vec::new();
    {
        let Some(ctx) = native_ctx() else {
            return;
        };
        let mut streams = lock_ok(&STREAMS);
        let mut mixer = lock_ok(&ctx.mixer);
        for st in streams.values_mut() {
            // A buffer whose open failed (no channel) still reports one
            // "stop" so script wait-sequences advance (AttentionVoice's
            // timed entries call play() on a failed open).
            if st.channel_id == 0 {
                if st.emitted_failed_stop {
                    st.emitted_failed_stop = false;
                    st.status = Status::Stop;
                    events.push((st.self_obj.raw_id(), PollEvent::StatusChanged("stop")));
                }
                continue;
            }
            let Some(ch) = mixer.channel(st.channel_id) else {
                continue;
            };
            if ch.fade_finished {
                ch.fade_finished = false;
                events.push((st.self_obj.raw_id(), PollEvent::FadeCompleted));
            }
            let derived = derive_status(ch);
            if derived != st.status {
                let arg = derived.as_str();
                st.status = derived;
                events.push((st.self_obj.raw_id(), PollEvent::StatusChanged(arg)));
            }
        }
    }

    // 3. Deliver the events (locks released; reentrancy is safe).
    //
    // The reference's `SetStatus` posts `onStatusChanged` to the
    // `WaveSoundBuffer` *instance* (`Owner`), not to the action owner.
    // `onStatusChanged`/`onFadeCompleted` then run through the instance's
    // class chain: a script subclass override (the game's
    // `SoundBuffer.onStatusChanged`, which advances the BGM playlist on
    // "stop") runs first, and its `super.onStatusChanged(...)` reaches the
    // native handler, which forwards the event dictionary to the action
    // owner's `action(ev)` (the game's `AttentionVoice`). A plain
    // `WaveSoundBuffer(this)` has no override, so the native handler runs
    // directly. This exactly mirrors the reference event flow, instead of
    // calling `action` and `onStatusChanged` on the action owner.
    for (target, event) in events {
        let result = match event {
            PollEvent::StatusChanged(s) => {
                engine.call_member(target, "onStatusChanged", &[TjsValue::String(s.into())])
            }
            PollEvent::FadeCompleted => engine.call_member(target, "onFadeCompleted", &[]),
        };
        if let Err(e) = result {
            log::warn!("WaveSoundBuffer poll event failed: {e}");
        }
    }
}

/// Escape a string for inclusion in a single-quoted TJS string literal.
fn escape_js(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}
