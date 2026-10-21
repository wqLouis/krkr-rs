//! `Layer` transitions (group 3): `beginTransition` / `stopTransition`.
//!
//! Reference: the native `beginTransition`/`stopTransition` members and
//! `tTJSNI_BaseLayer::StartTransition` / `InternalStopTransition`
//! (`reference/cpp/core/visual/LayerIntf.cpp:7770-8058`, `:9815-9858`) and
//! the L2 transition state machine they drive. The transition state
//! is stored per layer in [`LayerExtras`](super::LayerExtras);
//! [`advance_transitions`] is the frame step the frame body calls (it
//! advances the machine by the frame's elapsed milliseconds, stops
//! transitions the reference stops per tick, and fires
//! `onTransitionCompleted`). [`advance_self_updating_transitions`] is the
//! post-projection step for transitions whose destination was drawn.
//!
//! `onTransitionCompleted` itself is a no-op assignable handler (see
//! [`super::events`]); [`advance_transitions`] / [`end_transition`]
//! dispatch it by name on the destination layer object, so a script
//! subclass override wins — the reference fires it as a synchronous event
//! at the destination's owner (`LayerIntf.cpp:8000-8015`).
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::Scene;
use crate::engine_api::{
    DecodedImageMode, EffectDraw, RuleMask, ScrollFrom, ScrollStay, Trans, TransError, TransKind,
    TransitionFrame,
};
use tjs::{DetachedValue, NativeInstanceMethodDef, TjsValue, VAL_OBJECT, VAL_VOID, Value};

use super::super::bitmap::{COLOR_KEY_NONE, load_cached_storage_image};
use super::super::common::{
    ObjectHandle, ObjectKind, VisualState, duplicate_object, resolve_object_id, scene_mut,
    scene_ref,
};
use super::{LayerExtras, LayerInst, LayerTransition, bound};
use crate::native_util::{args, report_error, services, set_void, value_as_string};
use crate::natives::system::{tjs_string_to_real, value_as_bool};
use crate::natives::tjsnum::tjs_string_to_integer;

/// Per-transition state that is not part of the parent's `LayerTransition`:
/// the script-driven update mode, the retained tick callback and the time
/// accumulated while a self-updating transition waits for its next update.
#[derive(Default)]
struct TransitionRuntimeState {
    entries: HashMap<u32, TransitionRuntime>,
}

struct TransitionRuntime {
    self_update: bool,
    callback: Option<DetachedValue>,
    pending_ms: f64,
}

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
/// 4. the provider checks the source sizes and then parses and validates its
///    options while the script is inside the call; `time` is required and a
///    missing or non-numeric value raises;
/// 5. without children both endpoints must have a main image.
///
/// The options object's `selfupdate` and `callback` members are captured
/// before the transition is registered.
///
/// `withchildren` (default `true`, `LayerIntf.cpp:9819-9821`) selects the
/// whole layer tree: caching is enabled for both layers
/// (`LayerIntf.cpp:7869-7876`) and each frame blends the fully rendered
/// subtrees. Without it the reference requires the main images and blends
/// those.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_begin_transition`.
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
        // `withchildren` defaults to true; a `void` argument keeps the
        // default (`LayerIntf.cpp:9819-9821`).
        let with_children = match a.get(1) {
            Some(value) if value.ty != VAL_VOID => value_as_bool(value),
            _ => true,
        };
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
        {
            let extras = services(&ctx).state::<LayerExtras>();
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
        let scene = scene_ref(&ctx)?;
        let Some(dest_layer) = scene.layer(id) else {
            return Err("Layer.beginTransition: this layer no longer exists".into());
        };
        let source_layer = scene
            .layer(source)
            .ok_or_else(|| "Layer.beginTransition: transwith is not a Layer".to_string())?;
        // The provider receives `src1`/`src2` sizes: the layer rects when
        // the whole subtree is transitioned, the MainImage sizes otherwise
        // (`LayerIntf.cpp:7837-7851`).
        let (dest_size, source_size) = if with_children {
            (
                Some((dest_layer.rect.w, dest_layer.rect.h)),
                Some((source_layer.rect.w, source_layer.rect.h)),
            )
        } else {
            (main_image_size(scene, id), main_image_size(scene, source))
        };
        // The provider's up-front `src1`/`src2` size check runs before it
        // parses options (`wave.cpp:319-320`, `TransIntf.cpp:493-497`).
        if let (Some(dest_size), Some(source_size)) = (dest_size, source_size)
            && dest_size != source_size
        {
            return Err(format!(
                "Layer.beginTransition: transition source size mismatch: destination \
                 {}x{}, source {}x{}",
                dest_size.0, dest_size.1, source_size.0, source_size.1
            ));
        }
        let has_images = dest_layer.bitmap.is_some() && source_layer.bitmap.is_some();
        let trans = build_transition(&ctx, host.engine(), &name, a.get(3), dest_size)?;
        // Without children both endpoints must have a main image. The
        // provider parses its options first, so a missing `time` wins over
        // this check.
        if !with_children && !has_images {
            return Err(
                "Layer.beginTransition: Transition source and destination must have image".into(),
            );
        }
        let (self_update, callback) = transition_options(host.engine(), a.get(3))?;
        // Caching is enabled for both layers while a whole-subtree
        // transition runs (`LayerIntf.cpp:7869-7876`); the frame step
        // renders each subtree into its cache. Without children the frame
        // step blends the two main images into the destination's self image,
        // which the layer keeps drawing beside its live children.
        if with_children {
            let scene = scene_mut(&ctx)?;
            scene.inc_cache_enabled_count(id);
            scene.inc_cache_enabled_count(source);
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
        // SAFETY: see above.
        let mut runtime = unsafe { ctx.services() }.state::<TransitionRuntimeState>();
        runtime.entries.insert(
            id,
            TransitionRuntime {
                self_update,
                callback,
                pending_ms: 0.0,
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
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_stop_transition`.
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

/// A layer's main-image size in pixels, or `None` when it has no image.
fn main_image_size(scene: &Scene, id: u32) -> Option<(u32, u32)> {
    scene
        .layer(id)
        .and_then(|layer| layer.bitmap)
        .and_then(|bitmap| bitmap_size(scene, bitmap))
}

/// The size of the bitmap `id` in pixels.
fn bitmap_size(scene: &Scene, id: u32) -> Option<(u32, u32)> {
    scene.bitmap(id).map(|bitmap| (bitmap.width, bitmap.height))
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
/// An empty method name is the KAG default: a transition whose `method` is
/// `""` (the `normal`/`500sec`/`300secNosync` entries in the game's
/// `transitions` table) resolves to the reference's `crossfade` fallback,
/// so it maps to [`TransKind::CrossFade`] rather than being treated as an
/// unknown name.
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
    options: Option<&Value>,
    dest_size: Option<(u32, u32)>,
) -> Result<Trans, String> {
    let time = || options_duration(host, options);
    match name {
        // `""` is the KAG default method; `TVPFindTransHandlerProvider`
        // misses the empty key and falls back to `crossfade`
        // (`reference/cpp/core/visual/TransIntf.cpp:331-341`).
        "crossfade" | "fade" | "" => Trans::cross_fade(time()?).map_err(|error| error.to_string()),
        "scroll" | "slide" | "push" => {
            let duration = time()?;
            let from = scroll_from(host, options)?;
            let stay = scroll_stay(host, options)?;
            Trans::scroll(duration, from, stay).map_err(|error| error.to_string())
        }
        "universal" => {
            // The universal provider reads `time` first, then `vague`
            // (default 64) and a required `rule` image.
            let duration = time()?;
            let vague = option_int(host, options, &["vague"])?.unwrap_or(64);
            let rule = rule_mask(ctx, host, options)?;
            Trans::universal(duration, vague, rule).map_err(|error| error.to_string())
        }
        "mask" => {
            // The reference's `universal` with a hard threshold.
            let duration = time()?;
            let rule = rule_mask(ctx, host, options)?;
            Trans::mask(duration, rule).map_err(|error| error.to_string())
        }
        "wave" => {
            let duration = time()?;
            Trans::new(
                TransKind::Wave {
                    max_height: option_int(host, options, &["maxh"])?.unwrap_or(50),
                    max_omega: option_float(host, options, &["maxomega"])?.unwrap_or(0.2),
                    background1: option_color(host, options, &["bgcolor1"])?.unwrap_or(0),
                    background2: option_color(host, options, &["bgcolor2"])?.unwrap_or(0),
                    wave_type: option_int(host, options, &["wavetype"])?.unwrap_or(0),
                },
                duration,
            )
            .map_err(|error| error.to_string())
        }
        "mosaic" => {
            let duration = time()?;
            Trans::new(
                TransKind::Mosaic {
                    max_size: option_int(host, options, &["maxsize"])?.unwrap_or(30),
                },
                duration,
            )
            .map_err(|error| error.to_string())
        }
        "turn" => {
            let duration = time()?;
            Trans::new(
                TransKind::Turn {
                    background: option_color(host, options, &["bgcolor"])?.unwrap_or(0),
                },
                duration,
            )
            .map_err(|error| error.to_string())
        }
        "rotatezoom" => {
            let duration = time()?;
            Trans::new(
                TransKind::RotateZoom {
                    factor: option_float(host, options, &["factor"])?.unwrap_or(1.0),
                    accel: option_float(host, options, &["accel"])?.unwrap_or(0.0),
                    twist: option_float(host, options, &["twist"])?.unwrap_or(2.0),
                    twist_accel: option_float(host, options, &["twistaccel"])?.unwrap_or(-2.0),
                    center: rotation_center(host, options, dest_size)?,
                },
                duration,
            )
            .map_err(|error| error.to_string())
        }
        "rotatevanish" => {
            let duration = time()?;
            Trans::new(
                TransKind::RotateVanish {
                    accel: option_float(host, options, &["accel"])?.unwrap_or(2.0),
                    twist: option_float(host, options, &["twist"])?.unwrap_or(2.0),
                    twist_accel: option_float(host, options, &["twistaccel"])?.unwrap_or(2.0),
                    center: rotation_center(host, options, dest_size)?,
                },
                duration,
            )
            .map_err(|error| error.to_string())
        }
        "rotateswap" => {
            // The reference's `rotateswap` provider reads only `time`,
            // `bgcolor` and `twist` (`rotatetrans.cpp:453-465`) — it has no
            // `centerx`/`centery` option, so a supplied centre is dropped
            // here instead of moving the pivot.
            let duration = time()?;
            Trans::new(
                TransKind::RotateSwap {
                    background: option_color(host, options, &["bgcolor"])?.unwrap_or(0),
                    twist: option_float(host, options, &["twist"])?.unwrap_or(1.0),
                    center: None,
                },
                duration,
            )
            .map_err(|error| error.to_string())
        }
        "ripple" => {
            // The ripple provider reads `time` first, then validates its
            // options in its start step.
            let duration = time()?;
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
            let max_drift = option_int(host, options, &["maxdrift"])?.unwrap_or(24);
            let trans = Trans::new(
                TransKind::Ripple {
                    center,
                    map_width: option_int(host, options, &["rwidth"])?.unwrap_or(128),
                    roundness: option_float(host, options, &["roundness"])?.unwrap_or(1.0) as f32,
                    speed: option_float(host, options, &["speed"])?.unwrap_or(6.0) as f32,
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
///
/// The load goes through the scene's decoded-image cache — the same cache
/// `Layer.loadImages` uses — so a repeated transition with the same rule
/// decodes it once. The colour key is the port's `COLOR_KEY_NONE`: the
/// graphic keeps its own alpha, and the rule's grayscale comes from the
/// decoded pixels.
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
    // The rule graphic goes through the same decoded-image cache as every
    // other load, so a repeated transition does not decode it again.
    let image = load_cached_storage_image(ctx, &name, COLOR_KEY_NONE, DecodedImageMode::Full)?;
    RuleMask::from_rgba(image.width, image.height, &image.rgba).map_err(|error| error.to_string())
}

/// An integer option (`tjs_int` in the reference).
fn option_int(
    host: &tjs::Tjs2Engine,
    options: Option<&Value>,
    names: &[&str],
) -> Result<Option<i32>, String> {
    Ok(options_number(host, options, names)?.map(|value| value as i32))
}

/// A real option (`double` in the reference).
fn option_float(
    host: &tjs::Tjs2Engine,
    options: Option<&Value>,
    names: &[&str],
) -> Result<Option<f64>, String> {
    options_number(host, options, names)
}

/// A colour option (`tjs_uint32` in the reference, `0xAARRGGBB`).
fn option_color(
    host: &tjs::Tjs2Engine,
    options: Option<&Value>,
    names: &[&str],
) -> Result<Option<i64>, String> {
    Ok(options_number(host, options, names)?.map(|value| value as i64))
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
    let x = option_int(host, options, &["centerx"])?;
    let y = option_int(host, options, &["centery"])?;
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
    match options_number(host, options, &["from"])? {
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
    match options_number(host, options, &["stay"])? {
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

/// Read the required real-valued `time` option.
///
/// A missing or non-numeric member is an error, and values below 2 are
/// clamped to 2. The cast truncates toward zero before the clamp, so a
/// fractional time runs for its whole-millisecond part.
fn options_duration(host: &tjs::Tjs2Engine, options: Option<&Value>) -> Result<f64, String> {
    let value = options_time(host, options)
        .ok_or_else(|| "Layer.beginTransition: Specify option time".to_string())?;
    // Non-finite values have no defined `tjs_int64` cast in the reference;
    // they pass through to L2, which rejects them as a typed
    // `InvalidDuration` instead of clamping.
    if !value.is_finite() {
        return Ok(value);
    }
    Ok(value.trunc().max(2.0))
}

/// The numeric `time` member, converted with the engine's integer coercion
/// (a numeric string is accepted, an absent or `void` member is not).
fn options_time(host: &tjs::Tjs2Engine, options: Option<&Value>) -> Option<f64> {
    let value = options?;
    if value.ty != VAL_OBJECT {
        return None;
    }
    let object = host.retain_object_arg(value).ok()?;
    match host.member(object.raw_id(), "time") {
        Ok(TjsValue::Real(number)) => Some(number),
        Ok(TjsValue::Integer(number)) => Some(number as f64),
        Ok(TjsValue::String(text)) => Some(tjs_string_to_integer(&text) as f64),
        _ => None,
    }
}

/// The `selfupdate`/`callback` members of the options object.
///
/// `selfupdate` is false unless the member is present and non-void, and is
/// read through integer coercion; `callback` is retained only when it is an
/// object/function. A scalar callback is rejected here because the options
/// reader takes its object closure while the script is still inside
/// `beginTransition`; a null closure is retained and fails at the first tick.
fn transition_options(
    host: &tjs::Tjs2Engine,
    options: Option<&Value>,
) -> Result<(bool, Option<DetachedValue>), String> {
    let Some(value) = options else {
        return Ok((false, None));
    };
    if value.ty != VAL_OBJECT {
        return Ok((false, None));
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("Layer.beginTransition: cannot read the options: {error}"))?;
    let self_update = match host.member(object.raw_id(), "selfupdate") {
        Ok(TjsValue::Void) => false,
        Ok(value) => match value {
            TjsValue::Integer(number) => number != 0,
            TjsValue::Real(number) => number != 0.0,
            TjsValue::String(text) => tjs_string_to_integer(&text) != 0,
            TjsValue::Object | TjsValue::Retained(_) => true,
            TjsValue::Void => false,
        },
        Err(error) => {
            log::debug!("Layer.beginTransition: cannot read option selfupdate: {error}");
            false
        }
    };
    let callback = match host.member(object.raw_id(), "callback") {
        Ok(TjsValue::Void) => None,
        Ok(TjsValue::Object) => Some(host.retain_value_detached(&TjsValue::Object).map_err(
            |error| {
                format!("Layer.beginTransition: cannot retain the transition callback: {error}")
            },
        )?),
        Ok(other) => {
            return Err(format!(
                "Layer.beginTransition: transition option callback must be a function, got {other:?}"
            ));
        }
        Err(error) => {
            log::debug!("Layer.beginTransition: cannot read option callback: {error}");
            None
        }
    };
    Ok((self_update, callback))
}

/// Call the transition tick callback and convert its result with the
/// integer cast.
fn transition_tick(host: &tjs::Tjs2Engine, callback: &DetachedValue) -> Result<f64, String> {
    let result = host
        .call_detached(callback, &[])
        .map_err(|error| format!("Layer.beginTransition: transition callback failed: {error}"))?;
    match result {
        TjsValue::Void => Ok(0.0),
        TjsValue::Integer(number) => Ok(number as f64),
        TjsValue::Real(number) => Ok(number),
        TjsValue::String(text) => Ok(tjs_string_to_integer(&text) as f64),
        TjsValue::Object | TjsValue::Retained(_) => {
            Err("Layer.beginTransition: transition callback must return a numeric tick".to_string())
        }
    }
}

fn options_number(
    host: &tjs::Tjs2Engine,
    options: Option<&Value>,
    names: &[&str],
) -> Result<Option<f64>, String> {
    let Some(value) = options else {
        return Ok(None);
    };
    if value.ty != VAL_OBJECT {
        return Ok(None);
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("Layer.beginTransition: cannot read the options: {error}"))?;
    for name in names {
        match host.member(object.raw_id(), name) {
            Ok(TjsValue::Void) => {}
            Ok(TjsValue::Real(number)) => return Ok(Some(number)),
            Ok(TjsValue::Integer(number)) => return Ok(Some(number as f64)),
            Ok(TjsValue::String(text)) => {
                if let Some(number) = numeric_string(&text) {
                    return Ok(Some(number));
                }
            }
            Ok(TjsValue::Object | TjsValue::Retained(_)) => {
                return Err(format!(
                    "Layer.beginTransition: invalid transition option {name}: value is not a number"
                ));
            }
            Err(error) => {
                return Err(format!(
                    "Layer.beginTransition: cannot read option {name}: {error}"
                ));
            }
        }
    }
    Ok(None)
}

/// The numeric prefix of a string, using the engine's real-number coercion.
/// A non-numeric string yields `None` so the caller can try its textual alias.
fn numeric_string(text: &str) -> Option<f64> {
    let mut rest = text;
    if let Some(stripped) = rest.strip_prefix(['+', '-']) {
        rest = stripped;
    }
    rest = rest.trim_start_matches(' ');
    let starts_number = rest.as_bytes().first().is_some_and(u8::is_ascii_digit);
    let starts_keyword = ["true", "false", "NaN", "Infinity"]
        .iter()
        .any(|keyword| rest.starts_with(keyword));
    if starts_number || starts_keyword {
        Some(tjs_string_to_real(text))
    } else {
        None
    }
}

fn options_text(host: &tjs::Tjs2Engine, options: Option<&Value>, name: &str) -> Option<String> {
    let value = options?;
    if value.ty != VAL_OBJECT {
        return None;
    }
    let object = host.retain_object_arg(value).ok()?;
    match host.member(object.raw_id(), name) {
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
    let class = match kind {
        ObjectKind::Layer => "Layer",
        ObjectKind::Window => "Window",
    };
    let raw = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let state = unsafe { ctx.services() }.state::<VisualState>();
        let registry = match kind {
            ObjectKind::Window => &state.objects.windows,
            ObjectKind::Layer => &state.objects.layers,
        };
        registry.get(&id).map(ObjectHandle::as_ptr)
    };
    let Some(raw) = raw else {
        return false;
    };
    // SAFETY: `ctx` was resolved from the installed binding, so the engine is
    // live; the raw object is alive while its registry entry is present.
    // Retain it for the lookup, then release it.
    let Ok(retained) = (unsafe { ctx.engine() }).retain_object_detached(raw) else {
        return false;
    };
    // SAFETY: see above.
    unsafe { ctx.engine() }
        .native_instance_payload(retained.raw_id(), class)
        .is_ok_and(|payload| payload.is_some())
}

/// Whether the draw report contains transition `dest` or its source: a drawn
/// descendant means the layer's subtree was drawn, and a drawn cached ancestor
/// reports the descendants it covered.
fn drawn_signal(scene: &Scene, drawn: &[u32], dest: u32, source: u32) -> bool {
    drawn
        .iter()
        .any(|&id| layer_within(scene, id, dest) || layer_within(scene, id, source))
}

/// Whether `node` is `root` or one of its descendants.
fn layer_within(scene: &Scene, node: u32, root: u32) -> bool {
    let mut current = Some(node);
    while let Some(id) = current {
        if id == root {
            return true;
        }
        current = scene.layer(id).and_then(|layer| layer.parent);
    }
    false
}

/// Advance every active transition by `dt_ms` and tear down the finished or
/// stopped ones.
///
/// Each step produces one frame, so a running transition is visible frame by
/// frame. A host that composites the projection records the frame on the layer
/// (`composite_transition`); every other host blends it with [`Trans::apply`]
/// into the destination's transition frame, which the projection draws in place
/// of the subtree and whose children stay inside it. A transition without
/// children blends the two main images into the destination's self image
/// instead, which the projection draws in place of its own image while the
/// children stay live. The frame the transition completes on is released with
/// the caches, and the teardown clears the recorded frame (or the self image).
///
/// Like the reference's per-tick `InvokeTransition`
/// (`LayerIntf.cpp:8047-8061`), a transition whose destination node is no
/// longer visible is stopped at that frame without rendering it
/// (`:8056-8060`), and like its `Invalidate` (`:517-521`) a transition
/// whose destination or source script object has died is torn down without
/// the completion event (`:7997-8004`). The frame body calls this from its
/// `frame_tail` step; the completion handler
/// (`onTransitionCompleted`) is dispatched here through the object member
/// so a script subclass override runs (reference `LayerIntf.cpp:8000-8015`
/// fires the event synchronously on the layer with `(dest, src)`).
///
/// It first runs the frame's `onPaint` dispatch ([`super::paint_poll`]),
/// mirroring the reference's `tTJSNI_BaseLayer::BeforeCompletion`
/// (`LayerIntf.cpp:6390-6404`), which fires the `onPaint` event and then
/// processes the transition in the same pre-draw step.
///
/// A self-updating transition is not stepped here. It accumulates the frame
/// time and is advanced by [`advance_self_updating_transitions`] once the
/// projection reports that its layer was drawn.
pub fn advance_transitions(ctx: &crate::CompatCtx, dt_ms: f64) -> Result<(), String> {
    super::paint_poll(ctx)?;
    advance(ctx, dt_ms, None)
}

/// Advance the self-updating transitions whose layer the projection drew.
///
/// `drawn_layers` is the projection's actual emit report, including the
/// descendants covered by a drawn cached ancestor. A transition whose
/// destination or source subtree is in that report performs its pending step;
/// one that was not drawn keeps its accumulated time for the next draw.
pub fn advance_self_updating_transitions(
    ctx: &crate::CompatCtx,
    drawn_layers: &[u32],
) -> Result<(), String> {
    advance(ctx, 0.0, Some(drawn_layers))
}

fn advance(ctx: &crate::CompatCtx, dt_ms: f64, drawn_layers: Option<&[u32]>) -> Result<(), String> {
    let active: Vec<(u32, u32, bool)> = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        services
            .state::<LayerExtras>()
            .transitions
            .iter()
            .map(|(&id, entry)| (id, entry.source, entry.with_children))
            .collect()
    };
    // `(destination, source, fire_event)`.
    let mut ended: Vec<(u32, u32, bool)> = Vec::new();
    for (id, source, with_children) in active {
        // A dead endpoint tears the transition down without the event: both
        // the destination's `Owner` and the source's `Shutdown` gate the
        // dispatch (LayerIntf.cpp:7997-8004).
        if !object_alive(ctx, ObjectKind::Layer, id)
            || !object_alive(ctx, ObjectKind::Layer, source)
        {
            ended.push((id, source, false));
            continue;
        }
        let (self_update, callback) = {
            // SAFETY: see above.
            let services = unsafe { ctx.services() };
            let runtime = services.state::<TransitionRuntimeState>();
            let Some(entry) = runtime.entries.get(&id) else {
                return Err(format!(
                    "Layer.beginTransition: transition {id} has no runtime state"
                ));
            };
            let callback = match &entry.callback {
                Some(callback) => Some(
                    // SAFETY: see above.
                    unsafe { ctx.engine() }
                        .duplicate_retained(callback.raw_id())
                        .map_err(|error| {
                            format!(
                                "Layer.beginTransition: cannot retain the transition callback: \
                                 {error}"
                            )
                        })?,
                ),
                None => None,
            };
            (entry.self_update, callback)
        };
        match drawn_layers {
            None if self_update => {
                // A self-updating transition is not on the continuous hook.
                // Wall-clock time still runs, so accumulate it for the step
                // the next draw performs.
                // SAFETY: see above.
                let mut runtime = unsafe { ctx.services() }.state::<TransitionRuntimeState>();
                if let Some(entry) = runtime.entries.get_mut(&id)
                    && entry.callback.is_none()
                    && dt_ms.is_finite()
                    && dt_ms > 0.0
                {
                    entry.pending_ms += dt_ms;
                }
                continue;
            }
            None => {}
            Some(_) if !self_update => continue,
            Some(drawn_layers) => {
                let scene = scene_ref(ctx)?;
                if !drawn_signal(scene, drawn_layers, id, source) {
                    continue;
                }
            }
        }
        // The callback supplies the tick before the visibility check.
        let tick = match callback {
            Some(callback) => {
                // SAFETY: see above.
                Some(transition_tick(unsafe { ctx.engine() }, &callback)?)
            }
            None => None,
        };
        // An invisible destination stops at that frame, before rendering
        // (LayerIntf.cpp:8056-8060).
        if !scene_ref(ctx)?.node_visible(id) {
            ended.push((id, source, true));
            continue;
        }
        let pending_ms = {
            // SAFETY: see above.
            let mut runtime = unsafe { ctx.services() }.state::<TransitionRuntimeState>();
            let Some(entry) = runtime.entries.get_mut(&id) else {
                continue;
            };
            let pending_ms = entry.pending_ms;
            entry.pending_ms = 0.0;
            pending_ms
        };
        let done = {
            // SAFETY: see above.
            let services = unsafe { ctx.services() };
            let mut extras = services.state::<LayerExtras>();
            let Some(entry) = extras.transitions.get_mut(&id) else {
                continue;
            };
            match tick {
                Some(tick) => {
                    entry.trans.seek_ms(tick);
                }
                None if self_update => {
                    entry.trans.step(pending_ms);
                }
                None => {
                    entry.trans.step(dt_ms);
                }
            }
            let done = entry.trans.is_complete();
            // Render this frame through the borrowed machine — the same
            // `Process` write into the destination the reference performs
            // per tick; no per-frame clone of the transition (or its rule
            // image) is needed.
            composite_transition(ctx, id, source, &entry.trans, with_children)?;
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
    unsafe { ctx.services() }
        .state::<TransitionRuntimeState>()
        .entries
        .remove(&dest);
    // SAFETY: see above.
    let removed = unsafe { ctx.services() }
        .state::<LayerExtras>()
        .transitions
        .remove(&dest);
    let Some(entry) = removed else {
        // `InternalStopTransition` only acts while `InTransition`
        // (LayerIntf.cpp:7950).
        log::debug!("Layer.end_transition: layer {dest} had no active transition");
        return;
    };
    // A whole-subtree transition disabled caching on both layers and releases
    // the caches here (`DecCacheEnabledCount`, LayerIntf.cpp:7961-7964),
    // before the exchange.
    if entry.with_children
        && let Ok(scene) = scene_mut(ctx)
    {
        scene.dec_cache_enabled_count(dest);
        scene.dec_cache_enabled_count(source);
    }
    // The exchange happens before the event gate in the reference
    // (LayerIntf.cpp:7969-7991 vs `:7997`), so a handler observes the
    // exchanged layers. `InternalStopTransition` uses `Exchange` when the
    // transition ran with children and `Swap` (which also exchanges the
    // layers' direct children) otherwise (`:7969-7987`).
    // SAFETY: see above.
    if let Ok(scene) = scene_mut(ctx) {
        // A childless transition drew its frames in the layer's self image;
        // the teardown leaves the layer drawing its own image again.
        scene.set_transition_self_image(dest, None);
        scene.set_transition_frame(dest, None);
        scene.set_effect(dest, None);
        exchange_layers(scene, dest, source, !entry.with_children);
    }
    if event {
        dispatch_transition_completed(ctx, dest, source);
    }
}

/// The reference `tTJSNI_BaseLayer::Exchange` (`LayerIntf.cpp:754-979`) as
/// the `ttExchange` teardown uses it (`:7969-7991`): the two layers trade
/// places in the tree — parent, sibling order and primary slot — and then
/// the teardown swaps their position and visibility (`SetPosition`,
/// `:7988-7989`; `SetVisible`, `:7990-7991`). `swap_children` selects the
/// `Swap` form (`:260-263`), which also exchanges the layers' direct
/// children; `InternalStopTransition` uses it when the transition ran
/// without children (`:7969-7987`).
fn exchange_layers(scene: &mut Scene, dest: u32, source: u32, swap_children: bool) {
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
    let dest_parent = dest_layer.parent;
    let source_parent = source_layer.parent;
    let dest_window = dest_layer.window;
    let source_window = source_layer.window;
    let dest_primary = dest_layer.is_primary;
    let source_primary = source_layer.is_primary;
    let dest_children = dest_layer.children.clone();
    let source_children = source_layer.children.clone();
    let dest_index = child_index(scene, dest);
    let source_index = child_index(scene, source);
    let dest_ancestor = ancestor_child(scene, dest, source);
    let source_ancestor = ancestor_child(scene, source, dest);

    if dest_parent == source_parent {
        // Siblings: the shared parent's child list is the render order, so
        // swapping the two entries reproduces `SetOrderIndex` exactly.
        if let Some(parent) = dest_parent {
            if let Some(layer) = scene.layer_mut(parent) {
                let i = layer.children.iter().position(|&child| child == dest);
                let j = layer.children.iter().position(|&child| child == source);
                if let (Some(i), Some(j)) = (i, j) {
                    layer.children.swap(i, j);
                }
            }
        } else if dest_window == source_window
            && let Some(window) = scene.window_mut(dest_window)
        {
            let i = window.layers.iter().position(|&child| child == dest);
            let j = window.layers.iter().position(|&child| child == source);
            if let (Some(i), Some(j)) = (i, j) {
                window.layers.swap(i, j);
            }
        }
    } else {
        detach_layer(scene, dest);
        detach_layer(scene, source);
        // An ancestor exchange detaches the intermediate layer so the
        // subtree can change place (`LayerIntf.cpp:790-909`).
        let intermediate = dest_ancestor.or(source_ancestor);
        if let Some(intermediate) = intermediate {
            detach_layer(scene, intermediate);
        }
        match (dest_ancestor, source_ancestor) {
            (Some(intermediate), None) => {
                // `dest` is a descendant of `source`.
                let source_join = if dest_parent == Some(source) {
                    Some(dest)
                } else {
                    dest_parent
                };
                insert_layer(scene, dest, source_parent, source_index);
                insert_layer(scene, source, source_join, dest_index);
                if intermediate != dest {
                    insert_layer(scene, intermediate, Some(dest), None);
                }
            }
            (None, Some(intermediate)) => {
                // `source` is a descendant of `dest`.
                let dest_join = if source_parent == Some(dest) {
                    Some(source)
                } else {
                    source_parent
                };
                insert_layer(scene, dest, dest_join, source_index);
                insert_layer(scene, source, dest_parent, dest_index);
                if intermediate != source {
                    insert_layer(scene, intermediate, Some(source), None);
                }
            }
            _ => {
                insert_layer(scene, dest, source_parent, source_index);
                insert_layer(scene, source, dest_parent, dest_index);
            }
        }
    }

    // `SetPosition`/`SetVisible` (`LayerIntf.cpp:7988-7991`) and the sibling
    // order (`SetOrderIndex`, `:961-977`).
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

    // The primary slot follows the layer (`DetachPrimary`/`AttachPrimary`,
    // `LayerIntf.cpp:800-822`).
    if dest_primary && let Some(window) = scene.window_mut(dest_window) {
        window.primary_layer = Some(source);
    }
    if source_primary && let Some(window) = scene.window_mut(source_window) {
        window.primary_layer = Some(dest);
    }
    if let Some(layer) = scene.layer_mut(dest) {
        layer.is_primary = source_primary;
    }
    if let Some(layer) = scene.layer_mut(source) {
        layer.is_primary = dest_primary;
    }

    // The `Swap` form also exchanges the layers' direct children
    // (`LayerIntf.cpp:817-909`).
    if swap_children {
        if let Some(layer) = scene.layer_mut(dest) {
            layer.children.clone_from(&source_children);
        }
        if let Some(layer) = scene.layer_mut(source) {
            layer.children.clone_from(&dest_children);
        }
        for &child in &source_children {
            if let Some(layer) = scene.layer_mut(child) {
                layer.parent = Some(dest);
            }
        }
        for &child in &dest_children {
            if let Some(layer) = scene.layer_mut(child) {
                layer.parent = Some(source);
            }
        }
    }
}

/// The index of `id` in its parent's child list (or its window's root list).
fn child_index(scene: &Scene, id: u32) -> Option<usize> {
    let layer = scene.layer(id)?;
    match layer.parent {
        Some(parent) => scene
            .layer(parent)?
            .children
            .iter()
            .position(|&child| child == id),
        None => scene
            .window(layer.window)?
            .layers
            .iter()
            .position(|&child| child == id),
    }
}

/// Remove `id` from its parent's child list (or its window's root list).
fn detach_layer(scene: &mut Scene, id: u32) {
    let (parent, window) = match scene.layer(id) {
        Some(layer) => (layer.parent, layer.window),
        None => return,
    };
    match parent {
        Some(parent) => {
            if let Some(layer) = scene.layer_mut(parent) {
                layer.children.retain(|&child| child != id);
            }
        }
        None => {
            if let Some(window) = scene.window_mut(window) {
                window.layers.retain(|&child| child != id);
            }
        }
    }
    if let Some(layer) = scene.layer_mut(id) {
        layer.parent = None;
    }
}

/// Insert `id` under `parent` (or the window root list) at `index`.
fn insert_layer(scene: &mut Scene, id: u32, parent: Option<u32>, index: Option<usize>) {
    if let Some(layer) = scene.layer_mut(id) {
        layer.parent = parent;
    }
    match parent {
        Some(parent) => {
            if let Some(layer) = scene.layer_mut(parent) {
                let index = index
                    .unwrap_or(layer.children.len())
                    .min(layer.children.len());
                layer.children.insert(index, id);
            }
        }
        None => {
            let window = scene.layer(id).map(|layer| layer.window);
            if let Some(window) = window
                && let Some(state) = scene.window_mut(window)
            {
                let index = index.unwrap_or(state.layers.len()).min(state.layers.len());
                state.layers.insert(index, id);
            }
        }
    }
}

/// The child of `ancestor` on the path down to `id`, or `None` when `id` is
/// not a proper descendant of `ancestor`.
fn ancestor_child(scene: &Scene, id: u32, ancestor: u32) -> Option<u32> {
    if id == ancestor {
        return None;
    }
    let mut child = id;
    let mut current = scene.layer(id)?.parent;
    while let Some(parent) = current {
        if parent == ancestor {
            return Some(child);
        }
        child = parent;
        current = scene.layer(parent)?.parent;
    }
    None
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
    let src_value = TjsValue::Retained(src_object.raw_id().as_ptr() as usize as u64);
    // Consumed by the argument conversion below (see the menu click
    // dispatch: on an early error the forget leaks instead of risking a
    // double release).
    std::mem::forget(src_object);
    let args = [
        TjsValue::Retained(dest_arg.raw_id().as_ptr() as usize as u64),
        src_value,
    ];
    let result = host.call_member(target.raw_id(), "onTransitionCompleted", &args);
    std::mem::forget(dest_arg);
    if let Err(error) = result {
        log::warn!("Layer {dest}: onTransitionCompleted handler failed: {error}");
    }
    // `target` drops here, releasing its retention (never consumed).
}

/// Render one transition frame.
///
/// A host that composites transition frames records the frame the projection
/// draws: the two sources placed inside a frame that stands in for the
/// destination's subtree (with children) or for its own image (without). Every
/// other host blends that frame into a bitmap — the destination's transition
/// frame for a whole subtree, its self image otherwise — and the projection
/// draws the bitmap. Both main images stay read-only: a MainImage is what
/// script pixel reads, saves and piled copies observe.
///
/// A backend that can run effect pipelines takes neither route: the
/// transition records the custom-shader draw the projection draws, and no
/// bitmap is written at all.
fn composite_transition(
    ctx: &crate::CompatCtx,
    id: u32,
    source: u32,
    trans: &Trans,
    with_children: bool,
) -> Result<(), String> {
    let scene = scene_mut(ctx)?;
    let (src1, src2, w, h) = if with_children {
        transition_sources_subtree(scene, id, source)?
    } else {
        transition_sources_self(scene, id, source)?
    };
    // Both paths composite the two sources at one size; the blend rejects a
    // mismatch before it draws, so the frame does the same.
    let (src2_w, src2_h) = bitmap_size(scene, src2)
        .ok_or_else(|| "Layer.beginTransition: the transition source is missing".to_string())?;
    if (src2_w, src2_h) != (w, h) {
        return Err(TransError::SizeMismatch {
            src1: (w, h),
            src2: (src2_w, src2_h),
        }
        .to_string());
    }
    // The generic effect path: the projection draws the transition with a
    // shader, so nothing is written into a bitmap.
    let effect: Option<EffectDraw> =
        scene.transition_effect_draw(trans, src1, src2, (w, h), with_children);
    if let Some(effect) = effect {
        scene.set_effect(id, Some(effect));
        scene.set_transition_frame(id, None);
        return Ok(());
    }
    // A composited frame is drawn by the projection, which places the two
    // sources itself. Kinds that need per-pixel work return no frame and are
    // blended below on every host.
    let frame: Option<TransitionFrame> = if scene.composite_transition_frames {
        trans.transition_frame(src1, src2, (w, h), with_children)
    } else {
        None
    };
    if let Some(frame) = frame {
        scene.set_transition_frame(id, Some(frame));
        return Ok(());
    }
    // The blend needs a frame of its own, distinct from the content it reads,
    // whatever the destination's content happens to be.
    let target = if with_children {
        crate::engine_api::layer_ops::ensure_transition_display(scene, id, w, h).ok_or_else(
            || "Layer.beginTransition: cannot allocate the transition frame".to_string(),
        )?
    } else {
        crate::engine_api::layer_ops::ensure_transition_self_image(scene, id, w, h).ok_or_else(
            || "Layer.beginTransition: cannot allocate the transition self image".to_string(),
        )?
    };
    // The blend writes into the target and reads both sources, so all three
    // must hold the pixels their size names. The target is the frame the
    // blend writes: a cached frame that carries a layer's main image rather
    // than its own pixels is a target the blend cannot write into.
    {
        let target_state = scene
            .bitmap(target)
            .ok_or_else(|| "Layer.beginTransition: the transition frame is missing".to_string())?;
        super::util::bitmap_holds_pixels(target_state, "transition target")?;
    }
    // The blend reads both sources while it writes the target, so the three
    // must be distinct bitmaps. A source whose image is still decoding waits
    // for the decode: the blend would otherwise read a blank. A source that
    // resolved without its pixels is an error rather than a blank frame.
    scene.resolve_bitmap(src1);
    scene.resolve_bitmap(src2);
    {
        let src1_state = scene
            .bitmap(src1)
            .ok_or_else(|| "Layer.beginTransition: the transition source is missing".to_string())?;
        super::util::bitmap_holds_pixels(src1_state, "transition source")?;
        let src2_state = scene
            .bitmap(src2)
            .ok_or_else(|| "Layer.beginTransition: the transition source is missing".to_string())?;
        super::util::bitmap_holds_pixels(src2_state, "transition source")?;
    }
    let Some((target, src1_bitmap, src2_bitmap)) = scene.bitmap_mut_with_refs(target, src1, src2)
    else {
        return Err("Layer.beginTransition: the transition bitmaps are not distinct".to_string());
    };
    let region = Trans::full_region(target);
    trans
        .apply(target, src1_bitmap, src2_bitmap, region)
        .map_err(|error| error.to_string())
}

/// The two sources of a whole-subtree transition and their shared size: the
/// destination's composited subtree content and the source layer's.
///
/// `render_layer_subtree` returns the composited subtree content, reusing it
/// while the subtree is unchanged; a trivial layer's content is its MainImage,
/// which is read as a source and never written.
fn transition_sources_subtree(
    scene: &mut Scene,
    dest: u32,
    source: u32,
) -> Result<(u32, u32, u32, u32), String> {
    let dst_content =
        crate::engine_api::layer_ops::render_layer_subtree(scene, dest).ok_or_else(|| {
            "Layer.beginTransition: cannot render the destination subtree".to_string()
        })?;
    let src_content = crate::engine_api::layer_ops::render_layer_subtree(scene, source)
        .ok_or_else(|| {
            "Layer.beginTransition: cannot render the transition source subtree".to_string()
        })?;
    // The source layer draws its rendered content. A layer that is its own
    // transition source keeps the destination's slots instead.
    if source != dest {
        scene.set_cache_bitmap(source, Some(src_content));
    }
    let (w, h) = bitmap_size(scene, dst_content).ok_or_else(|| {
        "Layer.beginTransition: the transition destination content is missing".to_string()
    })?;
    Ok((dst_content, src_content, w, h))
}

/// The two sources of a transition without children and their shared size: the
/// destination's and the source layer's own main images.
///
/// Neither main image is written; the frame blends them, so its children keep
/// drawing live.
fn transition_sources_self(
    scene: &mut Scene,
    dest: u32,
    source: u32,
) -> Result<(u32, u32, u32, u32), String> {
    let dest_bitmap = scene
        .layer(dest)
        .ok_or_else(|| "Layer.beginTransition: the transition destination is gone".to_string())?
        .bitmap
        .ok_or_else(|| {
            "Layer.beginTransition: the transition destination has no image".to_string()
        })?;
    let (w, h) = bitmap_size(scene, dest_bitmap).ok_or_else(|| {
        "Layer.beginTransition: the transition destination image is missing".to_string()
    })?;
    let source_bitmap = scene
        .layer(source)
        .and_then(|layer| layer.bitmap)
        .ok_or_else(|| "Layer.beginTransition: the transition source has no image".to_string())?;
    Ok((dest_bitmap, source_bitmap, w, h))
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
