//! Geometry hit-test classes and the separate-layer / D3D / source-cache
//! adaptors.
//!
//! Behavioural reference: `reference/cpp/plugins/motionplayer/main.cpp`
//! (`NCB_REGISTER_SUBCLASS_DELAY(Point/Circle/Rect/Quad/LayerGetter/
//! SeparateLayerAdaptor/D3DAdaptor/SourceCache/ObjSource)`),
//! `SeparateLayerAdaptor.cpp`, `D3DAdaptor.cpp`, `SourceCache.cpp` and
//! `HitTestInternal.h`.
#![allow(unsafe_code)]

use std::ffi::c_void;

use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    TjsValue, VAL_OBJECT, Value,
};

use super::StoredValue;
use super::helpers::{inst_mut, set_retained, value_as_f64};
use crate::native_util::{args, report_error, set_void};
use crate::natives::system::{resolve, try_value_as_i64, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

// ---------------------------------------------------------------------------
// Point / Circle / Rect / Quad
// ---------------------------------------------------------------------------

/// `Motion.Point` payload.
pub(crate) struct PointInst {
    x: f64,
    y: f64,
}

impl Default for PointInst {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}

/// `Motion.Circle` payload.
pub(crate) struct CircleInst {
    x: f64,
    y: f64,
    r: f64,
}

impl Default for CircleInst {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            r: 0.0,
        }
    }
}

/// `Motion.Rect` payload.
pub(crate) struct RectInst {
    l: f64,
    t: f64,
    w: f64,
    h: f64,
}

impl Default for RectInst {
    fn default() -> Self {
        Self {
            l: 0.0,
            t: 0.0,
            w: 0.0,
            h: 0.0,
        }
    }
}

/// `Motion.Quad` payload.
///
/// The reference stores four corners but exposes them through nothing: the
/// registered `p` getter is a stub returning void
/// (`reference/cpp/plugins/motionplayer/SourceCache.h:158-162`), so the
/// payload carries no script-readable state.
#[derive(Default)]
pub(crate) struct QuadInst;

/// `Motion.LayerGetter` payload.
///
/// Defaults mirror the reference class
/// (`reference/cpp/plugins/motionplayer/SourceCache.h:205-224`): visible
/// flags start true, zoom starts at 1.0 and `opacity` is the integer 255.
pub(crate) struct LayerGetterInst {
    kind: i32,
    label: String,
    visible: bool,
    x: f64,
    y: f64,
    opacity: i64,
    angle: f64,
}

impl Default for LayerGetterInst {
    fn default() -> Self {
        Self {
            kind: 0,
            label: String::new(),
            visible: true,
            x: 0.0,
            y: 0.0,
            opacity: 255,
            angle: 0.0,
        }
    }
}

macro_rules! simple_ctor {
    ($create:ident, $destroy:ident, $ty:ty) => {
        extern "C" fn $create(_engine: *mut c_void) -> *mut c_void {
            Box::into_raw(Box::new(<$ty>::default())) as *mut c_void
        }

        extern "C" fn $destroy(_engine: *mut c_void, instance: *mut c_void) {
            if instance.is_null() {
                return;
            }
            // SAFETY: `instance` came from the matching create callback.
            drop(unsafe { Box::from_raw(instance as *mut $ty) });
        }
    };
}

simple_ctor!(point_create, point_destroy, PointInst);
simple_ctor!(circle_create, circle_destroy, CircleInst);
simple_ctor!(rect_create, rect_destroy, RectInst);
simple_ctor!(quad_create, quad_destroy, QuadInst);
simple_ctor!(getter_create, getter_destroy, LayerGetterInst);

extern "C" fn geometry_ctor(
    engine: *mut c_void,
    _instance: *mut c_void,
    _argc: std::os::raw::c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, super::helpers::NO_BINDING);
    }
    set_void(out);
    0
}

mp_get_int!(point_get_type, PointInst, |_p: &PointInst| 0);
mp_get_real!(point_get_x, PointInst, |p: &PointInst| p.x);
mp_get_real!(point_get_y, PointInst, |p: &PointInst| p.y);
// `Point::contains` is a stub that always misses and takes exactly the two
// hit-test coordinates (`reference/cpp/plugins/motionplayer/SourceCache.h:136`,
// arity gate `reference/cpp/core/plugin/ncbind.hpp:1178`).
mp_method_bool!(
    point_contains,
    PointInst,
    |_p: &mut PointInst, args: &[Value], _ctx: &CompatCtx| {
        super::helpers::require_args(args, 2, "Point.contains")?;
        Ok(false)
    }
);

mp_get_int!(circle_get_type, CircleInst, |_p: &CircleInst| 1);
mp_get_real!(circle_get_x, CircleInst, |p: &CircleInst| p.x);
mp_get_real!(circle_get_y, CircleInst, |p: &CircleInst| p.y);
mp_get_real!(circle_get_r, CircleInst, |p: &CircleInst| p.r);
mp_method_bool!(
    circle_contains,
    CircleInst,
    |p: &mut CircleInst, args: &[Value], _ctx: &CompatCtx| {
        // `reference/cpp/plugins/motionplayer/SourceCache.h:148-151`.
        super::helpers::require_args(args, 2, "Circle.contains")?;
        let x = value_as_f64(&args[0]);
        let y = value_as_f64(&args[1]);
        Ok((x - p.x).powi(2) + (y - p.y).powi(2) <= p.r * p.r)
    }
);

mp_get_int!(rect_get_type, RectInst, |_p: &RectInst| 2);
mp_get_real!(rect_get_l, RectInst, |p: &RectInst| p.l);
mp_get_real!(rect_get_t, RectInst, |p: &RectInst| p.t);
mp_get_real!(rect_get_w, RectInst, |p: &RectInst| p.w);
mp_get_real!(rect_get_h, RectInst, |p: &RectInst| p.h);
mp_method_bool!(
    rect_contains,
    RectInst,
    |p: &mut RectInst, args: &[Value], _ctx: &CompatCtx| {
        // Half-open edges, exactly like the reference
        // (`reference/cpp/plugins/motionplayer/SourceCache.h:164-166`).
        super::helpers::require_args(args, 2, "Rect.contains")?;
        let x = value_as_f64(&args[0]);
        let y = value_as_f64(&args[1]);
        Ok(x >= p.l && x < p.l + p.w && y >= p.t && y < p.t + p.h)
    }
);

mp_get_int!(quad_get_type, QuadInst, |_p: &QuadInst| 3);
mp_method_bool!(
    quad_contains,
    QuadInst,
    |_p: &mut QuadInst, args: &[Value], _ctx: &CompatCtx| {
        // `Quad::contains` is an unimplemented stub that always misses
        // (`reference/cpp/plugins/motionplayer/SourceCache.h:162`).
        super::helpers::require_args(args, 2, "Quad.contains")?;
        Ok(false)
    }
);

extern "C" fn quad_get_p(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    // `Quad::getP` is an unimplemented stub that returns void
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:161`).
    if resolve(engine).is_none() {
        return report_error(out_error, super::helpers::NO_BINDING);
    }
    set_void(out);
    0
}

mp_get_int!(
    getter_get_type,
    LayerGetterInst,
    |p: &LayerGetterInst| p.kind as i64
);
mp_get_string!(getter_get_label, LayerGetterInst, |p: &LayerGetterInst| p
    .label
    .clone());
mp_get_bool!(
    getter_get_visible,
    LayerGetterInst,
    |p: &LayerGetterInst| p.visible
);
mp_get_bool!(
    getter_get_branch_visible,
    LayerGetterInst,
    |p: &LayerGetterInst| p.visible
);
mp_get_bool!(
    getter_get_layer_visible,
    LayerGetterInst,
    |p: &LayerGetterInst| p.visible
);
mp_get_real!(getter_get_x, LayerGetterInst, |p: &LayerGetterInst| p.x);
mp_get_real!(getter_get_y, LayerGetterInst, |p: &LayerGetterInst| p.y);
mp_get_real!(getter_get_left, LayerGetterInst, |p: &LayerGetterInst| p.x);
mp_get_real!(getter_get_top, LayerGetterInst, |p: &LayerGetterInst| p.y);
mp_get_bool!(
    getter_get_flip_x,
    LayerGetterInst,
    |_p: &LayerGetterInst| false
);
mp_get_bool!(
    getter_get_flip_y,
    LayerGetterInst,
    |_p: &LayerGetterInst| false
);
mp_get_real!(
    getter_get_zoom_x,
    LayerGetterInst,
    |_p: &LayerGetterInst| 1.0
);
mp_get_real!(
    getter_get_zoom_y,
    LayerGetterInst,
    |_p: &LayerGetterInst| 1.0
);
mp_get_real!(
    getter_get_angle_deg,
    LayerGetterInst,
    |p: &LayerGetterInst| p.angle.to_degrees()
);
mp_get_real!(
    getter_get_angle_rad,
    LayerGetterInst,
    |p: &LayerGetterInst| p.angle
);
mp_get_real!(
    getter_get_slant_x,
    LayerGetterInst,
    |_p: &LayerGetterInst| 0.0
);
mp_get_real!(
    getter_get_slant_y,
    LayerGetterInst,
    |_p: &LayerGetterInst| 0.0
);
mp_get_real!(
    getter_get_origin_x,
    LayerGetterInst,
    |_p: &LayerGetterInst| 0.0
);
mp_get_real!(
    getter_get_origin_y,
    LayerGetterInst,
    |_p: &LayerGetterInst| 0.0
);
// `LayerGetter::getOpacity` returns the raw integer (default 255)
// (`reference/cpp/plugins/motionplayer/SourceCache.h:199`, `:221`).
mp_get_int!(
    getter_get_opacity,
    LayerGetterInst,
    |p: &LayerGetterInst| p.opacity
);

/// Variant-valued `LayerGetter` members (`mtx`, `vtx`, `color`,
/// `bezierPatch`, `shape`, `motion`, `particle`). The reference getters
/// return an unset `tTJSVariant`, i.e. void
/// (`reference/cpp/plugins/motionplayer/SourceCache.h:213-219`).
macro_rules! getter_void {
    ($name:ident) => {
        extern "C" fn $name(
            engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut std::ffi::c_char,
            _objthis: *mut c_void,
        ) -> std::os::raw::c_int {
            if resolve(engine).is_none() {
                return report_error(out_error, super::helpers::NO_BINDING);
            }
            set_void(out);
            0
        }
    };
}
getter_void!(getter_get_mtx);
getter_void!(getter_get_vtx);
getter_void!(getter_get_color);
getter_void!(getter_get_bezier);
getter_void!(getter_get_shape);
getter_void!(getter_get_motion);
getter_void!(getter_get_particle);

// ---------------------------------------------------------------------------
// SeparateLayerAdaptor
// ---------------------------------------------------------------------------

/// `Motion.SeparateLayerAdaptor` payload.
pub(crate) struct SeparateInst {
    /// The reference field is a `tjs_int`
    /// (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.h:147`).
    absolute: i64,
    target: StoredValue,
    /// The reference `_owner`, computed once from the construction-time
    /// `targetLayer.window` (`SeparateLayerAdaptor.cpp:26-34`, `:231`).
    owner: Option<DetachedValue>,
}

impl Default for SeparateInst {
    fn default() -> Self {
        Self {
            absolute: 0,
            target: StoredValue::Void,
            owner: None,
        }
    }
}

extern "C" fn separate_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(SeparateInst::default())) as *mut c_void
}

extern "C" fn separate_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `separate_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut SeparateInst) });
}

extern "C" fn separate_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<SeparateInst>(instance) }) else {
        return report_error(out_error, "SeparateLayerAdaptor: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    if let Some(value) = arguments.first() {
        inst.target = StoredValue::from_value(value, &res.ctx());
        // `_owner(separateLayerOwnerLike_0x6C69D4(targetLayer))`
        // (`SeparateLayerAdaptor.cpp:231`): read `targetLayer.window` once at
        // construction. A non-object, a missing member or a failed read
        // leaves the owner unset, like the reference's unset variant.
        if let Some(target_id) = inst.target.object_id() {
            let host = res.engine();
            if matches!(
                host.member(target_id as tjs::Tjs2ValueId, "window"),
                Ok(TjsValue::Object)
            ) && let Ok(owner) = host.retain_value_detached(&TjsValue::Object)
            {
                inst.owner = Some(owner);
            }
        }
    }
    set_void(out);
    0
}

// `absolute` is a `tjs_int` in the reference (`SeparateLayerAdaptor.h:97-98`),
// so the property round-trips integers, not booleans.
mp_get_int!(separate_get_absolute, SeparateInst, |p: &SeparateInst| p
    .absolute);
mp_set_int!(
    separate_set_absolute,
    SeparateInst,
    |p: &mut SeparateInst, v: i64| p.absolute = v
);
mp_get_variant!(
    separate_get_target,
    SeparateInst,
    |p: &SeparateInst, ctx: &CompatCtx| p.target.to_retained(ctx)
);
mp_set_variant!(
    separate_set_target,
    SeparateInst,
    |p: &mut SeparateInst, value: StoredValue| {
        p.target = value;
    }
);
mp_method_void!(
    separate_clear,
    SeparateInst,
    |_p: &mut SeparateInst, _args: &[Value], _ctx: &CompatCtx| {
        // `clear()` only releases the adaptor's private render state;
        // `targetLayer` and `absolute` are untouched
        // (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.cpp:262-279`).
        Ok(())
    }
);
mp_method_void!(
    separate_assign,
    SeparateInst,
    |_p: &mut SeparateInst, args: &[Value], _ctx: &CompatCtx| {
        // `assign` copies another adaptor's managed render lists; none of
        // that state is script-visible, and unlike this port's old
        // behaviour it never overwrites `targetLayer` — a non-adaptor
        // argument is a plain no-op
        // (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.cpp:438-467`).
        let _ = args;
        Ok(())
    }
);

extern "C" fn separate_owner_interface(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: std::os::raw::c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<SeparateInst>(instance) }) else {
        return report_error(out_error, "SeparateLayerAdaptor: null instance");
    };
    // `getLayerTreeOwnerInterfaceCompat` (`SeparateLayerAdaptor.cpp:297-330`):
    // an unset owner returns `TJS_S_OK` with the result left void; otherwise
    // the result is the owner's `layerTreeOwnerInterface` property. This port's
    // `Window.layerTreeOwnerInterface` is the window object itself.
    let Some(owner) = inst.owner.as_ref() else {
        set_void(out);
        return 0;
    };
    match res
        .engine()
        .member(owner.raw_id(), "layerTreeOwnerInterface")
    {
        Ok(TjsValue::Object) => match res.engine().retain_value_detached(&TjsValue::Object) {
            Ok(object) => set_retained(out, object),
            Err(error) => {
                return report_error(
                    out_error,
                    &format!("SeparateLayerAdaptor.layerTreeOwnerInterface: {error}"),
                );
            }
        },
        Ok(_) => set_void(out),
        Err(error) => {
            return report_error(
                out_error,
                &format!("SeparateLayerAdaptor.layerTreeOwnerInterface: {error}"),
            );
        }
    }
    0
}

// ---------------------------------------------------------------------------
// D3DAdaptor
// ---------------------------------------------------------------------------

/// `Motion.D3DAdaptor` payload.
pub(crate) struct D3DInst {
    visible: bool,
    alpha_op_add: bool,
    canvas_capture: bool,
    clear: bool,
    size: (i32, i32),
    clear_color: i32,
    resizable: bool,
}

impl Default for D3DInst {
    fn default() -> Self {
        // Reference field defaults (`D3DAdaptor.h:99-107`): `_visible`,
        // `_alphaOpAdd`, `_canvasCaptureEnabled` and `_resizable` are false,
        // `_clearEnabled` is true, and the size/clear color are 0.
        Self {
            visible: false,
            alpha_op_add: false,
            canvas_capture: false,
            clear: true,
            size: (0, 0),
            clear_color: 0,
            resizable: false,
        }
    }
}

extern "C" fn d3d_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(D3DInst::default())) as *mut c_void
}

extern "C" fn d3d_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `d3d_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut D3DInst) });
}

extern "C" fn d3d_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<D3DInst>(instance) }) else {
        return report_error(out_error, "D3DAdaptor: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // `if(numparams < 5) return TJS_E_BADPARAMCOUNT;` (`D3DAdaptor.cpp:19-20`).
    if arguments.len() < 5 {
        return report_error(
            out_error,
            "D3DAdaptor requires a Window, width, height, centerX and centerY",
        );
    }
    // `param[0]->AsObjectNoAddRef()` must be a `Window` instance or the
    // reference throws `"must set Window object"` (`D3DAdaptor.cpp:21-27`).
    if arguments[0].ty != VAL_OBJECT {
        return report_error(out_error, "must set Window object");
    }
    let Ok(window) = res.engine().retain_object_arg(&arguments[0]) else {
        return report_error(out_error, "must set Window object");
    };
    match res
        .engine()
        .native_instance_payload(window.raw_id(), "Window")
    {
        Ok(Some(_)) => {}
        Ok(None) => return report_error(out_error, "must set Window object"),
        Err(error) => {
            return report_error(out_error, &format!("D3DAdaptor: window lookup: {error}"));
        }
    }
    // `static_cast<int>(param[1..4]->AsInteger())`
    // (`D3DAdaptor.cpp:38-42`): `AsInteger` raises on an object, `null` or an
    // octet.
    let mut values = [0i32; 4];
    for (slot, value) in values.iter_mut().zip(&arguments[1..5]) {
        match try_value_as_i64(value) {
            Ok(value) => *slot = value as i32,
            Err(error) => return report_error(out_error, &error),
        }
    }
    inst.size = (values[0], values[1]);
    set_void(out);
    0
}

mp_method_void!(
    d3d_set_pos,
    D3DInst,
    |_p: &mut D3DInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setPos(int, int)` (`D3DAdaptor.h:39`) — ncbind arity 2.
        super::helpers::require_args(args, 2, "D3DAdaptor.setPos")?;
        Ok(())
    }
);
mp_method_void!(
    d3d_set_size,
    D3DInst,
    |p: &mut D3DInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setSize(int w, int h)` (`D3DAdaptor.h:40`) — ncbind arity 2.
        super::helpers::require_args(args, 2, "D3DAdaptor.setSize")?;
        p.size = (value_as_i64(&args[0]) as i32, value_as_i64(&args[1]) as i32);
        Ok(())
    }
);
mp_method_void!(
    d3d_set_clear_color,
    D3DInst,
    |p: &mut D3DInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setClearColor(int color)` (`D3DAdaptor.h:41`).
        super::helpers::require_args(args, 1, "D3DAdaptor.setClearColor")?;
        p.clear_color = value_as_i64(&args[0]) as i32;
        Ok(())
    }
);
mp_method_void!(
    d3d_set_resizable,
    D3DInst,
    |p: &mut D3DInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setResizable(bool v)` (`D3DAdaptor.h:42`).
        super::helpers::require_args(args, 1, "D3DAdaptor.setResizable")?;
        p.resizable = value_as_bool(&args[0]);
        Ok(())
    }
);
mp_method_void!(
    d3d_remove_all,
    D3DInst,
    |_p: &mut D3DInst, _args: &[Value], _ctx: &CompatCtx| Ok(())
);
mp_method_void!(
    d3d_register,
    D3DInst,
    |_p: &mut D3DInst, _args: &[Value], _ctx: &CompatCtx| Ok(())
);
mp_method_void!(
    d3d_unload_unused,
    D3DInst,
    |_p: &mut D3DInst, _args: &[Value], _ctx: &CompatCtx| Ok(())
);
mp_get_bool!(d3d_get_visible, D3DInst, |p: &D3DInst| p.visible);
mp_set_bool!(d3d_set_visible, D3DInst, |p: &mut D3DInst, v: bool| p
    .visible =
    v);
mp_get_bool!(d3d_get_alpha_add, D3DInst, |p: &D3DInst| p.alpha_op_add);
mp_set_bool!(d3d_set_alpha_add, D3DInst, |p: &mut D3DInst, v: bool| p
    .alpha_op_add =
    v);
mp_get_bool!(d3d_get_canvas, D3DInst, |p: &D3DInst| p.canvas_capture);
mp_set_bool!(d3d_set_canvas, D3DInst, |p: &mut D3DInst, v: bool| p
    .canvas_capture =
    v);
mp_get_bool!(d3d_get_clear, D3DInst, |p: &D3DInst| p.clear);
mp_set_bool!(d3d_set_clear, D3DInst, |p: &mut D3DInst, v: bool| p.clear =
    v);

extern "C" fn d3d_capture_canvas(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<D3DInst>(instance) }) else {
        return report_error(out_error, "D3DAdaptor: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // `if(numparams < 1 || !param[0]) return TJS_E_BADPARAMCOUNT;`
    // (`D3DAdaptor.cpp:65-67`).
    if arguments.is_empty() || arguments[0].ty != VAL_OBJECT {
        return report_error(out_error, "D3DAdaptor.captureCanvas expects a Layer");
    }
    let Ok(layer) = res.engine().retain_object_arg(&arguments[0]) else {
        return report_error(out_error, "D3DAdaptor.captureCanvas expects a Layer object");
    };
    // `layerObj->NativeInstanceSupport(..., tTJSNC_Layer::ClassID, ...)`;
    // failure is `TJS_E_INVALIDPARAM` (`D3DAdaptor.cpp:69-76`).
    match crate::CompatCtx::native_instance_id(res.engine(), layer.raw_id(), &["Layer"]) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return report_error(
                out_error,
                "D3DAdaptor.captureCanvas expects a native Layer object",
            );
        }
        Err(error) => {
            return report_error(out_error, &format!("D3DAdaptor.captureCanvas: {error}"));
        }
    }
    // `if(_width <= 0 || _height <= 0) return TJS_S_OK;` — the result stays
    // void (`D3DAdaptor.cpp:78-80`).
    if inst.size.0 <= 0 || inst.size.1 <= 0 {
        set_void(out);
        return 0;
    }
    // The remaining reference work copies the adaptor's render-target texture
    // into the layer (`D3DAdaptor.cpp:82-102`). The port has no L3 render
    // target: the texture is populated by `Player.draw(d3dAdaptor)`
    // (`PlayerRenderTargets.cpp:931-1070`), which is not wired. The reference
    // return value (the Layer argument) is preserved; no pixels are
    // fabricated.
    set_retained(out, layer);
    0
}

// ---------------------------------------------------------------------------
// SourceCache / ObjSource
// ---------------------------------------------------------------------------

/// `Motion.SourceCache` payload.
#[derive(Default)]
pub(crate) struct SourceCacheInst {
    size: i64,
    cached: bool,
}

/// `Motion.ObjSource` payload: a plain marker, the reference's `created`
/// state is never read by this port.
pub(crate) struct ObjSourceInst;

extern "C" fn source_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(SourceCacheInst::default())) as *mut c_void
}

extern "C" fn source_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `source_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut SourceCacheInst) });
}

extern "C" fn source_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<SourceCacheInst>(instance) }) else {
        return report_error(out_error, "SourceCache: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // `NCB_CONSTRUCTOR((tTJSVariant, tjs_int))` registers an arity of 2
    // (`reference/cpp/plugins/motionplayer/main.cpp:28`,
    // `reference/cpp/core/plugin/ncbind.hpp:1178`); the second parameter is
    // the layer type stored by the reference constructor
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:47`).
    if arguments.len() < 2 {
        return report_error(out_error, "SourceCache: bad parameter count");
    }
    let _ = res;
    inst.size = value_as_i64(&arguments[1]);
    set_void(out);
    0
}

mp_method_void!(
    source_load,
    SourceCacheInst,
    |p: &mut SourceCacheInst, args: &[Value], _ctx: &CompatCtx| {
        // `loadSource(tTJSVariant keyOrSource, tTJSVariant currentSource)`
        // is a two-parameter method
        // (`reference/cpp/plugins/motionplayer/SourceCache.h:62-63`,
        // `main.cpp:29`) — ncbind arity 2 (`ncbind.hpp:1178`).
        super::helpers::require_args(args, 2, "SourceCache.loadSource")?;
        p.cached = true;
        Ok(())
    }
);
mp_method_void!(
    source_clear,
    SourceCacheInst,
    |p: &mut SourceCacheInst, _args: &[Value], _ctx: &CompatCtx| {
        p.cached = false;
        Ok(())
    }
);
mp_get_variant!(
    source_get_buf,
    SourceCacheInst,
    |_p: &SourceCacheInst, _ctx: &CompatCtx| { Ok(tjs::RetainedValue::Value(tjs::TjsValue::Void)) }
);

extern "C" fn obj_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(ObjSourceInst)) as *mut c_void
}

extern "C" fn obj_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `obj_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut ObjSourceInst) });
}

extern "C" fn obj_ctor(
    engine: *mut c_void,
    _instance: *mut c_void,
    _argc: std::os::raw::c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, super::helpers::NO_BINDING);
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register `Point`, `Circle`, `Rect`, `Quad` and `LayerGetter`.
pub(crate) fn register_geometry(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Point",
        create: point_create,
        destroy: point_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Point",
                f: geometry_ctor,
            },
            NativeInstanceMethodDef {
                name: "contains",
                f: point_contains,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "type",
                get: Some(point_get_type),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "x",
                get: Some(point_get_x),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "y",
                get: Some(point_get_y),
                set: None,
            },
        ],
    })?;
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Circle",
        create: circle_create,
        destroy: circle_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Circle",
                f: geometry_ctor,
            },
            NativeInstanceMethodDef {
                name: "contains",
                f: circle_contains,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "type",
                get: Some(circle_get_type),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "x",
                get: Some(circle_get_x),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "y",
                get: Some(circle_get_y),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "r",
                get: Some(circle_get_r),
                set: None,
            },
        ],
    })?;
    ctx.register_instance(&NativeInstanceBuilder {
        name: "MotionRect",
        create: rect_create,
        destroy: rect_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "MotionRect",
                f: geometry_ctor,
            },
            NativeInstanceMethodDef {
                name: "contains",
                f: rect_contains,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "type",
                get: Some(rect_get_type),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "l",
                get: Some(rect_get_l),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "t",
                get: Some(rect_get_t),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "w",
                get: Some(rect_get_w),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "h",
                get: Some(rect_get_h),
                set: None,
            },
        ],
    })?;
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Quad",
        create: quad_create,
        destroy: quad_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Quad",
                f: geometry_ctor,
            },
            NativeInstanceMethodDef {
                name: "contains",
                f: quad_contains,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "type",
                get: Some(quad_get_type),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "p",
                get: Some(quad_get_p),
                set: None,
            },
        ],
    })?;
    ctx.register_instance(&NativeInstanceBuilder {
        name: "LayerGetter",
        create: getter_create,
        destroy: getter_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "LayerGetter",
            f: geometry_ctor,
        }],
        properties: vec![
            NativeInstancePropertyDef {
                name: "type",
                get: Some(getter_get_type),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "label",
                get: Some(getter_get_label),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(getter_get_visible),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "branchVisible",
                get: Some(getter_get_branch_visible),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "layerVisible",
                get: Some(getter_get_layer_visible),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "x",
                get: Some(getter_get_x),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "y",
                get: Some(getter_get_y),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "left",
                get: Some(getter_get_left),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "top",
                get: Some(getter_get_top),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "flipX",
                get: Some(getter_get_flip_x),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "flipY",
                get: Some(getter_get_flip_y),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "zoomX",
                get: Some(getter_get_zoom_x),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "zoomY",
                get: Some(getter_get_zoom_y),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "angleDeg",
                get: Some(getter_get_angle_deg),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "angleRad",
                get: Some(getter_get_angle_rad),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "slantX",
                get: Some(getter_get_slant_x),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "slantY",
                get: Some(getter_get_slant_y),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "originX",
                get: Some(getter_get_origin_x),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "originY",
                get: Some(getter_get_origin_y),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "opacity",
                get: Some(getter_get_opacity),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "mtx",
                get: Some(getter_get_mtx),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "vtx",
                get: Some(getter_get_vtx),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "color",
                get: Some(getter_get_color),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "bezierPatch",
                get: Some(getter_get_bezier),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "shape",
                get: Some(getter_get_shape),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "motion",
                get: Some(getter_get_motion),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "particle",
                get: Some(getter_get_particle),
                set: None,
            },
        ],
    })?;
    Ok(())
}

/// Register `SeparateLayerAdaptor`, `D3DAdaptor`, `SourceCache` and
/// `ObjSource`.
pub(crate) fn register_adaptors(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "SeparateLayerAdaptor",
        create: separate_create,
        destroy: separate_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "SeparateLayerAdaptor",
                f: separate_ctor,
            },
            NativeInstanceMethodDef {
                name: "clear",
                f: separate_clear,
            },
            NativeInstanceMethodDef {
                name: "assign",
                f: separate_assign,
            },
            NativeInstanceMethodDef {
                name: "layerTreeOwnerInterface",
                f: separate_owner_interface,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "absolute",
                get: Some(separate_get_absolute),
                set: Some(separate_set_absolute),
            },
            NativeInstancePropertyDef {
                name: "targetLayer",
                get: Some(separate_get_target),
                set: Some(separate_set_target),
            },
        ],
    })?;
    ctx.register_instance(&NativeInstanceBuilder {
        name: "D3DAdaptor",
        create: d3d_create,
        destroy: d3d_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "D3DAdaptor",
                f: d3d_ctor,
            },
            NativeInstanceMethodDef {
                name: "setPos",
                f: d3d_set_pos,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: d3d_set_size,
            },
            NativeInstanceMethodDef {
                name: "setClearColor",
                f: d3d_set_clear_color,
            },
            NativeInstanceMethodDef {
                name: "setResizable",
                f: d3d_set_resizable,
            },
            NativeInstanceMethodDef {
                name: "removeAllTextures",
                f: d3d_remove_all,
            },
            NativeInstanceMethodDef {
                name: "removeAllBg",
                f: d3d_remove_all,
            },
            NativeInstanceMethodDef {
                name: "removeAllCaption",
                f: d3d_remove_all,
            },
            NativeInstanceMethodDef {
                name: "registerBg",
                f: d3d_register,
            },
            NativeInstanceMethodDef {
                name: "registerCaption",
                f: d3d_register,
            },
            NativeInstanceMethodDef {
                name: "unloadUnusedTextures",
                f: d3d_unload_unused,
            },
            NativeInstanceMethodDef {
                name: "captureCanvas",
                f: d3d_capture_canvas,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(d3d_get_visible),
                set: Some(d3d_set_visible),
            },
            NativeInstancePropertyDef {
                name: "alphaOpAdd",
                get: Some(d3d_get_alpha_add),
                set: Some(d3d_set_alpha_add),
            },
            NativeInstancePropertyDef {
                name: "canvasCaptureEnabled",
                get: Some(d3d_get_canvas),
                set: Some(d3d_set_canvas),
            },
            NativeInstancePropertyDef {
                name: "clearEnabled",
                get: Some(d3d_get_clear),
                set: Some(d3d_set_clear),
            },
        ],
    })?;
    ctx.register_instance(&NativeInstanceBuilder {
        name: "SourceCache",
        create: source_create,
        destroy: source_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "SourceCache",
                f: source_ctor,
            },
            NativeInstanceMethodDef {
                name: "loadSource",
                f: source_load,
            },
            NativeInstanceMethodDef {
                name: "clearCache",
                f: source_clear,
            },
        ],
        properties: vec![NativeInstancePropertyDef {
            name: "bufLayer",
            get: Some(source_get_buf),
            set: None,
        }],
    })?;
    ctx.register_instance(&NativeInstanceBuilder {
        name: "ObjSource",
        create: obj_create,
        destroy: obj_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "ObjSource",
            f: obj_ctor,
        }],
        properties: vec![],
    })?;
    Ok(())
}
