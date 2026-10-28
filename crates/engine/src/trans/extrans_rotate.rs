//! `rotatezoom`, `rotatevanish` and `rotateswap` — the rotation family of
//! the `extrans` plugin.
//!
//! All three are `ttExchange` / `tutDivisible` providers built on one
//! shared base class, and this module mirrors that split: [`base_apply`]
//! is the base class and [`zoom_quads`] / [`swap_quads`] are its
//! `CalcPosition` overrides.
//!
//! # What the base does each frame
//!
//! `StartProcess` resets every destination scanline to one background
//! region and then calls the handler's `CalcPosition`, which places two
//! source quadrilaterals — `AddSource(points, type)` maps the source's
//! corner pixel `(0,0)` to `points[0]`, `(w-1,0)` to `points[1]` and
//! `(0,h-1)` to `points[2]`, deriving the fourth vertex as the
//! parallelogram corner. `Process` then paints, per scanline: the
//! background colour where no quadrilateral covers, otherwise a
//! nearest-neighbour walk of the source 16.16 fixed-point sampler
//! (`tRotateDrawLine`) of whichever source covers that span. Later
//! `AddSource` calls overwrite earlier ones, so the *last* source placed
//! wins where the two quadrilaterals overlap.
//!
//! Each `AddSource` issues at most **one** span per scanline, so the frame
//! is stored as at most two spans per line ([`LinePlan`]) instead of a
//! region list with its splitting `AddLine`; overlap
//! resolution by placement order is preserved through [`Quads::top`]. That
//! is the only structural simplification — the scanline geometry itself
//! (the edge walk over the vertex ring, the `65536 * dx / (dy + 1)` edge
//! steps, the `(ax - dx + 1) / (adx - ddx + 1)` in-span step and the
//! `>> 16` truncation) follows statement for statement.
//!
//! # Timing
//!
//! The animation is driven from elapsed milliseconds
//! (`CurTime / Time`, a division of two `tjs_int`s in a `float`), not from
//! the 0..255 phase; [`Frame`] reproduces those two values from
//! [`Trans::elapsed_ms`](super::Trans::elapsed_ms) and
//! [`Trans::duration_ms`](super::Trans::duration_ms), truncated to `i32`
//! exactly like the `(tjs_int)` casts, and applies the
//! provider's own `if(time < 2) time = 2;` guard. Every `float` intermediate
//! (`zm`, `tm`, `rad`, `s`, `c`) stays an `f32` here while
//! `double` expressions such as `pow(...)` and `2 * 3.14159265368979 *
//! Twist * tm` stay `f64` and are narrowed on assignment, so the
//! angle/zoom evolution keeps its own rounding.
//!
//! # Arithmetic
//!
//! `tjs_int` arithmetic runs on 32-bit wrapping integer
//! maths, so every fixed-point computation here uses
//! `wrapping_mul`/`wrapping_div`/`wrapping_add` with truncating division —
//! identical results on overflow instead of a debug-build panic. Float →
//! `i32` conversions truncate toward zero, as the script's casts do (Rust
//! saturates only where the cast would be undefined).
//!
//! The three blit branches (`memcpy` for `stepx == 65536 &&
//! stepy == 0`, `TVPStretchCopy` for `stepy == 0`, `TVPLinTransCopy`
//! otherwise) are the same nearest-neighbour walk with the same `>> 16`
//! truncation, so [`sample`] serves all three; a sample outside the source
//! comes back transparent through `read_pixel` where a `memcpy`
//! would read past its buffer.
//!
//! # Errors
//!
//! `StartTransition` rejects only a missing/void `time`
//! (validated up front by [`Trans::new`](super::Trans::new) as
//! [`TransError::InvalidDuration`](super::TransError::InvalidDuration)),
//! a source-size mismatch (as
//! [`TransError::SizeMismatch`](super::TransError::SizeMismatch), also
//! checked by the caller) and clamps `time < 2` itself. `factor`, `accel`,
//! `twist`, `twistaccel`, `bgcolor` and `centerx`/`centery` are all read
//! without validation, so these entry points have no option of their own to
//! reject and always return `Ok`.

use super::{Trans, TransError, argb_to_rgba, read_pixel, write_pixel};
use crate::layer_ops::{RectI, intersect, intersect_rect};
use crate::scene::BitmapState;

/// `rotatezoom` options (`tTVPRotateZoomTransHandlerProvider`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ZoomParams {
    /// Initial zoom factor (`factor`, default 1).
    pub factor: f64,
    /// Zoom acceleration (`accel`, default 0).
    pub accel: f64,
    /// Twist in turns (`twist`, default 2).
    pub twist: f64,
    /// Twist acceleration in turns (`twistaccel`, default -2).
    pub twist_accel: f64,
    /// Rotation centre; it defaults to the image centre.
    pub center: Option<(i32, i32)>,
}

/// `rotatevanish` options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VanishParams {
    /// Shrink acceleration (`accel`, default 2).
    pub accel: f64,
    /// Twist in turns (`twist`, default 2).
    pub twist: f64,
    /// Twist acceleration in turns (`twistaccel`, default 2).
    pub twist_accel: f64,
    /// Rotation centre; it defaults to the image centre.
    pub center: Option<(i32, i32)>,
}

/// `rotateswap` options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SwapParams {
    /// Colour revealed by the rotation (`bgcolor`, `0xAARRGGBB`).
    pub background: i64,
    /// Twist in turns (`twist`, default 1).
    pub twist: f64,
    /// Rotation centre; it defaults to the image centre.
    pub center: Option<(i32, i32)>,
}

/// Which source a rasterized span holds (`tRotateDrawRegionData`'s
/// `type 1`/`type 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `type 1` — pixels of source 1.
    Src1,
    /// `type 2` — pixels of source 2.
    Src2,
}

/// A destination-space vertex of a placed quadrilateral (`tPoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Point {
    /// Horizontal destination coordinate.
    x: i32,
    /// Vertical destination coordinate.
    y: i32,
}

/// One scanline's source walk (`tRotateDrawLine`): 16.16 fixed point, one
/// step per destination pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DrawLine {
    /// First destination column the walk is defined for (the unclipped
    /// span start, `start`).
    start: i32,
    /// Source x at `start`, 16.16 fixed point (`sx`).
    sx: i32,
    /// Source y at `start`, 16.16 fixed point (`sy`).
    sy: i32,
    /// Source x advance per destination column, 16.16 (`stepx`).
    stepx: i32,
    /// Source y advance per destination column, 16.16 (`stepy`).
    stepy: i32,
}

/// One source's coverage of one scanline: the `tRotateDrawRegionData` span
/// clipped to `[0, width)` plus the walk that samples it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    /// First covered destination column (inclusive, `left`).
    left: i32,
    /// First destination column after the span (exclusive, `right`).
    right: i32,
    /// Source walk over `[left, right)` (the `tRotateDrawLine` this
    /// `AddSource` wrote for the line).
    draw: DrawLine,
}

/// One scanline of the frame (`tRotateDrawData`): at most one span per
/// source, because a single `AddSource` issues exactly one `AddLine` per
/// line and there are two sources.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LinePlan {
    /// Span covering this line with source 1, if any.
    src1: Option<Span>,
    /// Span covering this line with source 2, if any.
    src2: Option<Span>,
}

/// The two quadrilaterals a handler's `CalcPosition` places for this frame
/// and the order they were placed in — the source placed **last**
/// ([`top`](Quads::top)) wins where they overlap, exactly as a later
/// `AddLine` overwrites the region list of `tTVPBaseRotateTransHandler`.
#[derive(Debug, Clone, Copy)]
struct Quads {
    /// Quadrilateral sampling source 1.
    src1: [Point; 3],
    /// Quadrilateral sampling source 2.
    src2: [Point; 3],
    /// Source placed last, which wins on overlap.
    top: Source,
}

/// One frame's timing, the `StartProcess` state of
/// `tTVPBaseRotateTransHandler`.
#[derive(Debug, Clone, Copy)]
struct Frame {
    /// Handler width (`Width`) — the source width in pixels.
    width: i32,
    /// Handler height (`Height`) — the source height in pixels.
    height: i32,
    /// Elapsed milliseconds truncated to `tjs_int`
    /// (`(tjs_int)CurTime`).
    cur: i32,
    /// Total milliseconds truncated to `tjs_int`, at least 2 like the
    /// `if(time < 2) time = 2;` guard (`(tjs_int)Time`).
    time: i32,
    /// Whether the frame is the last one (`CurTime == Time`), which pins
    /// `rad` to zero in `CalcPosition`.
    end: bool,
}

impl Frame {
    /// Derive `CurTime`/`Time` from the transition clock.
    fn new(trans: &Trans, width: i32, height: i32) -> Self {
        let cur = trans.elapsed_ms() as i32;
        let time = (trans.duration_ms() as i32).max(2);
        let end = trans.elapsed_ms() >= trans.duration_ms();
        Self {
            width,
            height,
            cur,
            time,
            end,
        }
    }

    /// `float zm = (float)(tjs_int)CurTime / (float)(tjs_int)Time;` — the
    /// normalized animation position every `CalcPosition` starts from.
    fn time_fraction(&self) -> f32 {
        self.cur as f32 / self.time as f32
    }
}

/// The state `tTVPRotateZoomTransHandler` is constructed with — shared by
/// `rotatezoom` and `rotatevanish`, which differ only in these members.
#[derive(Debug, Clone, Copy)]
struct ZoomState {
    /// Initial zoom factor (`Factor`).
    factor: f64,
    /// Final zoom factor (`TargetFactor`) — 1 for `rotatezoom`, 0 for
    /// `rotatevanish`.
    target_factor: f64,
    /// Zoom easing (`Accel`): negative decelerates, positive accelerates,
    /// zero is linear.
    accel: f64,
    /// Turns the frame twists through (`Twist`).
    twist: f64,
    /// Twist easing (`TwistAccel`), shaped like [`Self::accel`].
    twist_accel: f64,
    /// Rotation centre (`CenterX`, `CenterY`).
    center: (i32, i32),
    /// `FixSrc1`: whether the untransformed full-screen quadrilateral holds
    /// source 1 (`rotatezoom`) or source 2 (`rotatevanish`).
    fix_src1: bool,
}

/// `65536 * num / den` with `tjs_int` arithmetic: the multiply wraps first
/// and the division truncates toward zero.
fn fixed_step(num: i32, den: i32) -> i32 {
    (65536i32).wrapping_mul(num).wrapping_div(den)
}

/// The source span a *descent* edge (ring index `0..=3`, walked backward)
/// traverses per its whole destination length: `Height`, `-Width`,
/// `-Height`, `Width` — the `sdstep` switch.
fn descent_span(edge: usize, width: i32, height: i32) -> i32 {
    match edge {
        0 => height,
        1 => width.wrapping_neg(),
        2 => height.wrapping_neg(),
        _ => width,
    }
}

/// The source span an *ascent* edge (ring index `0..=3`, walked forward)
/// traverses per its whole destination length: `Width`, `Height`,
/// `-Width`, `-Height` — the `sastep` switch.
fn ascent_span(edge: usize, width: i32, height: i32) -> i32 {
    match edge {
        0 => width,
        1 => height,
        2 => width.wrapping_neg(),
        _ => height.wrapping_neg(),
    }
}

/// Ring successor of a vertex index (`++` followed by `&= 3`).
fn next(index: usize) -> usize {
    (index + 1) & 3
}

/// Ring predecessor of a vertex index (`--` followed by `&= 3`; C reaches
/// `-1` and masks it to `3`, which `+ 3` reproduces for `0..4`).
fn prev(index: usize) -> usize {
    (index + 3) & 3
}

/// Store one clipped span for `source` on this line
/// (`Clip(left, right, 0, Width)` followed by `AddLine`).
fn add_line(
    plan: &mut LinePlan,
    source: Source,
    left: i32,
    right: i32,
    width: i32,
    draw: DrawLine,
) {
    let left = left.max(0);
    let right = right.min(width);
    if left >= right {
        return;
    }
    let span = Span { left, right, draw };
    match source {
        Source::Src1 => plan.src1 = Some(span),
        Source::Src2 => plan.src2 = Some(span),
    }
}

/// `tTVPBaseRotateTransHandler::AddSource`: rasterize the parallelogram
/// through `points` (three corners; the fourth is derived) into `lines`,
/// writing each covered scanline's span and source walk.
fn add_source(lines: &mut [LinePlan], points: [Point; 3], source: Source, width: i32, height: i32) {
    // The four vertices, ringed `0..4`.
    let mut px = [0i32; 4];
    let mut py = [0i32; 4];

    // - upper-left
    px[0] = points[0].x;
    py[0] = points[0].y;
    let mut leftlimit = px[0];
    let mut rightlimit = px[0];
    let mut scanlinestart = py[0];
    let mut toppoint = 0usize;
    let mut scanlineend = py[0];
    let mut bottompoint = 0usize;

    // - upper-right
    px[1] = points[1].x;
    py[1] = points[1].y;
    if leftlimit > px[1] {
        leftlimit = px[1];
    }
    if rightlimit < px[1] {
        rightlimit = px[1];
    }
    if scanlinestart > py[1] {
        scanlinestart = py[1];
        toppoint = 1;
    }
    if scanlineend < py[1] {
        scanlineend = py[1];
        bottompoint = 1;
    }

    // - bottom-right: the parallelogram corner derived from the three
    //   given points.
    px[2] = points[1]
        .x
        .wrapping_sub(points[0].x)
        .wrapping_add(points[2].x);
    py[2] = points[1]
        .y
        .wrapping_sub(points[0].y)
        .wrapping_add(points[2].y);
    if leftlimit > px[2] {
        leftlimit = px[2];
    }
    if rightlimit < px[2] {
        rightlimit = px[2];
    }
    if scanlinestart > py[2] {
        scanlinestart = py[2];
        toppoint = 2;
    }
    if scanlineend < py[2] {
        scanlineend = py[2];
        bottompoint = 2;
    }

    // - bottom-left
    px[3] = points[2].x;
    py[3] = points[2].y;
    if leftlimit > px[3] {
        leftlimit = px[3];
    }
    if rightlimit < px[3] {
        rightlimit = px[3];
    }
    if scanlinestart > py[3] {
        scanlinestart = py[3];
        toppoint = 3;
    }
    if scanlineend < py[3] {
        scanlineend = py[3];
        bottompoint = 3;
    }

    // check destrect intersections
    if leftlimit >= width {
        return;
    }
    if rightlimit < 0 {
        return;
    }
    if scanlinestart >= height {
        return;
    }
    if scanlineend < 0 {
        return;
    }

    // prepare to transform...
    let mut pdstepx;
    let mut pastepx;
    let mut pdx;
    let mut pax;
    let mut sdstep;
    let mut sastep;
    let mut sd;
    let mut sa;
    let mut y = if 0 < scanlinestart { scanlinestart } else { 0 };
    let ylim = if height - 1 < scanlineend {
        height - 1
    } else {
        scanlineend
    };

    // skip to the first scanline

    // - for descent
    let mut pd = toppoint;
    let mut pdnext = prev(toppoint);

    while pdnext != bottompoint && py[pdnext] < y {
        pdnext = prev(pdnext);
        pd = prev(pd);
    }

    while pdnext != bottompoint && py[pdnext] == y {
        pd = prev(pd);
        pdnext = prev(pdnext);
    }

    // - for ascent
    let mut pa = toppoint;
    let mut panext = next(toppoint);

    while panext != bottompoint && py[panext] < y {
        panext = next(panext);
        pa = next(pa);
    }

    while panext != bottompoint && py[panext] == y {
        pa = next(pa);
        panext = next(panext);
    }

    // compute initial horizontal step per a line

    // - for descent
    let dy_descent = py[pdnext].wrapping_sub(py[pd]).wrapping_add(1);
    pdstepx = if dy_descent != 0 {
        fixed_step(px[pdnext].wrapping_sub(px[pd]), dy_descent)
    } else {
        65536
    };

    // - for ascent
    let dy_ascent = py[panext].wrapping_sub(py[pa]).wrapping_add(1);
    pastepx = if dy_ascent != 0 {
        fixed_step(px[panext].wrapping_sub(px[pa]), dy_ascent)
    } else {
        65536
    };

    // compute initial source step

    // - for descent
    sdstep = if dy_descent != 0 {
        fixed_step(descent_span(pd, width, height), dy_descent)
    } else {
        65536
    };

    // - for ascent
    sastep = if dy_ascent != 0 {
        fixed_step(ascent_span(pa, width, height), dy_ascent)
    } else {
        65536
    };

    // compute initial horizontal position

    // - for descent
    pdx = px[pd].wrapping_mul(65536);
    if py[pd] < y {
        pdx = pdx.wrapping_add(pdstepx.wrapping_mul(y - py[pd]));
    }

    // - for ascent
    pax = px[pa].wrapping_mul(65536);
    if py[pa] < y {
        pax = pax.wrapping_add(pastepx.wrapping_mul(y - py[pa]));
    }

    // The two ends of a source edge in 16.16: `Width * 65536 - 1` and
    // `Height * 65536 - 1`.
    let src_right = width.wrapping_mul(65536).wrapping_sub(1);
    let src_bottom = height.wrapping_mul(65536).wrapping_sub(1);

    // compute initial source position

    // - for descent
    sd = match pd {
        0 => 0,
        1 => src_right,
        2 => src_bottom,
        _ => 0,
    };
    if py[pd] < y {
        sd = sd.wrapping_add(sdstep.wrapping_mul(y - py[pd]));
    }

    // - for ascent
    sa = match pa {
        0 => 0,
        1 => 0,
        2 => src_right,
        _ => src_bottom,
    };
    if py[pa] < y {
        sa = sa.wrapping_add(sastep.wrapping_mul(y - py[pa]));
    }

    // process per a line
    while y <= ylim {
        // - compute descent x and ascent x
        let mut ddx = pdx >> 16;
        let mut adx = pax >> 16;

        // - compute descent source position
        let (mut dx, mut dy) = match pd {
            0 => (0, sd),
            1 => (sd, 0),
            2 => (src_right, sd),
            _ => (sd, src_bottom),
        };

        // - compute ascent source position
        let (mut ax, mut ay) = match pa {
            0 => (sa, 0),
            1 => (src_right, sa),
            2 => (sa, src_bottom),
            _ => (0, sa),
        };

        // - swap dx/dy ax/ay dax/ddx if dax < ddx
        if adx < ddx {
            std::mem::swap(&mut dx, &mut ax);
            std::mem::swap(&mut dy, &mut ay);
            std::mem::swap(&mut adx, &mut ddx);
        }

        // - compute source step across the span
        let (stepx, stepy) = if adx != ddx {
            let span = adx.wrapping_sub(ddx).wrapping_add(1);
            (
                ax.wrapping_sub(dx).wrapping_add(1).wrapping_div(span),
                ay.wrapping_sub(dy).wrapping_add(1).wrapping_div(span),
            )
        } else {
            (65536, 65536)
        };

        // add line — `start` is kept unclipped so a span cut
        // by the screen edge keeps extrapolating its walk.
        add_line(
            &mut lines[y as usize],
            source,
            ddx,
            adx.wrapping_add(1),
            width,
            DrawLine {
                start: ddx,
                sx: dx,
                sy: dy,
                stepx,
                stepy,
            },
        );

        // check descent point
        if py[pdnext] == y {
            loop {
                pd = prev(pd);
                pdnext = prev(pdnext);
                if pdnext == bottompoint || py[pdnext] != y {
                    break;
                }
            }

            let dy = py[pdnext].wrapping_sub(py[pd]).wrapping_add(1);
            pdstepx = if dy != 0 {
                fixed_step(px[pdnext].wrapping_sub(px[pd]), dy)
            } else {
                65536
            };

            sdstep = if dy != 0 {
                fixed_step(descent_span(pd, width, height), dy)
            } else {
                65536
            };

            sd = match pd {
                0 => 0,
                1 => src_right,
                2 => src_bottom,
                _ => 0,
            };

            pdx = px[pd].wrapping_mul(65536);
        }

        // check ascent point
        if py[panext] == y {
            loop {
                pa = next(pa);
                panext = next(panext);
                if panext == bottompoint || py[panext] != y {
                    break;
                }
            }

            let dy = py[panext].wrapping_sub(py[pa]).wrapping_add(1);
            pastepx = if dy != 0 {
                fixed_step(px[panext].wrapping_sub(px[pa]), dy)
            } else {
                65536
            };

            sastep = if dy != 0 {
                fixed_step(ascent_span(pa, width, height), dy)
            } else {
                65536
            };

            sa = match pa {
                0 => 0,
                1 => 0,
                2 => src_right,
                _ => src_bottom,
            };

            pax = px[pa].wrapping_mul(65536);
        }

        // to next ...
        pdx = pdx.wrapping_add(pdstepx);
        pax = pax.wrapping_add(pastepx);
        sd = sd.wrapping_add(sdstep);
        sa = sa.wrapping_add(sastep);
        y += 1;
    }
}

/// Sample `src` at destination column `x` through `draw`
/// (`TVPLinTransCopy`/`TVPStretchCopy`/the `memcpy` branch of `Process`,
/// all of which truncate the 16.16 walk with `>> 16`).
fn sample(src: &BitmapState, draw: DrawLine, x: i32) -> [u8; 4] {
    let offset = x.wrapping_sub(draw.start);
    let sx = draw.sx.wrapping_add(offset.wrapping_mul(draw.stepx)) >> 16;
    let sy = draw.sy.wrapping_add(offset.wrapping_mul(draw.stepy)) >> 16;
    read_pixel(src, sx, sy)
}

/// One destination pixel: the span of the source placed last, else the
/// span of the other source, else the background colour
/// (`tTVPBaseRotateTransHandler::Process`'s region walk).
fn shade(
    plan: LinePlan,
    top: Source,
    src1: &BitmapState,
    src2: &BitmapState,
    x: i32,
    background: [u8; 4],
) -> [u8; 4] {
    let (late_plan, early_plan) = match top {
        Source::Src1 => (plan.src1, plan.src2),
        Source::Src2 => (plan.src2, plan.src1),
    };
    let (late, early) = match top {
        Source::Src1 => (src1, src2),
        Source::Src2 => (src2, src1),
    };
    if let Some(span) = late_plan.filter(|span| x >= span.left && x < span.right) {
        return sample(late, span.draw, x);
    }
    if let Some(span) = early_plan.filter(|span| x >= span.left && x < span.right) {
        return sample(early, span.draw, x);
    }
    background
}

/// The shared base — `tTVPBaseRotateTransHandler` — for `0 < phase <
/// phase_max` (the caller owns both endpoints).
///
/// `calc` is the handler's `CalcPosition`: it reads `Frame` and returns the
/// frame's two quadrilaterals. The base rasterizes them, then renders
/// `region` like `Process`.
fn base_apply(
    trans: &Trans,
    dst: &mut BitmapState,
    src1: &BitmapState,
    src2: &BitmapState,
    region: RectI,
    background: i64,
    calc: impl FnOnce(&Frame) -> Quads,
) -> Result<(), TransError> {
    let width = src1.width.min(i32::MAX as u32) as i32;
    let height = src1.height.min(i32::MAX as u32) as i32;
    let frame = Frame::new(trans, width, height);

    // `StartProcess`: place both quadrilaterals on a background-only
    // frame. Nothing is drawn yet, so an empty processing rectangle
    // simply costs nothing.
    let Some(rect) =
        intersect_rect(region, (0, 0, width, height)).and_then(|rect| intersect(dst, rect))
    else {
        return Ok(());
    };

    let quads = calc(&frame);
    let mut lines = vec![LinePlan::default(); height as usize];
    add_source(&mut lines, quads.src1, Source::Src1, width, height);
    add_source(&mut lines, quads.src2, Source::Src2, width, height);

    // `Process`: one pass over the requested scanlines.
    let background = argb_to_rgba(background);
    let (x0, y0, x1, y1) = rect;
    for y in y0..y1 {
        let plan = lines[y as usize];
        for x in x0..x1 {
            write_pixel(dst, x, y, shade(plan, quads.top, src1, src2, x, background));
        }
    }
    dst.mark_dirty();
    Ok(())
}

/// `tTVPRotateZoomTransHandler::CalcPosition`: the untransformed
/// full-screen quadrilateral for one source and the rotating, zooming one
/// for the other.
///
/// `zm` eases `CurTime / Time` through `Accel` for the zoom and `cx`/`cy`
/// (the source point that lands on the image centre), `tm` eases the same
/// fraction through `TwistAccel` for `rad`; the zoom itself interpolates
/// from `factor` to `target_factor` along the eased `zm`. With `FixSrc1`
/// the full-screen quadrilateral belongs to source 1 and is placed first,
/// so the moving one (source 2) is the one that wins overlaps — reversed
/// for `rotatevanish`.
fn zoom_quads(frame: &Frame, state: &ZoomState) -> Quads {
    let width = frame.width;
    let height = frame.height;
    let scx = width / 2;
    let scy = height / 2;
    let w1 = width.wrapping_sub(1);
    let h1 = height.wrapping_sub(1);

    // The stationary quadrilateral: `points[0] = (0,0)`,
    // `points[1] = (Width-1,0)`, `points[2] = (0,Height-1)`.
    let full = [
        Point { x: 0, y: 0 },
        Point { x: w1, y: 0 },
        Point { x: 0, y: h1 },
    ];

    // eased zoom position
    let mut zm = frame.time_fraction();
    if state.accel < 0.0 {
        // 減速 ( start fast, slow down )
        zm = (1.0 - f64::from(zm)) as f32;
        zm = f64::powf(f64::from(zm), -state.accel) as f32;
        zm = (1.0 - f64::from(zm)) as f32;
    } else if state.accel > 0.0 {
        // 加速 ( start slow, speed up )
        zm = f64::powf(f64::from(zm), state.accel) as f32;
    }

    // the source point that lands on the image centre
    let cx = (scx.wrapping_sub(state.center.0) as f32 * zm + state.center.0 as f32) as i32;
    let cy = (scy.wrapping_sub(state.center.1) as f32 * zm + state.center.1 as f32) as i32;

    // eased twist position
    let mut tm = frame.time_fraction();
    if state.twist_accel < 0.0 {
        tm = (1.0 - f64::from(tm)) as f32;
        tm = f64::powf(f64::from(tm), -state.twist_accel) as f32;
        tm = (1.0 - f64::from(tm)) as f32;
    } else if state.twist_accel > 0.0 {
        tm = f64::powf(f64::from(tm), state.twist_accel) as f32;
    }

    let rad = if frame.end {
        0.0f32
    } else {
        (2.0 * 3.14159265368979 * state.twist * f64::from(tm)) as f32
    };
    let zoom = ((state.target_factor - state.factor) * f64::from(zm) + state.factor) as f32;
    let s = (f64::from(rad).sin() * f64::from(zoom)) as f32;
    let c = (f64::from(rad).cos() * f64::from(zoom)) as f32;

    // `points[0..2]` of the moving quadrilateral.
    let (ncx, ncy) = (cx.wrapping_neg(), cy.wrapping_neg());
    let moving = [
        Point {
            x: (ncx as f32 * c + ncy as f32 * s + scx as f32) as i32,
            y: (ncx as f32 * -s + ncy as f32 * c + scy as f32) as i32,
        },
        Point {
            x: (w1.wrapping_sub(cx) as f32 * c + ncy as f32 * s + scx as f32) as i32,
            y: (w1.wrapping_sub(cx) as f32 * -s + ncy as f32 * c + scy as f32) as i32,
        },
        Point {
            x: (ncx as f32 * c + h1.wrapping_sub(cy) as f32 * s + scx as f32) as i32,
            y: (ncx as f32 * -s + h1.wrapping_sub(cy) as f32 * c + scy as f32) as i32,
        },
    ];

    if state.fix_src1 {
        Quads {
            src1: full,
            src2: moving,
            top: Source::Src2,
        }
    } else {
        Quads {
            src1: moving,
            src2: full,
            top: Source::Src1,
        }
    }
}

/// `tTVPRotateSwapTransHandler::CalcPosition`: source 1 shrinks and twists
/// toward one corner while source 2 grows out of the opposite one, over
/// `bgcolor`.
///
/// The `cnt`-driven `switch` always computes both
/// quadrilaterals but enters at `case 0` (source 1 first) once
/// `CurTime >= Time / 2` and at `case 1` before that, so the source placed
/// last — [`Quads::top`] — flips at the midpoint.
///
/// `rotateswap` does not read `centerx`/`centery`; its
/// formulas pivot on the image centre `Width/2`, `Height/2`, which is what
/// `center` defaults to here, and a supplied centre replaces that pivot
/// everywhere `scx`/`scy` is spelled.
fn swap_quads(frame: &Frame, twist: f64, center: (i32, i32)) -> Quads {
    let width = frame.width;
    let height = frame.height;
    let (scx, scy) = center;
    let w1 = width.wrapping_sub(1);
    let h1 = height.wrapping_sub(1);
    let zm = frame.time_fraction();
    // `float Twist = twist * 3.14159265368979 * 2;` — a float member.
    let twist = (twist * 3.14159265368979 * 2.0) as f32;

    // --- source 1: `tm = zm * zm` — it is already full size at the start
    // and collapses into the top-left corner by the end.
    let tm1 = zm * zm;
    let head = scx.wrapping_neg() as f32 * tm1 + scx as f32;
    let cx1 =
        (f64::from(head) + (f64::from(tm1) * 3.14159265368979).sin() * f64::from(scx) * 1.5) as i32;
    let cy1 = (scy.wrapping_neg() as f32 * tm1 + scy as f32) as i32;
    let rad1 = tm1 * twist;
    let shrink = (1.0 - f64::from(tm1)) as f32;
    let s1 = (f64::from(rad1).sin() * f64::from(shrink)) as f32;
    let c1 = (f64::from(rad1).cos() * f64::from(shrink)) as f32;
    let quad1 = [
        Point {
            x: (scx.wrapping_neg() as f32 * c1 + scy.wrapping_neg() as f32 * s1 + cx1 as f32)
                as i32,
            y: ((scx.wrapping_neg() as f32 * -s1 + scy.wrapping_neg() as f32 * c1) * shrink
                + cy1 as f32) as i32,
        },
        Point {
            x: (w1.wrapping_sub(scx) as f32 * c1 + scy.wrapping_neg() as f32 * s1 + cx1 as f32)
                as i32,
            y: ((w1.wrapping_sub(scx) as f32 * -s1 + scy.wrapping_neg() as f32 * c1) * shrink
                + cy1 as f32) as i32,
        },
        Point {
            x: (scx.wrapping_neg() as f32 * c1 + h1.wrapping_sub(scy) as f32 * s1 + cx1 as f32)
                as i32,
            y: ((scx.wrapping_neg() as f32 * -s1 + h1.wrapping_sub(scy) as f32 * c1) * shrink
                + cy1 as f32) as i32,
        },
    ];

    // --- source 2: `tm = 1 - (1 - zm)²` — it is a point in the
    // bottom-right corner at the start and fills the image by the end.
    let tm2 = (1.0 - (1.0 - f64::from(zm)) * (1.0 - f64::from(zm))) as f32;
    let head = scx.wrapping_sub(w1) as f32 * tm2 + w1 as f32;
    let cx2 =
        (f64::from(head) - (f64::from(tm2) * 3.14159265368979).sin() * f64::from(scx) * 1.5) as i32;
    let cy2 = (scy.wrapping_sub(h1) as f32 * tm2 + h1 as f32) as i32;
    // `rad = (-1.0 + tm) * Twist;` — a double product narrowed to the
    // `float rad` the trigonometry then reads.
    let rad2 = ((-1.0 + f64::from(tm2)) * f64::from(twist)) as f32;
    let s2 = (f64::from(rad2).sin() * f64::from(tm2)) as f32;
    let c2 = (f64::from(rad2).cos() * f64::from(tm2)) as f32;
    let quad2 = [
        Point {
            x: (scx.wrapping_neg() as f32 * c2 + scy.wrapping_neg() as f32 * s2 + cx2 as f32)
                as i32,
            y: ((scx.wrapping_neg() as f32 * -s2 + scy.wrapping_neg() as f32 * c2) * tm2
                + cy2 as f32) as i32,
        },
        Point {
            x: (w1.wrapping_sub(scx) as f32 * c2 + scy.wrapping_neg() as f32 * s2 + cx2 as f32)
                as i32,
            y: ((w1.wrapping_sub(scx) as f32 * -s2 + scy.wrapping_neg() as f32 * c2) * tm2
                + cy2 as f32) as i32,
        },
        Point {
            x: (scx.wrapping_neg() as f32 * c2 + h1.wrapping_sub(scy) as f32 * s2 + cx2 as f32)
                as i32,
            y: ((scx.wrapping_neg() as f32 * -s2 + h1.wrapping_sub(scy) as f32 * c2) * tm2
                + cy2 as f32) as i32,
        },
    ];

    let top = if frame.cur >= frame.time / 2 {
        Source::Src2
    } else {
        Source::Src1
    };
    Quads {
        src1: quad1,
        src2: quad2,
        top,
    }
}

/// Composite the `rotatezoom` transition for `0 < phase < phase_max` (the
/// caller owns both endpoints): source 1 fills the image and source 2
/// rotates and zooms onto it from `factor` to 1.
///
/// See the [module docs](self) for the shared base and why this function
/// cannot fail.
pub(crate) fn apply_zoom(
    trans: &Trans,
    dst: &mut BitmapState,
    src1: &BitmapState,
    src2: &BitmapState,
    region: RectI,
    params: ZoomParams,
) -> Result<(), TransError> {
    base_apply(trans, dst, src1, src2, region, 0, |frame| {
        // `centerx = src1w / 2`, `centery = src1h / 2`.
        let center = params.center.unwrap_or((frame.width / 2, frame.height / 2));
        zoom_quads(
            frame,
            &ZoomState {
                factor: params.factor,
                target_factor: 1.0,
                accel: params.accel,
                twist: params.twist,
                twist_accel: params.twist_accel,
                center,
                fix_src1: true,
            },
        )
    })
}

/// Composite the `rotatevanish` transition for `0 < phase < phase_max`
/// (the caller owns both endpoints): source 2 fills the image while source
/// 1 twists and shrinks from 1 to a point, accelerating as it goes.
///
/// See the [module docs](self) for the shared base and why this function
/// cannot fail.
pub(crate) fn apply_vanish(
    trans: &Trans,
    dst: &mut BitmapState,
    src1: &BitmapState,
    src2: &BitmapState,
    region: RectI,
    params: VanishParams,
) -> Result<(), TransError> {
    base_apply(trans, dst, src1, src2, region, 0, |frame| {
        // `centerx = src1w / 2`, `centery = src1h / 2`.
        let center = params.center.unwrap_or((frame.width / 2, frame.height / 2));
        zoom_quads(
            frame,
            &ZoomState {
                factor: 1.0,
                target_factor: 0.0,
                accel: params.accel,
                twist: params.twist,
                twist_accel: params.twist_accel,
                center,
                fix_src1: false,
            },
        )
    })
}

/// Composite the `rotateswap` transition for `0 < phase < phase_max` (the
/// caller owns both endpoints): source 1 shrinks out of sight while source
/// 2 grows into place, both twisting, over `background`.
///
/// See the [module docs](self) for the shared base and why this function
/// cannot fail.
pub(crate) fn apply_swap(
    trans: &Trans,
    dst: &mut BitmapState,
    src1: &BitmapState,
    src2: &BitmapState,
    region: RectI,
    params: SwapParams,
) -> Result<(), TransError> {
    base_apply(trans, dst, src1, src2, region, params.background, |frame| {
        // `rotateswap` pivots on `Width/2`, `Height/2`
        // and reads no centre option; that is the default here.
        let center = params.center.unwrap_or((frame.width / 2, frame.height / 2));
        swap_quads(frame, params.twist, center)
    })
}
