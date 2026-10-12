//! The GDI+ geometry value types exposed by the `layerExDraw` plugin:
//! `PointF` and `RectF`.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/windows/main.cpp`
//! (`NCB_REGISTER_SUBCLASS_DELAY(PointF)` / `(RectF)`) and
//! `blend2d/LayerExDraw.hpp` (`class PointF` / `class RectF`). The types are
//! plain value holders; their math is the pure geometry already available in
//! [`api::geometry`], so this module only marshals the TJS properties to and
//! from it.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use api::geometry::RectF as GeometryRect;
use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, NativeMethodDef,
    NativeStaticMembers, TjsValue, Value,
};

use super::{new_pointf, new_rectf, set_real};
use crate::native_util::{args, report_error, set_bool, set_int, set_void};
use crate::{CompatCtx, CompatError};

// ---------------------------------------------------------------------------
// Reference argument conversions
// ---------------------------------------------------------------------------

/// The reference `AsReal` conversion (`reference/cpp/core/tjs2/tjsVariant.h:
/// 947-966`) for the `REAL` parameters: `void`/strings coerce, objects and
/// octets raise a conversion error instead of silently coercing to 0.
fn strict_real(value: &Value) -> Result<f64, String> {
    match value.ty {
        tjs::VAL_INTEGER | tjs::VAL_REAL | tjs::VAL_STRING | tjs::VAL_VOID => {
            Ok(super::value_as_f64(value))
        }
        _ => Err("the argument is not a number".to_string()),
    }
}

/// A `PointF`-shaped argument through the reference `PointFConvertor`
/// (`reference/cpp/plugins/layerex_draw/windows/main.cpp:75-104`, blend2d
/// `LayerExDraw.cpp:2922-2943`): a non-object argument — including TJS
/// `null` — is the default `PointF()` (0, 0) and never fails. Object
/// arguments convert through the shared reader, whose typed error for a
/// malformed object stands in for the reference `getRealValue` 0-defaults
/// (never silent).
fn read_point_like(engine: *mut c_void, value: &Value, what: &str) -> Result<(f64, f64), String> {
    if value.ty != tjs::VAL_OBJECT {
        return Ok((0.0, 0.0));
    }
    super::read_point_arg(engine, value, what)
}

/// A `RectF`-shaped argument through the reference `RectFConvertor`
/// (`reference/cpp/plugins/layerex_draw/windows/main.cpp:145-178`, blend2d
/// `LayerExDraw.cpp:2965-3000`) — the rectangle twin of [`read_point_like`].
fn read_rect_like(
    engine: *mut c_void,
    value: &Value,
    what: &str,
) -> Result<(f64, f64, f64, f64), String> {
    if value.ty != tjs::VAL_OBJECT {
        return Ok((0.0, 0.0, 0.0, 0.0));
    }
    super::read_rect_arg(engine, value, what)
}

// ---------------------------------------------------------------------------
// PointF
// ---------------------------------------------------------------------------

/// `PointF` payload.
#[derive(Clone, Copy, Default)]
pub(super) struct PointInst {
    pub(super) x: f64,
    pub(super) y: f64,
}

extern "C" fn point_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(PointInst::default())) as *mut c_void
}

extern "C" fn point_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: every payload came from `point_create`'s `Box::into_raw`.
    drop(unsafe { Box::from_raw(instance as *mut PointInst) });
}

/// `PointF(x, y)` — reference arity 2 (`NCB_CONSTRUCTOR((REAL, REAL))`,
/// `reference/cpp/plugins/layerex_draw/windows/main.cpp:133-136`, blend2d
/// `LayerExDraw.cpp:2945-2947`) so a shorter call raises NCB's
/// `TJS_E_BADPARAMCOUNT` (`reference/cpp/core/plugin/ncbind.hpp:1178`),
/// with `AsReal` per argument.
extern "C" fn point_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(out_error, "PointF requires 2 arguments");
    }
    let (x, y) = match (strict_real(&a[0]), strict_real(&a[1])) {
        (Ok(x), Ok(y)) => (x, y),
        _ => return report_error(out_error, "PointF: x and y must be numbers"),
    };
    // SAFETY: `instance` is the live `PointInst` payload the VM allocated for this script object; `point_ctor` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PointInst) };
    inst.x = x;
    inst.y = y;
    set_void(out);
    0
}

/// `Equals(a)` — exact component comparison (reference `PointF::Equals`,
/// `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:344`,
/// registered at `windows/main.cpp:137`). The argument converts with the
/// reference `PointFConvertor`: a non-object argument is `PointF()`
/// (`windows/main.cpp:98-100`), so it never fails; NCB requires the one
/// parameter (`reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn point_equals(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "PointF.Equals requires 1 argument");
    }
    // SAFETY: `instance` is the live `PointInst` payload the VM allocated for this script object; no `&mut` to it is live in `point_equals`.
    let inst = unsafe { &*(instance as *const PointInst) };
    let (x, y) = match read_point_like(engine, &a[0], "PointF.Equals") {
        Ok(point) => point,
        Err(error) => return report_error(out_error, &error),
    };
    set_bool(out, inst.x == x && inst.y == y);
    0
}

extern "C" fn point_x_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `PointInst` payload the VM allocated for this script object; no `&mut` to it is live in `point_x_get`.
    let inst = unsafe { &*(instance as *const PointInst) };
    set_real(out, inst.x);
    0
}

extern "C" fn point_y_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `PointInst` payload the VM allocated for this script object; no `&mut` to it is live in `point_y_get`.
    let inst = unsafe { &*(instance as *const PointInst) };
    set_real(out, inst.y);
    0
}

extern "C" fn point_x_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let raw = unsafe { &*value };
    let x = match strict_real(raw) {
        Ok(x) => x,
        Err(error) => return report_error(out_error, &format!("PointF.x: {error}")),
    };
    // SAFETY: `instance` is the live `PointInst` payload the VM allocated for this script object; `point_x_set` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PointInst) };
    inst.x = x;
    0
}

extern "C" fn point_y_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let raw = unsafe { &*value };
    let y = match strict_real(raw) {
        Ok(y) => y,
        Err(error) => return report_error(out_error, &format!("PointF.y: {error}")),
    };
    // SAFETY: `instance` is the live `PointInst` payload the VM allocated for this script object; `point_y_set` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PointInst) };
    inst.y = y;
    0
}

// ---------------------------------------------------------------------------
// RectF
// ---------------------------------------------------------------------------

/// `RectF` payload.
#[derive(Clone, Copy, Default)]
pub(super) struct RectInst {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

extern "C" fn rect_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(RectInst::default())) as *mut c_void
}

extern "C" fn rect_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: every payload came from `rect_create`'s `Box::into_raw`.
    drop(unsafe { Box::from_raw(instance as *mut RectInst) });
}

/// `RectF(x, y, width, height)` — reference arity 4
/// (`NCB_CONSTRUCTOR((REAL, REAL, REAL, REAL))`,
/// `reference/cpp/plugins/layerex_draw/windows/main.cpp:181-184`, blend2d
/// `LayerExDraw.cpp:3001-3003`); shorter calls raise NCB's
/// `TJS_E_BADPARAMCOUNT` (`reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn rect_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 4 {
        return report_error(out_error, "RectF requires 4 arguments");
    }
    let mut components = [0.0f64; 4];
    for (slot, value) in components.iter_mut().zip(a.iter()) {
        match strict_real(value) {
            Ok(number) => *slot = number,
            Err(error) => return report_error(out_error, &format!("RectF: {error}")),
        }
    }
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; `rect_ctor` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut RectInst) };
    inst.x = components[0];
    inst.y = components[1];
    inst.w = components[2];
    inst.h = components[3];
    set_void(out);
    0
}

/// `Clone()` — a fresh `RectF` with the same components.
extern "C" fn rect_clone(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((host, _ctx)) = super::bound(engine) else {
        return report_error(out_error, "RectF.Clone: compat context is not bound");
    };
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; no `&mut` to it is live in `rect_clone`.
    let inst = unsafe { &*(instance as *const RectInst) };
    match new_rectf(host.engine(), inst.x, inst.y, inst.w, inst.h) {
        Ok(object) => {
            super::set_retained_out(out, object.raw_id());
            std::mem::forget(object);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `Equals(a)` — exact component comparison (reference `RectF::Equals`,
/// `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:370-373`,
/// registered at `windows/main.cpp:199`); the argument converts with the
/// reference `RectFConvertor` (a non-object is `RectF()` —
/// `windows/main.cpp:171-173`), and NCB requires the one parameter
/// (`reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn rect_equals(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "RectF.Equals requires 1 argument");
    }
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; no `&mut` to it is live in `rect_equals`.
    let inst = unsafe { &*(instance as *const RectInst) };
    let other = match read_rect_like(engine, &a[0], "RectF.Equals") {
        Ok(rect) => rect,
        Err(error) => return report_error(out_error, &error),
    };
    set_bool(
        out,
        inst.x == other.0 && inst.y == other.1 && inst.w == other.2 && inst.h == other.3,
    );
    0
}

/// `Inflate(width, height)` — grow the rectangle by the given amounts
/// (reference `RectF::Inflate(REAL, REAL)`,
/// `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:373-378`).
/// NCB requires both parameters (`reference/cpp/core/plugin/ncbind.hpp:1178`)
/// and converts them with `AsReal`.
extern "C" fn rect_inflate(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(out_error, "RectF.Inflate requires 2 arguments");
    }
    let (dw, dh) = match (strict_real(&a[0]), strict_real(&a[1])) {
        (Ok(dw), Ok(dh)) => (dw, dh),
        _ => return report_error(out_error, "RectF.Inflate: width and height must be numbers"),
    };
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; `rect_inflate` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut RectInst) };
    inst.x -= dw;
    inst.y -= dh;
    inst.w += dw * 2.0;
    inst.h += dh * 2.0;
    set_void(out);
    0
}

/// `InflatePoint(point)` — `Inflate(point.x, point.y)` (reference
/// `NCB_METHOD_DETAIL(Inflate, Class, void, Class::Inflate, (const PointF
/// &))`, `reference/cpp/plugins/layerex_draw/windows/main.cpp:201-202`).
/// The point converts with the lenient `PointFConvertor`, so any argument
/// is accepted (a non-object inflates by 0, 0); only the parameter count is
/// enforced (`reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn rect_inflate_point(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "RectF.InflatePoint requires 1 argument");
    }
    let (dx, dy) = match read_point_like(engine, &a[0], "RectF.InflatePoint") {
        Ok(point) => point,
        Err(error) => return report_error(out_error, &error),
    };
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; `rect_inflate_point` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut RectInst) };
    inst.x -= dx;
    inst.y -= dy;
    inst.w += dx * 2.0;
    inst.h += dy * 2.0;
    set_void(out);
    0
}

/// `IntersectsWith(rect)` — reference `RectF::IntersectsWith`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:380-385`);
/// the rectangle converts with the lenient `RectFConvertor` and NCB
/// requires the one parameter (`reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn rect_intersects(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "RectF.IntersectsWith requires 1 argument");
    }
    let (ox, oy, ow, oh) = match read_rect_like(engine, &a[0], "RectF.IntersectsWith") {
        Ok(rect) => rect,
        Err(error) => return report_error(out_error, &error),
    };
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; no `&mut` to it is live in `rect_intersects`.
    let inst = unsafe { &*(instance as *const RectInst) };
    let hits =
        !(ox > inst.x + inst.w || ox + ow < inst.x || oy > inst.y + inst.h || oy + oh < inst.y);
    set_bool(out, hits);
    0
}

/// `IsEmptyArea()`.
extern "C" fn rect_is_empty(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; no `&mut` to it is live in `rect_is_empty`.
    let inst = unsafe { &*(instance as *const RectInst) };
    set_bool(out, inst.w <= 0.0 || inst.h <= 0.0);
    0
}

/// `Offset(dx, dy)` — reference `RectF::Offset(tjs_real, tjs_real)`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:385-388`);
/// NCB requires both parameters (`reference/cpp/core/plugin/ncbind.hpp:1178`)
/// and converts them with `AsReal`.
extern "C" fn rect_offset(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(out_error, "RectF.Offset requires 2 arguments");
    }
    let (dx, dy) = match (strict_real(&a[0]), strict_real(&a[1])) {
        (Ok(dx), Ok(dy)) => (dx, dy),
        _ => return report_error(out_error, "RectF.Offset: dx and dy must be numbers"),
    };
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; `rect_offset` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut RectInst) };
    inst.x += dx;
    inst.y += dy;
    set_void(out);
    0
}

/// `Union(rect)` / `Union(a, b)` — the receiver form is a port extension;
/// the reference registers the `static bool Union(RectF &ret, const RectF
/// &a, const RectF &b)` (`reference/cpp/plugins/layerex_draw/blend2d/
/// LayerExDraw.hpp:389-397`, `windows/main.cpp:210`) which is registered
/// separately as the class-level [`static_union`]. Rectangles convert with
/// the lenient `RectFConvertor`, so any argument is accepted.
extern "C" fn rect_union(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((host, _ctx)) = super::bound(engine) else {
        return report_error(out_error, "RectF.Union: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; no `&mut` to it is live in `rect_union`.
    let inst = unsafe { &*(instance as *const RectInst) };
    let result = match a.len() {
        0 => (inst.x, inst.y, inst.w, inst.h),
        1 => {
            let r = match read_rect_like(engine, &a[0], "RectF.Union") {
                Ok(rect) => rect,
                Err(error) => return report_error(out_error, &error),
            };
            let union =
                GeometryRect::new(inst.x as f32, inst.y as f32, inst.w as f32, inst.h as f32)
                    .union(&GeometryRect::new(
                        r.0 as f32, r.1 as f32, r.2 as f32, r.3 as f32,
                    ));
            (
                union.x as f64,
                union.y as f64,
                union.width as f64,
                union.height as f64,
            )
        }
        2 => {
            let ra = match read_rect_like(engine, &a[0], "RectF.Union") {
                Ok(rect) => rect,
                Err(error) => return report_error(out_error, &error),
            };
            let rb = match read_rect_like(engine, &a[1], "RectF.Union") {
                Ok(rect) => rect,
                Err(error) => return report_error(out_error, &error),
            };
            let union =
                GeometryRect::new(ra.0 as f32, ra.1 as f32, ra.2 as f32, ra.3 as f32).union(
                    &GeometryRect::new(rb.0 as f32, rb.1 as f32, rb.2 as f32, rb.3 as f32),
                );
            (
                union.x as f64,
                union.y as f64,
                union.width as f64,
                union.height as f64,
            )
        }
        _ => {
            return report_error(out_error, "RectF.Union accepts at most two rectangles");
        }
    };
    match new_rectf(host.engine(), result.0, result.1, result.2, result.3) {
        Ok(object) => {
            super::set_retained_out(out, object.raw_id());
            std::mem::forget(object);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// The reference class-level `static bool Union(RectF &ret, const RectF &a,
/// const RectF &b)` (`reference/cpp/plugins/layerex_draw/blend2d/
/// LayerExDraw.hpp:389-397`, registered with `NCB_METHOD(Union)` at
/// `windows/main.cpp:210` / blend2d `LayerExDraw.cpp:3022`, so NCB requires
/// three parameters — `reference/cpp/core/plugin/ncbind.hpp:1178`): the
/// first rectangle is overwritten with the bounding rectangle of the other
/// two and the call returns `true` (a TJS integer 1). The target must be an
/// object — a non-object would give the reference a null `RectF&` and crash,
/// which the port reports as a typed error instead.
extern "C" fn static_union(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 3 {
        return report_error(out_error, "RectF.Union requires 3 arguments");
    }
    if a[0].ty != tjs::VAL_OBJECT {
        return report_error(out_error, "RectF.Union: the first argument must be a RectF");
    }
    let Ok((host, _ctx)) = super::bound(engine) else {
        return report_error(out_error, "RectF.Union: compat context is not bound");
    };
    let ra = match read_rect_like(engine, &a[1], "RectF.Union") {
        Ok(rect) => rect,
        Err(error) => return report_error(out_error, &error),
    };
    let rb = match read_rect_like(engine, &a[2], "RectF.Union") {
        Ok(rect) => rect,
        Err(error) => return report_error(out_error, &error),
    };
    let union = GeometryRect::new(ra.0 as f32, ra.1 as f32, ra.2 as f32, ra.3 as f32).union(
        &GeometryRect::new(rb.0 as f32, rb.1 as f32, rb.2 as f32, rb.3 as f32),
    );
    let Ok(target) = host.engine().retain_object_arg(&a[0]) else {
        return report_error(
            out_error,
            "RectF.Union: the first argument is not an object",
        );
    };
    let id = target.raw_id();
    let set_component = |name: &str, value: f64| -> Result<(), String> {
        host.engine()
            .set_member(id, name, &TjsValue::Real(value))
            .map_err(|error| format!("RectF.Union: cannot write {name}: {error}"))
    };
    for (name, value) in [
        ("x", union.x as f64),
        ("y", union.y as f64),
        ("width", union.width as f64),
        ("height", union.height as f64),
    ] {
        if let Err(error) = set_component(name, value) {
            return report_error(out_error, &error);
        }
    }
    set_int(out, 1);
    0
}

// ---------------------------------------------------------------------------
// RectF properties
// ---------------------------------------------------------------------------

macro_rules! rect_component {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
            let inst = unsafe { &*(instance as *const RectInst) };
            set_real(out, inst.$field);
            0
        }

        // `NCB_MEMBER_PROPERTY($field, REAL, ...)` converts the assigned
        // value with `AsReal` (`reference/cpp/plugins/layerex_draw/windows/
        // main.cpp:185-188`, `reference/cpp/core/tjs2/tjsVariant.h:947-966`),
        // which raises on object/octet arguments.
        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
            let raw = unsafe { &*value };
            let number = match strict_real(raw) {
                Ok(number) => number,
                Err(error) => return report_error(out_error, &format!("RectF property: {error}")),
            };
            // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; this callback is its only live accessor.
            let inst = unsafe { &mut *(instance as *mut RectInst) };
            inst.$field = number;
            0
        }
    };
}

rect_component!(rect_x_get, rect_x_set, x);
rect_component!(rect_y_get, rect_y_set, y);
rect_component!(rect_w_get, rect_w_set, w);
rect_component!(rect_h_get, rect_h_set, h);

macro_rules! rect_derived {
    ($name:ident, $expr:expr) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
            let inst = unsafe { &*(instance as *const RectInst) };
            let f: fn(&RectInst) -> f64 = $expr;
            set_real(out, f(inst));
            0
        }
    };
}

rect_derived!(rect_left_get, |r| r.x);
rect_derived!(rect_top_get, |r| r.y);
rect_derived!(rect_right_get, |r| r.x + r.w);
rect_derived!(rect_bottom_get, |r| r.y + r.h);

/// `location` — the minimum corner as a `PointF`.
extern "C" fn rect_location_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((host, _ctx)) = super::bound(engine) else {
        return report_error(out_error, "RectF.location: compat context is not bound");
    };
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; no `&mut` to it is live in `rect_location_get`.
    let inst = unsafe { &*(instance as *const RectInst) };
    match new_pointf(host.engine(), inst.x, inst.y) {
        Ok(object) => {
            super::set_retained_out(out, object.raw_id());
            std::mem::forget(object);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `bounds` — a fresh `RectF` with the same components (reference
/// `GetBounds(RectF&)`).
extern "C" fn rect_bounds_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((host, _ctx)) = super::bound(engine) else {
        return report_error(out_error, "RectF.bounds: compat context is not bound");
    };
    // SAFETY: `instance` is the live `RectInst` payload the VM allocated for this script object; no `&mut` to it is live in `rect_bounds_get`.
    let inst = unsafe { &*(instance as *const RectInst) };
    match new_rectf(host.engine(), inst.x, inst.y, inst.w, inst.h) {
        Ok(object) => {
            super::set_retained_out(out, object.raw_id());
            std::mem::forget(object);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub(super) fn point_method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![NativeInstanceMethodDef {
        name: "Equals",
        f: point_equals,
    }]
}

pub(super) fn point_property_defs() -> Vec<NativeInstancePropertyDef> {
    vec![
        NativeInstancePropertyDef {
            name: "x",
            get: Some(point_x_get),
            set: Some(point_x_set),
        },
        NativeInstancePropertyDef {
            name: "y",
            get: Some(point_y_get),
            set: Some(point_y_set),
        },
    ]
}

pub(super) fn rect_method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        NativeInstanceMethodDef {
            name: "Clone",
            f: rect_clone,
        },
        NativeInstanceMethodDef {
            name: "Equals",
            f: rect_equals,
        },
        NativeInstanceMethodDef {
            name: "Inflate",
            f: rect_inflate,
        },
        NativeInstanceMethodDef {
            name: "InflatePoint",
            f: rect_inflate_point,
        },
        NativeInstanceMethodDef {
            name: "IntersectsWith",
            f: rect_intersects,
        },
        NativeInstanceMethodDef {
            name: "IsEmptyArea",
            f: rect_is_empty,
        },
        NativeInstanceMethodDef {
            name: "Offset",
            f: rect_offset,
        },
        NativeInstanceMethodDef {
            name: "Union",
            f: rect_union,
        },
    ]
}

pub(super) fn rect_property_defs() -> Vec<NativeInstancePropertyDef> {
    vec![
        NativeInstancePropertyDef {
            name: "x",
            get: Some(rect_x_get),
            set: Some(rect_x_set),
        },
        NativeInstancePropertyDef {
            name: "y",
            get: Some(rect_y_get),
            set: Some(rect_y_set),
        },
        NativeInstancePropertyDef {
            name: "width",
            get: Some(rect_w_get),
            set: Some(rect_w_set),
        },
        NativeInstancePropertyDef {
            name: "height",
            get: Some(rect_h_get),
            set: Some(rect_h_set),
        },
        NativeInstancePropertyDef {
            name: "left",
            get: Some(rect_left_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "top",
            get: Some(rect_top_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "right",
            get: Some(rect_right_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "bottom",
            get: Some(rect_bottom_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "location",
            get: Some(rect_location_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "bounds",
            get: Some(rect_bounds_get),
            set: None,
        },
    ]
}

/// Register the `PointF` and `RectF` classes.
pub(super) fn register_classes(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let mut point_methods = vec![NativeInstanceMethodDef {
        name: "PointF",
        f: point_ctor,
    }];
    point_methods.extend(point_method_defs());
    ctx.register_instance(&NativeInstanceBuilder {
        name: "PointF",
        create: point_create,
        destroy: point_destroy,
        invalidate: None,
        methods: point_methods,
        properties: point_property_defs(),
    })?;

    let mut rect_methods = vec![NativeInstanceMethodDef {
        name: "RectF",
        f: rect_ctor,
    }];
    rect_methods.extend(rect_method_defs());
    ctx.register_instance(&NativeInstanceBuilder {
        name: "RectF",
        create: rect_create,
        destroy: rect_destroy,
        invalidate: None,
        methods: rect_methods,
        properties: rect_property_defs(),
    })?;

    // The reference `Union` is a *static* member (`static bool Union(RectF
    // &ret, const RectF &a, const RectF &b)`, blend2d `LayerExDraw.hpp:389`):
    // it is declared with `TJS_STATICMEMBER`, lives on the class object and
    // stays invisible to instances (`reference/cpp/core/plugin/ncbind.hpp:
    // 1218-1233`).
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "RectF",
        methods: vec![NativeMethodDef {
            name: "Union",
            f: static_union,
        }],
        properties: Vec::new(),
    })?;
    Ok(())
}
