//! `Layer` transitions (group 3): `beginTransition` / `stopTransition`.
//!
//! Reference: `tTJSNI_BaseLayer::StartTransition` / `StopTransition`
//! (`reference/cpp/core/visual/LayerIntf.cpp:9833`) and the L2
//! [`engine::trans`] state machine it drives. The transition state is stored
//! per layer in [`LayerExtras`](super::LayerExtras); [`poll_transitions`] is
//! the frame step the layer-tree owner calls (it advances the machine with the
//! L1 [`platform::Clock`] and fires `onTransitionCompleted`).
//!
//! `onTransitionCompleted` itself is a no-op assignable handler (see
//! [`super::events`]); the owner invokes it by name.

use std::ffi::{c_char, c_int, c_void};

use engine::trans::{ScrollFrom, ScrollStay, Trans};
use tjs::{NativeInstanceMethodDef, TjsValue, VAL_OBJECT, VAL_VOID, Value};

use super::super::common::{resolve_object_id, scene_ref};
use super::{LayerExtras, LayerInst, LayerTransition, bound};
use crate::native_util::{args, report_error, set_void, value_as_string};
use crate::natives::system::value_as_bool;

/// `beginTransition(name [, withchildren [, transwith [, options]]])`.
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
        let with_children = match a.get(1) {
            Some(value) if value.ty != VAL_VOID => value_as_bool(value),
            _ => true,
        };
        // The transition source is required by the reference
        // (`TVPSpecifyLayer`).
        let source = match a.get(2) {
            Some(value) if value.ty == VAL_OBJECT => {
                let source = resolve_object_id(host.engine(), value)?;
                if source < 0 {
                    return Err("Layer.beginTransition: transwith is not a Layer".into());
                }
                source as u32
            }
            _ => return Err("Layer.beginTransition: transwith is required".into()),
        };
        let duration = options_duration(host.engine(), a.get(3)).unwrap_or(300.0);
        let trans = build_transition(host.engine(), &name, duration, a.get(3))?;
        // The source must exist and have a main image (it is composited into
        // this layer during the transition).
        {
            let scene = scene_ref(&ctx)?;
            let source_layer = scene
                .layer(source)
                .ok_or_else(|| "Layer.beginTransition: transwith is not a Layer".to_string())?;
            if source_layer.bitmap.is_none() {
                return Err(
                    "Layer.beginTransition: the transition source has no main image".into(),
                );
            }
            if scene.layer(id).is_none() {
                return Err("Layer.beginTransition: this layer no longer exists".into());
            }
        }
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let mut extras = unsafe { ctx.services() }.state::<LayerExtras>();
        extras.transitions.insert(
            id,
            LayerTransition {
                trans,
                source,
                with_children,
            },
        );
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
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let removed = unsafe { ctx.services() }
        .state::<LayerExtras>()
        .transitions
        .remove(&id);
    if removed.is_none() {
        log::debug!("Layer.stopTransition: layer {id} had no active transition");
    }
    set_void(out);
    0
}

/// Build the L2 transition for a script name.
fn build_transition(
    host: &tjs::Tjs2Engine,
    name: &str,
    duration: f64,
    options: Option<&Value>,
) -> Result<Trans, String> {
    match name {
        "crossfade" | "crossFade" | "fade" => {
            Trans::cross_fade(duration).map_err(|error| error.to_string())
        }
        "scroll" | "slide" | "push" => {
            let from = options_text(host, options, "from")
                .map(|value| scroll_from(&value))
                .unwrap_or(ScrollFrom::Left);
            let stay = options_text(host, options, "stay")
                .map(|value| scroll_stay(&value))
                .unwrap_or(ScrollStay::NoStay);
            Trans::scroll(duration, from, stay).map_err(|error| error.to_string())
        }
        "universal" => {
            // A `universal` transition needs a rule image; the L1 image
            // decoder that would supply it is not exposed through L2/L3 yet.
            Err(
                "Layer.beginTransition: the universal transition requires a rule image from \
                 the L1 image decoder, which is not exposed through the L2/L3 service set yet"
                    .into(),
            )
        }
        "mask" => Err(
            "Layer.beginTransition: the mask transition requires a rule image from the L1 \
             image decoder, which is not exposed through the L2/L3 service set yet"
                .into(),
        ),
        other => Err(format!(
            "Layer.beginTransition: unsupported transition {other:?} (supported: \
             crossfade, scroll)"
        )),
    }
}

fn scroll_from(value: &str) -> ScrollFrom {
    match value.to_ascii_lowercase().as_str() {
        "top" | "up" => ScrollFrom::Top,
        "right" => ScrollFrom::Right,
        "bottom" | "down" => ScrollFrom::Bottom,
        _ => ScrollFrom::Left,
    }
}

fn scroll_stay(value: &str) -> ScrollStay {
    match value.to_ascii_lowercase().as_str() {
        "dest" | "destination" => ScrollStay::StayDest,
        "src" | "source" => ScrollStay::StaySrc,
        _ => ScrollStay::NoStay,
    }
}

/// Read a real-valued member from the options object (`duration`, `time` or
/// `ms`).
fn options_duration(host: &tjs::Tjs2Engine, options: Option<&Value>) -> Option<f64> {
    let value = options_number(host, options, &["duration", "time", "ms"])?;
    (value > 0.0).then_some(value)
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

/// Advance every active transition by `dt_ms` and drop the completed ones.
/// The layer-tree owner calls this from its frame step; the completion
/// handler (`onTransitionCompleted`) is dispatched by the owner through the
/// object member so a script subclass override runs.
#[allow(dead_code)]
pub(crate) fn advance_transitions(ctx: &crate::CompatCtx, dt_ms: f64) -> Result<(), String> {
    let active: Vec<u32> = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        services
            .state::<LayerExtras>()
            .transitions
            .keys()
            .copied()
            .collect()
    };
    let mut completed = Vec::new();
    for id in active {
        // SAFETY: see above.
        let services = unsafe { ctx.services() };
        let mut extras = services.state::<LayerExtras>();
        if let Some(entry) = extras.transitions.get_mut(&id) {
            entry.trans.step(dt_ms);
            if entry.trans.is_complete() {
                completed.push(id);
            }
        }
    }
    for id in completed {
        // SAFETY: see above.
        let services = unsafe { ctx.services() };
        services.state::<LayerExtras>().transitions.remove(&id);
    }
    Ok(())
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
