//! The `GdiPlus.Matrix` value type.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp`
//! (`MatrixConvertor`, `MatrixFactory`, `NCB_REGISTER_GDIP_SUBCLASS2(
//! GdipMatrix, MatrixConvertor)`) and `blend2d/LayerExDraw.hpp`
//! (`class GdipMatrix`). The wrapper holds a blend2d `BLMatrix2D` plus the
//! cached property values `OffsetX`/`OffsetY` report. The port models the
//! matrix with the shared [`super::draw::Mat`] affine type and replays the
//! reference's operation order and cached-value updates.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeInstanceBuilder, NativeInstanceMethodDef, TjsValue, Value};

use super::draw::Mat;
use super::set_real;
use crate::native_util::{args, report_error, set_bool, set_int, set_void};
use crate::{CompatCtx, CompatError};

/// `MatrixOrderPrepend` (`reference/cpp/plugins/layerex_draw/blend2d/
/// LayerExDraw.hpp:208`).
const MATRIX_ORDER_PREPEND: i64 = 0;

/// The reference `GdipMatrix` payload (`reference/cpp/plugins/layerex_draw/
/// blend2d/LayerExDraw.hpp:418-597`): the affine core plus the cached values
/// `OffsetX`/`OffsetY` return.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MatrixInst {
    core: Mat,
    offsetx: f64,
    offsety: f64,
    rotation: f64,
    scale: f64,
    shearx: f64,
    sheary: f64,
}

impl Default for MatrixInst {
    fn default() -> Self {
        // `GdipMatrix()` leaves `_core` at its identity default and the
        // tracked members at their in-class initializers.
        MatrixInst {
            core: Mat::IDENTITY,
            offsetx: 0.0,
            offsety: 0.0,
            rotation: 0.0,
            scale: 1.0,
            shearx: 0.0,
            sheary: 0.0,
        }
    }
}

impl MatrixInst {
    /// Reference `GdipMatrix::updateProperties`
    /// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:578-593`):
    /// recompute the cached values from the core. The reference narrows the
    /// core to `float` before the scale/rotation math, which the port keeps.
    fn update_properties(&mut self) {
        let m00 = self.core.m11 as f32;
        let m01 = self.core.m12 as f32;
        let m10 = self.core.m21 as f32;
        let m11 = self.core.m22 as f32;
        self.offsetx = self.core.dx;
        self.offsety = self.core.dy;
        self.scale = f64::from(m00 * m00 + m01 * m01).sqrt();
        self.rotation = if f64::from(m00).abs() > 1e-6 {
            f64::from(m01).atan2(f64::from(m00))
        } else {
            f64::from(-m10).atan2(f64::from(m11))
        };
        self.shearx = f64::from(m10) / self.scale;
        self.sheary = f64::from(m01) / self.scale;
    }

    /// Reference `GdipMatrix::updateOffset`.
    fn update_offset(&mut self) {
        self.offsetx = self.core.dx;
        self.offsety = self.core.dy;
    }
}

/// The reference `BLMatrix2D::makeRotation` affine.
fn rotation_matrix(rad: f64) -> Mat {
    let (s, c) = rad.sin_cos();
    Mat {
        m11: c,
        m12: s,
        m21: -s,
        m22: c,
        dx: 0.0,
        dy: 0.0,
    }
}

/// The reference `BLMatrix2D::makeScaling` affine.
fn scaling_matrix(sx: f64, sy: f64) -> Mat {
    Mat {
        m11: sx,
        m22: sy,
        ..Mat::IDENTITY
    }
}

/// The reference `BLMatrix2D::invert` affine, or `None` when the
/// determinant is zero (the reference `BL_ERROR_INVALID_MATRIX`).
fn invert(matrix: &Mat) -> Option<Mat> {
    let det = matrix.m11 * matrix.m22 - matrix.m12 * matrix.m21;
    if det == 0.0 {
        return None;
    }
    let inv = 1.0 / det;
    Some(Mat {
        m11: matrix.m22 * inv,
        m12: -matrix.m12 * inv,
        m21: -matrix.m21 * inv,
        m22: matrix.m11 * inv,
        dx: (matrix.m21 * matrix.dy - matrix.m22 * matrix.dx) * inv,
        dy: (matrix.m12 * matrix.dx - matrix.m11 * matrix.dy) * inv,
    })
}

/// The reference `AsReal` conversion (`reference/cpp/core/tjs2/tjsVariant.h:
/// 947-966`): `void`/strings coerce, objects and octets raise.
fn strict_real(value: &Value) -> Result<f64, String> {
    match value.ty {
        tjs::VAL_INTEGER | tjs::VAL_REAL | tjs::VAL_STRING | tjs::VAL_VOID => {
            Ok(super::value_as_f64(value))
        }
        _ => Err("the argument is not a number".to_string()),
    }
}

/// The reference `MatrixConvertor` (`reference/cpp/plugins/layerex_draw/
/// blend2d/LayerExDraw.cpp:3124-3157`): a non-object is NULL, a native
/// `Matrix` unwraps its core, and anything else is read as an array of six
/// reals or an object with `m11`/`m12`/`m21`/`m22`/`dx`/`dy`.
fn read_matrix_arg(engine: *mut c_void, value: &Value, what: &str) -> Result<Option<Mat>, String> {
    let Ok((resolved, _ctx)) = super::bound(engine) else {
        return Err(format!("{what}: compat context is not bound"));
    };
    let host = resolved.engine();
    if value.ty != tjs::VAL_OBJECT {
        return Ok(None);
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("{what}: the matrix argument is not an object: {error}"))?;
    let payload = host
        .native_instance_payload(object.raw_id(), "Matrix")
        .map_err(|error| format!("{what}: {error}"))?;
    if let Some(payload) = payload {
        // SAFETY: the native-instance lookup returned a `Matrix` payload
        // created by this module's `create`, so it is a `MatrixInst`.
        let inst = unsafe { &*(payload.as_ptr() as *const MatrixInst) };
        return Ok(Some(inst.core));
    }
    let number = |name: &str| -> Result<f64, String> {
        match host.member(object.raw_id(), name) {
            Ok(TjsValue::Integer(value)) => Ok(value as f64),
            Ok(TjsValue::Real(value)) => Ok(value),
            Ok(_) => Err(format!("{what}: the matrix member {name} is not numeric")),
            Err(error) => Err(format!("{what}: the matrix member {name}: {error}")),
        }
    };
    let is_array = matches!(
        host.member(object.raw_id(), "count"),
        Ok(TjsValue::Integer(count)) if count >= 6
    );
    if is_array {
        let mut values = [0.0f64; 6];
        for (index, slot) in values.iter_mut().enumerate() {
            *slot = number(&index.to_string())?;
        }
        return Ok(Some(Mat {
            m11: values[0],
            m12: values[1],
            m21: values[2],
            m22: values[3],
            dx: values[4],
            dy: values[5],
        }));
    }
    Ok(Some(Mat {
        m11: number("m11")?,
        m12: number("m12")?,
        m21: number("m21")?,
        m22: number("m22")?,
        dx: number("dx")?,
        dy: number("dy")?,
    }))
}

/// Read a `MatrixOrder` argument with the reference `AsInteger` conversion
/// (`reference/cpp/core/plugin/ncbind.hpp:404`, `reference/cpp/core/tjs2/
/// tjsVariant.h:862-877`), which raises on object/null/octet arguments.
fn read_order(value: &Value, what: &str) -> Result<i64, String> {
    crate::natives::system::try_value_as_i64(value).map_err(|error| format!("{what}: {error}"))
}

extern "C" fn create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(MatrixInst::default())) as *mut c_void
}

extern "C" fn destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: every payload came from `create`'s `Box::into_raw`.
    drop(unsafe { Box::from_raw(instance as *mut MatrixInst) });
}

/// `Matrix(...)` — the reference `MatrixFactory` (`reference/cpp/plugins/
/// layerex_draw/blend2d/LayerExDraw.cpp:3206-3285`): no arguments is the
/// identity, six arguments are the six matrix elements, and any other arity
/// returns `TJS_E_INVALIDPARAM`. The two-argument `(RectF, points)` branch
/// never matches in the reference (it reads the PointF adaptor from
/// `params[0]`), so it also raises.
extern "C" fn ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    if a.is_empty() {
        *inst = MatrixInst::default();
        set_void(out);
        return 0;
    }
    if a.len() == 6 {
        let mut values = [0.0f64; 6];
        for (slot, value) in values.iter_mut().zip(a.iter()) {
            match strict_real(value) {
                Ok(number) => *slot = number,
                Err(error) => return report_error(out_error, &format!("GdiPlus.Matrix: {error}")),
            }
        }
        inst.core = Mat {
            m11: values[0],
            m12: values[1],
            m21: values[2],
            m22: values[3],
            dx: values[4],
            dy: values[5],
        };
        inst.offsetx = 0.0;
        inst.offsety = 0.0;
        inst.rotation = 0.0;
        inst.scale = 1.0;
        inst.shearx = 0.0;
        inst.sheary = 0.0;
        set_void(out);
        return 0;
    }
    report_error(out_error, "GdiPlus.Matrix: invalid parameter")
}

/// `OffsetX()` / `OffsetY()`.
macro_rules! offset_getter {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { &*(instance as *const MatrixInst) };
            set_real(out, inst.$field);
            0
        }
    };
}

offset_getter!(offset_x, offsetx);
offset_getter!(offset_y, offsety);

/// `Equals(other)` — exact core comparison (reference `GdipMatrix::Equals`,
/// `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:432-434`).
/// A non-object argument is the reference's null matrix; the reference would
/// dereference it, so the port raises a typed error instead.
extern "C" fn equals(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "GdiPlus.Matrix.Equals requires 1 argument");
    }
    let other = match read_matrix_arg(engine, &a[0], "GdiPlus.Matrix.Equals") {
        Ok(Some(matrix)) => matrix,
        Ok(None) => {
            return report_error(
                out_error,
                "GdiPlus.Matrix.Equals: the argument is not a matrix",
            );
        }
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const MatrixInst) };
    set_bool(out, inst.core == other);
    0
}

/// `SetElements(m11, m12, m21, m22, dx, dy)` — replace the core and refresh
/// the cached values (reference `GdipMatrix::SetElements`,
/// `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:443-455`).
extern "C" fn set_elements(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.len() < 6 {
        return report_error(out_error, "GdiPlus.Matrix.SetElements requires 6 arguments");
    }
    let mut values = [0.0f64; 6];
    for (slot, value) in values.iter_mut().zip(a.iter()) {
        match strict_real(value) {
            Ok(number) => *slot = number,
            Err(error) => {
                return report_error(out_error, &format!("GdiPlus.Matrix.SetElements: {error}"));
            }
        }
    }
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    inst.core = Mat {
        m11: values[0],
        m12: values[1],
        m21: values[2],
        m22: values[3],
        dx: values[4],
        dy: values[5],
    };
    inst.update_properties();
    set_int(out, 0);
    0
}

/// `GetLastStatus()` — the reference always returns `Ok`.
extern "C" fn get_last_status(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, 0);
    0
}

/// `Invert()` — reference `GdipMatrix::Invert` (`reference/cpp/plugins/
/// layerex_draw/blend2d/LayerExDraw.hpp:459-466`): `Ok` (0) on success,
/// `InvalidParameter` (2) when the matrix is singular.
extern "C" fn invert_method(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    match invert(&inst.core) {
        Some(inverse) => {
            inst.core = inverse;
            inst.update_properties();
            set_int(out, 0);
        }
        None => set_int(out, 2),
    }
    0
}

/// `IsIdentity()` — exact core equality with the identity.
extern "C" fn is_identity(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const MatrixInst) };
    set_bool(out, inst.core == Mat::IDENTITY);
    0
}

/// `IsInvertible()` — nonzero determinant.
extern "C" fn is_invertible(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const MatrixInst) };
    set_bool(out, invert(&inst.core).is_some());
    0
}

/// `Multiply(matrix, order)` — reference `GdipMatrix::Multiply`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:476-490`):
/// `InvalidParameter` (2) for a null matrix, otherwise the product in the
/// requested order.
extern "C" fn multiply(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(out_error, "GdiPlus.Matrix.Multiply requires 2 arguments");
    }
    let other = match read_matrix_arg(engine, &a[0], "GdiPlus.Matrix.Multiply") {
        Ok(Some(matrix)) => matrix,
        Ok(None) => {
            set_int(out, 2);
            return 0;
        }
        Err(error) => return report_error(out_error, &error),
    };
    let order = match read_order(&a[1], "GdiPlus.Matrix.Multiply") {
        Ok(order) => order,
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    inst.core = if order == MATRIX_ORDER_PREPEND {
        inst.core.mul(&other)
    } else {
        other.mul(&inst.core)
    };
    inst.update_properties();
    set_int(out, 0);
    0
}

/// `Reset()` — identity core and default cached values.
extern "C" fn reset(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    *inst = MatrixInst::default();
    set_void(out);
    0
}

/// `Rotate(angle, order)` — reference `GdipMatrix::Rotate` (`reference/cpp/
/// plugins/layerex_draw/blend2d/LayerExDraw.hpp:502-516`). The angle is
/// degrees; the reference narrows the radians to `float`.
extern "C" fn rotate(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(out_error, "GdiPlus.Matrix.Rotate requires 2 arguments");
    }
    let angle = match strict_real(&a[0]) {
        Ok(angle) => angle,
        Err(error) => return report_error(out_error, &format!("GdiPlus.Matrix.Rotate: {error}")),
    };
    let order = match read_order(&a[1], "GdiPlus.Matrix.Rotate") {
        Ok(order) => order,
        Err(error) => return report_error(out_error, &error),
    };
    let rad = (angle * std::f64::consts::PI / 180.0) as f32;
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    inst.rotation += f64::from(rad);
    if order == MATRIX_ORDER_PREPEND {
        inst.core = inst.core.mul(&rotation_matrix(f64::from(rad)));
    } else {
        let rotation = rotation_matrix(f64::from(rad));
        inst.core = rotation.mul(&inst.core);
    }
    inst.update_offset();
    set_void(out);
    0
}

/// `RotateAt(angle, center, order)` — reference `GdipMatrix::RotateAt`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:518-537`).
/// The center converts with the lenient `PointFConvertor`, so a non-object
/// is `PointF(0, 0)`.
extern "C" fn rotate_at(
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
        return report_error(out_error, "GdiPlus.Matrix.RotateAt requires 3 arguments");
    }
    let angle = match strict_real(&a[0]) {
        Ok(angle) => angle,
        Err(error) => return report_error(out_error, &format!("GdiPlus.Matrix.RotateAt: {error}")),
    };
    let (cx, cy) = match read_point_arg(engine, &a[1], "GdiPlus.Matrix.RotateAt") {
        Ok(center) => center,
        Err(error) => return report_error(out_error, &error),
    };
    let order = match read_order(&a[2], "GdiPlus.Matrix.RotateAt") {
        Ok(order) => order,
        Err(error) => return report_error(out_error, &error),
    };
    let rad = (angle * std::f64::consts::PI / 180.0) as f32;
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    if order == MATRIX_ORDER_PREPEND {
        inst.core = inst.core.mul(&Mat::translate(-cx, -cy));
        inst.core = inst.core.mul(&rotation_matrix(f64::from(rad)));
        inst.core = inst.core.mul(&Mat::translate(cx, cy));
    } else {
        let mat = Mat::IDENTITY
            .mul(&Mat::translate(-cx, -cy))
            .mul(&rotation_matrix(f64::from(rad)))
            .mul(&Mat::translate(cx, cy));
        inst.core = mat.mul(&inst.core);
    }
    inst.rotation += f64::from(rad);
    inst.update_properties();
    set_void(out);
    0
}

/// `Scale(sx, sy, order)` — reference `GdipMatrix::Scale` (`reference/cpp/
/// plugins/layerex_draw/blend2d/LayerExDraw.hpp:539-551`).
extern "C" fn scale(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.len() < 3 {
        return report_error(out_error, "GdiPlus.Matrix.Scale requires 3 arguments");
    }
    let (sx, sy) = match (strict_real(&a[0]), strict_real(&a[1])) {
        (Ok(sx), Ok(sy)) => (sx, sy),
        _ => {
            return report_error(
                out_error,
                "GdiPlus.Matrix.Scale: the factors must be numbers",
            );
        }
    };
    let order = match read_order(&a[2], "GdiPlus.Matrix.Scale") {
        Ok(order) => order,
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    inst.scale *= sx.abs().max(sy.abs());
    if order == MATRIX_ORDER_PREPEND {
        inst.core = inst.core.mul(&scaling_matrix(sx, sy));
    } else {
        let scaling = scaling_matrix(sx, sy);
        inst.core = scaling.mul(&inst.core);
    }
    inst.update_offset();
    set_void(out);
    0
}

/// `Shear(shx, shy, order)` — reference `GdipMatrix::Shear` (`reference/cpp/
/// plugins/layerex_draw/blend2d/LayerExDraw.hpp:553-570`).
extern "C" fn shear(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.len() < 3 {
        return report_error(out_error, "GdiPlus.Matrix.Shear requires 3 arguments");
    }
    let (shx, shy) = match (strict_real(&a[0]), strict_real(&a[1])) {
        (Ok(shx), Ok(shy)) => (shx, shy),
        _ => {
            return report_error(
                out_error,
                "GdiPlus.Matrix.Shear: the factors must be numbers",
            );
        }
    };
    let order = match read_order(&a[2], "GdiPlus.Matrix.Shear") {
        Ok(order) => order,
        Err(error) => return report_error(out_error, &error),
    };
    let shear = Mat {
        m11: 1.0,
        m12: shy,
        m21: shx,
        m22: 1.0,
        dx: 0.0,
        dy: 0.0,
    };
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    inst.shearx += shx;
    inst.sheary += shy;
    if order == MATRIX_ORDER_PREPEND {
        inst.core = inst.core.mul(&shear);
    } else {
        inst.core = shear.mul(&inst.core);
    }
    inst.update_offset();
    set_void(out);
    0
}

/// `Translate(dx, dy, order)` — reference `GdipMatrix::Translate`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:572-584`).
extern "C" fn translate(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.len() < 3 {
        return report_error(out_error, "GdiPlus.Matrix.Translate requires 3 arguments");
    }
    let (dx, dy) = match (strict_real(&a[0]), strict_real(&a[1])) {
        (Ok(dx), Ok(dy)) => (dx, dy),
        _ => {
            return report_error(
                out_error,
                "GdiPlus.Matrix.Translate: the offsets must be numbers",
            );
        }
    };
    let order = match read_order(&a[2], "GdiPlus.Matrix.Translate") {
        Ok(order) => order,
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &mut *(instance as *mut MatrixInst) };
    inst.offsetx += dx;
    inst.offsety += dy;
    if order == MATRIX_ORDER_PREPEND {
        inst.core = inst.core.mul(&Mat::translate(dx, dy));
    } else {
        let translation = Mat::translate(dx, dy);
        inst.core = translation.mul(&inst.core);
    }
    set_void(out);
    0
}

/// Read a `PointF`-shaped argument with the reference lenient
/// `PointFConvertor` (`reference/cpp/plugins/layerex_draw/blend2d/
/// LayerExDraw.cpp:2922-2943`): a non-object is `PointF(0, 0)`.
fn read_point_arg(engine: *mut c_void, value: &Value, what: &str) -> Result<(f64, f64), String> {
    if value.ty != tjs::VAL_OBJECT {
        return Ok((0.0, 0.0));
    }
    super::read_point_arg(engine, value, what)
}

/// The method table.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        NativeInstanceMethodDef {
            name: "OffsetX",
            f: offset_x,
        },
        NativeInstanceMethodDef {
            name: "OffsetY",
            f: offset_y,
        },
        NativeInstanceMethodDef {
            name: "Equals",
            f: equals,
        },
        NativeInstanceMethodDef {
            name: "SetElements",
            f: set_elements,
        },
        NativeInstanceMethodDef {
            name: "GetLastStatus",
            f: get_last_status,
        },
        NativeInstanceMethodDef {
            name: "Invert",
            f: invert_method,
        },
        NativeInstanceMethodDef {
            name: "IsIdentity",
            f: is_identity,
        },
        NativeInstanceMethodDef {
            name: "IsInvertible",
            f: is_invertible,
        },
        NativeInstanceMethodDef {
            name: "Multiply",
            f: multiply,
        },
        NativeInstanceMethodDef {
            name: "Reset",
            f: reset,
        },
        NativeInstanceMethodDef {
            name: "Rotate",
            f: rotate,
        },
        NativeInstanceMethodDef {
            name: "RotateAt",
            f: rotate_at,
        },
        NativeInstanceMethodDef {
            name: "Scale",
            f: scale,
        },
        NativeInstanceMethodDef {
            name: "Shear",
            f: shear,
        },
        NativeInstanceMethodDef {
            name: "Translate",
            f: translate,
        },
    ]
}

/// Register the `Matrix` class.
pub(super) fn register_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let mut methods = vec![NativeInstanceMethodDef {
        name: "Matrix",
        f: ctor,
    }];
    methods.extend(method_defs());
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Matrix",
        create,
        destroy,
        invalidate: None,
        methods,
        properties: Vec::new(),
    })
}
