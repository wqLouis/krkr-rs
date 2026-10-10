//! `VideoOverlay` native class.
//!
//! Behavioral reference: `main`'s `crates/tvp-natives/src/video_overlay.rs`
//! and the reference `tTJSNC_VideoOverlay`
//! (`reference/cpp/core/visual/VideoOvlIntf.cpp` +
//! `reference/cpp/core/visual/impl/VideoOvlImpl.{h,cpp}`).
//!
//! # What this composes
//!
//! The class is a thin adapter over the L2 [`engine::video::VideoOverlay`]
//! playback machine: playback commands, the position/frame clock, bounds,
//! visibility, whole-stream/segment looping, the period event, the play rate
//! and the event queue all live in the L2 value. L3 owns only the per-object
//! *dispatch* state ([`VideoState`]) — the id → [`VideoSlot`] registry, the
//! `layer1`/`layer2`/mixing-layer script objects and the backend passthrough
//! fields (`mode`, stream selection, color adjustment) the L2 model does not
//! carry — exactly the established per-family pattern
//! ([`CompatServices::state`]).
//!
//! # No decoder
//!
//! L3 does **not** link `media`/FFmpeg: decoded frames are supplied by the
//! host. A host installs a [`VideoSource`] through
//! [`CompatHost::set_video_source`](crate::CompatHost::set_video_source) (the
//! public seam) with the container's [`VideoSpec`] and the frames it decoded.
//! `open(path)` asks that source for the spec and raises a TJS error when the
//! host has none; it never fabricates metadata. The frame body's `video_poll`
//! step advances every open overlay on the L1 clock and presents the host
//! frame at the current position.
//!
//! # Events
//!
//! [`VideoOverlay::poll`] events are mapped to the script members
//! `onStatusChanged`/`onPeriod`/`onFrameUpdate`; the `onCallbackCommand` hook
//! is registered (the reference fires it from host decoder callbacks). A
//! `TransitionComplete` event has no registered callback (the reference
//! `setTransitionCompleteCall` is not part of the 70-member surface), so it is
//! dropped.
//!
//! [`CompatServices::state`]: crate::CompatServices::state
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use api::media::{RgbaFrame, VideoSpec};
use engine::video::{VideoBounds, VideoEvent, VideoOverlay};
use platform::Clock;
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue, VAL_INTEGER, VAL_OBJECT, VAL_REAL,
    VAL_RETAINED, VAL_STRING, Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError, CompatServices};

// ---------------------------------------------------------------------------
// Host seam: decoded video data
// ---------------------------------------------------------------------------

/// Host-supplied decoded video data.
///
/// L3 owns no decoder: the host probes the container with L1 `media`, decodes
/// frames and hands both to the overlay through this trait. It is installed
/// with [`CompatHost::set_video_source`](crate::CompatHost::set_video_source);
/// `open(path)` asks for the spec and the frame body asks for the frame at the
/// current position.
pub trait VideoSource {
    /// The decoded stream description for `path`, or `None` when the host has
    /// not decoded that file.
    fn spec(&self, path: &str) -> Option<VideoSpec>;

    /// The decoded frame at `at_ms` for `path`, or `None` when the host has no
    /// frame for that position.
    fn frame_at(&mut self, path: &str, at_ms: i64) -> Option<RgbaFrame>;
}

/// The default host source: a spec plus the pre-decoded frame buffer the host
/// registered. `frame_at` picks the last frame whose timestamp is at or before
/// the requested position (the first frame before the first timestamp).
struct BufferedSource {
    spec: VideoSpec,
    frames: Vec<RgbaFrame>,
}

impl VideoSource for BufferedSource {
    fn spec(&self, _path: &str) -> Option<VideoSpec> {
        Some(self.spec)
    }

    fn frame_at(&mut self, _path: &str, at_ms: i64) -> Option<RgbaFrame> {
        let mut chosen = self.frames.first()?;
        for frame in &self.frames {
            if frame.pts_ms as i64 <= at_ms {
                chosen = frame;
            } else {
                break;
            }
        }
        Some(chosen.clone())
    }
}

/// Install the host-decoded source. Called by
/// [`CompatHost::set_video_source`](crate::CompatHost::set_video_source).
pub(crate) fn set_source(services: &CompatServices, spec: VideoSpec, frames: Vec<RgbaFrame>) {
    let mut state = services.state::<VideoState>();
    state.source = Some(Box::new(BufferedSource { spec, frames }));
}

// ---------------------------------------------------------------------------
// Per-family dispatch state (never a process global)
// ---------------------------------------------------------------------------

/// One overlay's L2 playback value plus the L3 dispatch fields the L2 model
/// does not carry.
struct VideoSlot {
    /// The L2 playback machine (position, status, bounds, events, ...).
    overlay: VideoOverlay,
    /// The path the host source was opened with (empty before `open`).
    path: String,
    /// The script object this payload backs (event dispatch target).
    objthis: *mut c_void,
    /// `layer1`/`layer2` assignment (reference `Layer1`/`Layer2`).
    layer1: Option<LayerRef>,
    layer2: Option<LayerRef>,
    /// `setMixingLayer` state (reference `vomMixer` mixing bitmap).
    mixing_layer: Option<LayerRef>,
    /// The mixing layer's opacity in `0.0..=1.0` (reference `opacity / 255`).
    mixing_layer_alpha: f64,
    /// `mode` (`tTVPVideoOverlayMode`; reference default `vomOverlay`).
    mode: i64,
    /// Audio balance/volume passthrough (reference `TVPDSAttenuateToPan`).
    audio_balance: i64,
    audio_volume: i64,
    /// Selected audio/video stream numbers (`-1` when none is selected).
    enabled_audio_stream: i64,
    enabled_video_stream: i64,
    /// `mixingMovieAlpha`/`mixingMovieBGColor` (reference `vomMixer`).
    mixing_alpha: f64,
    mixing_bg: i64,
    /// Color-adjustment passthrough (inert without a decoder).
    contrast: f64,
    brightness: f64,
    hue: f64,
    saturation: f64,
    /// Stream counts from the host spec (`VideoSpec` has no stream count).
    number_of_audio_stream: i64,
    number_of_video_stream: i64,
}

impl VideoSlot {
    fn new() -> Self {
        Self {
            overlay: VideoOverlay::new(),
            path: String::new(),
            objthis: std::ptr::null_mut(),
            layer1: None,
            layer2: None,
            mixing_layer: None,
            mixing_layer_alpha: 1.0,
            mode: 0,
            audio_balance: 0,
            audio_volume: 0,
            enabled_audio_stream: 0,
            enabled_video_stream: 0,
            mixing_alpha: 1.0,
            mixing_bg: 0,
            contrast: 1.0,
            brightness: 0.0,
            hue: 0.0,
            saturation: 1.0,
            number_of_audio_stream: 0,
            number_of_video_stream: 0,
        }
    }
}

/// A retained script `Layer` object plus its scene id.
struct LayerRef {
    /// Keeps the script object alive while the overlay references it.
    keepalive: DetachedValue,
    /// The scene layer id (the VM native-instance lookup of the `Layer`
    /// class), used for the L2 binding.
    id: u32,
}

/// The video family's dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
struct VideoState {
    /// Id → overlay state; ids are allocated by `create` and freed by
    /// `destroy`.
    overlays: HashMap<u32, VideoSlot>,
    /// The next object id.
    next_id: u32,
    /// The host-decoded source (installed by the host before `open`).
    source: Option<Box<dyn VideoSource>>,
    /// The L1 monotonic clock that drives playback.
    clock: Clock,
    /// The last `video_poll` timestamp (to derive the per-frame `dt`).
    last_now_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Native instance payload
// ---------------------------------------------------------------------------

/// `VideoOverlay` payload: the id of the [`VideoSlot`] in [`VideoState`].
#[derive(Default)]
struct VideoOverlayInst {
    id: Option<u32>,
}

// ---------------------------------------------------------------------------
// Small marshalling helpers
// ---------------------------------------------------------------------------

/// Resolve the installed engine → context binding for a native callback.
fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "VideoOverlay: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// Write a real result into a native callback's out slot.
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

/// TJS `AsReal` coercion.
fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        VAL_INTEGER => v.integer as f64,
        VAL_REAL => v.real,
        VAL_STRING => value_as_string(v).parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Write a retained id into a native callback's out slot. The trampoline
/// consumes the retention.
fn set_retained_out(out: *mut Value, id: Tjs2ValueId) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = id as usize;
    }
}

/// Write TJS `null` into `out` (reference `layer1`/`layer2` read back null).
fn set_null_out(host: &Tjs2Engine, out: *mut Value) {
    if out.is_null() {
        return;
    }
    if let Ok(RetainedValue::Object(object)) = host.eval_retained("null", "video.null") {
        set_retained_out(out, object.raw_id());
        std::mem::forget(object);
        return;
    }
    set_void(out);
}

/// Resolve a script object argument to a scene id. An object argument is
/// resolved through the VM's native-instance lookup for the `Window` class
/// (the owning-window argument of the constructor).
fn object_id(host: &Tjs2Engine, value: &Value) -> Result<i64, String> {
    CompatCtx::resolve_object_id(host, value, &["Window"])
}

/// Resolve a `layer1`/`layer2`/`setMixingLayer` argument to a [`LayerRef`].
///
/// A null/void/non-object argument clears the slot; any other object must be
/// a native `Layer`, resolved through the VM's native-instance lookup with no
/// member fallback. A non-Layer object raises `TVPSpecifyLayer`.
fn resolve_layer_ref(host: &Tjs2Engine, value: &Value) -> Result<Option<LayerRef>, String> {
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return Ok(None);
    }
    let keepalive = host
        .retain_object_arg(value)
        .map_err(|_| "VideoOverlay: specify layer".to_string())?;
    let id = CompatCtx::native_instance_id(host, keepalive.raw_id(), &["Layer"])
        .map_err(|error| format!("VideoOverlay: specify layer: {error}"))?
        .ok_or_else(|| "VideoOverlay: specify layer".to_string())?;
    Ok(Some(LayerRef { keepalive, id }))
}

/// Read a boolean member, defaulting to `true` when absent (the reference
/// `Layer.visible` default).
fn member_is_true(host: &Tjs2Engine, id: Tjs2ValueId, name: &str) -> bool {
    match host.member(id, name) {
        Ok(TjsValue::Integer(v)) => v != 0,
        Ok(TjsValue::Real(v)) => v != 0.0,
        _ => true,
    }
}

/// Read a numeric member, or `None` when it is absent.
fn member_as_f64(host: &Tjs2Engine, id: Tjs2ValueId, name: &str) -> Option<f64> {
    match host.member(id, name) {
        Ok(TjsValue::Integer(v)) => Some(v as f64),
        Ok(TjsValue::Real(v)) => Some(v),
        _ => None,
    }
}

/// Clear the mixing-layer binding, preserving the constructor window binding
/// (the L2 `unbind` clears both).
fn reset_mixing(slot: &mut VideoSlot) {
    let window = slot.overlay.binding().window;
    slot.overlay.unbind();
    if let Some(window) = window {
        slot.overlay.bind_window(window);
    }
    slot.mixing_layer = None;
    slot.mixing_layer_alpha = 1.0;
}

// ---------------------------------------------------------------------------
// Instance property accessor macros
// ---------------------------------------------------------------------------

/// Resolve the callback context and the object's [`VideoSlot`], then run
/// `$body` with `$slot` bound. On a missing binding/state it reports the TJS
/// error and returns.
macro_rules! with_slot {
    ($engine:expr, $instance:expr, $out_error:expr, $ctx:ident, $slot:ident, $body:block) => {{
        let Ok((_resolved, $ctx)) = bound($engine) else {
            return report_error($out_error, "VideoOverlay: compat context is not bound");
        };
        let inst = unsafe { &*($instance as *const VideoOverlayInst) };
        let Some(id) = inst.id else {
            return report_error($out_error, "VideoOverlay: object is not constructed");
        };
        let services = unsafe { $ctx.services() };
        let mut state = services.state::<VideoState>();
        let Some($slot) = state.overlays.get_mut(&id) else {
            return report_error($out_error, "VideoOverlay: object state is missing");
        };
        $body
    }};
}

/// Read/write an integer [`VideoSlot`] field.
macro_rules! slot_int_prop {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_slot!(engine, instance, out_error, _ctx, slot, {
                set_int(out, slot.$field);
                0
            })
        }
        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_slot!(engine, instance, out_error, _ctx, slot, {
                slot.$field = value_as_i64(unsafe { &*value });
                0
            })
        }
    };
}

/// Read/write a real [`VideoSlot`] field.
macro_rules! slot_real_prop {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_slot!(engine, instance, out_error, _ctx, slot, {
                set_real(out, slot.$field);
                0
            })
        }
        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_slot!(engine, instance, out_error, _ctx, slot, {
                slot.$field = value_as_f64(unsafe { &*value });
                0
            })
        }
    };
}

/// Read-only integer [`VideoSlot`] field.
macro_rules! slot_ro_int {
    ($get:ident, $field:ident) => {
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_slot!(engine, instance, out_error, _ctx, slot, {
                set_int(out, slot.$field);
                0
            })
        }
    };
}

/// Read-only real constant (the reference's backend-defined
/// `*RangeMin`/`*RangeMax`/`*DefaultValue`/`*StepSize` metadata).
macro_rules! const_real_prop {
    ($get:ident, $value:expr) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_real(out, $value);
            0
        }
    };
}

// ---------------------------------------------------------------------------
// Playback commands
// ---------------------------------------------------------------------------

extern "C" fn video_create(engine: *mut c_void) -> *mut c_void {
    let mut inst = Box::new(VideoOverlayInst::default());
    if let Ok((_resolved, ctx)) = bound(engine) {
        let services = unsafe { ctx.services() };
        let mut state = services.state::<VideoState>();
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.overlays.insert(id, VideoSlot::new());
        inst.id = Some(id);
    }
    Box::into_raw(inst) as *mut c_void
}

extern "C" fn video_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // Remove the slot while the engine is alive: dropping it releases the
        // retained `layer1`/`layer2` objects.
        let services = unsafe { ctx.services() };
        services.state::<VideoState>().overlays.remove(&id);
    }
    // SAFETY: instance came from `video_create`'s `Box::into_raw` and is freed
    // exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut VideoOverlayInst) });
}

/// `VideoOverlay(win)` — the constructor hook. The optional first argument is
/// the owning window (reference `Construct`); `null`/no argument is accepted
/// (the headless port cannot validate the class).
extern "C" fn video_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "VideoOverlay: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay: object is not constructed");
    };
    let values = unsafe { args(argv, argc) };
    let window = match values.first() {
        Some(value) if value.ty == VAL_OBJECT && !value.object_handle().is_null() => {
            match object_id(resolved.engine(), value) {
                Ok(id) if id >= 0 => Some(id as u32),
                Ok(_) => None,
                Err(error) => return report_error(out_error, &format!("VideoOverlay: {error}")),
            }
        }
        _ => None,
    };
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    let Some(slot) = state.overlays.get_mut(&id) else {
        return report_error(out_error, "VideoOverlay: object state is missing");
    };
    slot.objthis = objthis;
    if let Some(window) = window {
        slot.overlay.bind_window(window);
    }
    drop(state);
    set_void(out);
    0
}

/// `open(file)` — open the host-decoded source for `file`.
extern "C" fn vo_open(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "VideoOverlay.open: compat context is not bound");
    };
    let values = unsafe { args(argv, argc) };
    let Some(path_value) = values.first() else {
        return report_error(out_error, "VideoOverlay.open requires 1 argument");
    };
    let path = value_as_string(path_value);
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.open: object is not constructed");
    };
    let services = unsafe { ctx.services() };
    // Ask the host for the spec without holding the state borrow.
    let spec = {
        let state = services.state::<VideoState>();
        state.source.as_ref().and_then(|source| source.spec(&path))
    };
    let Some(spec) = spec else {
        return report_error(
            out_error,
            &format!("VideoOverlay.open: no decoded source for {path:?}"),
        );
    };
    let mut state = services.state::<VideoState>();
    let Some(slot) = state.overlays.get_mut(&id) else {
        return report_error(out_error, "VideoOverlay.open: object state is missing");
    };
    if let Err(error) = slot.overlay.open(spec) {
        return report_error(out_error, &format!("VideoOverlay.open: {error}"));
    }
    slot.path = path;
    slot.number_of_audio_stream = if spec.has_audio() { 1 } else { 0 };
    slot.number_of_video_stream = if spec.width > 0 && spec.height > 0 {
        1
    } else {
        0
    };
    drop(state);
    set_void(out);
    0
}

/// `close()` — drop the media and return to `unload`.
extern "C" fn vo_close(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.close();
        slot.path.clear();
        slot.number_of_audio_stream = 0;
        slot.number_of_video_stream = 0;
        set_void(out);
        0
    })
}

/// `play()`.
extern "C" fn vo_play(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        match slot.overlay.play() {
            Ok(()) => {
                set_void(out);
                0
            }
            Err(error) => report_error(out_error, &format!("VideoOverlay.play: {error}")),
        }
    })
}

/// `pause()`.
extern "C" fn vo_pause(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        match slot.overlay.pause() {
            Ok(()) => {
                set_void(out);
                0
            }
            Err(error) => report_error(out_error, &format!("VideoOverlay.pause: {error}")),
        }
    })
}

/// `stop()`.
extern "C" fn vo_stop(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        match slot.overlay.stop() {
            Ok(()) => {
                set_void(out);
                0
            }
            Err(error) => report_error(out_error, &format!("VideoOverlay.stop: {error}")),
        }
    })
}

/// `rewind()`.
extern "C" fn vo_rewind(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.rewind();
        set_void(out);
        0
    })
}

/// `prepare()`.
extern "C" fn vo_prepare(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        match slot.overlay.prepare() {
            Ok(()) => {
                set_void(out);
                0
            }
            Err(error) => report_error(out_error, &format!("VideoOverlay.prepare: {error}")),
        }
    })
}

/// `setPos(left, top)`.
extern "C" fn vo_set_pos(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = unsafe { args(argv, argc) };
    if values.len() < 2 {
        return report_error(out_error, "VideoOverlay.setPos requires 2 arguments");
    }
    let left = value_as_i64(&values[0]) as i32;
    let top = value_as_i64(&values[1]) as i32;
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.set_position(left, top);
        set_void(out);
        0
    })
}

/// `setSize(width, height)`.
extern "C" fn vo_set_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = unsafe { args(argv, argc) };
    if values.len() < 2 {
        return report_error(out_error, "VideoOverlay.setSize requires 2 arguments");
    }
    let width = value_as_i64(&values[0]).max(0) as u32;
    let height = value_as_i64(&values[1]).max(0) as u32;
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.set_size(width, height);
        set_void(out);
        0
    })
}

/// `setBounds(left, top, width, height)`.
extern "C" fn vo_set_bounds(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = unsafe { args(argv, argc) };
    if values.len() < 4 {
        return report_error(out_error, "VideoOverlay.setBounds requires 4 arguments");
    }
    let bounds = VideoBounds::new(
        value_as_i64(&values[0]) as i32,
        value_as_i64(&values[1]) as i32,
        value_as_i64(&values[2]).max(0) as u32,
        value_as_i64(&values[3]).max(0) as u32,
    );
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.bounds = bounds;
        set_void(out);
        0
    })
}

/// `setSegmentLoop(startFrame, endFrame)`.
extern "C" fn vo_set_segment_loop(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = unsafe { args(argv, argc) };
    if values.len() < 2 {
        return report_error(
            out_error,
            "VideoOverlay.setSegmentLoop requires 2 arguments",
        );
    }
    let start = value_as_i64(&values[0]);
    let end = value_as_i64(&values[1]);
    with_slot!(engine, instance, out_error, _ctx, slot, {
        match slot.overlay.set_segment_loop(start, end) {
            Ok(()) => {
                set_void(out);
                0
            }
            Err(error) => report_error(out_error, &format!("VideoOverlay.setSegmentLoop: {error}")),
        }
    })
}

/// `cancelSegmentLoop()`.
extern "C" fn vo_cancel_segment_loop(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.cancel_segment_loop();
        set_void(out);
        0
    })
}

/// `setPeriodEvent([frame])` — no argument clears it (reference).
extern "C" fn vo_set_period_event(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = unsafe { args(argv, argc) };
    let frame = match values.len() {
        0 => -1,
        1 => value_as_i64(&values[0]),
        // The reference ignores 2+ arguments entirely.
        _ => {
            set_void(out);
            return 0;
        }
    };
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.set_period_event(frame);
        set_void(out);
        0
    })
}

/// `cancelPeriodEvent()`.
extern "C" fn vo_cancel_period_event(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.cancel_period_event();
        set_void(out);
        0
    })
}

/// `setMixingLayer(layer)` — a null or non-visible layer resets the mixing
/// bitmap (reference `tTJSNI_VideoOverlay::SetMixingLayer`).
extern "C" fn vo_set_mixing_layer(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.setMixingLayer: compat context is not bound",
        );
    };
    let values = unsafe { args(argv, argc) };
    let Some(value) = values.first() else {
        return report_error(out_error, "VideoOverlay.setMixingLayer requires 1 argument");
    };
    let layer = match resolve_layer_ref(resolved.engine(), value) {
        Ok(layer) => layer,
        Err(error) => return report_error(out_error, &error),
    };
    // A visible layer contributes its opacity; anything else resets.
    let stored = match layer {
        Some(layer) if member_is_true(resolved.engine(), layer.keepalive.raw_id(), "visible") => {
            let opacity = member_as_f64(resolved.engine(), layer.keepalive.raw_id(), "opacity")
                .unwrap_or(255.0);
            Some((layer, (opacity / 255.0).clamp(0.0, 1.0)))
        }
        _ => None,
    };
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "VideoOverlay.setMixingLayer: object is not constructed",
        );
    };
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    let Some(slot) = state.overlays.get_mut(&id) else {
        return report_error(
            out_error,
            "VideoOverlay.setMixingLayer: object state is missing",
        );
    };
    match stored {
        Some((layer, alpha)) => {
            slot.overlay.bind_layer(layer.id);
            slot.overlay.set_binding_alpha(alpha as f32);
            slot.mixing_layer = Some(layer);
            slot.mixing_layer_alpha = alpha;
        }
        None => reset_mixing(slot),
    }
    drop(state);
    set_void(out);
    0
}

/// `resetMixingLayer()`.
extern "C" fn vo_reset_mixing_layer(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        reset_mixing(slot);
        set_void(out);
        0
    })
}

/// `selectAudioStream(n)`.
extern "C" fn vo_select_audio_stream(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = unsafe { args(argv, argc) };
    let stream = values.first().map(value_as_i64).unwrap_or(0);
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.enabled_audio_stream = stream;
        set_void(out);
        0
    })
}

// -- overridable event hooks -------------------------------------------------

extern "C" fn vo_on_status_changed(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

extern "C" fn vo_on_callback_command(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

extern "C" fn vo_on_period(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

extern "C" fn vo_on_frame_update(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

// -- rectangle / visibility -------------------------------------------------

extern "C" fn vo_left_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, i64::from(slot.overlay.bounds.left));
        0
    })
}

extern "C" fn vo_left_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        let top = slot.overlay.bounds.top;
        slot.overlay
            .set_position(value_as_i64(unsafe { &*value }) as i32, top);
        0
    })
}

extern "C" fn vo_top_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, i64::from(slot.overlay.bounds.top));
        0
    })
}

extern "C" fn vo_top_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        let left = slot.overlay.bounds.left;
        slot.overlay
            .set_position(left, value_as_i64(unsafe { &*value }) as i32);
        0
    })
}

extern "C" fn vo_width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, i64::from(slot.overlay.bounds.width));
        0
    })
}

extern "C" fn vo_width_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        let height = slot.overlay.bounds.height;
        slot.overlay
            .set_size(value_as_i64(unsafe { &*value }).max(0) as u32, height);
        0
    })
}

extern "C" fn vo_height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, i64::from(slot.overlay.bounds.height));
        0
    })
}

extern "C" fn vo_height_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        let width = slot.overlay.bounds.width;
        slot.overlay
            .set_size(width, value_as_i64(unsafe { &*value }).max(0) as u32);
        0
    })
}

extern "C" fn vo_visible_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_bool(out, slot.overlay.visible());
        0
    })
}

extern "C" fn vo_visible_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.set_visible(value_as_bool(unsafe { &*value }));
        0
    })
}

// -- playback state ---------------------------------------------------------

extern "C" fn vo_position_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.position_ms());
        0
    })
}

extern "C" fn vo_position_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.seek_ms(value_as_i64(unsafe { &*value }));
        0
    })
}

extern "C" fn vo_frame_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.frame());
        0
    })
}

extern "C" fn vo_frame_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.seek_frame(value_as_i64(unsafe { &*value }));
        0
    })
}

extern "C" fn vo_loop_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_bool(out, slot.overlay.looping);
        0
    })
}

extern "C" fn vo_loop_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.looping = value_as_bool(unsafe { &*value });
        0
    })
}

extern "C" fn vo_play_rate_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_real(out, slot.overlay.play_rate());
        0
    })
}

extern "C" fn vo_play_rate_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        let rate = value_as_f64(unsafe { &*value });
        match slot.overlay.set_play_rate(rate) {
            Ok(()) => 0,
            Err(error) => report_error(out_error, &format!("VideoOverlay.playRate: {error}")),
        }
    })
}

extern "C" fn vo_period_event_frame_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.period_event().unwrap_or(-1));
        0
    })
}

extern "C" fn vo_period_event_frame_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay
            .set_period_event(value_as_i64(unsafe { &*value }));
        0
    })
}

extern "C" fn vo_segment_loop_start_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            slot.overlay.segment_loop().map_or(-1, |(start, _)| start),
        );
        0
    })
}

extern "C" fn vo_segment_loop_end_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.segment_loop().map_or(-1, |(_, end)| end));
        0
    })
}

// -- audio / video streams --------------------------------------------------

slot_int_prop!(vo_audio_balance_get, vo_audio_balance_set, audio_balance);
slot_int_prop!(vo_audio_volume_get, vo_audio_volume_set, audio_volume);
slot_int_prop!(
    vo_enabled_audio_stream_get,
    vo_enabled_audio_stream_set,
    enabled_audio_stream
);
slot_int_prop!(
    vo_enabled_video_stream_get,
    vo_enabled_video_stream_set,
    enabled_video_stream
);

// -- mixing -----------------------------------------------------------------

slot_real_prop!(vo_mixing_alpha_get, vo_mixing_alpha_set, mixing_alpha);
slot_int_prop!(vo_mixing_bg_get, vo_mixing_bg_set, mixing_bg);

// -- mode -------------------------------------------------------------------

slot_int_prop!(vo_mode_get, vo_mode_set, mode);

// -- color adjustment (backend-defined; inert without a decoder) ------------

slot_real_prop!(vo_contrast_get, vo_contrast_set, contrast);
slot_real_prop!(vo_brightness_get, vo_brightness_set, brightness);
slot_real_prop!(vo_hue_get, vo_hue_set, hue);
slot_real_prop!(vo_saturation_get, vo_saturation_set, saturation);

const_real_prop!(vo_contrast_range_min_get, 0.0);
const_real_prop!(vo_contrast_range_max_get, 2.0);
const_real_prop!(vo_contrast_default_value_get, 1.0);
const_real_prop!(vo_contrast_step_size_get, 0.01);
const_real_prop!(vo_brightness_range_min_get, -1.0);
const_real_prop!(vo_brightness_range_max_get, 1.0);
const_real_prop!(vo_brightness_default_value_get, 0.0);
const_real_prop!(vo_brightness_step_size_get, 0.01);
const_real_prop!(vo_hue_range_min_get, -180.0);
const_real_prop!(vo_hue_range_max_get, 180.0);
const_real_prop!(vo_hue_default_value_get, 0.0);
const_real_prop!(vo_hue_step_size_get, 1.0);
const_real_prop!(vo_saturation_range_min_get, 0.0);
const_real_prop!(vo_saturation_range_max_get, 2.0);
const_real_prop!(vo_saturation_default_value_get, 1.0);
const_real_prop!(vo_saturation_step_size_get, 0.01);

// -- media metadata ---------------------------------------------------------

extern "C" fn vo_original_width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            slot.overlay.spec().map_or(0, |spec| i64::from(spec.width)),
        );
        0
    })
}

extern "C" fn vo_original_height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            slot.overlay.spec().map_or(0, |spec| i64::from(spec.height)),
        );
        0
    })
}

extern "C" fn vo_number_of_frame_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.total_frames());
        0
    })
}

extern "C" fn vo_fps_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_real(out, slot.overlay.fps());
        0
    })
}

extern "C" fn vo_total_time_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.duration_ms());
        0
    })
}

slot_ro_int!(vo_number_of_audio_stream_get, number_of_audio_stream);
slot_ro_int!(vo_number_of_video_stream_get, number_of_video_stream);

// -- layer1 / layer2 --------------------------------------------------------

extern "C" fn vo_layer1_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.layer1: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.layer1: object is not constructed");
    };
    let services = unsafe { ctx.services() };
    let raw = {
        let state = services.state::<VideoState>();
        state
            .overlays
            .get(&id)
            .and_then(|slot| slot.layer1.as_ref().map(|layer| layer.keepalive.raw_id()))
    };
    match raw {
        Some(raw) => match resolved.engine().duplicate_retained(raw) {
            Ok(duplicate) => {
                set_retained_out(out, duplicate.raw_id());
                std::mem::forget(duplicate);
            }
            Err(_) => set_null_out(resolved.engine(), out),
        },
        None => set_null_out(resolved.engine(), out),
    }
    0
}

extern "C" fn vo_layer1_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.layer1: compat context is not bound",
        );
    };
    let layer = match resolve_layer_ref(resolved.engine(), unsafe { &*value }) {
        Ok(layer) => layer,
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.layer1: object is not constructed");
    };
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    if let Some(slot) = state.overlays.get_mut(&id) {
        slot.layer1 = layer;
    }
    0
}

extern "C" fn vo_layer2_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.layer2: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.layer2: object is not constructed");
    };
    let services = unsafe { ctx.services() };
    let raw = {
        let state = services.state::<VideoState>();
        state
            .overlays
            .get(&id)
            .and_then(|slot| slot.layer2.as_ref().map(|layer| layer.keepalive.raw_id()))
    };
    match raw {
        Some(raw) => match resolved.engine().duplicate_retained(raw) {
            Ok(duplicate) => {
                set_retained_out(out, duplicate.raw_id());
                std::mem::forget(duplicate);
            }
            Err(_) => set_null_out(resolved.engine(), out),
        },
        None => set_null_out(resolved.engine(), out),
    }
    0
}

extern "C" fn vo_layer2_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.layer2: compat context is not bound",
        );
    };
    let layer = match resolve_layer_ref(resolved.engine(), unsafe { &*value }) {
        Ok(layer) => layer,
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.layer2: object is not constructed");
    };
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    if let Some(slot) = state.overlays.get_mut(&id) {
        slot.layer2 = layer;
    }
    0
}

// ---------------------------------------------------------------------------
// Event dispatch
// ---------------------------------------------------------------------------

/// A sink the event mapper calls for each queued [`VideoEvent`].
trait EventSink {
    /// Call `member` on the overlay object with `args`.
    fn call_member(
        &self,
        objthis: *mut c_void,
        member: &str,
        args: &[TjsValue],
    ) -> Result<TjsValue, String>;
}

/// Map queued L2 events to the script `on*` members (the reference
/// `FireStatusChanged`/`FirePeriodEvent`/`FireFrameUpdateEvent`).
fn dispatch_events_with(sink: &dyn EventSink, objthis: *mut c_void, events: &[VideoEvent]) {
    if objthis.is_null() {
        return;
    }
    for event in events {
        let (member, args) = match event {
            VideoEvent::StatusChanged(status) => (
                "onStatusChanged",
                vec![TjsValue::String(status.as_str().to_string())],
            ),
            VideoEvent::Period(reason) => ("onPeriod", vec![TjsValue::Integer(reason.code())]),
            VideoEvent::FrameUpdate(frame) => ("onFrameUpdate", vec![TjsValue::Integer(*frame)]),
            // No registered `setTransitionCompleteCall` in the 70-member
            // surface: the host observes the completion through the queue.
            VideoEvent::TransitionComplete => continue,
        };
        if let Err(error) = sink.call_member(objthis, member, &args) {
            log::warn!("VideoOverlay: {member} failed: {error}");
        }
    }
}

/// The real sink: retain the object for the duration of the dispatch and call
/// the member through the VM (a script override wins over the native hook).
struct EngineSink<'a> {
    host: &'a Tjs2Engine,
    owner: DetachedValue,
}

impl EventSink for EngineSink<'_> {
    fn call_member(
        &self,
        _objthis: *mut c_void,
        member: &str,
        args: &[TjsValue],
    ) -> Result<TjsValue, String> {
        self.host.call_member(self.owner.raw_id(), member, args)
    }
}

/// Dispatch an overlay's queued events through the VM.
fn dispatch_events(host: &Tjs2Engine, objthis: *mut c_void, events: &[VideoEvent]) {
    if objthis.is_null() {
        return;
    }
    let Ok(owner) = host.retain_object_detached(objthis) else {
        return;
    };
    let sink = EngineSink { host, owner };
    dispatch_events_with(&sink, objthis, events);
}

// ---------------------------------------------------------------------------
// Frame-body poll
// ---------------------------------------------------------------------------

/// Advance every open overlay by `dt_ms`, present the host frame at the new
/// position and dispatch the queued events.
///
/// This is the testable core of [`video_poll`]: the frame body derives `dt_ms`
/// from the L1 clock, while tests drive the state machine deterministically.
///
/// The host re-exports this (with [`video_poll`]) from the crate root; until
/// then it is unreachable from outside the crate.
pub fn video_advance(ctx: &CompatCtx, dt_ms: f64) -> Result<(), CompatError> {
    let host = unsafe { ctx.engine() };
    let services = unsafe { ctx.services() };
    // Phase 1: advance and present frames. The state borrow is released before
    // any script callback runs, so a re-entrant native call cannot alias it.
    let pending: Vec<(*mut c_void, Vec<VideoEvent>)> = {
        let mut state = services.state::<VideoState>();
        let mut source = state.source.take();
        let mut pending = Vec::new();
        for slot in state.overlays.values_mut() {
            if !slot.overlay.is_open() {
                continue;
            }
            if let Some(source) = source.as_mut() {
                let at = slot.overlay.position_ms();
                if let Some(frame) = source.frame_at(&slot.path, at) {
                    let _ = slot.overlay.present_frame(frame);
                }
            }
            slot.overlay.advance(dt_ms);
            let events = slot.overlay.poll();
            if !events.is_empty() {
                pending.push((slot.objthis, events));
            }
        }
        state.source = source;
        pending
    };
    // Phase 2: dispatch outside the state borrow.
    for (objthis, events) in pending {
        dispatch_events(host, objthis, &events);
    }
    Ok(())
}

/// The frame body's `video_poll` step: derive the per-frame `dt` from the L1
/// clock and advance every open overlay.
///
/// The host re-exports this from the crate root; until then it is unreachable
/// from outside the crate.
pub fn video_poll(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let services = unsafe { ctx.services() };
    let now = services.state::<VideoState>().clock.now_ms();
    let dt_ms = {
        let mut state = services.state::<VideoState>();
        let dt_ms = match state.last_now_ms {
            Some(last) => now.saturating_sub(last) as f64,
            None => 0.0,
        };
        state.last_now_ms = Some(now);
        dt_ms
    };
    video_advance(ctx, dt_ms)
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `VideoOverlay` class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "VideoOverlay",
        create: video_create,
        destroy: video_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "VideoOverlay",
                f: video_ctor,
            },
            NativeInstanceMethodDef {
                name: "open",
                f: vo_open,
            },
            NativeInstanceMethodDef {
                name: "close",
                f: vo_close,
            },
            NativeInstanceMethodDef {
                name: "play",
                f: vo_play,
            },
            NativeInstanceMethodDef {
                name: "pause",
                f: vo_pause,
            },
            NativeInstanceMethodDef {
                name: "stop",
                f: vo_stop,
            },
            NativeInstanceMethodDef {
                name: "rewind",
                f: vo_rewind,
            },
            NativeInstanceMethodDef {
                name: "prepare",
                f: vo_prepare,
            },
            NativeInstanceMethodDef {
                name: "setPos",
                f: vo_set_pos,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: vo_set_size,
            },
            NativeInstanceMethodDef {
                name: "setBounds",
                f: vo_set_bounds,
            },
            NativeInstanceMethodDef {
                name: "setSegmentLoop",
                f: vo_set_segment_loop,
            },
            NativeInstanceMethodDef {
                name: "cancelSegmentLoop",
                f: vo_cancel_segment_loop,
            },
            NativeInstanceMethodDef {
                name: "setPeriodEvent",
                f: vo_set_period_event,
            },
            NativeInstanceMethodDef {
                name: "cancelPeriodEvent",
                f: vo_cancel_period_event,
            },
            NativeInstanceMethodDef {
                name: "setMixingLayer",
                f: vo_set_mixing_layer,
            },
            NativeInstanceMethodDef {
                name: "resetMixingLayer",
                f: vo_reset_mixing_layer,
            },
            NativeInstanceMethodDef {
                name: "selectAudioStream",
                f: vo_select_audio_stream,
            },
            NativeInstanceMethodDef {
                name: "onStatusChanged",
                f: vo_on_status_changed,
            },
            NativeInstanceMethodDef {
                name: "onCallbackCommand",
                f: vo_on_callback_command,
            },
            NativeInstanceMethodDef {
                name: "onPeriod",
                f: vo_on_period,
            },
            NativeInstanceMethodDef {
                name: "onFrameUpdate",
                f: vo_on_frame_update,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "left",
                get: Some(vo_left_get),
                set: Some(vo_left_set),
            },
            NativeInstancePropertyDef {
                name: "top",
                get: Some(vo_top_get),
                set: Some(vo_top_set),
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(vo_width_get),
                set: Some(vo_width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(vo_height_get),
                set: Some(vo_height_set),
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(vo_visible_get),
                set: Some(vo_visible_set),
            },
            NativeInstancePropertyDef {
                name: "position",
                get: Some(vo_position_get),
                set: Some(vo_position_set),
            },
            NativeInstancePropertyDef {
                name: "frame",
                get: Some(vo_frame_get),
                set: Some(vo_frame_set),
            },
            NativeInstancePropertyDef {
                name: "loop",
                get: Some(vo_loop_get),
                set: Some(vo_loop_set),
            },
            NativeInstancePropertyDef {
                name: "mode",
                get: Some(vo_mode_get),
                set: Some(vo_mode_set),
            },
            NativeInstancePropertyDef {
                name: "playRate",
                get: Some(vo_play_rate_get),
                set: Some(vo_play_rate_set),
            },
            NativeInstancePropertyDef {
                name: "periodEventFrame",
                get: Some(vo_period_event_frame_get),
                set: Some(vo_period_event_frame_set),
            },
            NativeInstancePropertyDef {
                name: "segmentLoopStartFrame",
                get: Some(vo_segment_loop_start_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "segmentLoopEndFrame",
                get: Some(vo_segment_loop_end_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "audioBalance",
                get: Some(vo_audio_balance_get),
                set: Some(vo_audio_balance_set),
            },
            NativeInstancePropertyDef {
                name: "audioVolume",
                get: Some(vo_audio_volume_get),
                set: Some(vo_audio_volume_set),
            },
            NativeInstancePropertyDef {
                name: "enabledAudioStream",
                get: Some(vo_enabled_audio_stream_get),
                set: Some(vo_enabled_audio_stream_set),
            },
            NativeInstancePropertyDef {
                name: "enabledVideoStream",
                get: Some(vo_enabled_video_stream_get),
                set: Some(vo_enabled_video_stream_set),
            },
            NativeInstancePropertyDef {
                name: "mixingMovieAlpha",
                get: Some(vo_mixing_alpha_get),
                set: Some(vo_mixing_alpha_set),
            },
            NativeInstancePropertyDef {
                name: "mixingMovieBGColor",
                get: Some(vo_mixing_bg_get),
                set: Some(vo_mixing_bg_set),
            },
            NativeInstancePropertyDef {
                name: "contrast",
                get: Some(vo_contrast_get),
                set: Some(vo_contrast_set),
            },
            NativeInstancePropertyDef {
                name: "contrastRangeMin",
                get: Some(vo_contrast_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "contrastRangeMax",
                get: Some(vo_contrast_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "contrastDefaultValue",
                get: Some(vo_contrast_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "contrastStepSize",
                get: Some(vo_contrast_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightness",
                get: Some(vo_brightness_get),
                set: Some(vo_brightness_set),
            },
            NativeInstancePropertyDef {
                name: "brightnessRangeMin",
                get: Some(vo_brightness_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightnessRangeMax",
                get: Some(vo_brightness_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightnessDefaultValue",
                get: Some(vo_brightness_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightnessStepSize",
                get: Some(vo_brightness_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hue",
                get: Some(vo_hue_get),
                set: Some(vo_hue_set),
            },
            NativeInstancePropertyDef {
                name: "hueRangeMin",
                get: Some(vo_hue_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hueRangeMax",
                get: Some(vo_hue_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hueDefaultValue",
                get: Some(vo_hue_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hueStepSize",
                get: Some(vo_hue_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturation",
                get: Some(vo_saturation_get),
                set: Some(vo_saturation_set),
            },
            NativeInstancePropertyDef {
                name: "saturationRangeMin",
                get: Some(vo_saturation_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturationRangeMax",
                get: Some(vo_saturation_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturationDefaultValue",
                get: Some(vo_saturation_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturationStepSize",
                get: Some(vo_saturation_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "originalWidth",
                get: Some(vo_original_width_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "originalHeight",
                get: Some(vo_original_height_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfFrame",
                get: Some(vo_number_of_frame_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "fps",
                get: Some(vo_fps_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "totalTime",
                get: Some(vo_total_time_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfAudioStream",
                get: Some(vo_number_of_audio_stream_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfVideoStream",
                get: Some(vo_number_of_video_stream_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "layer1",
                get: Some(vo_layer1_get),
                set: Some(vo_layer1_set),
            },
            NativeInstancePropertyDef {
                name: "layer2",
                get: Some(vo_layer2_get),
                set: Some(vo_layer2_set),
            },
        ],
    })
}

// ---------------------------------------------------------------------------
// Unit tests (pure: no VM, so they are parallel-safe)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use engine::video::PlayStatus;
    use std::cell::RefCell;

    struct FakeSink {
        calls: RefCell<Vec<(String, Vec<TjsValue>)>>,
    }

    impl EventSink for FakeSink {
        fn call_member(
            &self,
            _objthis: *mut c_void,
            member: &str,
            args: &[TjsValue],
        ) -> Result<TjsValue, String> {
            self.calls
                .borrow_mut()
                .push((member.to_string(), args.to_vec()));
            Ok(TjsValue::Void)
        }
    }

    #[test]
    fn dispatch_maps_events_to_script_members() {
        let sink = FakeSink {
            calls: RefCell::new(Vec::new()),
        };
        let events = vec![
            VideoEvent::StatusChanged(PlayStatus::Play),
            VideoEvent::Period(engine::video::PeriodReason::SegmentLoop),
            VideoEvent::FrameUpdate(7),
            VideoEvent::TransitionComplete,
        ];
        dispatch_events_with(&sink, std::ptr::dangling_mut::<c_void>(), &events);
        let calls = sink.calls.borrow();
        assert_eq!(calls.len(), 3, "TransitionComplete has no script member");
        assert_eq!(
            calls[0],
            (
                "onStatusChanged".to_string(),
                vec![TjsValue::String("play".to_string())]
            )
        );
        assert_eq!(
            calls[1],
            ("onPeriod".to_string(), vec![TjsValue::Integer(3)])
        );
        assert_eq!(
            calls[2],
            ("onFrameUpdate".to_string(), vec![TjsValue::Integer(7)])
        );
    }

    #[test]
    fn buffered_source_selects_the_frame_at_a_position() {
        let frame = |pts_ms: u64| RgbaFrame::new(1, 1, pts_ms, vec![0; 4]);
        let mut source = BufferedSource {
            spec: VideoSpec {
                width: 1,
                height: 1,
                fps: 30.0,
                total_frames: Some(3),
                sample_rate: 0,
                channels: 0,
            },
            frames: vec![frame(0), frame(100), frame(200)],
        };
        assert_eq!(source.frame_at("x", 0).unwrap().pts_ms, 0);
        assert_eq!(source.frame_at("x", 150).unwrap().pts_ms, 100);
        assert_eq!(source.frame_at("x", 9999).unwrap().pts_ms, 200);
        assert!(
            BufferedSource {
                spec: source.spec,
                frames: Vec::new(),
            }
            .frame_at("x", 0)
            .is_none()
        );
    }
}
