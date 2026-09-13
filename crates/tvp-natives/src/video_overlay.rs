//! `VideoOverlay` native class — non-throwing stub of the reference
//! `tTJSNC_VideoOverlay` (`reference/cpp/core/visual/VideoOvlIntf.cpp` +
//! `VideoOvlImpl.{h,cpp}`).
//!
//! The reference `VideoOverlay` is a **standalone** native class (it does
//! *not* derive from `Layer`): it owns a rectangle/visibility/playback state
//! and exposes `layer1`/`layer2` properties plus video decoding. The game's
//! `MovieLayer` (`system/system_movie.tjs`) is a *script* class that extends
//! this native base and supplies its own `Layer`; the separate
//! `layerExMovie.dll` plugin (`reference/cpp/plugins/layerExMovie.cpp`)
//! derives from `Layer` instead and is a different, newer interface.
//!
//! krkr-rs does not decode video, so this stub mirrors the reference
//! interface with per-instance state for the members the game reads/writes:
//!
//! * `system/system_enveffect.tjs` (`EnvEffectFilter.start`) constructs
//!   `new VideoOverlay(win)`, sets `onStatusChanged`/`onPeriod`/`mode`/
//!   `layer1`/`loop`, calls `open(file)`, reads `originalWidth`/
//!   `originalHeight`, and calls `setSegmentLoop`/`cancelSegmentLoop`/
//!   `setPeriodEvent`/`cancelPeriodEvent`/`play`/`pause`/`stop`.
//! * `system/system_movie.tjs` (`MovieLayer`) extends it, calls
//!   `super.VideoOverlay(...)`, `open`, `play`, `stop`, and reads
//!   `numberOfAudioStream` / writes `audioVolume`.
//!
//! Every method is a no-op and every property returns a sensible default
//! (0/false/void) until the game writes it, so nothing ever throws and the
//! effect timer keeps running instead of being disabled.
//!
//! Omitted on purpose (unused by the game, and plain object members on a
//! native instance read as void / accept writes without throwing anyway):
//! the reference's `contrast`/`brightness`/`hue`/`saturation` families and
//! the event dispatcher methods (`onStatusChanged`, `onCallbackCommand`,
//! `onPeriod`, `onFrameUpdate`) — the game assigns its own callbacks to
//! those names, which shadow any native member.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine, Value,
};

use crate::{args, set_void_out, value_as_i64};

/// Per-object playback/rectangle state. All fields default to 0/false; the
/// loop fields default to their reference "unset" value of -1, so the
/// getters report an empty segment/period before the game sets one.
struct VideoOverlayInst {
    left: i64,
    top: i64,
    width: i64,
    height: i64,
    visible: bool,
    looping: bool,
    frame: i64,
    position: i64,
    mode: i64,
    play_rate: f64,
    period_event_frame: i64,
    segment_loop_start: i64,
    segment_loop_end: i64,
    audio_balance: i64,
    audio_volume: i64,
    enabled_audio_stream: i64,
    enabled_video_stream: i64,
    mixing_alpha: f64,
    mixing_bg: i64,
}

impl Default for VideoOverlayInst {
    fn default() -> Self {
        Self {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            visible: false,
            looping: false,
            frame: 0,
            position: 0,
            mode: 0,
            play_rate: 0.0,
            period_event_frame: -1,
            segment_loop_start: -1,
            segment_loop_end: -1,
            audio_balance: 0,
            audio_volume: 0,
            enabled_audio_stream: 0,
            enabled_video_stream: 0,
            mixing_alpha: 0.0,
            mixing_bg: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Instance property accessor generators
// ---------------------------------------------------------------------------

/// Read/write integer property backed by an `i64` field.
macro_rules! int_prop {
    ($getter:ident, $setter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            crate::set_int_out(out, inst.$field);
            0
        }

        extern "C" fn $setter(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is live; `value` is valid for the call.
            let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
            inst.$field = crate::value_as_i64(unsafe { &*value });
            0
        }
    };
}

/// Read/write boolean property backed by a `bool` field.
macro_rules! bool_prop {
    ($getter:ident, $setter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            crate::set_int_out(out, i64::from(inst.$field));
            0
        }

        extern "C" fn $setter(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is live; `value` is valid for the call.
            let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
            inst.$field = crate::value_as_bool(unsafe { &*value });
            0
        }
    };
}

/// Read/write real property backed by an `f64` field.
macro_rules! real_prop {
    ($getter:ident, $setter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            crate::set_real_out(out, inst.$field);
            0
        }

        extern "C" fn $setter(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is live; `value` is valid for the call.
            let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
            inst.$field = crate::value_as_f64(unsafe { &*value });
            0
        }
    };
}

/// Read-only integer property backed by an `i64` field.
macro_rules! ro_int_field {
    ($getter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            crate::set_int_out(out, inst.$field);
            0
        }
    };
}

/// Read-only integer property returning a constant.
macro_rules! ro_int_const {
    ($getter:ident, $value:expr) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            crate::set_int_out(out, $value);
            0
        }
    };
}

/// Read-only real property returning a constant.
macro_rules! ro_real_const {
    ($getter:ident, $value:expr) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            crate::set_real_out(out, $value);
            0
        }
    };
}

// -- rectangle / visibility -------------------------------------------------

int_prop!(left_get, left_set, left);
int_prop!(top_get, top_set, top);
int_prop!(width_get, width_set, width);
int_prop!(height_get, height_set, height);
bool_prop!(visible_get, visible_set, visible);

// -- playback state ---------------------------------------------------------

int_prop!(position_get, position_set, position);
int_prop!(frame_get, frame_set, frame);
bool_prop!(loop_get, loop_set, looping);
int_prop!(mode_get, mode_set, mode);
real_prop!(play_rate_get, play_rate_set, play_rate);
int_prop!(
    period_event_frame_get,
    period_event_frame_set,
    period_event_frame
);
ro_int_field!(segment_loop_start_get, segment_loop_start);
ro_int_field!(segment_loop_end_get, segment_loop_end);

// -- audio / video streams --------------------------------------------------

int_prop!(audio_balance_get, audio_balance_set, audio_balance);
int_prop!(audio_volume_get, audio_volume_set, audio_volume);
int_prop!(
    enabled_audio_stream_get,
    enabled_audio_stream_set,
    enabled_audio_stream
);
int_prop!(
    enabled_video_stream_get,
    enabled_video_stream_set,
    enabled_video_stream
);
real_prop!(mixing_alpha_get, mixing_alpha_set, mixing_alpha);
int_prop!(mixing_bg_get, mixing_bg_set, mixing_bg);

// -- read-only media info ---------------------------------------------------

ro_int_const!(original_width_get, 0);
ro_int_const!(original_height_get, 0);
ro_int_const!(total_frame_get, 0);
ro_real_const!(fps_get, 0.0);
ro_int_const!(number_of_frame_get, 0);
ro_int_const!(total_time_get, 0);
ro_int_const!(number_of_audio_stream_get, 0);
ro_int_const!(number_of_video_stream_get, 0);

// ---------------------------------------------------------------------------
// Instance methods
// ---------------------------------------------------------------------------

extern "C" fn vo_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(VideoOverlayInst::default())) as *mut c_void
}

extern "C" fn vo_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from vo_create's Box::into_raw.
        drop(unsafe { Box::from_raw(instance as *mut VideoOverlayInst) });
    }
}

/// `new VideoOverlay(win)` / `super.VideoOverlay(...)`: accept any argument
/// list (the reference requires a Window, but the stub has no window state).
extern "C" fn vo_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// Shared body for every argument-less no-op method (`open`, `play`, ...).
extern "C" fn vo_noop_method(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `setPos(left, top)`
extern "C" fn vo_set_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let a = args(argv, argc);
    inst.left = a.first().map(value_as_i64).unwrap_or(0);
    inst.top = a.get(1).map(value_as_i64).unwrap_or(0);
    set_void_out(out);
    0
}

/// `setSize(width, height)`
extern "C" fn vo_set_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let a = args(argv, argc);
    inst.width = a.first().map(value_as_i64).unwrap_or(0);
    inst.height = a.get(1).map(value_as_i64).unwrap_or(0);
    set_void_out(out);
    0
}

/// `setBounds(left, top, width, height)`
extern "C" fn vo_set_bounds(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let a = args(argv, argc);
    inst.left = a.first().map(value_as_i64).unwrap_or(0);
    inst.top = a.get(1).map(value_as_i64).unwrap_or(0);
    inst.width = a.get(2).map(value_as_i64).unwrap_or(0);
    inst.height = a.get(3).map(value_as_i64).unwrap_or(0);
    set_void_out(out);
    0
}

/// `setSegmentLoop(startFrame, endFrame)`
extern "C" fn vo_set_segment_loop(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let a = args(argv, argc);
    inst.segment_loop_start = a.first().map(value_as_i64).unwrap_or(-1);
    inst.segment_loop_end = a.get(1).map(value_as_i64).unwrap_or(-1);
    set_void_out(out);
    0
}

/// `cancelSegmentLoop()`
extern "C" fn vo_cancel_segment_loop(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    inst.segment_loop_start = -1;
    inst.segment_loop_end = -1;
    set_void_out(out);
    0
}

/// `setPeriodEvent(frame)` (no argument clears it, like the reference).
extern "C" fn vo_set_period_event(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let a = args(argv, argc);
    inst.period_event_frame = a.first().map(value_as_i64).unwrap_or(-1);
    set_void_out(out);
    0
}

/// `cancelPeriodEvent()`
extern "C" fn vo_cancel_period_event(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    inst.period_event_frame = -1;
    set_void_out(out);
    0
}

/// `selectAudioStream(n)`
extern "C" fn vo_select_audio_stream(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    inst.enabled_audio_stream = args(argv, argc).first().map(value_as_i64).unwrap_or(0);
    set_void_out(out);
    0
}

/// `layer1` getter: no Layer is attached, so read as void (reference returns
/// the layer or null).
extern "C" fn vo_layer_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `layer1`/`layer2` setter: accept any value and ignore it.
extern "C" fn vo_layer_set(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `VideoOverlay` native class on the engine's global object.
pub fn register_video_overlay(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "VideoOverlay",
        create: vo_create,
        destroy: vo_destroy,
        methods: vec![
            NativeInstanceMethodDef {
                name: "VideoOverlay", // class-name ctor hook
                f: vo_ctor,
            },
            NativeInstanceMethodDef {
                name: "open",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "close",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "play",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "stop",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "pause",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "rewind",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "prepare",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "setTransitionCompleteCall",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "setMixingLayer",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "resetMixingLayer",
                f: vo_noop_method,
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
                name: "selectAudioStream",
                f: vo_select_audio_stream,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "left",
                get: Some(left_get),
                set: Some(left_set),
            },
            NativeInstancePropertyDef {
                name: "top",
                get: Some(top_get),
                set: Some(top_set),
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(width_get),
                set: Some(width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(height_get),
                set: Some(height_set),
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(visible_get),
                set: Some(visible_set),
            },
            NativeInstancePropertyDef {
                name: "position",
                get: Some(position_get),
                set: Some(position_set),
            },
            NativeInstancePropertyDef {
                name: "frame",
                get: Some(frame_get),
                set: Some(frame_set),
            },
            NativeInstancePropertyDef {
                name: "loop",
                get: Some(loop_get),
                set: Some(loop_set),
            },
            NativeInstancePropertyDef {
                name: "mode",
                get: Some(mode_get),
                set: Some(mode_set),
            },
            NativeInstancePropertyDef {
                name: "playRate",
                get: Some(play_rate_get),
                set: Some(play_rate_set),
            },
            NativeInstancePropertyDef {
                name: "periodEventFrame",
                get: Some(period_event_frame_get),
                set: Some(period_event_frame_set),
            },
            NativeInstancePropertyDef {
                name: "segmentLoopStartFrame",
                get: Some(segment_loop_start_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "segmentLoopEndFrame",
                get: Some(segment_loop_end_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "audioBalance",
                get: Some(audio_balance_get),
                set: Some(audio_balance_set),
            },
            NativeInstancePropertyDef {
                name: "audioVolume",
                get: Some(audio_volume_get),
                set: Some(audio_volume_set),
            },
            NativeInstancePropertyDef {
                name: "enabledAudioStream",
                get: Some(enabled_audio_stream_get),
                set: Some(enabled_audio_stream_set),
            },
            NativeInstancePropertyDef {
                name: "enabledVideoStream",
                get: Some(enabled_video_stream_get),
                set: Some(enabled_video_stream_set),
            },
            NativeInstancePropertyDef {
                name: "mixingMovieAlpha",
                get: Some(mixing_alpha_get),
                set: Some(mixing_alpha_set),
            },
            NativeInstancePropertyDef {
                name: "mixingMovieBGColor",
                get: Some(mixing_bg_get),
                set: Some(mixing_bg_set),
            },
            NativeInstancePropertyDef {
                name: "originalWidth",
                get: Some(original_width_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "originalHeight",
                get: Some(original_height_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "totalFrame",
                get: Some(total_frame_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "fps",
                get: Some(fps_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfFrame",
                get: Some(number_of_frame_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "totalTime",
                get: Some(total_time_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfAudioStream",
                get: Some(number_of_audio_stream_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfVideoStream",
                get: Some(number_of_video_stream_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "layer1",
                get: Some(vo_layer_get),
                set: Some(vo_layer_set),
            },
            NativeInstancePropertyDef {
                name: "layer2",
                get: Some(vo_layer_get),
                set: Some(vo_layer_set),
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use crate::test_lock::vm_lock;

    use super::*;

    /// The members `EnvEffectFilter` (system_enveffect.tjs) calls on its
    /// `_player`, plus the constructor. None may throw.
    #[test]
    fn video_overlay_stub_members_do_not_throw() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        // register_all wires VideoOverlay in and publishes the TVP globals
        // (`vomLayer`) the game's EnvEffectFilter uses.
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                r#"
                var v = new VideoOverlay(null);
                v.open("watch_long.mpg");
                var w = v.originalWidth;
                var h = v.originalHeight;
                var total = v.totalFrame;
                v.play();
                v.pause();
                v.stop();
                v.setTransitionCompleteCall(null);
                v.setPos(1, 2);
                v.setSize(3, 4);
                v.setBounds(5, 6, 7, 8);
                v.setSegmentLoop(0, 10);
                v.cancelSegmentLoop();
                v.setPeriodEvent(3);
                v.cancelPeriodEvent();
                v.mode = vomLayer;
                v.layer1 = null;
                v.layer2 = null;
                v.loop = true;
                v.frame = 12;
                v.visible = true;
                v.position = 42;
                v.audioVolume = 100;
                v.enabledAudioStream = 0;
                "#,
                "video_overlay_test",
            )
            .expect("stub members must not throw");
        assert_eq!(
            engine.eval("w", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0)
        );
        assert_eq!(
            engine.eval("h", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0)
        );
        assert_eq!(
            engine.eval("total", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0)
        );
        // Writes round-trip through the instance state.
        assert_eq!(
            engine.eval("v.frame", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(12)
        );
        assert_eq!(
            engine.eval("v.loop", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(1)
        );
        assert_eq!(
            engine.eval("v.visible", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(1)
        );
        assert_eq!(
            engine.eval("v.width", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(7)
        );
        assert_eq!(
            engine.eval("v.height", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(8)
        );
        assert_eq!(
            engine.eval("v.segmentLoopStartFrame", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(-1)
        );
        assert_eq!(
            engine.eval("v.mode === vomLayer", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(1)
        );
    }

    /// `MovieLayer` (system_movie.tjs) is a script class that extends the
    /// native base; its members must resolve through to the stub.
    #[test]
    fn script_subclass_inherits_video_overlay_members() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        register_video_overlay(&engine).expect("register VideoOverlay");
        engine
            .exec_script(
                r#"
                class TestMovie extends VideoOverlay {
                    function TestMovie(win) {
                        super.VideoOverlay(...);
                    }
                    function go() {
                        open("y.mpg");
                        play();
                        pause = true;
                        play();
                        return originalWidth;
                    }
                }
                var movie = new TestMovie(null);
                var r = movie.go();
                "#,
                "video_overlay_test",
            )
            .expect("script subclass must inherit the stub");
        assert_eq!(
            engine.eval("r", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0)
        );
    }
}
