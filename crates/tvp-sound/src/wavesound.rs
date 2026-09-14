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
//! - `open(name)` — read and probe `name` from storage, then decode it on
//!   a background worker (whole-file for short sounds, bounded streaming
//!   for long tracks). Also loads the `<name>.sli` side-car loop points
//!   (`crate::sli`). The buffer stays "unload" until the data is ready.
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
//! its `SoundBuffer` instances; all ported from
//! `reference/cpp/core/sound/WaveIntf.cpp` + `win32/WaveImpl.cpp`):
//!
//! - `volume` (rw, 0..=100000 — divided by 100000 for the mixer).
//! - `volume2` (rw, 0..=100000) — secondary volume, multiplied with
//!   `volume` (reference `Volume2`; the game's config/per-voice volume).
//! - `pan` (rw, 0..=100000).
//! - `position` (rw, **milliseconds**), `samplePosition` (rw, samples).
//! - `totalTime` (ro, milliseconds), `frequency` (rw, Hz — a playback-rate
//!   control), `bits` (ro), `channels` (ro).
//! - `status` (ro) — `"unload"|"play"|"stop"` (the reference's
//!   `GetStatusString` has no `pause`; `paused` does not change status).
//! - `looping` (rw, bool), `paused` (rw, bool — settable before `play`).
//! - `speed` (rw) — **stored only**: the value round-trips, but the mixer
//!   does not apply the phase-vocoder time stretch. Use `frequency` for a
//!   real rate change.
//! - `filters` (ro) — the buffer's stable, real TJS `Array` (reference
//!   `GetFiltersNoAddRef`). The game's BGM path does `.filters.clear()` /
//!   `.filters.add(new WaveSoundBuffer.PhaseVocoder())` and later reads
//!   `filters[0]`; the array keeps identity across reads.
//! - `WaveSoundBuffer.PhaseVocoder` — a real native filter class with the
//!   reference's `window`/`overlap`/`pitch`/`time`/`interface` members. The
//!   phase-vocoder DSP itself is not applied by the mixer (documented gap),
//!   but the members store and return their values like the reference.
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

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use tjs2_sys::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, TjsValue,
};

use crate::ffi;
use crate::mixer::{Channel, lock_ok};
use crate::natives::native_ctx;
use crate::sli::{TVP_WL_MAX_FLAG_VALUE, TVP_WL_MAX_FLAGS, WaveLabel};

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
///
/// The reference's `GetStatusString` only ever maps `ssUnload`/`ssStop`/
/// `ssPlay`; `paused` does **not** change the status (the pause property is
/// independent), so there is no `pause` string here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    /// No data loaded yet.
    Unload,
    /// Loaded but not playing.
    Stop,
    /// Playing (or paused; the reference keeps reporting `play`).
    Play,
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Status::Unload => "unload",
            Status::Stop => "stop",
            Status::Play => "play",
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
    /// Set when a dispatch to this stream returned `TJSInvalidObject` (the
    /// script dropped the buffer with `invalidate`, so `self_obj`/
    /// `action_owner` are dead). The stream is skipped from then on and
    /// reaped (retained objects released, mixer channel removed) at a safe
    /// point after the VM calls return. Mirrors the input bridge's
    /// `dead_layers` set.
    dead: bool,
    /// Conditional loop flags (`WaveFlags`), consulted for `.sli` link
    /// selection at the loop point.
    flags: [i32; TVP_WL_MAX_FLAGS],
    /// `useVisBuffer` state (the visualisation capture is host/wave-out
    /// only; see the module docs).
    use_vis_buffer: bool,
    /// `.sli` labels for the loaded track, sorted by sample position (the
    /// reference keeps a `LabelEventQueue` sorted by offset). Empty until
    /// `open` installs a track.
    labels: Vec<WaveLabel>,
    /// The track's sample rate, used to convert label sample positions to
    /// the millisecond `position` the reference's `labels` dictionary
    /// carries.
    label_rate: u32,
    /// Next `.sli` label index to fire (labels are fired in position order).
    next_label: usize,
    /// Playback position (source seconds) at the last poll, to detect label
    /// crossings and backwards seeks/loop wraps.
    last_sample: f64,
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
    /// `frequency` property override (reference `SetFrequency`), applied as
    /// a playback-rate multiplier. `None` = the source's native rate.
    frequency: Option<u32>,
    /// Stable `filters` array (reference `Filters`, a real TJS Array). The
    /// array keeps object identity across property reads because it lives in
    /// a uniquely named global; this is the keepalive. The game does
    /// `.filters.clear()` / `.filters.add(...)` / `filters[0]`.
    filters_array: Option<DetachedValue>,
    /// Global name holding the `filters` array (the ABI cannot re-retain an
    /// existing id, so the getter re-evaluates the global).
    filters_global: Option<String>,
}

extern "C" fn ws_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(WaveSoundBufferInst {
        stream_id: 0,
        speed: 1.0,
        frequency: None,
        filters_array: None,
        filters_global: None,
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
    // Prefer the constructor argument's own object handle (correct even
    // when more than one object is in play); fall back to the engine's
    // most-recent-object resolution for a plain `null` owner.
    let action_owner = if let Some(arg) = args
        .first()
        .filter(|a| a.ty == tjs2_sys::VAL_OBJECT && !a.object_handle().is_null())
    {
        engine.retain_object_arg(arg)
    } else {
        engine.retain_value_detached(&tjs2_sys::TjsValue::Object)
    };
    let action_owner = match action_owner {
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
            dead: false,
            flags: [0; TVP_WL_MAX_FLAGS],
            use_vis_buffer: false,
            labels: Vec::new(),
            label_rate: 0,
            next_label: 0,
            last_sample: 0.0,
        },
    );
    inst.stream_id = id;
    // Build the stable `filters` array the game mutates. A named global keeps
    // the array alive and gives the getter a way to return the same object
    // (the ABI cannot re-retain an existing retained id).
    let filters_name = format!("__krkr_wsb_filters_{id}");
    if engine
        .exec_script(
            &format!("global.{filters_name} = [];"),
            "WaveSoundBuffer.filters",
        )
        .is_ok()
        && let Ok(RetainedValue::Object(array)) =
            engine.eval_retained(&format!("global.{filters_name}"), "WaveSoundBuffer.filters")
    {
        inst.filters_array = Some(array);
        inst.filters_global = Some(filters_name);
    }
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
    let track = match crate::source::open_track(&ctx.storage, &name) {
        Ok(t) => t,
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
    // Snapshot the `.sli` labels (sorted like the reference's
    // `LabelEventQueue`) and the track's rate for the `labels` dictionary
    // and `onLabel` polling, before the track moves into the channel.
    let mut labels: Vec<WaveLabel> = track.labels().to_vec();
    labels.sort_by_key(|l| l.position);
    st.labels = labels;
    st.label_rate = track.sample_rate();
    st.next_label = 0;
    st.last_sample = 0.0;

    // Install the (possibly still-loading/streaming) track without starting
    // playback; the poll derives "stop" once it is ready.
    ch.source = Some(track);
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
    // The argument is an optional start position in **milliseconds**
    // (reference `position`/`SetPosition` units). The reference `play()`
    // itself takes no argument; the game calls `.play(pos)` and then sets
    // `.position = pos`, so honouring it here is harmless and saves a beat.
    let pos_ms = ffi::args(argv, argc).first().map_or(0.0, ffi::value_as_f64);
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
        let Some(track) = ch.source.clone() else {
            // Nothing loaded (open failed): the reference's Play simply
            // does nothing when there is no decoder.
            ffi::set_void_out(out);
            return 0;
        };
        // Reference `Play()` returns immediately when `BufferPlaying` is
        // already set (position keeps going) instead of restarting at 0.
        if ch.playing {
            if pos_ms > 0.0 {
                ch.set_position(pos_ms / 1000.0);
            }
            ffi::set_void_out(out);
            return 0;
        }
        // `Play()` keeps the pause flag (reference `StartPlay` checks
        // `Paused`); the eyecatch jingle relies on `paused = true` before
        // `play()` and unpausing later.
        let paused = ch.paused;
        ch.play_track(track);
        ch.paused = paused;
        if pos_ms > 0.0 {
            ch.set_position(pos_ms / 1000.0);
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
        // The reference `Stop()` rewinds the decoder to 0 as well.
        ch.set_position(0.0);
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
        ch.paused = true;
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
        ch.paused = false;
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
    if time_ms <= 0.0 || delay_ms < 0.0 {
        return ffi::report_error(out_error, "WaveSoundBuffer.fade: invalid time");
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
    dispatch_action(
        engine,
        inst.stream_id,
        "onStatusChanged",
        &[("status", &status)],
    );
    0
}

/// The native `onLabel(name)` handler (reference `WaveIntf.cpp`
/// `onLabel`): forwards `%[type:"onLabel", target:this, name:name]` to the
/// action owner. The reference's `InvokeLabelEvent` posts this to the
/// buffer instance when playback crosses a `.sli` `Label`; the poll below
/// delivers the same event through the instance's class chain.
extern "C" fn ws_on_label(
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
    let name = ffi::args(argv, argc)
        .first()
        .map(ffi::value_as_string)
        .unwrap_or_default();
    if let Some(engine) = context_engine() {
        dispatch_action(engine, inst.stream_id, "onLabel", &[("name", &name)]);
    }
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
        dispatch_action(engine, inst.stream_id, "onFadeCompleted", &[]);
    }
    0
}

/// Build the KiriKiri event dictionary and call the stream's action
/// owner's `action(ev)` (reference `TVP_ACTION_INVOKE_BEGIN` / `_MEMBER` /
/// `_END`). `members` are the extra dictionary members the event carries
/// (`status` for `onStatusChanged`, `name` for `onLabel`).
///
/// The reference macro ignores the `FuncCall` return code, so a missing
/// `action` member is silently tolerated (the action owner only implements
/// `action` when it wants the events; e.g. the game's `SoundBuffer` passes
/// a manager that may not). Only a genuine script error is logged.
fn dispatch_action(
    engine: &Tjs2Engine,
    stream_id: u64,
    event_type: &str,
    members: &[(&str, &str)],
) {
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
    for (key, value) in members {
        expr.push_str(&format!(" d.{key} = '{}';", escape_js(value)));
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
        if is_invalidated_error(&e) {
            // The action owner was invalidated too; stop dispatching to this
            // stream and let the poll reap it.
            mark_stream_dead(stream_id);
        } else if !e.contains("does not exist") {
            // `Member "" does not exist` is the reference-tolerated missing
            // `action`; keep it quiet. Anything else is a real handler error.
            log::warn!("WaveSoundBuffer action({event_type}) dispatch failed: {e}");
        }
    }
}

/// Whether a native-callback error is TJS `TJSInvalidObject` (the script
/// object was dropped/`invalidate`d). The reference keeps dispatching
/// forever; we treat it as terminal for the stream.
fn is_invalidated_error(e: &str) -> bool {
    e.contains("already invalidated")
}

/// Flag one stream for reaping (see [`reap_dead_streams`]). Safe to call
/// while a VM call is in progress: it only flips a flag.
fn mark_stream_dead(stream_id: u64) {
    if let Some(st) = lock_ok(&STREAMS).get_mut(&stream_id) {
        st.dead = true;
    }
}

/// Remove every stream whose object was invalidated: drop the retained
/// `self_obj`/`action_owner` (releasing their engine references) and
/// remove its mixer channel, which in turn drops the `AudioTrack` and
/// cancels any decode worker.
///
/// The lock is released before the `DetachedValue`s drop (their release
/// can re-enter the engine), and the caller must not be inside a VM call on
/// the stream being reaped.
fn reap_dead_streams() {
    let dead: Vec<Stream> = {
        let mut streams = lock_ok(&STREAMS);
        let ids: Vec<u64> = streams
            .iter()
            .filter(|(_, s)| s.dead)
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| streams.remove(&id))
            .collect()
    };
    if dead.is_empty() {
        return;
    }
    if let Some(ctx) = native_ctx() {
        let mut mixer = lock_ok(&ctx.mixer);
        for st in &dead {
            mixer.remove_channel(st.channel_id);
        }
    }
    // `dead` drops here, releasing the retained script objects outside any
    // lock (and outside active VM calls).
    drop(dead);
}

/// Number of live `WaveSoundBuffer` streams (diagnostic / tests): entries
/// remaining in the poll's stream registry. Invalidated buffers are reaped
/// during [`sound_poll`], so this drops after their last dispatch.
pub fn active_stream_count() -> usize {
    lock_ok(&STREAMS).len()
}

/// Read one conditional loop flag (`WaveFlags` property).
pub(crate) fn stream_flag(stream_id: u64, index: usize) -> i32 {
    lock_ok(&STREAMS)
        .get(&stream_id)
        .and_then(|s| s.flags.get(index))
        .copied()
        .unwrap_or(0)
}

/// Write one conditional loop flag, clamped to the reference's
/// `TVP_WL_MAX_FLAG_VALUE`.
pub(crate) fn set_stream_flag(stream_id: u64, index: usize, value: i32) {
    if let Some(st) = lock_ok(&STREAMS).get_mut(&stream_id)
        && let Some(slot) = st.flags.get_mut(index)
    {
        *slot = value.clamp(0, TVP_WL_MAX_FLAG_VALUE);
    }
}

/// Clear every conditional loop flag (`WaveFlags.reset`).
pub(crate) fn reset_stream_flags(stream_id: u64) {
    if let Some(st) = lock_ok(&STREAMS).get_mut(&stream_id) {
        st.flags = [0; TVP_WL_MAX_FLAGS];
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

/// `volume2` getter — 0..=100000 (reference `GetVolume2`). Multiplied with
/// `volume` and the global volume at render time.
extern "C" fn ws_volume2_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        ffi::set_int_out(out, (f64::from(ch.volume2) * TVP_VOLUME_SCALE) as i64);
        0
    })
}

/// `volume2` setter — 100000-scale, divided for the mixer.
extern "C" fn ws_volume2_set(
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
        ch.set_volume2((ffi::value_as_f64(v) / TVP_VOLUME_SCALE) as f32);
        0
    })
}

/// `position` getter — **milliseconds** (reference `GetPosition`:
/// `GetSamplePosition() * 1000 / SamplesPerSec`).
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
        ffi::set_int_out(out, (ch.position_seconds * 1000.0) as i64);
        0
    })
}

/// `position` setter — **milliseconds**, converted to seconds for the
/// mixer (reference `SetPosition`). The reference ignores a seek at/after
/// the known end; mirror that so a bad script value cannot wedge the
/// channel past EOF.
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
    let ms = ffi::value_as_f64(v);
    with_channel!(inst, out_error, ch, {
        let total_ms = ch
            .source
            .as_ref()
            .and_then(|t| t.known_duration_seconds())
            .map(|d| d * 1000.0);
        if total_ms.is_none_or(|t| ms < t) {
            ch.set_position(ms / 1000.0);
        }
        0
    })
}

/// `samplePosition` getter — playback position in sample granules
/// (reference `GetSamplePosition`).
extern "C" fn ws_sample_position_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        let rate = ch.source.as_ref().map_or(0, |t| t.sample_rate());
        ffi::set_int_out(out, (ch.position_seconds * f64::from(rate)) as i64);
        0
    })
}

/// `samplePosition` setter — sample granules to seconds.
extern "C" fn ws_sample_position_set(
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
    let samples = ffi::value_as_f64(v);
    with_channel!(inst, out_error, ch, {
        let rate = ch.source.as_ref().map_or(0, |t| t.sample_rate());
        if rate > 0 {
            let total = ch.source.as_ref().and_then(|t| t.total_frames());
            if total.is_none_or(|t| samples < t as f64) {
                ch.set_position(samples / f64::from(rate));
            }
        }
        0
    })
}

/// `totalTime` getter — total duration in **milliseconds** (reference
/// `GetTotalTime`), or 0 when the container did not state it.
extern "C" fn ws_total_time_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    with_channel!(inst, out_error, ch, {
        let ms = ch
            .source
            .as_ref()
            .and_then(|t| t.known_duration_seconds())
            .map_or(0, |d| (d * 1000.0) as i64);
        ffi::set_int_out(out, ms);
        0
    })
}

/// `frequency` getter — the source's sample rate, or the override set on
/// this buffer (reference `GetFrequency`).
extern "C" fn ws_frequency_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let requested = inst.frequency;
    with_channel!(inst, out_error, ch, {
        let freq = requested.unwrap_or_else(|| ch.source.as_ref().map_or(0, |t| t.sample_rate()));
        ffi::set_int_out(out, i64::from(freq));
        0
    })
}

/// `frequency` setter — changes the playback rate (reference
/// `SetFrequency` sets the DirectSound buffer frequency; pitch and tempo
/// both change).
extern "C" fn ws_frequency_set(
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
    let freq = ffi::value_as_f64(v);
    inst.frequency = if freq > 0.0 { Some(freq as u32) } else { None };
    let requested = inst.frequency;
    with_channel!(inst, out_error, ch, {
        let native = ch.source.as_ref().map_or(0, |t| t.sample_rate());
        if let Some(f) = requested {
            if native > 0 {
                ch.set_rate(f64::from(f) / f64::from(native));
            }
        } else {
            ch.set_rate(1.0);
        }
        0
    })
}

/// `bits` getter — reference `GetBitsPerSample`. Every decoder in this port
/// produces f32 from a 16-bit-class source, so report 16 (the game uses it
/// only to estimate a byte rate).
extern "C" fn ws_bits_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    ffi::set_int_out(out, 16);
    0
}

/// `channels` getter — the source's channel count (reference
/// `GetChannels`).
extern "C" fn ws_channels_get(
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
            i64::from(ch.source.as_ref().map_or(0, |t| t.channels())),
        );
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
        // The reference `SetPaused` only sets the flag; `Play()`/`StartPlay`
        // respect it (a buffer can be paused before it ever plays).
        ch.paused = ffi::value_as_bool(v);
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

/// `filters` getter — the buffer's stable filter array (reference
/// `GetFiltersNoAddRef`). The game's BGM path (`SoundBuffer.speed` setter,
/// `system/sound.tjs`) does `.filters.clear()` / `.filters.add(new
/// WaveSoundBuffer.PhaseVocoder())` and later reads `filters[0]`, so the
/// array must keep identity across reads. The array is a real TJS `Array`;
/// the PhaseVocoder objects added to it are real native instances.
extern "C" fn ws_filters_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let Some(name) = inst.filters_global.as_ref() else {
        ffi::set_empty_array_out(out);
        return 0;
    };
    let Some(engine) = context_engine() else {
        return ffi::report_error(out_error, "WaveSoundBuffer.filters: engine context not set");
    };
    match engine.eval_retained(&format!("global.{name}"), "WaveSoundBuffer.filters") {
        Ok(RetainedValue::Object(array)) => {
            ffi::set_retained_out(out, array);
            0
        }
        Ok(RetainedValue::Value(_)) => {
            ffi::set_empty_array_out(out);
            0
        }
        Err(e) => ffi::report_error(out_error, &format!("WaveSoundBuffer.filters: {e}")),
    }
}

/// Evaluate `expr` and hand its object result back as the callback result
/// (the C ABI cannot construct dictionaries/objects directly, but it can
/// transfer a retained object; see `tvp-natives::set_object_result`).
fn set_eval_object_out(out: *mut tjs2_sys::Value, expr: &str, name: &str) -> Result<(), String> {
    let engine = context_engine().ok_or_else(|| "sound natives are not registered".to_string())?;
    engine.eval(expr, name).map_err(|e| e.to_string())?;
    let dv = engine.retain_value_detached(&TjsValue::Object)?;
    ffi::set_retained_out(out, dv);
    Ok(())
}

/// `setPos(x, y, z)` — reference `WaveIntf.cpp` `setPos` →
/// `WaveImpl.cpp::SetPos`: store the 3D position and apply DirectSound3D
/// distance attenuation at render time (see `Channel::spatial_attenuation`).
extern "C" fn ws_set_pos(
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
    if args.len() < 3 {
        return ffi::report_error(out_error, "WaveSoundBuffer.setPos requires x, y, z");
    }
    let (x, y, z) = (
        ffi::value_as_f64(&args[0]) as f32,
        ffi::value_as_f64(&args[1]) as f32,
        ffi::value_as_f64(&args[2]) as f32,
    );
    with_channel!(inst, out_error, ch, {
        ch.set_pos(x, y, z);
        ffi::set_void_out(out);
        0
    })
}

/// Generate the `posX`/`posY`/`posZ` getter/setter pair for one axis of
/// the channel's 3D position (reference `GetPosX`/`SetPosX`).
macro_rules! pos_properties {
    ($($axis:literal => $get:ident, $set:ident),* $(,)?) => {
        $(
            extern "C" fn $get(
                _engine: *mut c_void,
                instance: *mut c_void,
                out: *mut tjs2_sys::Value,
                out_error: *mut *mut c_char,
                _objthis: *mut c_void,
            ) -> c_int {
                // SAFETY: instance is a valid WaveSoundBufferInst payload.
                let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
                with_channel!(inst, out_error, ch, {
                    ffi::set_real_out(out, f64::from(ch.pos[$axis]));
                    0
                })
            }

            extern "C" fn $set(
                _engine: *mut c_void,
                instance: *mut c_void,
                value: *const tjs2_sys::Value,
                out_error: *mut *mut c_char,
                _objthis: *mut c_void,
            ) -> c_int {
                // SAFETY: instance is a valid payload; value is valid for the call.
                let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
                // SAFETY: value points at the property value for the call.
                let v = unsafe { &*value };
                let new = ffi::value_as_f64(v) as f32;
                with_channel!(inst, out_error, ch, {
                    ch.pos[$axis] = new;
                    0
                })
            }
        )*
    };
}

pos_properties!(
    0 => ws_pos_x_get, ws_pos_x_set,
    1 => ws_pos_y_get, ws_pos_y_set,
    2 => ws_pos_z_get, ws_pos_z_set,
);

/// `flags` getter — a `WaveFlags` object bound to this buffer's stream
/// (reference `WaveIntf.cpp` `flags` → `GetWaveFlagsObjectNoAddRef`). The
/// reference caches one instance per buffer; this port builds a fresh one
/// per access and returns it retained.
extern "C" fn ws_flags_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let expr = format!("new WaveFlags({})", inst.stream_id);
    match set_eval_object_out(out, &expr, "WaveSoundBuffer.flags") {
        Ok(()) => 0,
        Err(e) => ffi::report_error(out_error, &format!("WaveSoundBuffer.flags: {e}")),
    }
}

/// `labels` getter — a Dictionary of the `.sli` labels keyed by name
/// (reference `WaveIntf.cpp` `labels` → `GetWaveLabelsObjectNoAddRef`).
/// Each value carries `name`, `samplePosition` (granules) and `position`
/// (milliseconds). Empty names are skipped, like the reference.
extern "C" fn ws_labels_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let (labels, rate) = {
        let streams = lock_ok(&STREAMS);
        let Some(st) = streams.get(&inst.stream_id) else {
            return ffi::report_error(out_error, "WaveSoundBuffer: not constructed");
        };
        (st.labels.clone(), st.label_rate)
    };
    let mut expr = String::from("(function(){ var d = %[];");
    let mut n = 0usize;
    for label in &labels {
        if label.name.is_empty() {
            continue;
        }
        let name = escape_js(&label.name);
        let pos = label.position;
        let ms = if rate > 0 {
            (pos as i64) * 1000 / i64::from(rate)
        } else {
            0
        };
        expr.push_str(&format!(
            " var e{n} = %[]; e{n}.name = '{name}'; e{n}.samplePosition = {pos}; \
             e{n}.position = {ms}; d['{name}'] = e{n};"
        ));
        n += 1;
    }
    expr.push_str(" return d; })()");
    match set_eval_object_out(out, &expr, "WaveSoundBuffer.labels") {
        Ok(()) => 0,
        Err(e) => ffi::report_error(out_error, &format!("WaveSoundBuffer.labels: {e}")),
    }
}

/// `useVisBuffer` getter — stored flag (reference `GetUseVisBuffer`).
extern "C" fn ws_use_vis_buffer_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    let on = lock_ok(&STREAMS)
        .get(&inst.stream_id)
        .is_some_and(|s| s.use_vis_buffer);
    ffi::set_int_out(out, i64::from(on));
    0
}

/// `useVisBuffer` setter — stored flag (reference `SetUseVisBuffer`). Using
/// the visualization buffer only allocates the reference's DirectSound
/// capture ring; this port has no wave-out cursor, so the flag is state
/// only (see the module docs).
extern "C" fn ws_use_vis_buffer_set(
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
    let on = ffi::value_as_bool(v);
    if let Some(st) = lock_ok(&STREAMS).get_mut(&inst.stream_id) {
        st.use_vis_buffer = on;
    }
    0
}

/// `getVisBuffer(dest, numsamples, channels[, aheadsamples])` — reference
/// `WaveImpl.cpp::GetVisBuffer`. The reference copies samples out of its
/// DirectSound write ring into the caller's raw `dest` pointer.
///
/// This port mixes with a headless clock-driven mixer and has no DirectSound
/// wave-out cursor, and the raw `dest` pointer cannot cross the
/// `tjs2_value` ABI. The method is registered for surface parity and returns
/// 0 samples — exactly the reference's early-out when no visualization ring
/// exists (`!UseVisBuffer`/`!VisBuffer`). See the module docs.
extern "C" fn ws_get_vis_buffer(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid WaveSoundBufferInst payload for the call.
    let _inst = unsafe { &mut *ffi::instance_ptr::<WaveSoundBufferInst>(instance) };
    if argc < 3 {
        return ffi::report_error(
            out_error,
            "WaveSoundBuffer.getVisBuffer requires dest, numsamples, channels",
        );
    }
    ffi::set_int_out(out, 0);
    0
}

/// `freeDirectSound()` — reference `WaveImpl.cpp` static `freeDirectSound`
/// calls `TVPReleaseDirectSound()`. krkr-rs has no process-wide DirectSound
/// device: real output is an optional rodio stream owned by the app's
/// `OutputGuard`, which this native cannot reach. Documented host-only; see
/// the module docs.
extern "C" fn ws_free_direct_sound(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    ffi::set_void_out(out);
    0
}

/// `globalVolume` getter — the process-wide mixer gain (reference
/// `tTJSNI_WaveSoundBuffer::GetGlobalVolume`). The reference exposes this
/// as a *static* class property; the instance-class ABI has no static
/// members, so this port registers it as an instance property backed by the
/// same global mixer state (documented deviation).
extern "C" fn ws_global_volume_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(ctx) = native_ctx() else {
        return ffi::report_error(out_error, "sound natives are not registered");
    };
    let v = lock_ok(&ctx.mixer).global_volume();
    ffi::set_int_out(out, (f64::from(v) * TVP_VOLUME_SCALE) as i64);
    0
}

/// `globalVolume` setter (reference `SetGlobalVolume`, clamped 0..=100000).
extern "C" fn ws_global_volume_set(
    _engine: *mut c_void,
    _instance: *mut c_void,
    value: *const tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value points at the property value for the duration of the call.
    let v = unsafe { &*value };
    let Some(ctx) = native_ctx() else {
        return ffi::report_error(out_error, "sound natives are not registered");
    };
    let gain = ffi::value_as_f64(v) / TVP_VOLUME_SCALE;
    lock_ok(&ctx.mixer).set_global_volume(gain as f32);
    0
}

/// `globalFocusMode` getter — `0` never mute, `1` mute-on-minimize,
/// `2` mute-on-deactivate (reference `GetGlobalFocusMode`). Registered as
/// an instance property, like `globalVolume`.
extern "C" fn ws_global_focus_mode_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(ctx) = native_ctx() else {
        return ffi::report_error(out_error, "sound natives are not registered");
    };
    let mode = lock_ok(&ctx.mixer).global_focus_mode();
    ffi::set_int_out(out, i64::from(mode));
    0
}

/// `globalFocusMode` setter (reference `SetGlobalFocusMode`).
extern "C" fn ws_global_focus_mode_set(
    _engine: *mut c_void,
    _instance: *mut c_void,
    value: *const tjs2_sys::Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value points at the property value for the duration of the call.
    let v = unsafe { &*value };
    let Some(ctx) = native_ctx() else {
        return ffi::report_error(out_error, "sound natives are not registered");
    };
    lock_ok(&ctx.mixer).set_global_focus_mode(ffi::value_as_f64(v) as i32);
    0
}

// ---------------------------------------------------------------------------
// registration + the per-frame poll
// ---------------------------------------------------------------------------

fn method(name: &'static str, f: tjs2_sys::NativeInstanceMethodFn) -> NativeInstanceMethodDef {
    NativeInstanceMethodDef { name, f }
}

// ---------------------------------------------------------------------------
// PhaseVocoder (exposed as `WaveSoundBuffer.PhaseVocoder`)
// ---------------------------------------------------------------------------

/// Payload of one `WaveSoundBuffer.PhaseVocoder` filter object. Reference
/// `tTJSNI_PhaseVocoder` (`PhaseVocoderFilter.cpp:139`) defaults are
/// `window=4096`, `overlap=0`, `pitch=1.0`, `time=1.0`. The properties are
/// real stored state; the phase-vocoder DSP itself is not applied by the
/// mixer (see the module docs), so `time` round-trips but does not alter
/// playback yet.
struct PhaseVocoderInst {
    window: i64,
    overlap: i64,
    pitch: f64,
    time: f64,
}

impl Default for PhaseVocoderInst {
    fn default() -> Self {
        Self {
            window: 4096,
            overlap: 0,
            pitch: 1.0,
            time: 1.0,
        }
    }
}

extern "C" fn phase_vocoder_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(PhaseVocoderInst::default())) as *mut c_void
}

extern "C" fn phase_vocoder_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from phase_vocoder_create's Box::into_raw.
        drop(unsafe { Box::from_raw(instance as *mut PhaseVocoderInst) });
    }
}

/// Borrow the PhaseVocoder instance payload.
fn phase_vocoder(instance: *mut c_void) -> &'static mut PhaseVocoderInst {
    // SAFETY: the dispatcher guarantees `instance` is the create payload.
    unsafe { &mut *(instance as *mut PhaseVocoderInst) }
}

/// `interface` — the reference returns the `iTVPBasicWaveFilter` pointer as
/// an opaque integer token. The `PhaseVocoderInst` address is the analogue.
extern "C" fn pv_interface_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    ffi::set_int_out(out, instance as usize as i64);
    0
}

/// Generate the integer and real PhaseVocoder properties.
macro_rules! pv_i64_property {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut tjs2_sys::Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            ffi::set_int_out(out, phase_vocoder(instance).$field);
            0
        }

        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const tjs2_sys::Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value points at the property value for the call.
            let v = unsafe { &*value };
            phase_vocoder(instance).$field = ffi::value_as_f64(v) as i64;
            0
        }
    };
}

macro_rules! pv_f64_property {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut tjs2_sys::Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            ffi::set_real_out(out, phase_vocoder(instance).$field);
            0
        }

        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const tjs2_sys::Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value points at the property value for the call.
            let v = unsafe { &*value };
            phase_vocoder(instance).$field = ffi::value_as_f64(v);
            0
        }
    };
}

pv_i64_property!(pv_window_get, pv_window_set, window);
pv_i64_property!(pv_overlap_get, pv_overlap_set, overlap);
pv_f64_property!(pv_pitch_get, pv_pitch_set, pitch);
pv_f64_property!(pv_time_get, pv_time_set, time);

/// Register the `PhaseVocoder` native class (exposed by the script as
/// `WaveSoundBuffer.PhaseVocoder`, mirroring the reference's
/// `ScriptMgnIntf.cpp:515` nested-class registration).
fn register_phase_vocoder(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "__TvpPhaseVocoder",
        create: phase_vocoder_create,
        destroy: phase_vocoder_destroy,
        invalidate: None,
        methods: vec![],
        properties: vec![
            NativeInstancePropertyDef {
                name: "interface",
                get: Some(pv_interface_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "window",
                get: Some(pv_window_get),
                set: Some(pv_window_set),
            },
            NativeInstancePropertyDef {
                name: "overlap",
                get: Some(pv_overlap_get),
                set: Some(pv_overlap_set),
            },
            NativeInstancePropertyDef {
                name: "pitch",
                get: Some(pv_pitch_get),
                set: Some(pv_pitch_set),
            },
            NativeInstancePropertyDef {
                name: "time",
                get: Some(pv_time_get),
                set: Some(pv_time_set),
            },
        ],
    })
}

/// Register the `WaveSoundBuffer` native class on `engine`. Must be called
/// on the VM thread; the engine pointer is kept for the poll's event
/// delivery.
pub(crate) fn register_wavesound(engine: &Tjs2Engine) -> Result<(), String> {
    let _ = ENGINE.set(engine as *const Tjs2Engine as usize);
    // The `flags` property constructs a `WaveFlags` object, so the class
    // must exist before scripts can read it.
    crate::waveflags::register_waveflags(engine)?;
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "WaveSoundBuffer",
        create: ws_create,
        destroy: ws_destroy,
        invalidate: None,
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
            method("setPos", ws_set_pos),
            method("getVisBuffer", ws_get_vis_buffer),
            method("freeDirectSound", ws_free_direct_sound),
            method("onStatusChanged", ws_on_status_changed),
            method("onFadeCompleted", ws_on_fade_completed),
            method("onLabel", ws_on_label),
        ],
        properties: vec![
            property("volume", ws_volume_get, Some(ws_volume_set)),
            property("volume2", ws_volume2_get, Some(ws_volume2_set)),
            property("pan", ws_pan_get, Some(ws_pan_set)),
            property("position", ws_position_get, Some(ws_position_set)),
            property(
                "samplePosition",
                ws_sample_position_get,
                Some(ws_sample_position_set),
            ),
            property("totalTime", ws_total_time_get, None),
            property("frequency", ws_frequency_get, Some(ws_frequency_set)),
            property("bits", ws_bits_get, None),
            property("channels", ws_channels_get, None),
            property("status", ws_status_get, None),
            property("looping", ws_looping_get, Some(ws_looping_set)),
            property("paused", ws_paused_get, Some(ws_paused_set)),
            property("speed", ws_speed_get, Some(ws_speed_set)),
            property("filters", ws_filters_get, None),
            property("posX", ws_pos_x_get, Some(ws_pos_x_set)),
            property("posY", ws_pos_y_get, Some(ws_pos_y_set)),
            property("posZ", ws_pos_z_get, Some(ws_pos_z_set)),
            property("flags", ws_flags_get, None),
            property("labels", ws_labels_get, None),
            property(
                "useVisBuffer",
                ws_use_vis_buffer_get,
                Some(ws_use_vis_buffer_set),
            ),
            property(
                "globalVolume",
                ws_global_volume_get,
                Some(ws_global_volume_set),
            ),
            property(
                "globalFocusMode",
                ws_global_focus_mode_get,
                Some(ws_global_focus_mode_set),
            ),
        ],
    })?;
    // The reference's `WaveSoundBuffer` exposes nested filter classes; the
    // game's `SoundLayer` does `new WaveSoundBuffer.PhaseVocoder()` when a
    // BGM is played with `createFilter:1` (the title/the OP). The class is a
    // real native instance (window/overlap/pitch/time); the phase-vocoder
    // DSP is not applied by the mixer yet.
    register_phase_vocoder(engine)?;
    engine
        .exec_script(
            r#"
            if(typeof WaveSoundBuffer.PhaseVocoder == "undefined"){
                WaveSoundBuffer.PhaseVocoder = __TvpPhaseVocoder;
            }
            "#,
            "WaveSoundBuffer_PhaseVocoder",
        )
        .map_err(|e| format!("register WaveSoundBuffer.PhaseVocoder: {e}"))?;
    Ok(())
}

/// Derive a stream's script-visible status from its mixer channel state.
///
/// A source that is still decoding or failed reads as `unload`; the status
/// only becomes `stop`/`play`/`pause` once the track is ready, so scripts
/// never see a spurious `stop` before the data lands. A failed decode reads
/// as `stop` (matching the silent-fallback behaviour) so wait-sequences
/// still advance.
fn derive_status(ch: &Channel) -> Status {
    match &ch.source {
        None => Status::Unload,
        Some(s) if s.is_failed() => Status::Stop,
        Some(s) if !s.is_ready() => Status::Unload,
        Some(_) if !ch.playing => Status::Stop,
        // The reference keeps reporting `play` while paused; `pause` is not
        // a status.
        Some(_) => Status::Play,
    }
}

/// One event queued by [`sound_poll`] for delivery after the locks drop.
enum PollEvent {
    StatusChanged(&'static str),
    FadeCompleted,
    /// A `.sli` `Label` the playhead crossed (reference
    /// `InvokeLabelEvent`); the payload is the label name.
    Label(String),
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
    let mut events: Vec<(u64, tjs2_sys::Tjs2ValueId, PollEvent)> = Vec::new();
    {
        let Some(ctx) = native_ctx() else {
            return;
        };
        let mut streams = lock_ok(&STREAMS);
        let mut mixer = lock_ok(&ctx.mixer);
        for (id, st) in streams.iter_mut() {
            // Never dispatch to a stream whose script object was already
            // found invalid (reaped at the end of this poll).
            if st.dead {
                continue;
            }
            // A buffer whose open failed (no channel) still reports one
            // "stop" so script wait-sequences advance (AttentionVoice's
            // timed entries call play() on a failed open).
            if st.channel_id == 0 {
                if st.emitted_failed_stop {
                    st.emitted_failed_stop = false;
                    st.status = Status::Stop;
                    events.push((*id, st.self_obj.raw_id(), PollEvent::StatusChanged("stop")));
                }
                continue;
            }
            let Some(ch) = mixer.channel(st.channel_id) else {
                continue;
            };
            if ch.fade_finished {
                ch.fade_finished = false;
                events.push((*id, st.self_obj.raw_id(), PollEvent::FadeCompleted));
            }
            // `.sli` label events: fire every label whose sample position
            // the playhead passed since the previous poll (reference
            // `FireLabelEventsAndGetNearestLabelEventStep`). A backwards jump
            // (loop wrap or seek) re-arms from the new position, so labels
            // inside the loop region fire once per pass, like the reference
            // rebuilding its `LabelEventQueue` from the re-decoded segment.
            if !st.labels.is_empty() {
                let rate = f64::from(
                    ch.source
                        .as_ref()
                        .map_or(st.label_rate.max(1), |s| s.sample_rate().max(1)),
                );
                let current = ch.position_seconds * rate;
                if !ch.is_playing() {
                    // Keep the watermark aligned with a paused/stopped
                    // position so a later restart does not fire labels the
                    // playhead never passed.
                    st.last_sample = current;
                } else {
                    if current < st.last_sample {
                        st.next_label =
                            st.labels.partition_point(|l| (l.position as f64) < current);
                    }
                    while st.next_label < st.labels.len() {
                        let pos = st.labels[st.next_label].position;
                        if (pos as f64) > current {
                            break;
                        }
                        let name = st.labels[st.next_label].name.clone();
                        events.push((*id, st.self_obj.raw_id(), PollEvent::Label(name)));
                        st.next_label += 1;
                    }
                    st.last_sample = current;
                }
            }

            let derived = derive_status(ch);
            if derived != st.status {
                let arg = derived.as_str();
                st.status = derived;
                events.push((*id, st.self_obj.raw_id(), PollEvent::StatusChanged(arg)));
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
    let mut dead: HashSet<u64> = HashSet::new();
    for (stream_id, target, event) in events {
        if dead.contains(&stream_id) {
            continue;
        }
        let result = match event {
            PollEvent::StatusChanged(s) => {
                engine.call_member(target, "onStatusChanged", &[TjsValue::String(s.into())])
            }
            PollEvent::FadeCompleted => engine.call_member(target, "onFadeCompleted", &[]),
            PollEvent::Label(name) => {
                engine.call_member(target, "onLabel", &[TjsValue::String(name)])
            }
        };
        if let Err(e) = result {
            if is_invalidated_error(&e) {
                // The script dropped the buffer; stop dispatching to it and
                // reap it below (drop retained objects + mixer channel).
                dead.insert(stream_id);
                mark_stream_dead(stream_id);
            } else {
                log::warn!("WaveSoundBuffer poll event failed: {e}");
            }
        }
    }

    // 4. Reap streams whose objects were invalidated (either during this
    // delivery, or by a nested `dispatch_action` through a native handler).
    // Safe here: no VM call is in progress for these streams.
    reap_dead_streams();
}

/// Escape a string for inclusion in a single-quoted TJS string literal.
fn escape_js(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `filters` array must keep object identity across property reads
    /// (the game does `.filters.clear()`/`.filters.add(...)` and later reads
    /// `filters[0]`), and `WaveSoundBuffer.PhaseVocoder` must be a real
    /// native class whose properties round-trip.
    #[test]
    fn filters_array_is_stable_and_phase_vocoder_roundtrips() {
        let engine = Tjs2Engine::new().expect("create engine");
        register_wavesound(&engine).expect("register wavesound");
        engine
            .exec_script(
                "var b = new WaveSoundBuffer(%[]); \
                 b.filters.add(new WaveSoundBuffer.PhaseVocoder()); \
                 b.filters[0].time = 2.5; \
                 b.filters[0].pitch = 0.5; \
                 var count = b.filters.count; \
                 var t = b.filters[0].time; \
                 var p = b.filters[0].pitch; \
                 var same = (b.filters === b.filters); \
                 var win = b.filters[0].window;",
                "filters",
            )
            .expect("filters script");
        assert_eq!(engine.eval("count", "t").unwrap(), TjsValue::Integer(1));
        assert_eq!(engine.eval("t", "t").unwrap(), TjsValue::Real(2.5));
        assert_eq!(engine.eval("p", "t").unwrap(), TjsValue::Real(0.5));
        assert_eq!(engine.eval("same", "t").unwrap(), TjsValue::Integer(1));
        assert_eq!(engine.eval("win", "t").unwrap(), TjsValue::Integer(4096));
    }
}
