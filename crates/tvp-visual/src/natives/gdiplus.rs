//! Real `GdiPlus` appearance natives, co-located with the visual natives that
//! consume them.
//!
//! The game constructs `new GdiPlus.Appearance()` and appends draw infos with
//! `addBrush` (a fill) / `addPen` (a stroke), then passes the appearance to
//! `Layer.drawPolygon` / `drawRectangle` / `drawLine` / `drawLines` /
//! `drawArc` / `drawBeziers`. The reference `Appearance` is an **ordered list
//! of draw infos** (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp`),
//! not a brush itself: `drawPath` fills with each brush and strokes with each
//! pen in order. We mirror that here.
//!
//! # Color convention
//!
//! GDI+ colors are `0xAARRGGBB` (TJS `ARGB`), so the high byte **is** the
//! alpha (e.g. the game's `addBrush(0x7ff00000)` is a translucent red, and
//! `RGBA(r,g,b,a)` produces the same encoding). We therefore convert with the
//! existing [`super::layer::argb_to_rgba`] and honor the high byte. This is
//! intentionally different from `Layer.drawText`, whose game colors are
//! 24-bit RGB with the alpha supplied separately by `opa`.
//!
//! # State and the object-handle ABI
//!
//! An appearance's infos live in a process-global registry keyed by the
//! script object's raw `iTJSDispatch2*`. The method callbacks receive that
//! pointer as `objthis`, and `Layer.draw*` reads the same pointer from its
//! first argument via [`tjs2_sys::Value::object_handle`] (the per-argument
//! handle added for exactly this `drawPolygon(app, points)` case). The VM is
//! single-threaded, so a plain `Mutex<HashMap<..>>` is sufficient.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::sync::{LazyLock, Mutex};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine,
    Tjs2ValueId, TjsValue, VAL_INTEGER, VAL_OBJECT, VAL_REAL, Value,
};

use super::ffi::{arg_f64, arg_i64, instance_ref, set_int_out, set_void_out};
use super::layer::argb_to_rgba;

/// Internal global class name. The game only reaches it through
/// `GdiPlus.Appearance`, so a unique bare name avoids squatting on a generic
/// global.
const APPEARANCE_CLASS: &str = "GdiPlusAppearance";

/// GDI+ `BrushType` enum values.
const BRUSH_TYPE_HATCH_FILL: i64 = 1;

/// One fill/stroke color (straight-alpha RGBA).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BrushKind {
    /// A flat color.
    Solid([u8; 4]),
    /// A hatch fill (approximated by a two-color diagonal pattern).
    Hatch {
        style: i32,
        fore: [u8; 4],
        back: [u8; 4],
    },
}

/// One entry of an appearance's ordered draw-info list.
#[derive(Clone, Debug)]
pub(crate) enum DrawKind {
    /// A fill applied to the current path.
    Brush(BrushKind),
    /// A stroke applied to the current path.
    Pen { brush: BrushKind, width: f64 },
}

/// State of one script-visible `GdiPlus.Appearance`.
#[derive(Default, Clone, Debug)]
pub(crate) struct AppearanceState {
    pub infos: Vec<DrawKind>,
}

/// Opaque payload for one `GdiPlus.Appearance`; the draw infos live in
/// [`APPEARANCES`] keyed by `objthis` so `Layer.draw*` can find them from the
/// argument handle.
#[derive(Default)]
struct AppearanceInst {
    objthis: *mut c_void,
}

static APPEARANCES: LazyLock<Mutex<HashMap<usize, AppearanceState>>> =
    LazyLock::new(Default::default);

fn appearances() -> std::sync::MutexGuard<'static, HashMap<usize, AppearanceState>> {
    APPEARANCES.lock().unwrap_or_else(|p| p.into_inner())
}

/// Drop every registered appearance. Called by the test harness so state
/// cannot leak between tests that reuse an object address.
#[cfg(test)]
pub(crate) fn reset_gdiplus_registry() {
    appearances().clear();
}

/// Snapshot an appearance's draw infos by its raw object handle, or `None`
/// when the handle is null / unknown (the draw methods then paint nothing).
pub(crate) fn appearance_snapshot(handle: *mut c_void) -> Option<AppearanceState> {
    if handle.is_null() {
        return None;
    }
    appearances().get(&(handle as usize)).cloned()
}

fn member_i64(engine: &Tjs2Engine, id: Tjs2ValueId, name: &str) -> Option<i64> {
    match engine.get_member(id, name) {
        Ok(TjsValue::Integer(v)) => Some(v),
        Ok(TjsValue::Real(v)) => Some(v as i64),
        _ => None,
    }
}

fn member_f64(engine: &Tjs2Engine, id: Tjs2ValueId, name: &str) -> Option<f64> {
    match engine.get_member(id, name) {
        Ok(TjsValue::Integer(v)) => Some(v as f64),
        Ok(TjsValue::Real(v)) => Some(v),
        _ => None,
    }
}

/// Parse an `addBrush`/`addPen` color-or-brush argument.
///
/// * integer / real → a solid `ARGB` fill;
/// * dictionary → `type` (default solid); hatch reads
///   `hatchStyle`/`foreColor`/`backColor`, solid reads `color`
///   (falling back to `foreColor`) with the reference defaults.
fn parse_brush(engine: &Tjs2Engine, arg: &Value) -> BrushKind {
    match arg.ty {
        VAL_INTEGER | VAL_REAL => BrushKind::Solid(argb_to_rgba(arg_i64(arg))),
        VAL_OBJECT => {
            let Ok(dv) = engine.retain_object_arg(arg) else {
                return BrushKind::Solid([255, 255, 255, 255]);
            };
            let brush_type = member_i64(engine, dv.raw_id(), "type").unwrap_or(0);
            if brush_type == BRUSH_TYPE_HATCH_FILL {
                let style = member_i64(engine, dv.raw_id(), "hatchStyle").unwrap_or(0) as i32;
                let fore = member_i64(engine, dv.raw_id(), "foreColor")
                    .map(argb_to_rgba)
                    .unwrap_or([255, 255, 255, 255]);
                let back = member_i64(engine, dv.raw_id(), "backColor")
                    .map(argb_to_rgba)
                    .unwrap_or([0, 0, 0, 255]);
                BrushKind::Hatch { style, fore, back }
            } else {
                let color = member_i64(engine, dv.raw_id(), "color")
                    .or_else(|| member_i64(engine, dv.raw_id(), "foreColor"))
                    .map(argb_to_rgba)
                    .unwrap_or([255, 255, 255, 255]);
                BrushKind::Solid(color)
            }
        }
        _ => BrushKind::Solid([255, 255, 255, 255]),
    }
}

extern "C" fn appearance_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<AppearanceInst>::default()) as *mut c_void
}

extern "C" fn appearance_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: the pointer came from appearance_create's Box::into_raw.
    let objthis = {
        let inst = unsafe { instance_ref::<AppearanceInst>(instance) };
        inst.objthis
    };
    if !objthis.is_null() {
        appearances().remove(&(objthis as usize));
    }
    // SAFETY: instance came from Box::into_raw.
    unsafe { drop(Box::from_raw(instance as *mut AppearanceInst)) };
}

/// `new GdiPlus.Appearance(...)` — tolerates any arity and initializes the
/// registry slot for this object.
extern "C" fn appearance_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<AppearanceInst>(instance) };
    inst.objthis = objthis;
    if !objthis.is_null() {
        appearances().entry(objthis as usize).or_default();
    }
    set_void_out(out);
    0
}

/// `addBrush(colorOrDict[, ...])` — append a fill.
extern "C" fn appearance_add_brush(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let engine = crate::natives::context_engine();
    let brush = args
        .first()
        .map(|a| parse_brush(engine, a))
        .unwrap_or(BrushKind::Solid([255, 255, 255, 255]));
    if !objthis.is_null() {
        appearances()
            .entry(objthis as usize)
            .or_default()
            .infos
            .push(DrawKind::Brush(brush));
    }
    set_void_out(out);
    0
}

/// `addPen(colorOrDict, widthOrDict[, ...])` — append a stroke.
extern "C" fn appearance_add_pen(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let engine = crate::natives::context_engine();
    let brush = args
        .first()
        .map(|a| parse_brush(engine, a))
        .unwrap_or(BrushKind::Solid([255, 255, 255, 255]));
    let width = match args.get(1) {
        Some(a) if a.ty == VAL_INTEGER || a.ty == VAL_REAL => arg_f64(a),
        Some(a) if a.ty == VAL_OBJECT => engine
            .retain_object_arg(a)
            .ok()
            .and_then(|dv| member_f64(engine, dv.raw_id(), "width"))
            .unwrap_or(1.0),
        _ => 1.0,
    };
    if !objthis.is_null() {
        appearances()
            .entry(objthis as usize)
            .or_default()
            .infos
            .push(DrawKind::Pen {
                brush,
                width: width.max(0.0),
            });
    }
    set_void_out(out);
    0
}

/// `clear()` — empty the ordered draw-info list.
extern "C" fn appearance_clear(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if !objthis.is_null()
        && let Some(state) = appearances().get_mut(&(objthis as usize))
    {
        state.infos.clear();
    }
    set_void_out(out);
    0
}

/// `count` — the number of draw infos (debug/test aid; the reference has no
/// such property and the game never reads it).
extern "C" fn appearance_count_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let count = if objthis.is_null() {
        0
    } else {
        appearances()
            .get(&(objthis as usize))
            .map_or(0, |s| s.infos.len())
    };
    set_int_out(out, count as i64);
    0
}

/// Register `GdiPlus.Appearance` plus the `GdiPlus` namespace object the game
/// scripts reference (`GdiPlus.BrushTypeHatchFill`, `GdiPlus.HatchStyleDiagonalBrick`,
/// …). Mirrors the reference plugin's namespace object.
pub(crate) fn register_gdiplus(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: APPEARANCE_CLASS,
        create: appearance_create,
        destroy: appearance_destroy,
        methods: vec![
            NativeInstanceMethodDef {
                name: APPEARANCE_CLASS, // class-name constructor hook
                f: appearance_ctor,
            },
            NativeInstanceMethodDef {
                name: "addBrush",
                f: appearance_add_brush,
            },
            NativeInstanceMethodDef {
                name: "addPen",
                f: appearance_add_pen,
            },
            NativeInstanceMethodDef {
                name: "clear",
                f: appearance_clear,
            },
        ],
        properties: vec![NativeInstancePropertyDef {
            name: "count",
            get: Some(appearance_count_get),
            set: None,
        }],
    })?;

    // The scripts only need `Appearance` plus the two GDI+ constants, but we
    // expose the full brush-type set and the common hatch styles so a title
    // using another style still resolves.
    let namespace = format!(
        "global.GdiPlus = %[Appearance: {APPEARANCE_CLASS}, \
         BrushTypeSolidColor: 0, BrushTypeHatchFill: 1, \
         BrushTypeTextureFill: 2, BrushTypePathGradient: 3, \
         BrushTypeLinearGradient: 4, \
         HatchStyleHorizontal: 0, HatchStyleVertical: 1, \
         HatchStyleForwardDiagonal: 2, HatchStyleBackwardDiagonal: 3, \
         HatchStyleCross: 4, HatchStyleDiagonalCross: 5, \
         HatchStyleDiagonalBrick: 38];"
    );
    engine
        .exec_script(&namespace, "GdiPlus_namespace")
        .map_err(|e| format!("failed to build GdiPlus namespace: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::natives::tests::TestEnv;

    /// The game's exact construction + `addBrush`/`addPen`/`clear` shapes:
    /// integer ARGB, dictionary hatch, and `clear()`.
    #[test]
    fn appearance_brush_pen_list_and_clear() {
        let env = TestEnv::new("gdiplus-list");
        assert_eq!(env.eval_int("GdiPlus.BrushTypeHatchFill"), 1);
        assert_eq!(env.eval_int("GdiPlus.HatchStyleDiagonalBrick"), 38);
        assert_eq!(env.eval_int("GdiPlus.BrushTypeSolidColor"), 0);

        env.run(
            "var app = new GdiPlus.Appearance(); \
             app.addBrush(0x7ff00000, 0, 0, 0); \
             app.addBrush(0x4000007f, 0, 0); \
             app.addPen(0xffffffff, 2, 0, 0); \
             app.addPen(0xff000000, %[width: 3], 0, 0);",
        )
        .unwrap();
        assert_eq!(env.eval_int("app.count"), 4, "four draw infos appended");

        env.run("app.clear();").unwrap();
        assert_eq!(env.eval_int("app.count"), 0, "clear empties the list");

        // Dictionary hatch brush (ADVScreen debug placeholder): must parse
        // and append exactly one info, and tolerate extra args.
        env.run(
            "app.addBrush(%[type: GdiPlus.BrushTypeHatchFill, \
             hatchStyle: GdiPlus.HatchStyleDiagonalBrick, \
             foreColor: 0, backColor: 0x203040], 0, 0);",
        )
        .unwrap();
        assert_eq!(env.eval_int("app.count"), 1);
    }

    /// Repeated constructions each get independent state (per-object list).
    #[test]
    fn appearances_have_independent_state() {
        let env = TestEnv::new("gdiplus-independent");
        env.run(
            "var a = new GdiPlus.Appearance(); a.addBrush(0xffff0000); \
             var b = new GdiPlus.Appearance(); b.addBrush(0xff00ff00); b.addBrush(0xff0000ff);",
        )
        .unwrap();
        assert_eq!(env.eval_int("a.count"), 1);
        assert_eq!(env.eval_int("b.count"), 2);
    }
}
