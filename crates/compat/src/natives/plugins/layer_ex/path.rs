//! The GDI+ `Path` value type exposed by the `layerExDraw` plugin.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp`
//! (`class Path` wrapping a `BLPath`, `:944-977`) and
//! `blend2d/LayerExDraw.cpp` (`Path::*`, `:1133-1451`). The reference keeps
//! one "current figure" flag: primitives after `startFigure()` connect to
//! the current figure with `lineTo` (`:1368-1381`), closed primitives
//! (`drawRectangle`/`drawEllipse`/`drawRectangles`) always add their own
//! figure and reset the flag (`:1433`, `:1357`, `:1449`), and `closeFigure`
//! closes the current one (`:1138`). The port models the same thing as a
//! list of figures where the *open* last figure is the current one: an
//! open-ended primitive extends it, a closed one replaces it. Every figure
//! is flattened to a polyline as it is added, using the L2
//! `engine_api::raster` flattening primitives, so `Layer.drawPath` can fill
//! and stroke it with the existing CPU rasterizer.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::raster::{flatten_arc, flatten_cubic};
use tjs::{NativeInstanceBuilder, NativeInstanceMethodDef, Value};

use crate::native_util::{args, report_error, set_void};
use crate::{CompatCtx, CompatError};

/// One closed or open figure of a path, already flattened to a polyline.
#[derive(Clone, Debug, Default)]
pub(super) struct Figure {
    pub(super) points: Vec<(f64, f64)>,
    pub(super) closed: bool,
}

/// The flattened geometry of one `Path` object.
#[derive(Clone, Debug, Default)]
pub(super) struct PathData {
    pub(super) figures: Vec<Figure>,
}

impl PathData {
    /// The current figure: the last one while it is still open (reference
    /// `figureStarted`, `reference/cpp/plugins/layerex_draw/blend2d/
    /// LayerExDraw.cpp:1174-1176`), `None` when the next primitive must
    /// start a new one.
    fn current_mut(&mut self) -> Option<&mut Figure> {
        match self.figures.last_mut() {
            Some(figure) if !figure.closed => Some(figure),
            _ => None,
        }
    }

    /// Append `points` to the current open figure, starting a new one when
    /// there is none — the `lineTo` branch of the reference `drawLine`
    /// (`blend2d/LayerExDraw.cpp:1368-1381`) and friends. An empty
    /// `points` list never changes the path (reference guards such as
    /// `if(ps.empty()) return`, `:1407-1408`).
    fn extend(&mut self, points: Vec<(f64, f64)>) {
        if points.is_empty() {
            return;
        }
        match self.current_mut() {
            Some(figure) => figure.points.extend(points),
            None => self.figures.push(Figure {
                points,
                closed: false,
            }),
        }
    }

    /// Add a *closed* figure of its own (`drawRectangle`/`drawEllipse`/
    /// `drawPolygon`/`drawPie`/`drawRectangles`, which always `addRect`/
    /// `moveTo` + `close` a fresh figure — `blend2d/LayerExDraw.cpp:1433`,
    /// `:1357`, `:1412-1424`), consuming an empty placeholder left by
    /// `startFigure`. Figures with fewer than two points cannot be filled or
    /// stroked and are dropped (`reference … :1407-1408` guards empty point
    /// arrays the same way).
    fn push_closed(&mut self, points: Vec<(f64, f64)>) {
        if points.len() < 2 {
            return;
        }
        if matches!(
            self.figures.last(),
            Some(figure) if figure.points.is_empty() && !figure.closed
        ) {
            self.figures.pop();
        }
        self.figures.push(Figure {
            points,
            closed: true,
        });
    }
}

/// A cardinal spline through `points`, flattened to a polyline.
///
/// `tension` is the GDI+ cardinal tension (`0.5` when omitted). `closed`
/// wraps the endpoints; `offset`/`segments` select a sub-range of an open
/// curve (reference `GraphicsPath::AddCurve` / blend2d `Path::drawCurve3`,
/// `blend2d/LayerExDraw.cpp:1273-1310`). `segments == 0` means "every
/// segment of the open curve" (`numberOfSegments` defaults to
/// `ps.size() - 1`, `:1279-1280`); callers must range-check explicit
/// values the way the reference does (`:1284-1286`).
///
/// `start` is the current point of an open figure: the first segment then
/// starts there instead of at `points[offset]`, exactly like the reference
/// `cubicTo` that follows the `if(!figureStarted) moveTo(…)` guard
/// (`:1285-1307`).
pub(super) fn cardinal_spline(
    points: &[(f64, f64)],
    closed: bool,
    tension: f64,
    offset: usize,
    segments: usize,
) -> Vec<(f64, f64)> {
    cardinal_spline_from(points, closed, tension, offset, segments, None)
}

fn cardinal_spline_from(
    points: &[(f64, f64)],
    closed: bool,
    tension: f64,
    offset: usize,
    segments: usize,
    start: Option<(f64, f64)>,
) -> Vec<(f64, f64)> {
    let n = points.len();
    if n < 2 {
        return points.to_vec();
    }
    let at = |i: isize| -> (f64, f64) {
        if closed {
            let wrapped = i.rem_euclid(n as isize) as usize;
            points[wrapped]
        } else {
            let clamped = i.clamp(0, n as isize - 1) as usize;
            points[clamped]
        }
    };
    let (first, last) = if closed {
        (0, n)
    } else {
        let start_index = offset.min(n.saturating_sub(1));
        let end = if segments == 0 {
            // "All segments": `numberOfSegments` defaults to `size - 1`,
            // i.e. the loop runs `i in 0..size - 1` (`blend2d/LayerExDraw.cpp:
            // 1279-1280, 1301-1307`).
            start_index + (n - 1 - start_index)
        } else {
            (start_index + segments).min(n.saturating_sub(1))
        };
        (start_index, end)
    };
    let mut out = Vec::new();
    if first > last {
        return out;
    }
    match start {
        // Appending to an open figure: the figure already holds the current
        // point, so the first segment starts there (reference `cubicTo`
        // without a preceding `moveTo`).
        Some(current) if !closed => {
            let mut current = current;
            for i in first..last {
                let p0 = at(i as isize - 1);
                let p1 = at(i as isize);
                let p2 = at(i as isize + 1);
                let p3 = at(i as isize + 2);
                let t = tension / 3.0;
                let c1 = (p1.0 + (p2.0 - p0.0) * t, p1.1 + (p2.1 - p0.1) * t);
                let c2 = (p2.0 - (p3.0 - p1.0) * t, p2.1 - (p3.1 - p1.1) * t);
                let flat = flatten_cubic(current, c1, c2, p2, 12);
                out.extend(flat.into_iter().skip(1));
                current = p2;
            }
            out
        }
        _ => {
            out.push(points[first]);
            for i in first..last {
                let p0 = at(i as isize - 1);
                let p1 = at(i as isize);
                let p2 = at(i as isize + 1);
                let p3 = at(i as isize + 2);
                let t = tension / 3.0;
                let c1 = (p1.0 + (p2.0 - p0.0) * t, p1.1 + (p2.1 - p0.1) * t);
                let c2 = (p2.0 - (p3.0 - p1.0) * t, p2.1 - (p3.1 - p1.1) * t);
                let flat = flatten_cubic(p1, c1, c2, p2, 12);
                out.extend(flat.into_iter().skip(1));
            }
            out
        }
    }
}

/// A cubic Bézier chain (reference `Path::drawBeziers`,
/// `blend2d/LayerExDraw.cpp:1186-1207`): the first point is the start,
/// then every group of three points is control, control, end. The reference
/// silently ignores a point count that is below 4 or not `3n + 1`
/// (`:1190-1191`) — an invalid chain adds *nothing*.
///
/// `start` is the current point of an open figure: the first curve then
/// starts there and `points[0]` is ignored, exactly like the reference
/// `cubicTo` guard that skips the `moveTo` (`:1193-1195`).
pub(super) fn beziers(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    beziers_from(points, None)
}

fn beziers_from(points: &[(f64, f64)], start: Option<(f64, f64)>) -> Vec<(f64, f64)> {
    if points.len() < 4 || !(points.len() - 1).is_multiple_of(3) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut current = match start {
        Some(current) => current,
        None => {
            out.push(points[0]);
            points[0]
        }
    };
    let mut i = 1;
    while i + 2 < points.len() {
        let flat = flatten_cubic(current, points[i], points[i + 1], points[i + 2], 12);
        out.extend(flat.into_iter().skip(1));
        current = points[i + 2];
        i += 3;
    }
    out
}

/// The reference `AsReal` conversion (`reference/cpp/core/tjs2/tjsVariant.h:
/// 947-966`) for the scalar parameters: `void`/strings coerce, objects and
/// octets raise a conversion error instead of silently coercing to 0.
fn strict_real(value: &Value) -> Result<f64, String> {
    match value.ty {
        tjs::VAL_INTEGER | tjs::VAL_REAL | tjs::VAL_STRING | tjs::VAL_VOID => {
            Ok(super::value_as_f64(value))
        }
        _ => Err("the argument is not a number".to_string()),
    }
}

/// [`strict_real`] for a fixed-size parameter list — the NCB-converted
/// scalar parameters of one primitive (fewer arguments than `N` cannot
/// happen after the arity check, but be safe).
fn strict_reals<const N: usize>(a: &[Value]) -> Result<[f64; N], String> {
    let mut out = [0.0f64; N];
    for (slot, value) in out.iter_mut().zip(a.iter()) {
        *slot = strict_real(value)?;
    }
    Ok(out)
}

/// The current figure's last point — the `moveTo`/`lineTo` cursor the
/// reference connects from.
fn cursor(inst: &PathInst) -> Option<(f64, f64)> {
    inst.data
        .figures
        .last()
        .filter(|figure| !figure.closed)
        .and_then(|figure| figure.points.last().copied())
}

/// `Path` payload: the flattened path geometry.
pub(super) struct PathInst {
    pub(super) data: PathData,
}

extern "C" fn create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(PathInst {
        data: PathData::default(),
    })) as *mut c_void
}

extern "C" fn destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: every payload came from `create`'s `Box::into_raw`.
    drop(unsafe { Box::from_raw(instance as *mut PathInst) });
}

/// `Path()`.
extern "C" fn ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `ctor` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    inst.data = PathData::default();
    set_void(out);
    0
}

/// `startFigure()` — reference `Path::startFigure`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1133-1134`)
/// only sets `figureStarted`: the *next* primitive connects to the current
/// figure instead of starting a new one (the `lineTo` branch of `drawLine`,
/// `:1369-1372`). The port's figure model already extends an open figure,
/// so the flag only needs an empty placeholder when the current figure is
/// closed or missing — that placeholder is consumed by the next primitive.
extern "C" fn start_figure(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `start_figure` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    if !matches!(inst.data.figures.last(), Some(figure) if !figure.closed) {
        inst.data.figures.push(Figure::default());
    }
    set_void(out);
    0
}

/// `closeFigure()` — reference `Path::closeFigure` closes the current
/// figure (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:
/// 1138-1139`); with no open figure it changes nothing.
extern "C" fn close_figure(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `close_figure` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    if let Some(last) = inst.data.figures.last_mut()
        && !last.closed
    {
        last.closed = true;
    }
    set_void(out);
    0
}

/// One scalar-parameter primitive: NCB requires the reference parameter
/// count (`reference/cpp/core/plugin/ncbind.hpp:1178`), every coordinate
/// converts with `AsReal`, and the resulting polyline either extends the
/// current open figure or stands alone as a closed one.
macro_rules! path_scalar_method {
    ($name:ident, $arity:expr, $closed:expr, $expr:expr) => {
        extern "C" fn $name(
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
            if a.len() < $arity {
                return report_error(
                    out_error,
                    concat!(
                        "Path.",
                        stringify!($name),
                        " requires ",
                        stringify!($arity),
                        " arguments"
                    ),
                );
            }
            let build: fn(&[Value]) -> Result<Vec<(f64, f64)>, String> = $expr;
            let points = match build(a) {
                Ok(points) => points,
                Err(error) => {
                    return report_error(out_error, &format!("Path.{}: {error}", stringify!($name)))
                }
            };
            let _ = engine;
            // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; this callback is its only live accessor.
            let inst = unsafe { &mut *(instance as *mut PathInst) };
            if $closed {
                inst.data.push_closed(points);
            } else {
                inst.data.extend(points);
            }
            set_void(out);
            0
        }
    };
}

// `drawLine(x1, y1, x2, y2)` — the reference `drawLine`
// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1368-1381`)
// `moveTo(x1, y1)`s a new figure and `lineTo(x2, y2)`s, or `lineTo`s both
// points into the current figure.
path_scalar_method!(draw_line, 4, false, |a| {
    let p = strict_reals::<4>(a)?;
    Ok(vec![(p[0], p[1]), (p[2], p[3])])
});

// `drawRectangle(x, y, width, height)` — always its own closed figure
// (`addRect`, `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:
// 1429-1437`).
path_scalar_method!(draw_rectangle, 4, true, |a| {
    let p = strict_reals::<4>(a)?;
    let (x, y, w, h) = (p[0], p[1], p[2], p[3]);
    Ok(vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)])
});

// `drawEllipse(x, y, width, height)` — always its own closed figure
// (`addEllipse` + `figureStarted = false`,
// `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1348-1359`).
path_scalar_method!(draw_ellipse, 4, true, |a| {
    let p = strict_reals::<4>(a)?;
    Ok(flatten_arc(p[0], p[1], p[2], p[3], 0.0, 360.0))
});

/// `drawArc(x, y, width, height, startAngle, sweepAngle)` — the reference
/// `arcTo` appends the arc to the current figure, connecting from the
/// current point when one is open (`reference/cpp/plugins/layerex_draw/
/// blend2d/LayerExDraw.cpp:1149-1160`); the appended polyline connects the
/// same way.
extern "C" fn draw_arc(
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
    if a.len() < 6 {
        return report_error(out_error, "Path.drawArc requires 6 arguments");
    }
    let p = match strict_reals::<6>(a) {
        Ok(p) => p,
        Err(error) => return report_error(out_error, &format!("Path.drawArc: {error}")),
    };
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `draw_arc` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    inst.data
        .extend(flatten_arc(p[0], p[1], p[2], p[3], p[4], p[5]));
    set_void(out);
    0
}

/// `drawPie(x, y, width, height, startAngle, sweepAngle)` — its own closed
/// figure: `moveTo(center)`, the arc, back to the centre, `close()`
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1312-1341`).
extern "C" fn draw_pie(
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
    if a.len() < 6 {
        return report_error(out_error, "Path.drawPie requires 6 arguments");
    }
    let p = match strict_reals::<6>(a) {
        Ok(p) => p,
        Err(error) => return report_error(out_error, &format!("Path.drawPie: {error}")),
    };
    let (x, y, w, h) = (p[0], p[1], p[2], p[3]);
    let mut points = flatten_arc(x, y, w, h, p[4], p[5]);
    points.push((x + w / 2.0, y + h / 2.0));
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `draw_pie` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    inst.data.push_closed(points);
    set_void(out);
    0
}

/// `drawBezier(x1, y1, x2, y2, x3, y3, x4, y4)` — the reference starts a
/// new figure with `moveTo(x1, y1)` only when none is open; otherwise the
/// `cubicTo` starts at the current point and `(x1, y1)` is ignored
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1172-1184`).
extern "C" fn draw_bezier(
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
    if a.len() < 8 {
        return report_error(out_error, "Path.drawBezier requires 8 arguments");
    }
    let p = match strict_reals::<8>(a) {
        Ok(p) => p,
        Err(error) => return report_error(out_error, &format!("Path.drawBezier: {error}")),
    };
    let (p0, c1, c2, p3) = ((p[0], p[1]), (p[2], p[3]), (p[4], p[5]), (p[6], p[7]));
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `draw_bezier` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    match cursor(inst) {
        Some(current) => {
            // Appending: the curve starts at the current point and the
            // provided start is ignored.
            let flat = flatten_cubic(current, c1, c2, p3, 24);
            inst.data.extend(flat.into_iter().skip(1).collect());
        }
        None => inst.data.extend(flatten_cubic(p0, c1, c2, p3, 24)),
    }
    set_void(out);
    0
}

/// `drawBeziers(points)` — NCB requires the one parameter and the point
/// array must hold a valid chain of `3n + 1` points or the call adds
/// nothing (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:
/// 1186-1207`); the first curve starts at the current point when a figure
/// is open (`:1193-1195`).
extern "C" fn draw_beziers(
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
        return report_error(out_error, "Path.drawBeziers requires 1 argument");
    }
    let Ok(points) = super::read_points(engine, &a[0], "Path.drawBeziers") else {
        return report_error(out_error, "Path.drawBeziers: invalid points");
    };
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `draw_beziers` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    let points = beziers_from(&points, cursor(inst));
    inst.data.extend(points);
    set_void(out);
    0
}

/// `drawLines(points)` — `moveTo(ps[0])` for a new figure, then `lineTo`
/// every point (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:
/// 1383-1403`); an empty array changes nothing (`:1386-1387`).
extern "C" fn draw_lines(
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
        return report_error(out_error, "Path.drawLines requires 1 argument");
    }
    let Ok(points) = super::read_points(engine, &a[0], "Path.drawLines") else {
        return report_error(out_error, "Path.drawLines: invalid points");
    };
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `draw_lines` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    inst.data.extend(points);
    set_void(out);
    0
}

/// `drawPolygon(points)` — always its own figure: `moveTo(ps[0])`, the
/// sides, `close()` (`reference/cpp/plugins/layerex_draw/blend2d/
/// LayerExDraw.cpp:1405-1425`); an empty array changes nothing
/// (`:1407-1408`).
extern "C" fn draw_polygon(
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
        return report_error(out_error, "Path.drawPolygon requires 1 argument");
    }
    let Ok(points) = super::read_points(engine, &a[0], "Path.drawPolygon") else {
        return report_error(out_error, "Path.drawPolygon: invalid points");
    };
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `draw_polygon` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    inst.data.push_closed(points);
    set_void(out);
    0
}

/// `drawRectangles(rects)` — one closed figure per rectangle
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1441-1451`).
extern "C" fn draw_rectangles(
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
        return report_error(out_error, "Path.drawRectangles requires 1 argument");
    }
    let Ok(rects) = super::read_rects(engine, &a[0], "Path.drawRectangles") else {
        return report_error(out_error, "Path.drawRectangles: invalid rectangles");
    };
    // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; `draw_rectangles` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut PathInst) };
    for (x, y, w, h) in rects {
        inst.data
            .push_closed(vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]);
    }
    set_void(out);
    0
}

/// One cardinal-spline primitive: NCB requires the reference parameter
/// count (`reference/cpp/core/plugin/ncbind.hpp:1178`), and the reference
/// range guard is reproduced exactly — a point count below 2, a negative
/// `offset`, or `offset + numberOfSegments >= size` makes the call a no-op
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1277-1287`);
/// closed curves always draw every segment (`:1216-1247`).
macro_rules! path_curve_method {
    ($name:ident, $arity:expr, $closed:expr, $extract:expr) => {
        extern "C" fn $name(
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
            if a.len() < $arity {
                return report_error(
                    out_error,
                    concat!(
                        "Path.",
                        stringify!($name),
                        " requires ",
                        stringify!($arity),
                        " arguments"
                    ),
                );
            }
            let Ok(points) = super::read_points(engine, &a[0], "Path curve") else {
                return report_error(out_error, "Path curve: invalid points");
            };
            // `(offset, segments, tension)`; `segments < 0` is "all".
            let extract: fn(&[Value]) -> Result<(i64, i64, f64), String> = $extract;
            let (offset, segments, tension) = match extract(a) {
                Ok(values) => values,
                Err(error) => {
                    return report_error(out_error, &format!("Path.{}: {error}", stringify!($name)))
                }
            };
            // SAFETY: `instance` is the live `PathInst` payload the VM allocated for this script object; this callback is its only live accessor.
            let inst = unsafe { &mut *(instance as *mut PathInst) };
            if $closed {
                // Reference `drawClosedCurve2` draws all `size` segments and
                // closes the figure (`blend2d/LayerExDraw.cpp:1220-1247`).
                let curve = cardinal_spline_from(&points, true, tension, 0, 0, None);
                inst.data.push_closed(curve);
            } else {
                let size = points.len() as i64;
                // `if(ps.size() < 2) return;`
                // (`blend2d/LayerExDraw.cpp:1277-1278`).
                if size >= 2 {
                    // `if(numberOfSegments < 0) numberOfSegments = size - 1;`
                    // (`:1279-1280`).
                    let segments = if segments < 0 { size - 1 } else { segments };
                    // `if(offset < 0 || offset + numberOfSegments >= size)
                    // return;` (`:1283-1286`).
                    if offset >= 0 && offset + segments < size {
                        let start = cursor(inst);
                        if segments > 0 {
                            let curve = cardinal_spline_from(
                                &points,
                                false,
                                tension,
                                offset as usize,
                                segments as usize,
                                start,
                            );
                            inst.data.extend(curve);
                        }
                        // `segments == 0` draws nothing — the reference loop
                        // `for(i = offset; i < offset + 0; …)` has no
                        // iterations and its lone `moveTo` point is invisible.
                    }
                }
            }
            set_void(out);
            0
        }
    };
}

// `drawCurve(points)` — every segment, tension 0.5
// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1253`,
// `:1285-1310`).
path_curve_method!(draw_curve, 1, false, |_a: &[Value]| Ok((0, -1, 0.5)));

// `drawCurve2(points, tension)` — reference `:1261-1271`.
path_curve_method!(draw_curve2, 2, false, |a: &[Value]| {
    Ok((0, -1, strict_real(&a[1])?))
});

// `drawCurve3(points, offset, numberOfSegments, tension)` — reference
// `:1273-1310`; the offset/segment count converts with `AsInteger`.
path_curve_method!(draw_curve3, 4, false, |a: &[Value]| {
    let offset = crate::natives::system::try_value_as_i64(&a[1])?;
    let segments = crate::natives::system::try_value_as_i64(&a[2])?;
    Ok((offset, segments, strict_real(&a[3])?))
});

// `drawClosedCurve(points)` — reference `:1216-1222` (tension 0.5).
path_curve_method!(draw_closed_curve, 1, true, |_a: &[Value]| Ok((0, -1, 0.5)));

// `drawClosedCurve2(points, tension)` — reference `:1224-1247`.
path_curve_method!(draw_closed_curve2, 2, true, |a: &[Value]| {
    Ok((0, -1, strict_real(&a[1])?))
});

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        NativeInstanceMethodDef {
            name: "startFigure",
            f: start_figure,
        },
        NativeInstanceMethodDef {
            name: "closeFigure",
            f: close_figure,
        },
        NativeInstanceMethodDef {
            name: "drawArc",
            f: draw_arc,
        },
        NativeInstanceMethodDef {
            name: "drawPie",
            f: draw_pie,
        },
        NativeInstanceMethodDef {
            name: "drawBezier",
            f: draw_bezier,
        },
        NativeInstanceMethodDef {
            name: "drawBeziers",
            f: draw_beziers,
        },
        NativeInstanceMethodDef {
            name: "drawClosedCurve",
            f: draw_closed_curve,
        },
        NativeInstanceMethodDef {
            name: "drawClosedCurve2",
            f: draw_closed_curve2,
        },
        NativeInstanceMethodDef {
            name: "drawCurve",
            f: draw_curve,
        },
        NativeInstanceMethodDef {
            name: "drawCurve2",
            f: draw_curve2,
        },
        NativeInstanceMethodDef {
            name: "drawCurve3",
            f: draw_curve3,
        },
        NativeInstanceMethodDef {
            name: "drawEllipse",
            f: draw_ellipse,
        },
        NativeInstanceMethodDef {
            name: "drawLine",
            f: draw_line,
        },
        NativeInstanceMethodDef {
            name: "drawLines",
            f: draw_lines,
        },
        NativeInstanceMethodDef {
            name: "drawPolygon",
            f: draw_polygon,
        },
        NativeInstanceMethodDef {
            name: "drawRectangle",
            f: draw_rectangle,
        },
        NativeInstanceMethodDef {
            name: "drawRectangles",
            f: draw_rectangles,
        },
    ]
}

/// Register the `Path` class.
pub(super) fn register_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let mut methods = vec![NativeInstanceMethodDef {
        name: "Path",
        f: ctor,
    }];
    methods.extend(method_defs());
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Path",
        create,
        destroy,
        invalidate: None,
        methods,
        properties: Vec::new(),
    })
}
