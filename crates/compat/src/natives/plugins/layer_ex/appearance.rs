//! The `Appearance` value type: an ordered list of fills and strokes.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp`
//! (`Appearance::addBrush`/`addPen`/`clear`, `createBrush`) and
//! `blend2d/LayerExDraw.hpp` (`class Appearance`). An `Appearance` is *not*
//! a single brush: every entry is drawn in insertion order, brushes fill and
//! pens stroke. The port keeps that model and rasterizes the supported paint
//! kinds (solid, hatch, linear gradient); texture and path-gradient brushes
//! raise a typed error instead of silently drawing nothing.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use engine::raster::hatch_foreground;
use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine,
    TjsValue, Value,
};

use super::{argb_to_rgba, bound, value_as_f64};
use crate::native_util::{args, report_error, set_int, set_void};
use crate::{CompatCtx, CompatError};

/// One paint description.
#[derive(Clone, Debug)]
pub(super) enum Paint {
    /// Solid straight-alpha RGBA.
    Solid([u8; 4]),
    /// A two-color hatch pattern.
    Hatch {
        style: i32,
        fore: [u8; 4],
        back: [u8; 4],
    },
    /// A two-color linear gradient from `p1` to `p2`.
    LinearGradient {
        color1: [u8; 4],
        color2: [u8; 4],
        p1: (f64, f64),
        p2: (f64, f64),
    },
}

/// One entry of an appearance: a paint plus its display offset.
#[derive(Clone, Debug)]
pub(super) struct DrawInfo {
    pub(super) paint: Paint,
    pub(super) is_pen: bool,
    pub(super) width: f64,
    pub(super) ox: f64,
    pub(super) oy: f64,
}

/// The appearance's ordered draw list.
#[derive(Clone, Debug, Default)]
pub(super) struct AppearanceData {
    pub(super) infos: Vec<DrawInfo>,
}

fn lerp_channel(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// The straight-alpha color of `paint` at the sample point `(x, y)`.
pub(super) fn paint_color_at(paint: &Paint, x: f64, y: f64) -> [u8; 4] {
    match paint {
        Paint::Solid(color) => *color,
        Paint::Hatch { fore, back, style } => {
            if hatch_foreground(*style, x.floor() as i32, y.floor() as i32) {
                *fore
            } else {
                *back
            }
        }
        Paint::LinearGradient {
            color1,
            color2,
            p1,
            p2,
        } => {
            let dx = p2.0 - p1.0;
            let dy = p2.1 - p1.1;
            let len2 = dx * dx + dy * dy;
            let t = if len2 <= f64::EPSILON {
                0.0
            } else {
                (((x - p1.0) * dx + (y - p1.1) * dy) / len2).clamp(0.0, 1.0)
            };
            [
                lerp_channel(color1[0], color2[0], t),
                lerp_channel(color1[1], color2[1], t),
                lerp_channel(color1[2], color2[2], t),
                lerp_channel(color1[3], color2[3], t),
            ]
        }
    }
}

/// Resolve `value` to a solid ARGB color with the reference `AsInteger`
/// conversion (`reference/cpp/core/tjs2/tjsVariant.h:862-877`): the C++
/// `createBrush`/`addPen` cast every non-object colour argument with
/// `Color((tjs_int)colorOrBrush)` (`reference/cpp/plugins/layerex_draw/
/// windows/LayerExDraw.cpp:627`, `:784`) — `void` is 0 (fully transparent),
/// strings go through `TJSStringToInteger` (so `"0xFFFF0000"` is red), and
/// objects/octets raise a conversion error instead of being silently
/// zeroed.
fn argb_value(value: &Value) -> Result<[u8; 4], String> {
    Ok(argb_to_rgba(crate::natives::system::try_value_as_i64(
        value,
    )?))
}

/// The reference `TJSStringToInteger` (`reference/cpp/core/tjs2/
/// tjsVariant.cpp:301-307`) applied to a member value: strings are parsed
/// through the same helper `value_as_i64` uses, other types fall back to
/// `default`.
fn member_i64(host: &Tjs2Engine, id: tjs::Tjs2ValueId, name: &str, default: i64) -> i64 {
    match host.member(id, name) {
        Ok(TjsValue::Integer(value)) => value,
        Ok(TjsValue::Real(value)) => value as i64,
        Ok(TjsValue::String(value)) => {
            let mut bytes = value.into_bytes();
            bytes.push(0);
            let raw = Value {
                ty: tjs::VAL_STRING,
                integer: 0,
                real: 0.0,
                string: bytes.as_ptr().cast(),
                array: std::ptr::null(),
                array_count: 0,
                retained: 0,
            };
            crate::natives::system::value_as_i64(&raw)
        }
        _ => default,
    }
}

fn member_f64(host: &Tjs2Engine, id: tjs::Tjs2ValueId, name: &str, default: f64) -> f64 {
    match host.member(id, name) {
        Ok(TjsValue::Integer(value)) => value as f64,
        Ok(TjsValue::Real(value)) => value,
        _ => default,
    }
}

/// The gradient line of the reference `LinearGradientBrush(rect, color1,
/// color2, angle, ...)` form (`reference/cpp/plugins/layerex_draw/windows/
/// LayerExDraw.cpp:725-731`): a line through the centre of `rect` whose
/// direction is `angle` degrees clockwise from the +x axis (y-down screen,
/// the same orientation as the `LinearGradientMode*` mapping), spanning the
/// projection of the half-rect onto that direction. The CPU rasterizer
/// interpolates the two colours along this line — an approximation of the
/// GDI+ brush, not a pixel-exact clone.
fn gradient_line(rect: (f64, f64, f64, f64), angle_deg: f64) -> ((f64, f64), (f64, f64)) {
    let (rx, ry, rw, rh) = rect;
    let rad = angle_deg.to_radians();
    let (dx, dy) = (rad.cos(), rad.sin());
    let (cx, cy) = (rx + rw / 2.0, ry + rh / 2.0);
    let half = rw / 2.0 * dx.abs() + rh / 2.0 * dy.abs();
    (
        (cx - dx * half, cy - dy * half),
        (cx + dx * half, cy + dy * half),
    )
}

/// The `angle` member of a linear-gradient brush: `Some` whenever the
/// reference `info.HasValue(TJS_W("angle"))` check succeeds
/// (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:725`) —
/// present but non-numeric members read as 0 through `getRealValue`'s
/// `AsReal` conversion.
fn member_angle(host: &Tjs2Engine, id: tjs::Tjs2ValueId, name: &str) -> Option<f64> {
    match host.member(id, name) {
        Ok(TjsValue::Integer(value)) => Some(value as f64),
        Ok(TjsValue::Real(value)) => Some(value),
        Ok(TjsValue::String(value)) => Some(value.parse().unwrap_or(0.0)),
        Ok(_) => Some(0.0),
        Err(_) => None,
    }
}

/// Parse the `colorOrBrush` argument of `addBrush`/`addPen`.
///
/// Reference `createBrush` (`reference/cpp/plugins/layerex_draw/windows/
/// LayerExDraw.cpp:626-756`): a non-object argument is cast with
/// `Color((tjs_int)colorOrBrush)` — `void` is 0 (fully transparent),
/// strings go through `TJSStringToInteger`, octets raise a conversion
/// error. An object is a brush dictionary; `null` and arrays are objects
/// without `type`/`color` members, i.e. the dictionary defaults (opaque
/// white, `:633-638`).
pub(super) fn parse_paint(engine: *mut c_void, value: &Value, what: &str) -> Result<Paint, String> {
    match value.ty {
        tjs::VAL_INTEGER | tjs::VAL_REAL | tjs::VAL_STRING | tjs::VAL_VOID => {
            Ok(Paint::Solid(argb_value(value)?))
        }
        // TJS `null` and arrays are `tvtObject`s without `type`/`color`
        // members: `getIntValue("type", BrushTypeSolidColor)` and
        // `getIntValue("color", 0xffffffff)` yield the solid-white default
        // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:633-638`).
        tjs::VAL_NULL | tjs::VAL_ARRAY => Ok(Paint::Solid([255, 255, 255, 255])),
        tjs::VAL_OBJECT => {
            let Ok((host, _ctx)) = bound(engine) else {
                return Err(format!("{what}: compat context is not bound"));
            };
            let object = host
                .engine()
                .retain_object_arg(value)
                .map_err(|error| format!("{what}: {error}"))?;
            let ty = member_i64(host.engine(), object.raw_id(), "type", 0);
            match ty {
                0 => {
                    let color = member_i64(host.engine(), object.raw_id(), "color", 0xffff_ffff);
                    Ok(Paint::Solid(argb_to_rgba(color)))
                }
                1 => Ok(Paint::Hatch {
                    style: member_i64(host.engine(), object.raw_id(), "hatchStyle", 0) as i32,
                    fore: argb_to_rgba(member_i64(
                        host.engine(),
                        object.raw_id(),
                        "foreColor",
                        0xffff_ffff,
                    )),
                    back: argb_to_rgba(member_i64(
                        host.engine(),
                        object.raw_id(),
                        "backColor",
                        0xff00_0000,
                    )),
                }),
                4 => {
                    let color1 = member_i64(host.engine(), object.raw_id(), "color1", 0);
                    let color2 = member_i64(host.engine(), object.raw_id(), "color2", 0);
                    let p1 = super::read_point_member(host.engine(), object.raw_id(), "point1")
                        .map_err(|error| format!("{what}: {error}"))?;
                    let p2 = super::read_point_member(host.engine(), object.raw_id(), "point2")
                        .map_err(|error| format!("{what}: {error}"))?;
                    let (p1, p2) = match (p1, p2) {
                        (Some(p1), Some(p2)) => (p1, p2),
                        _ => {
                            // The `rect` form: a present `angle` member
                            // selects the angle constructor
                            // (`windows/LayerExDraw.cpp:725-731`), otherwise
                            // the `mode` enum decides the direction
                            // (`:733-744`).
                            let rect =
                                super::read_rect_member(host.engine(), object.raw_id(), "rect")
                                    .map_err(|error| format!("{what}: {error}"))?;
                            let Some(rect) = rect else {
                                return Err(format!(
                                    "{what}: a linear gradient needs point1/point2 or rect"
                                ));
                            };
                            let angle = member_angle(host.engine(), object.raw_id(), "angle");
                            if let Some(angle) = angle {
                                gradient_line(rect, angle)
                            } else {
                                let (rx, ry, rw, rh) = rect;
                                let mode = member_i64(host.engine(), object.raw_id(), "mode", 0);
                                match mode {
                                    0 => ((rx, ry), (rx + rw, ry)),
                                    1 => ((rx, ry), (rx, ry + rh)),
                                    2 => ((rx, ry), (rx + rw, ry + rh)),
                                    _ => ((rx + rw, ry), (rx, ry + rh)),
                                }
                            }
                        }
                    };
                    Ok(Paint::LinearGradient {
                        color1: argb_to_rgba(color1),
                        color2: argb_to_rgba(color2),
                        p1,
                        p2,
                    })
                }
                2 => Err(format!(
                    "{what}: texture brushes are not supported by the CPU layer rasterizer"
                )),
                3 => Err(format!(
                    "{what}: path-gradient brushes are not supported by the CPU layer rasterizer"
                )),
                other => Err(format!("{what}: unknown brush type {other}")),
            }
        }
        // `TJSOctet::AsInteger` raises `TJSThrowVariantConvertError`
        // (`reference/cpp/core/tjs2/tjsVariant.h:874`).
        _ => Err(format!("{what}: an octet is not a colour or brush")),
    }
}

/// `Appearance` payload.
pub(super) struct AppearanceInst {
    pub(super) data: AppearanceData,
}

extern "C" fn create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(AppearanceInst {
        data: AppearanceData::default(),
    })) as *mut c_void
}

extern "C" fn destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: every payload came from `create`'s `Box::into_raw`.
    drop(unsafe { Box::from_raw(instance as *mut AppearanceInst) });
}

/// `Appearance()`.
extern "C" fn ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut AppearanceInst) };
    inst.data = AppearanceData::default();
    set_void(out);
    0
}

/// Hidden property used by the `Layer.draw*` methods to recover the native
/// draw list (the ABI carries no cross-class native instance handle).
extern "C" fn handle_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, instance as usize as i64);
    0
}

/// `clear()`.
extern "C" fn clear(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut AppearanceInst) };
    inst.data.infos.clear();
    set_void(out);
    0
}

/// The reference `AsReal` conversion (`reference/cpp/core/tjs2/tjsVariant.h:
/// 947-966`) for the `REAL` parameters (`ox`/`oy`/`widthOrOption`): `void`
/// is 0, strings parse through `TJSStringToReal`, and objects/`null`/octets
/// raise a conversion error instead of silently coercing to 0.
fn strict_real(value: &Value) -> Result<f64, String> {
    match value.ty {
        tjs::VAL_INTEGER | tjs::VAL_REAL | tjs::VAL_STRING | tjs::VAL_VOID => {
            Ok(value_as_f64(value))
        }
        _ => Err("the argument is not a number".to_string()),
    }
}

/// Convert an optional `ox`/`oy` argument: an omitted argument arrives as
/// `void` and converts to `0` (ncbind default-argument passing,
/// `reference/cpp/core/plugin/ncbind.hpp:963`).
fn optional_real(value: Option<&Value>) -> Result<f64, String> {
    match value {
        Some(value) => strict_real(value),
        None => Ok(0.0),
    }
}

/// `addBrush(colorOrBrush, ox, oy)`.
///
/// Reference arity: `void Appearance::addBrush(tTJSVariant, REAL ox = 0,
/// REAL oy = 0)` (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:
/// 767`, blend2d `LayerExDraw.hpp:921`). A default argument is not part of a C++
/// member-pointer type, so ncbind's `ArgsCount` is the full parameter count and
/// a short call is rejected before the void-default path
/// (`reference/cpp/core/plugin/ncbind.hpp:921,1178`).
extern "C" fn add_brush(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.len() < 3 {
        return report_error(out_error, "Appearance.addBrush requires 3 arguments");
    }
    let value = &a[0];
    let paint = match parse_paint(engine, value, "Appearance.addBrush") {
        Ok(paint) => paint,
        Err(error) => return report_error(out_error, &error),
    };
    let (ox, oy) = match (optional_real(a.get(1)), optional_real(a.get(2))) {
        (Ok(ox), Ok(oy)) => (ox, oy),
        _ => return report_error(out_error, "Appearance.addBrush: ox and oy must be numbers"),
    };
    let inst = unsafe { &mut *(instance as *mut AppearanceInst) };
    inst.data.infos.push(DrawInfo {
        paint,
        is_pen: false,
        width: 0.0,
        ox,
        oy,
    });
    set_void(out);
    0
}

/// `addPen(colorOrBrush, widthOrOption, ox, oy)` — reference arity 4; the
/// declared `ox`/`oy` defaults do not lower ncbind's `ArgsCount`
/// (`reference/cpp/core/plugin/ncbind.hpp:921,1178`;
/// `windows/LayerExDraw.cpp:778`, blend2d `LayerExDraw.hpp:930-931`);
/// `widthOrOption` is either a number
/// (`pen->SetWidth((REAL)(tjs_real) widthOrOption)`, `:789-791` — `void` is
/// width 0) or a dictionary whose `width` member defaults to 1.0 (`:793-796`).
extern "C" fn add_pen(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.len() < 4 {
        return report_error(out_error, "Appearance.addPen requires 4 arguments");
    }
    let paint = match parse_paint(engine, &a[0], "Appearance.addPen") {
        Ok(paint) => paint,
        Err(error) => return report_error(out_error, &error),
    };
    // `widthOrOption` may be a number or a dictionary with `width`.
    let width = match a[1].ty {
        tjs::VAL_OBJECT => {
            let Ok((host, _ctx)) = bound(engine) else {
                return report_error(out_error, "Appearance.addPen: compat context is not bound");
            };
            match host.engine().retain_object_arg(&a[1]) {
                Ok(object) => member_f64(host.engine(), object.raw_id(), "width", 1.0),
                Err(_) => 1.0,
            }
        }
        // A TJS `null` is an object without a `width` member → 1.0.
        tjs::VAL_NULL => 1.0,
        tjs::VAL_INTEGER | tjs::VAL_REAL | tjs::VAL_STRING | tjs::VAL_VOID => {
            match strict_real(&a[1]) {
                Ok(width) => width,
                Err(_) => {
                    return report_error(out_error, "Appearance.addPen: width must be a number");
                }
            }
        }
        _ => return report_error(out_error, "Appearance.addPen: width must be a number"),
    };
    let (ox, oy) = match (optional_real(a.get(2)), optional_real(a.get(3))) {
        (Ok(ox), Ok(oy)) => (ox, oy),
        _ => return report_error(out_error, "Appearance.addPen: ox and oy must be numbers"),
    };
    let inst = unsafe { &mut *(instance as *mut AppearanceInst) };
    inst.data.infos.push(DrawInfo {
        paint,
        is_pen: true,
        width: width.max(0.0),
        ox,
        oy,
    });
    set_void(out);
    0
}

/// The method table.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        NativeInstanceMethodDef {
            name: "clear",
            f: clear,
        },
        NativeInstanceMethodDef {
            name: "addBrush",
            f: add_brush,
        },
        NativeInstanceMethodDef {
            name: "addPen",
            f: add_pen,
        },
    ]
}

/// The property table.
pub(super) fn property_defs() -> Vec<NativeInstancePropertyDef> {
    vec![NativeInstancePropertyDef {
        name: "__handle",
        get: Some(handle_get),
        set: None,
    }]
}

/// Register the `Appearance` class.
pub(super) fn register_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let mut methods = vec![NativeInstanceMethodDef {
        name: "Appearance",
        f: ctor,
    }];
    methods.extend(method_defs());
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Appearance",
        create,
        destroy,
        invalidate: None,
        methods,
        properties: property_defs(),
    })
}
