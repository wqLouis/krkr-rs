//! `Layer` transitions (group 3): `beginTransition` / `stopTransition`.
//!
//! Reference: the native `beginTransition`/`stopTransition` members and
//! `tTJSNI_BaseLayer::StartTransition` / `InternalStopTransition`
//! (`reference/cpp/core/visual/LayerIntf.cpp:7770-8058`, `:9815-9858`) and
//! the L2 [`engine::trans`] state machine they drive. The transition state
//! is stored per layer in [`LayerExtras`](super::LayerExtras);
//! [`advance_transitions`] is the frame step the frame body calls (it
//! advances the machine with the L1 [`platform::Clock`], stops transitions
//! the reference stops per tick, and fires `onTransitionCompleted`).
//!
//! `onTransitionCompleted` itself is a no-op assignable handler (see
//! [`super::events`]); [`advance_transitions`] / [`end_transition`]
//! dispatch it by name on the destination layer object, so a script
//! subclass override wins — the reference fires it as a synchronous event
//! at the destination's owner (`LayerIntf.cpp:8000-8015`).

use std::ffi::{c_char, c_int, c_void};

use engine::Scene;
use engine::trans::{RuleMask, ScrollFrom, ScrollStay, Trans, TransKind};
use tjs::{NativeInstanceMethodDef, TjsValue, VAL_OBJECT, Value};

use super::super::bitmap::{decode_image, read_image_from_storage};
use super::super::common::{
    ObjectKind, VisualState, duplicate_object, resolve_object_id, scene_mut, scene_ref,
};
use super::{LayerExtras, LayerInst, LayerTransition, bound};
use crate::native_util::{args, report_error, set_void, value_as_string};

/// `beginTransition(name [, withchildren [, transwith [, options]]])`.
///
/// Mirrors the native member (`LayerIntf.cpp:9815-9848`) and
/// `StartTransition` (`LayerIntf.cpp:7770`), in the reference's order of
/// error precedence:
///
/// 1. the transition source is required and must be a `Layer`
///    (`TVPSpecifyLayer`, `LayerIntf.cpp:9828-9841`);
/// 2. a layer already inside a transition is rejected
///    (`TVPCurrentTransitionMustBeStopping`, `LayerIntf.cpp:7776-7778`);
/// 3. a source that is itself transitioning *from this* layer is rejected
///    (`TVPTransitionMutualSource`, `LayerIntf.cpp:7779-7781`);
/// 4. the handler provider parses and validates its options while the
///    script is inside the call (`wave.cpp:327-351`,
///    `ripple.cpp:1574-1627`, `TransIntf.cpp:512-516`), and rejects
///    mismatched source sizes (`wave.cpp:319-320`,
///    `TransIntf.cpp:493-497`).
///
/// `withchildren` (default `true`, `LayerIntf.cpp:9819-9821`) selects the
/// whole-layer tree in the reference; the port transitions main images
/// only and accepts the argument without acting on it.
extern "C" fn layer_begin_transition(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.beginTransition requires a name");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.beginTransition: this layer is not constructed",
        );
    };
    let Ok((host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.beginTransition: compat context is not bound",
        );
    };
    let result: Result<(), String> = (|| {
        let name = value_as_string(&a[0]);
        // The transition source is required by the reference
        // (`TVPSpecifyLayer`, LayerIntf.cpp:9828-9841).
        let source = match a.get(2) {
            Some(value) if value.ty == VAL_OBJECT => {
                // The object → native-instance lookup (`Layer` only); an
                // object of another class or an unresolvable one is
                // `TVPSpecifyLayer` (`LayerIntf.cpp:9828-9841`). A resolution
                // error is this same rejection, not a raw member error.
                let id = resolve_object_id(host.engine(), value, &["Layer"])
                    .ok()
                    .filter(|&id| id >= 0)
                    .ok_or_else(|| "Layer.beginTransition: transwith is not a Layer".to_string())?;
                id as u32
            }
            _ => return Err("Layer.beginTransition: transwith is required".into()),
        };
        // `StartTransition` rejects a second transition on the same layer
        // up front (LayerIntf.cpp:7776-7778), then the mutual-source cycle
        // (LayerIntf.cpp:7779-7781).
        // SAFETY: the caller resolved `ctx` from the installed binding.
        {
            let extras = unsafe { ctx.services() }.state::<LayerExtras>();
            if extras.transitions.contains_key(&id) {
                return Err(
                    "Layer.beginTransition: a transition is already running on this layer \
                     (reference: TVPCurrentTransitionMustBeStopping)"
                        .into(),
                );
            }
            if extras
                .transitions
                .get(&source)
                .is_some_and(|entry| entry.source == id)
            {
                return Err(
                    "Layer.beginTransition: the transition source is already transitioning \
                     from this layer (reference: TVPTransitionMutualSource)"
                        .into(),
                );
            }
        }
        // The destination must still exist; its main image size is the
        // reference's `src1` (`LayerIntf.cpp:7827-7831`) and seeds the
        // size-dependent option defaults/checks below.
        let scene = scene_ref(&ctx)?;
        if scene.layer(id).is_none() {
            return Err("Layer.beginTransition: this layer no longer exists".into());
        }
        let dest_size = scene
            .layer(id)
            .and_then(|layer| layer.bitmap)
            .and_then(|bitmap| scene.bitmap(bitmap))
            .map(|bitmap| (bitmap.width, bitmap.height));
        // The provider's up-front `src1`/`src2` size check runs before it
        // parses options (`wave.cpp:319-320`, `TransIntf.cpp:493-497`); a
        // source with no main image is caught after the provider
        // (`LayerIntf.cpp:7860-7868`).
        let source_size = scene
            .layer(source)
            .and_then(|layer| layer.bitmap)
            .and_then(|bitmap| scene.bitmap(bitmap))
            .map(|bitmap| (bitmap.width, bitmap.height));
        if let (Some(dest_size), Some(source_size)) = (dest_size, source_size)
            && dest_size != source_size
        {
            return Err(format!(
                "Layer.beginTransition: transition source size mismatch: destination \
                 {}x{}, source {}x{}",
                dest_size.0, dest_size.1, source_size.0, source_size.1
            ));
        }
        let duration = options_duration(host.engine(), a.get(3)).unwrap_or(300.0);
        let trans = build_transition(&ctx, host.engine(), &name, duration, a.get(3), dest_size)?;
        // The source must exist and have a main image (it is composited
        // into this layer during the transition).
        {
            let source_layer = scene
                .layer(source)
                .ok_or_else(|| "Layer.beginTransition: transwith is not a Layer".to_string())?;
            if source_layer.bitmap.is_none() {
                return Err(
                    "Layer.beginTransition: the transition source has no main image".into(),
                );
            }
        }
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let mut extras = unsafe { ctx.services() }.state::<LayerExtras>();
        extras
            .transitions
            .insert(id, LayerTransition { trans, source });
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `stopTransition()`.
///
/// The reference's `StopTransition` runs the full `InternalStopTransition`
/// teardown even for a manually stopped transition
/// (`LayerIntf.cpp:8033-8036`, `:7950-8031`): the `ttExchange` layer
/// exchange happens and `onTransitionCompleted` is fired synchronously —
/// this forwards to [`end_transition`], which does both. The event is
/// gated by the source being alive (`transsrcalive`, `:7997-8004`), so a
/// transition whose source was invalidated completes silently.
extern "C" fn layer_stop_transition(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.stopTransition: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.stopTransition: compat context is not bound",
        );
    };
    let source = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<LayerExtras>()
            .transitions
            .get(&id)
            .map(|entry| entry.source)
    };
    match source {
        Some(source) => {
            // `InternalStopTransition`'s `transsrcalive` gate
            // (LayerIntf.cpp:7997-8004): a source that has been invalidated
            // suppresses the event but still runs the exchange.
            let event = object_alive(&ctx, ObjectKind::Layer, source);
            end_transition(&ctx, id, source, event);
        }
        // `InternalStopTransition` only acts while `InTransition`
        // (LayerIntf.cpp:7950): stopping a layer without a transition is a
        // silent no-op.
        None => log::debug!("Layer.stopTransition: layer {id} had no active transition"),
    }
    set_void(out);
    0
}

/// Build the L2 transition for a script name.
///
/// The names are the engine's own providers (`crossfade`, `scroll`,
/// `universal`, `mask`) plus the seven `extrans.dll` providers, matched the
/// way the reference's provider table is looked up: an exact, case-sensitive
/// `ttstr` hash key with no silent fallback in this port
/// (`TVPFindTransHandlerProvider`, `reference/cpp/core/visual/TransIntf.cpp:296-299,331-341`
/// — the reference falls back to `crossfade` with a message box on a miss;
/// a typed error here).
///
/// Defaults are the reference's (`wave.cpp:327-331`, `mosaic.cpp:436-445`,
/// `turn.cpp:555-563`, `rotatetrans.cpp:183-193,284-291,453-465`,
/// `ripple.cpp:1574-1627`); every option is read through
/// [`options_number`] / [`options_text`] and validated inside L2, so an
/// out-of-range value is a typed error rather than a silent clamp.
///
/// `dest_size` is the destination main-image size — the reference's `src1`
/// (`LayerIntf.cpp:7827-7831`) — used where a provider validates an option
/// against the image (`ripple.cpp:1598-1627`).
fn build_transition(
    ctx: &crate::CompatCtx,
    host: &tjs::Tjs2Engine,
    name: &str,
    duration: f64,
    options: Option<&Value>,
    dest_size: Option<(u32, u32)>,
) -> Result<Trans, String> {
    match name {
        "crossfade" | "fade" => Trans::cross_fade(duration).map_err(|error| error.to_string()),
        "scroll" | "slide" | "push" => {
            let from = scroll_from(host, options)?;
            let stay = scroll_stay(host, options)?;
            Trans::scroll(duration, from, stay).map_err(|error| error.to_string())
        }
        "universal" => {
            // `tTVPUniversalTransHandlerProvider`: `vague` (default 64) and a
            // required `rule` image (`TransIntf.cpp:806-817`).
            let vague = option_int(host, options, &["vague"]).unwrap_or(64);
            let rule = rule_mask(ctx, host, options)?;
            Trans::universal(duration, vague, rule).map_err(|error| error.to_string())
        }
        "mask" => {
            // The reference's `universal` with a hard threshold.
            let rule = rule_mask(ctx, host, options)?;
            Trans::mask(duration, rule).map_err(|error| error.to_string())
        }
        "wave" => Trans::new(
            TransKind::Wave {
                max_height: option_int(host, options, &["maxh"]).unwrap_or(50),
                max_omega: option_float(host, options, &["maxomega"]).unwrap_or(0.2),
                background1: option_color(host, options, &["bgcolor1"]).unwrap_or(0),
                background2: option_color(host, options, &["bgcolor2"]).unwrap_or(0),
                wave_type: option_int(host, options, &["wavetype"]).unwrap_or(0),
            },
            duration,
        )
        .map_err(|error| error.to_string()),
        "mosaic" => Trans::new(
            TransKind::Mosaic {
                max_size: option_int(host, options, &["maxsize"]).unwrap_or(30),
            },
            duration,
        )
        .map_err(|error| error.to_string()),
        "turn" => Trans::new(
            TransKind::Turn {
                background: option_color(host, options, &["bgcolor"]).unwrap_or(0),
            },
            duration,
        )
        .map_err(|error| error.to_string()),
        "rotatezoom" => Trans::new(
            TransKind::RotateZoom {
                factor: option_float(host, options, &["factor"]).unwrap_or(1.0),
                accel: option_float(host, options, &["accel"]).unwrap_or(0.0),
                twist: option_float(host, options, &["twist"]).unwrap_or(2.0),
                twist_accel: option_float(host, options, &["twistaccel"]).unwrap_or(-2.0),
                center: rotation_center(host, options, dest_size)?,
            },
            duration,
        )
        .map_err(|error| error.to_string()),
        "rotatevanish" => Trans::new(
            TransKind::RotateVanish {
                accel: option_float(host, options, &["accel"]).unwrap_or(2.0),
                twist: option_float(host, options, &["twist"]).unwrap_or(2.0),
                twist_accel: option_float(host, options, &["twistaccel"]).unwrap_or(2.0),
                center: rotation_center(host, options, dest_size)?,
            },
            duration,
        )
        .map_err(|error| error.to_string()),
        "rotateswap" => {
            // The reference's `rotateswap` provider reads only `time`,
            // `bgcolor` and `twist` (`rotatetrans.cpp:453-465`) — it has no
            // `centerx`/`centery` option, so a supplied centre is dropped
            // here instead of moving the pivot.
            Trans::new(
                TransKind::RotateSwap {
                    background: option_color(host, options, &["bgcolor"]).unwrap_or(0),
                    twist: option_float(host, options, &["twist"]).unwrap_or(1.0),
                    center: None,
                },
                duration,
            )
            .map_err(|error| error.to_string())
        }
        "ripple" => {
            let center = rotation_center(host, options, dest_size)?;
            // `ripple` validates its options in `StartTransition` in this
            // order (`ripple.cpp:1596-1627`): centre inside the image, then
            // `rwidth`, `roundness` and the `maxdrift` range (all three
            // inside L2's construction checks), then `maxdrift` below both
            // image dimensions. The size-dependent halves run here so they
            // surface during `beginTransition`, like the reference.
            if let Some((width, height)) = dest_size {
                let (center_x, center_y) =
                    center.unwrap_or(((width >> 1) as i32, (height >> 1) as i32));
                if center_x < 0
                    || center_y < 0
                    || center_x as u32 >= width
                    || center_y as u32 >= height
                {
                    return Err(
                        "invalid transition option centerx: centerx and centery must be inside \
                         the source image"
                            .to_string(),
                    );
                }
            }
            let max_drift = option_int(host, options, &["maxdrift"]).unwrap_or(24);
            let trans = Trans::new(
                TransKind::Ripple {
                    center,
                    map_width: option_int(host, options, &["rwidth"]).unwrap_or(128),
                    roundness: option_float(host, options, &["roundness"]).unwrap_or(1.0) as f32,
                    speed: option_float(host, options, &["speed"]).unwrap_or(6.0) as f32,
                    max_drift,
                },
                duration,
            )
            .map_err(|error| error.to_string())?;
            // `Trans::new` already rejected a `maxdrift` outside `0..=127`,
            // so the remaining failure is the size bound.
            if let Some((width, height)) = dest_size
                && ((max_drift as u32) >= width || (max_drift as u32) >= height)
            {
                return Err(
                    "invalid transition option maxdrift: maxdrift must be lesser than both image \
                     width and height"
                        .to_string(),
                );
            }
            Ok(trans)
        }
        other => Err(format!(
            "Layer.beginTransition: unsupported transition {other:?} (supported: crossfade, \
             scroll, universal, mask, wave, mosaic, turn, rotatezoom, rotatevanish, \
             rotateswap, ripple)"
        )),
    }
}

/// Load the `rule` option as an 8-bit rule graphic.
///
/// Reference: `tTVPUniversalTransHandlerProvider::StartTransition` →
/// `imagepro->LoadImage(rulename, 8, …)` (`TransIntf.cpp:812-821`), which
/// decodes the graphic in `glmGrayscale` and tiles it when it is smaller
/// than the source (`GraphicsLoaderIntf.cpp:1689-1696`). L3 reads it
/// through the same storage + image-decoder pair `Layer.loadImages` uses,
/// then hands the grayscale to L2 as a [`RuleMask`].
fn rule_mask(
    ctx: &crate::CompatCtx,
    host: &tjs::Tjs2Engine,
    options: Option<&Value>,
) -> Result<RuleMask, String> {
    let name = options_text(host, options, "rule").ok_or_else(|| {
        "Layer.beginTransition: this transition requires a `rule` image \
         (reference: TVPSpecifyOption)"
            .to_string()
    })?;
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let storage = services
        .storage()
        .ok_or_else(|| "Layer.beginTransition: no storage is attached".to_string())?;
    // SAFETY: the storage pointer is the host's live L2 `Storage`, attached
    // for the lifetime of the binding.
    let (resolved, bytes) = read_image_from_storage(unsafe { &*storage }, &name)?;
    let image = decode_image(services, &resolved, &bytes)?;
    RuleMask::from_rgba(image.width, image.height, &image.rgba).map_err(|error| error.to_string())
}

/// An integer option (`tjs_int` in the reference).
fn option_int(host: &tjs::Tjs2Engine, options: Option<&Value>, names: &[&str]) -> Option<i32> {
    options_number(host, options, names).map(|value| value as i32)
}

/// A real option (`double` in the reference).
fn option_float(host: &tjs::Tjs2Engine, options: Option<&Value>, names: &[&str]) -> Option<f64> {
    options_number(host, options, names)
}

/// A colour option (`tjs_uint32` in the reference, `0xAARRGGBB`).
fn option_color(host: &tjs::Tjs2Engine, options: Option<&Value>, names: &[&str]) -> Option<i64> {
    options_number(host, options, names).map(|value| value as i64)
}

/// The `centerx`/`centery` pair shared by the rotate and ripple providers.
///
/// The reference reads the two options independently and defaults each to
/// half the source-1 (destination) size (`ripple.cpp:1574,1593-1596`,
/// `rotatetrans.cpp:186-187,206-209`), so supplying one without the other
/// is legal here too: the missing side takes the reference default from
/// `dest_size`. Returning `None` leaves the default to L2, which computes
/// the same image centre.
fn rotation_center(
    host: &tjs::Tjs2Engine,
    options: Option<&Value>,
    dest_size: Option<(u32, u32)>,
) -> Result<Option<(i32, i32)>, String> {
    let x = option_int(host, options, &["centerx"]);
    let y = option_int(host, options, &["centery"]);
    let missing = || {
        "Layer.beginTransition: centerx/centery needs a destination image to default the other \
         side"
            .to_string()
    };
    match (x, y) {
        (None, None) => Ok(None),
        (Some(x), Some(y)) => Ok(Some((x, y))),
        (Some(x), None) => dest_size
            .map(|(_, height)| Some((x, (height / 2) as i32)))
            .ok_or_else(missing),
        (None, Some(y)) => dest_size
            .map(|(width, _)| Some(((width / 2) as i32, y)))
            .ok_or_else(missing),
    }
}

/// The `from` option of `scroll`: a number (`tTVPScrollTransFrom`,
/// `TransIntf.h:118`) or the port's string alias.
fn scroll_from(host: &tjs::Tjs2Engine, options: Option<&Value>) -> Result<ScrollFrom, String> {
    match options_number(host, options, &["from"]) {
        // The reference casts the number straight to the enum
        // (`TransIntf.cpp:1097-1102`); an out-of-range value has no
        // reference meaning, so it is a typed error here.
        Some(value) => match value as i32 {
            0 => Ok(ScrollFrom::Left),
            1 => Ok(ScrollFrom::Top),
            2 => Ok(ScrollFrom::Right),
            3 => Ok(ScrollFrom::Bottom),
            other => Err(format!(
                "Layer.beginTransition: invalid transition option from: {other} is not a \
                 tTVPScrollTransFrom value (0..3)"
            )),
        },
        // Compat extension: a string direction (the reference only reads a
        // number here; a non-numeric string converts to 0 = `sttLeft`).
        None => Ok(options_text(host, options, "from")
            .map(|value| scroll_from_text(&value))
            .unwrap_or(ScrollFrom::Left)),
    }
}

/// The `stay` option of `scroll`: a number (`tTVPScrollTransStay`,
/// `TransIntf.h:119`) or the port's string alias.
fn scroll_stay(host: &tjs::Tjs2Engine, options: Option<&Value>) -> Result<ScrollStay, String> {
    match options_number(host, options, &["stay"]) {
        // Numeric cast like [`scroll_from`] (`TransIntf.cpp:1104-1108`).
        Some(value) => match value as i32 {
            0 => Ok(ScrollStay::NoStay),
            1 => Ok(ScrollStay::StayDest),
            2 => Ok(ScrollStay::StaySrc),
            other => Err(format!(
                "Layer.beginTransition: invalid transition option stay: {other} is not a \
                 tTVPScrollTransStay value (0..2)"
            )),
        },
        None => Ok(options_text(host, options, "stay")
            .map(|value| scroll_stay_text(&value))
            .unwrap_or(ScrollStay::NoStay)),
    }
}

fn scroll_from_text(value: &str) -> ScrollFrom {
    match value.to_ascii_lowercase().as_str() {
        "top" | "up" => ScrollFrom::Top,
        "right" => ScrollFrom::Right,
        "bottom" | "down" => ScrollFrom::Bottom,
        _ => ScrollFrom::Left,
    }
}

fn scroll_stay_text(value: &str) -> ScrollStay {
    match value.to_ascii_lowercase().as_str() {
        "dest" | "destination" => ScrollStay::StayDest,
        "src" | "source" => ScrollStay::StaySrc,
        _ => ScrollStay::NoStay,
    }
}

/// Read the real-valued duration member (`duration`, `time` or `ms`).
///
/// The reference reads only `time`, casts it to `tjs_int64` and raises
/// values below 2 to 2 (`reference/cpp/plugins/wave.cpp:332-336`,
/// `reference/cpp/core/visual/TransIntf.cpp:512-516`); an absent option is
/// a `TVPSpecifyOption` error there. This port keeps a compat default of
/// 300 ms when the option is absent (several scripts omit it) but follows
/// the reference clamp when it is present, so a short or zero time is a 2 ms
/// transition and never a silent fallback to the default.
fn options_duration(host: &tjs::Tjs2Engine, options: Option<&Value>) -> Option<f64> {
    let value = options_number(host, options, &["duration", "time", "ms"])?;
    // Non-finite values have no defined `tjs_int64` cast in the reference;
    // they pass through to L2, which rejects them as a typed
    // `InvalidDuration` instead of clamping.
    if !value.is_finite() {
        return Some(value);
    }
    // The reference's `(tjs_int64)` cast truncates toward zero before the
    // `< 2` raise, so a fractional time runs for its whole-millisecond part.
    Some(value.trunc().max(2.0))
}

fn options_number(host: &tjs::Tjs2Engine, options: Option<&Value>, names: &[&str]) -> Option<f64> {
    let value = options?;
    if value.ty != VAL_OBJECT {
        return None;
    }
    let object = host.retain_object_arg(value).ok()?;
    for name in names {
        match host.get_member(object.raw_id(), name) {
            Ok(TjsValue::Real(number)) => return Some(number),
            Ok(TjsValue::Integer(number)) => return Some(number as f64),
            _ => {}
        }
    }
    None
}

fn options_text(host: &tjs::Tjs2Engine, options: Option<&Value>, name: &str) -> Option<String> {
    let value = options?;
    if value.ty != VAL_OBJECT {
        return None;
    }
    let object = host.retain_object_arg(value).ok()?;
    match host.get_member(object.raw_id(), name) {
        Ok(TjsValue::String(text)) => Some(text),
        _ => None,
    }
}

/// Whether a script object of `kind` is still alive (registered and not
/// invalidated).
///
/// The registry entry outlives `invalidate obj;` — the `Layer`/`Window`
/// classes release it only at destroy — so the registration alone does not
/// prove the object is usable. The VM's native-instance lookup returns the
/// payload only while the instance is valid and is empty once it has been
/// invalidated (`GetNativePtr`, `tjs2_abi.cpp`), which is the reference's
/// `tTJSNI_BaseLayer::Shutdown` flag (`LayerIntf.cpp:517-521`) that gates
/// both the event and the peer teardown in `InternalStopTransition`
/// (`:7997-8004`).
fn object_alive(ctx: &crate::CompatCtx, kind: ObjectKind, id: u32) -> bool {
    let (raw, class) = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let state = unsafe { ctx.services() }.state::<VisualState>();
        let object = match kind {
            ObjectKind::Layer => state.objects.layers.get(&id),
            ObjectKind::Window => state.objects.windows.get(&id),
        };
        let class = match kind {
            ObjectKind::Layer => "Layer",
            ObjectKind::Window => "Window",
        };
        match object {
            Some(object) => (object.raw_id(), class),
            None => return false,
        }
    };
    // SAFETY: `ctx` was resolved from the installed binding, so the engine is
    // live; `raw` is a retained id the registry keeps alive across this call.
    unsafe { ctx.engine() }
        .native_instance_payload(raw, class)
        .is_ok_and(|payload| payload.is_some())
}

/// Advance every active transition by `dt_ms` and tear down the finished or
/// stopped ones.
///
/// Each step renders one frame through [`Trans::apply`] — the reference's
/// `iTVPDivisibleTransHandler::Process` writing into the destination layer —
/// so a running transition is visible frame by frame, and the frame it
/// completes on leaves the destination holding source 2 verbatim (the
/// reference's `Phase == PhaseMax` short-circuit,
/// `reference/cpp/core/visual/TransIntf.cpp:596-601`).
///
/// Like the reference's per-tick `InvokeTransition`
/// (`LayerIntf.cpp:8047-8061`), a transition whose destination node is no
/// longer visible is stopped at that frame without rendering it
/// (`:8056-8060`), and like its `Invalidate` (`:517-521`) a transition
/// whose destination or source script object has died is torn down without
/// the completion event (`:7997-8004`). The frame body calls this from its
/// `project_and_render` step; the completion handler
/// (`onTransitionCompleted`) is dispatched here through the object member
/// so a script subclass override runs (reference `LayerIntf.cpp:8000-8015`
/// fires the event synchronously on the layer with `(dest, src)`).
///
/// It first runs the frame's `onPaint` dispatch ([`super::paint_poll`]),
/// mirroring the reference's `tTJSNI_BaseLayer::BeforeCompletion`
/// (`LayerIntf.cpp:6390-6404`), which fires the `onPaint` event and then
/// processes the transition in the same pre-draw step.
pub fn advance_transitions(ctx: &crate::CompatCtx, dt_ms: f64) -> Result<(), String> {
    super::paint_poll(ctx)?;
    let active: Vec<(u32, u32)> = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        services
            .state::<LayerExtras>()
            .transitions
            .iter()
            .map(|(&id, entry)| (id, entry.source))
            .collect()
    };
    // `(destination, source, fire_event)`.
    let mut ended: Vec<(u32, u32, bool)> = Vec::new();
    for (id, source) in active {
        // A dead endpoint tears the transition down without the event: both
        // the destination's `Owner` and the source's `Shutdown` gate the
        // dispatch (LayerIntf.cpp:7997-8004).
        if !object_alive(ctx, ObjectKind::Layer, id)
            || !object_alive(ctx, ObjectKind::Layer, source)
        {
            ended.push((id, source, false));
            continue;
        }
        // An invisible destination stops at this frame, before rendering
        // (LayerIntf.cpp:8056-8060).
        if !scene_ref(ctx)?.node_visible(id) {
            ended.push((id, source, true));
            continue;
        }
        let done = {
            // SAFETY: see above.
            let services = unsafe { ctx.services() };
            let mut extras = services.state::<LayerExtras>();
            let Some(entry) = extras.transitions.get_mut(&id) else {
                continue;
            };
            entry.trans.step(dt_ms);
            let done = entry.trans.is_complete();
            // Render this frame through the borrowed machine — the same
            // `Process` write into the destination the reference performs
            // per tick; no per-frame clone of the transition (or its rule
            // image) is needed.
            composite_transition(ctx, id, source, &entry.trans)?;
            done
        };
        if done {
            ended.push((id, source, true));
        }
    }
    for (id, source, event) in ended {
        // The teardown removes the entry before the dispatch, so a handler
        // that starts a new transition for this layer registers cleanly (the
        // reference releases its transition state before firing the
        // synchronous event, LayerIntf.cpp:7991-8015).
        end_transition(ctx, id, source, event);
    }
    Ok(())
}

/// End the transition running on `dest` (reference
/// `tTJSNI_BaseLayer::InternalStopTransition`, `LayerIntf.cpp:7950-8031`):
/// release the transition state, run the `ttExchange` teardown (the two
/// layers exchange position, visibility and sibling order,
/// `:7969-7991`) and, when `event` is set, fire `onTransitionCompleted`
/// through the object (the reference's synchronous event, `:7997-8015`).
fn end_transition(ctx: &crate::CompatCtx, dest: u32, source: u32, event: bool) {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let removed = unsafe { ctx.services() }
        .state::<LayerExtras>()
        .transitions
        .remove(&dest);
    if removed.is_none() {
        // `InternalStopTransition` only acts while `InTransition`
        // (LayerIntf.cpp:7950).
        log::debug!("Layer.end_transition: layer {dest} had no active transition");
        return;
    }
    // The exchange happens before the event gate in the reference
    // (LayerIntf.cpp:7969-7991 vs `:7997`), so a handler observes the
    // exchanged layers.
    // SAFETY: see above.
    if let Ok(scene) = scene_mut(ctx) {
        exchange_layers(scene, dest, source);
    }
    if event {
        dispatch_transition_completed(ctx, dest, source);
    }
}

/// The `ttExchange` teardown of `InternalStopTransition`
/// (`reference/cpp/core/visual/LayerIntf.cpp:7969-7991`): the destination
/// and source layers exchange position (`SetPosition`, `:7988-7989`),
/// visibility (`:7990-7991`) and sibling order (the `Exchange` order-index
/// swap, `:961-977`; layers without a parent keep their window order).
/// The port transitions main images only, so the tree re-parenting of
/// `Exchange` (`:754-979`) is not modelled.
fn exchange_layers(scene: &mut Scene, dest: u32, source: u32) {
    if dest == source {
        return;
    }
    let (Some(dest_layer), Some(source_layer)) = (scene.layer(dest), scene.layer(source)) else {
        return;
    };
    let (dest_x, dest_y, dest_visible, dest_order) = (
        dest_layer.rect.x,
        dest_layer.rect.y,
        dest_layer.visible,
        dest_layer.z_order,
    );
    let (source_x, source_y, source_visible, source_order) = (
        source_layer.rect.x,
        source_layer.rect.y,
        source_layer.visible,
        source_layer.z_order,
    );
    if dest_x == source_x
        && dest_y == source_y
        && dest_visible == source_visible
        && dest_order == source_order
    {
        return;
    }
    if let Some(layer) = scene.layer_mut(dest) {
        layer.rect.x = source_x;
        layer.rect.y = source_y;
        layer.visible = source_visible;
        layer.z_order = source_order;
    }
    if let Some(layer) = scene.layer_mut(source) {
        layer.rect.x = dest_x;
        layer.rect.y = dest_y;
        layer.visible = dest_visible;
        layer.z_order = dest_order;
    }
}

/// Fire `onTransitionCompleted(dest, src)` on the destination layer object.
///
/// The lookup goes **through the object**, exactly like the reference's
/// synchronous event (`LayerIntf.cpp:8000-8015`), so a script subclass that
/// overrides `onTransitionCompleted` runs instead of the native no-op
/// default. The reference fires the event only when the destination's owner
/// is alive and the source has not shut down (`:7997-8004`) — a dead source
/// suppresses the event entirely rather than passing a null argument.
/// Handler failures are logged like the other layer event handlers and never
/// abort the frame.
fn dispatch_transition_completed(ctx: &crate::CompatCtx, dest: u32, src: u32) {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let host = unsafe { ctx.engine() };
    // The source half of the `transsrcalive` gate (LayerIntf.cpp:7997-8004).
    let Some(src_object) = duplicate_object(ctx, host, ObjectKind::Layer, src) else {
        log::debug!("transition for layer {dest}: source is gone; skipping onTransitionCompleted");
        return;
    };
    // The target handle: its retention is released by `Drop` (call_member
    // never consumes the *target* id).
    let Some(target) = duplicate_object(ctx, host, ObjectKind::Layer, dest) else {
        log::debug!("transition for layer {dest}: object is gone; skipping onTransitionCompleted");
        return;
    };
    // The `(dest, src)` arguments are separate retentions: an argument id is
    // consumed by the argument conversion before the call runs, so it must
    // not double as the target id (the target lookup would fail afterwards).
    let Some(dest_arg) = duplicate_object(ctx, host, ObjectKind::Layer, dest) else {
        return;
    };
    let src_value = TjsValue::Retained(src_object.raw_id() as usize as u64);
    // Consumed by the argument conversion below (see the menu click
    // dispatch: on an early error the forget leaks instead of risking a
    // double release).
    std::mem::forget(src_object);
    let args = [
        TjsValue::Retained(dest_arg.raw_id() as usize as u64),
        src_value,
    ];
    let result = host.call_member(target.raw_id(), "onTransitionCompleted", &args);
    std::mem::forget(dest_arg);
    if let Err(error) = result {
        log::warn!("Layer {dest}: onTransitionCompleted handler failed: {error}");
    }
    // `target` drops here, releasing its retention (never consumed).
}

/// Render one transition frame into the destination layer's image.
///
/// `src1` is the destination's own current image and `src2` the `transwith`
/// layer's, exactly as the reference captures them in `StartTransition`.
/// Both sources are cloned before the destination is borrowed mutably — the
/// same read-source/write-destination shape the rest of the L3 pixel natives
/// use (`Layer.copyRect` and friends).
///
/// A layer with no image simply does not composite; the timing still runs,
/// matching a reference layer whose image contributes nothing.
fn composite_transition(
    ctx: &crate::CompatCtx,
    id: u32,
    source: u32,
    trans: &Trans,
) -> Result<(), String> {
    let scene = scene_mut(ctx)?;
    let (Some(dst_layer), Some(src_layer)) = (scene.layer(id), scene.layer(source)) else {
        return Ok(());
    };
    let (Some(dst_id), Some(src2_id)) = (dst_layer.bitmap, src_layer.bitmap) else {
        return Ok(());
    };
    let (Some(src1), Some(src2)) = (
        scene.bitmap(dst_id).cloned(),
        scene.bitmap(src2_id).cloned(),
    ) else {
        return Ok(());
    };
    let Some(dst) = scene.bitmap_mut(dst_id) else {
        return Ok(());
    };
    let region = Trans::full_region(dst);
    trans
        .apply(dst, &src1, &src2, region)
        .map_err(|error| error.to_string())
}

fn d(name: &'static str, f: tjs::NativeInstanceMethodFn) -> NativeInstanceMethodDef {
    NativeInstanceMethodDef { name, f }
}

/// Every group-3 member as a `NativeInstanceMethodDef` (`onTransitionCompleted`
/// and `onBeforeFocus` live in [`super::events`]).
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        d("beginTransition", layer_begin_transition),
        d("stopTransition", layer_stop_transition),
    ]
}
