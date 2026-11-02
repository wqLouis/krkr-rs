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
use super::helpers::{eval_array, inst_mut, set_retained, value_as_f64};
use super::model::MotionNode;
use crate::engine_api::Scene;
use crate::engine_api::layer_ops::blit_copy;
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
/// The three array members hold no value until the player fills them in,
/// which is how an instance the script created on its own reports them.
pub(crate) struct LayerGetterInst {
    kind: i32,
    label: String,
    visible: bool,
    branch_visible: bool,
    layer_visible: bool,
    x: f64,
    y: f64,
    flip_x: bool,
    flip_y: bool,
    zoom_x: f64,
    zoom_y: f64,
    angle_deg: f64,
    angle_rad: f64,
    slant_x: f64,
    slant_y: f64,
    origin_x: f64,
    origin_y: f64,
    opacity: i64,
    /// The matrix, the vertex list and the colour, each built once and kept
    /// so every read hands out the same array.
    mtx: Option<DetachedValue>,
    vtx: Option<DetachedValue>,
    color: Option<DetachedValue>,
}

impl Default for LayerGetterInst {
    fn default() -> Self {
        Self {
            kind: 0,
            label: String::new(),
            visible: true,
            branch_visible: true,
            layer_visible: true,
            x: 0.0,
            y: 0.0,
            flip_x: false,
            flip_y: false,
            zoom_x: 1.0,
            zoom_y: 1.0,
            angle_deg: 0.0,
            angle_rad: 0.0,
            slant_x: 0.0,
            slant_y: 0.0,
            origin_x: 0.0,
            origin_y: 0.0,
            opacity: 255,
            mtx: None,
            vtx: None,
            color: None,
        }
    }
}

/// Fill a layer getter with the state of a motion node.
///
/// The values are the ones the layer getter of a motion node carries: the
/// node's type and label, its inherited visibility, its position, its
/// transform and its colour.
pub(crate) fn populate_layer_getter(
    getter: &mut LayerGetterInst,
    node: &MotionNode,
    ctx: &CompatCtx,
    origin: (f64, f64),
    mtx: [f64; 6],
    vtx: [f64; 8],
    color: [i64; 4],
) -> Result<(), String> {
    getter.kind = node.node_type;
    getter.label = node.label.clone();
    // The visibility is the inherited chain, and the layer's own flag says
    // whether the node is drawn at all.
    getter.visible = node.accumulated.active;
    getter.branch_visible = node.accumulated.active;
    getter.layer_visible = node.draw_flag;
    getter.x = node.accumulated.x;
    getter.y = node.accumulated.y;
    getter.flip_x = node.accumulated.flip_x;
    getter.flip_y = node.accumulated.flip_y;
    getter.zoom_x = node.accumulated.scale_x;
    getter.zoom_y = node.accumulated.scale_y;
    // The radian field carries the accumulated angle, and the degree field
    // carries that value scaled by 180 over pi.
    getter.angle_rad = node.accumulated.angle;
    getter.angle_deg = node.accumulated.angle * 180.0 / std::f64::consts::PI;
    getter.slant_x = node.accumulated.slant_x;
    getter.slant_y = node.accumulated.slant_y;
    getter.origin_x = origin.0;
    getter.origin_y = origin.1;
    getter.opacity = (node.accumulated.opacity * 255.0).round().clamp(0.0, 255.0) as i64;
    // The three arrays are built once and kept, so a script that writes into
    // one of them keeps the write until the getter is rebuilt.
    getter.mtx = Some(eval_array(
        ctx,
        &mtx.iter()
            .map(|value| TjsValue::Real(*value))
            .collect::<Vec<_>>(),
    )?);
    getter.vtx = Some(eval_array(
        ctx,
        &vtx.iter()
            .map(|value| TjsValue::Real(*value))
            .collect::<Vec<_>>(),
    )?);
    getter.color = Some(eval_array(
        ctx,
        &color
            .iter()
            .map(|value| TjsValue::Integer(*value))
            .collect::<Vec<_>>(),
    )?);
    Ok(())
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
    |p: &LayerGetterInst| p.branch_visible
);
mp_get_bool!(
    getter_get_layer_visible,
    LayerGetterInst,
    |p: &LayerGetterInst| p.layer_visible
);
mp_get_real!(getter_get_x, LayerGetterInst, |p: &LayerGetterInst| p.x);
mp_get_real!(getter_get_y, LayerGetterInst, |p: &LayerGetterInst| p.y);
mp_get_real!(getter_get_left, LayerGetterInst, |p: &LayerGetterInst| p.x);
mp_get_real!(getter_get_top, LayerGetterInst, |p: &LayerGetterInst| p.y);
mp_get_bool!(getter_get_flip_x, LayerGetterInst, |p: &LayerGetterInst| p
    .flip_x);
mp_get_bool!(getter_get_flip_y, LayerGetterInst, |p: &LayerGetterInst| p
    .flip_y);
mp_get_real!(getter_get_zoom_x, LayerGetterInst, |p: &LayerGetterInst| p
    .zoom_x);
mp_get_real!(getter_get_zoom_y, LayerGetterInst, |p: &LayerGetterInst| p
    .zoom_y);
mp_get_real!(
    getter_get_angle_deg,
    LayerGetterInst,
    |p: &LayerGetterInst| p.angle_deg
);
mp_get_real!(
    getter_get_angle_rad,
    LayerGetterInst,
    |p: &LayerGetterInst| p.angle_rad
);
mp_get_real!(
    getter_get_slant_x,
    LayerGetterInst,
    |p: &LayerGetterInst| p.slant_x
);
mp_get_real!(
    getter_get_slant_y,
    LayerGetterInst,
    |p: &LayerGetterInst| p.slant_y
);
mp_get_real!(
    getter_get_origin_x,
    LayerGetterInst,
    |p: &LayerGetterInst| p.origin_x
);
mp_get_real!(
    getter_get_origin_y,
    LayerGetterInst,
    |p: &LayerGetterInst| p.origin_y
);
// `LayerGetter::getOpacity` returns the raw integer (default 255)
// (`reference/cpp/plugins/motionplayer/SourceCache.h:199`, `:221`).
mp_get_int!(
    getter_get_opacity,
    LayerGetterInst,
    |p: &LayerGetterInst| p.opacity
);

// The array members of the layer getter. The player fills them with the
// node's matrix, its vertex list and its colour; an instance the script
// created on its own reports no value at all.
mp_get_value_array!(getter_get_mtx, LayerGetterInst, |p: &LayerGetterInst| p
    .mtx
    .as_ref()
    .map(DetachedValue::raw_id));
mp_get_value_array!(getter_get_vtx, LayerGetterInst, |p: &LayerGetterInst| p
    .vtx
    .as_ref()
    .map(DetachedValue::raw_id));
mp_get_value_array!(getter_get_color, LayerGetterInst, |p: &LayerGetterInst| p
    .color
    .as_ref()
    .map(DetachedValue::raw_id));

/// The remaining variant-valued `LayerGetter` members (`bezierPatch`,
/// `shape`, `motion`, `particle`). Nothing fills them, so the getters
/// return an unset `tTJSVariant`, i.e. void.
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

impl SeparateInst {
    /// The object id of the live `targetLayer`, if one is set.
    pub(crate) fn target_object_id(&self) -> Option<usize> {
        self.target.object_id()
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
                host.member(
                    tjs::Tjs2ValueId::from_raw(target_id as *mut std::ffi::c_void),
                    "window"
                ),
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
    /// The scene layer the adaptor's render target lives in.
    ///
    /// The render target is the surface the pose is painted into and the
    /// capture copies out of.
    render_target: Option<u32>,
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
            render_target: None,
        }
    }
}

impl D3DInst {
    /// The canvas size the render target is sized like, when it is a usable
    /// one.
    fn canvas_size(&self) -> Option<(u32, u32)> {
        let (Ok(width), Ok(height)) = (u32::try_from(self.size.0), u32::try_from(self.size.1))
        else {
            return None;
        };
        (width > 0 && height > 0).then_some((width, height))
    }

    /// Whether the adaptor renders into its render target at all: a capture
    /// that is switched off leaves the target empty.
    pub(crate) fn captures_canvas(&self) -> bool {
        self.canvas_capture
    }

    /// The fill the render target is cleared with before the pose, when the
    /// adaptor clears it.
    pub(crate) fn clear_fill(&self) -> Option<[u8; 4]> {
        self.clear.then(|| argb_fill(self.clear_color))
    }
}

/// The fill the adaptor's render target is cleared with: the colour stored
/// as `0xAARRGGBB`.
fn argb_fill(color: i32) -> [u8; 4] {
    let value = color as u32;
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    ]
}

/// The `Motion.D3DAdaptor`'s render target: the layer the pose is composited
/// into and `captureCanvas` copies out.
///
/// The target is sized like the adaptor and allocated detached, so it is in no
/// window's draw order: it is never rendered on its own, and only the capture
/// reads it.
pub(crate) fn d3d_render_target(adaptor: &mut D3DInst, scene: &mut Scene) -> Result<u32, String> {
    let Some((width, height)) = adaptor.canvas_size() else {
        return Err("Motion.D3DAdaptor: the canvas has no size".to_string());
    };
    if let Some(layer) = adaptor.render_target
        && scene
            .layer(layer)
            .is_some_and(|layer| layer.rect.w == width && layer.rect.h == height)
    {
        return Ok(layer);
    }
    // A target that no longer matches the canvas — the adaptor was resized,
    // or the scene dropped the layer — is released before a new one is
    // allocated.
    if let Some(layer) = adaptor.render_target.take() {
        scene.remove_layer(layer);
    }
    let id = scene.add_detached_layer();
    if let Some(layer) = scene.layer_mut(id) {
        layer.rect.w = width;
        layer.rect.h = height;
        layer.visible = false;
    }
    if !scene.ensure_layer_image(id, width, height) {
        scene.remove_layer(id);
        return Err("Motion.D3DAdaptor: the render target could not be allocated".to_string());
    }
    adaptor.render_target = Some(id);
    Ok(id)
}

/// Copy the adaptor's render target into the destination layer's image.
///
/// The destination's image takes the adaptor's canvas size, and the rows are
/// copied one to one.
fn capture_render_target(
    adaptor: &mut D3DInst,
    scene: &mut Scene,
    destination: u32,
) -> Result<(), String> {
    let Some((width, height)) = adaptor.canvas_size() else {
        return Err("Motion.D3DAdaptor: the canvas has no size".to_string());
    };
    // The destination's image takes the adaptor's canvas size.
    if !scene.internal_set_image_size(destination, width, height) {
        return Err(
            "Motion.D3DAdaptor.captureCanvas: the destination layer has no image".to_string(),
        );
    }
    let source = d3d_render_target(adaptor, scene)?;
    let Some(source_bitmap) = scene.layer(source).and_then(|layer| layer.bitmap) else {
        return Err("Motion.D3DAdaptor.captureCanvas: the render target has no image".to_string());
    };
    let Some(destination_bitmap) = scene.layer(destination).and_then(|layer| layer.bitmap) else {
        return Err(
            "Motion.D3DAdaptor.captureCanvas: the destination layer has no image".to_string(),
        );
    };
    {
        let Some((target, origin)) =
            scene.bitmap_mut_with_source(destination_bitmap, source_bitmap)
        else {
            return Err(
                "Motion.D3DAdaptor.captureCanvas: the destination and the render target are one \
                 image"
                    .to_string(),
            );
        };
        // The rows are copied one to one: the render-target rows land in the
        // layer's pixel buffer.
        blit_copy(
            target,
            0,
            0,
            origin,
            (
                0,
                0,
                origin.width.min(i32::MAX as u32) as i32,
                origin.height.min(i32::MAX as u32) as i32,
            ),
        );
    }
    scene.mark_layer_image_modified(destination);
    Ok(())
}

/// Release the adaptor's render target, so the next draw allocates a fresh
/// one.
///
/// A new canvas size starts from an empty target: the old one is released
/// with the adaptor's canvas.
fn release_render_target(adaptor: &mut D3DInst, ctx: &CompatCtx) {
    let Some(layer) = adaptor.render_target.take() else {
        return;
    };
    // SAFETY: the host keeps the attached scene alive for the binding.
    let Some(scene) = (unsafe { ctx.services() }).scene() else {
        return;
    };
    // SAFETY: the host keeps the attached scene alive for the binding.
    let scene = unsafe { &mut *scene };
    scene.remove_layer(layer);
}

extern "C" fn d3d_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(D3DInst::default())) as *mut c_void
}

extern "C" fn d3d_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `d3d_create` and is released once.
    let mut adaptor = unsafe { Box::from_raw(instance as *mut D3DInst) };
    // The render target is a scene layer; it goes with the adaptor.
    if let Some(res) = resolve(engine) {
        let ctx = res.ctx();
        release_render_target(&mut adaptor, &ctx);
    }
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
    |p: &mut D3DInst, args: &[Value], ctx: &CompatCtx| {
        // `void setSize(int w, int h)` (`D3DAdaptor.h:40`) — ncbind arity 2.
        super::helpers::require_args(args, 2, "D3DAdaptor.setSize")?;
        p.size = (value_as_i64(&args[0]) as i32, value_as_i64(&args[1]) as i32);
        // The old render target goes with the old canvas size, so the next
        // draw starts from an empty target.
        release_render_target(p, ctx);
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
    let destination =
        match crate::CompatCtx::native_instance_id(res.engine(), layer.raw_id(), &["Layer"]) {
            Ok(Some(id)) => id,
            Ok(None) => {
                return report_error(
                    out_error,
                    "D3DAdaptor.captureCanvas expects a native Layer object",
                );
            }
            Err(error) => {
                return report_error(out_error, &format!("D3DAdaptor.captureCanvas: {error}"));
            }
        };
    // `if(_width <= 0 || _height <= 0) return TJS_S_OK;` — the result stays
    // void (`D3DAdaptor.cpp:78-80`).
    if inst.size.0 <= 0 || inst.size.1 <= 0 {
        set_void(out);
        return 0;
    }
    // The remaining work copies the adaptor's render target into the layer.
    // The render target is a layer in the scene, sized like the adaptor, that
    // `Player.draw` composites the pose into.
    let ctx = res.ctx();
    // SAFETY: the host keeps the attached scene alive for the binding.
    let Some(scene) = (unsafe { ctx.services() }).scene() else {
        return report_error(
            out_error,
            "D3DAdaptor.captureCanvas: no scene is attached to the compat host",
        );
    };
    // SAFETY: the host keeps the attached scene alive for the binding.
    let scene = unsafe { &mut *scene };
    if let Err(error) = capture_render_target(inst, scene, destination) {
        return report_error(out_error, &error);
    }
    // The capture hands the layer back to the script.
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
