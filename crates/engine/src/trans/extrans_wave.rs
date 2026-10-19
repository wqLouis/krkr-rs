//! The `wave` transition of `extrans.dll` (`sample/extrans/wave.cpp`).
//!
//! The reference (`tTVPWaveTransHandler`) slides every scanline of the two
//! sources horizontally under a travelling sine: row `y` is sampled at
//! `x - d(y)` with `d(y) = trunc(sin(rad) · CurH)` and
//! `rad = y · CurOmega + CurRadStart`, the two sources are cross-faded at
//! `BlendRatio` (`TVPConstAlphaBlend_SD`), and the strip a shift uncovers
//! is painted with `CurBGColor` — `bgcolor1` and `bgcolor2` cross-faded at
//! the same ratio. `CurH` (amplitude) and `CurOmega` (angular frequency)
//! are recomputed on every update, so the wave swells and decays over the
//! transition instead of running at a fixed strength.
//!
//! # The wave schedule (`StartProcess`)
//!
//! Let `Time` be the transition duration and `HalfTime = Time / 2` —
//! integer division in the reference (`tjs_uint64 HalfTime = time / 2`).
//! The elapsed time is folded around the midpoint (`t >= HalfTime` →
//! `t = Time - t`, negative → `0`), then raised through a quarter sine:
//!
//! ```text
//! tt        = sin((PI_TRUNCATED/2.0) · t / HalfTime)   // 0 ..= 1
//! CurH      = trunc(tt · maxh)
//! CurOmega  = maxomega · tt                          (wavetype 0)
//!           | maxomega · (Time − CurTime) / Time     (wavetype 1, easing out)
//!           | maxomega · CurTime / Time              (wavetype 2, easing in)
//! CurRadStart = −CurOmega · (Height / 2)             // integer half-height
//! BlendRatio  = trunc(CurTime · 255 / Time), capped at 255
//! ```
//!
//! The transition clock is the millisecond clock of the reference
//! (`TVPGetTickCount`); L2 hands it over as [`Trans::elapsed_ms`] and
//! [`Trans::duration_ms`] and the formulas above consume it directly —
//! every *integer* operation of the C code (`HalfTime = time / 2`, the
//! `tjs_int` casts of `CurH`/`BlendRatio`/`d`) stays an integer operation
//! through `as` casts, while the `double` maths stays `f64`.
//!
//! # Rendering (`Process`)
//!
//! Per row the reference clips three spans: the background strip `[0, d)`
//! for `d > 0`, the background strip `[Width + d, Width)` for `d < 0`, and
//! the blend span `[d, d + Width)` (samples `[0, Width)`, always in range).
//! This port instead tests `0 <= x - d < Width` per pixel, which is the
//! same partition of `[0, Width)`: exactly where `x - d` falls outside the
//! source rows, the background colour is written; everywhere else the
//! displaced row is blended. That also covers a destination wider (or
//! taller) than the sources, where the reference's buffers never differ.
//!
//! The caller ([`Trans::apply`](super::Trans::apply)) already copies
//! source 1 at `phase <= 0` and source 2 at `phase >= phase_max`, so this
//! module only ever runs mid-flight — the same place the reference's
//! `EndProcess` keeps running until `BlendRatio == 255`.
//!
//! # Errors
//!
//! The reference's `StartTransition` rejects only a missing/void `time`
//! (validated up front by [`Trans::new`](super::Trans::new)) and sources of
//! different sizes (checked by the caller as
//! [`TransError::SizeMismatch`](super::TransError::SizeMismatch)). Two of
//! its silent guards become typed errors here, because L2 never clamps
//! silently:
//!
//! * `time < 2` — the reference raises short times to `2` so `HalfTime`,
//!   its sine divisor, cannot be zero; this port returns
//!   [`TransError::InvalidOption`] instead.
//! * `wavetype` outside `0..=2` — the reference's `switch` has no `default`
//!   and would read an uninitialized (stale) `CurOmega`; rejected here.
//!
//! # Not ported
//!
//! `Process` dispatches on the destination layer type between
//! `TVPConstAlphaBlend_SD`, `_a` (additive alpha) and `_d` (alpha-vs-alpha).
//! L2 stores one straight-alpha representation, so all three collapse into
//! [`blend_level`], the shared `TVPConstAlphaBlend_SD` arithmetic of the
//! parent module.

use super::{PI_TRUNCATED, Trans, TransError, argb_to_rgba, blend_level, read_pixel, write_pixel};
use crate::layer_ops::{RectI, intersect};
use crate::scene::BitmapState;

/// The `wave` options of `wave.cpp`'s `StartTransition`, as the caller
/// parsed them from the script.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Params {
    /// Peak displacement in pixels (`maxh`, default 50).
    pub max_height: i32,
    /// Angular frequency (`maxomega`, default 0.2).
    pub max_omega: f64,
    /// Colour behind source 1 (`bgcolor1`, `0xAARRGGBB`).
    pub background1: i64,
    /// Colour behind source 2 (`bgcolor2`, `0xAARRGGBB`).
    pub background2: i64,
    /// Wave shape selector (`wavetype`, default 0).
    pub wave_type: i32,
}

/// Render one `wave` frame of `trans` into `dst`, restricted to `region`.
///
/// This is `tTVPWaveTransHandler::StartProcess` + `Process`: the wave
/// parameters are recomputed from the elapsed time exactly like the
/// reference recomputes them from its tick, then every row of `region` is
/// displaced, blended and backed with the background colour.
///
/// # Errors
///
/// Returns [`TransError::InvalidOption`] rather than a silent clamp:
/// `time < 2` ms (the reference raises it to 2 so `HalfTime = time / 2`
/// is non-zero) or a `wavetype` outside `0..=2` (the reference's `switch`
/// has no `default` and would read a stale `CurOmega`).
// The truncated π literal (`PI_TRUNCATED`) keeps the `sin` arguments at the
// exact values the boundary pixels were tuned for; `f64::consts::PI` differs
// in the last digits and would move them.
pub(crate) fn apply(
    trans: &Trans,
    dst: &mut BitmapState,
    src1: &BitmapState,
    src2: &BitmapState,
    region: RectI,
    params: Params,
) -> Result<(), TransError> {
    let Params {
        max_height,
        max_omega,
        background1,
        background2,
        wave_type,
    } = params;

    // -- StartTransition guards ----------------------------------------
    // `Time`: whole milliseconds from the script. The reference clamps
    // `time < 2` up to 2 (comment: "小さい数値を指定すると問題が発生する");
    // L2 reports it instead. `HalfTime` below would be zero otherwise.
    let time = trans.duration_ms();
    if time < 2.0 {
        return Err(TransError::InvalidOption {
            option: "time",
            reason: "must be at least 2 ms",
        });
    }
    if !(0..=2).contains(&wave_type) {
        return Err(TransError::InvalidOption {
            option: "wavetype",
            reason: "must be 0, 1 or 2",
        });
    }

    // `HalfTime = time / 2` — integer division on `tjs_uint64`.
    let half_time = (time as u64 / 2) as f64;

    // -- StartProcess: the per-update wave parameters -------------------
    // `tjs_int64 t = CurTime = tick - StartTick`, where only `CurTime` is
    // clamped to `Time` (the folded `t` is clamped at 0 instead).
    let elapsed = trans.elapsed_ms();
    let cur_time = elapsed.min(time);
    let mut t = elapsed;
    if t >= half_time {
        t = time - t;
    }
    if t < 0.0 {
        t = 0.0;
    }

    // `tt = sin((PI_TRUNCATED/2.0) * t / HalfTime)` — the truncated π literal.
    let tt = (PI_TRUNCATED / 2.0 * t / half_time).sin();

    // `CurH = tt * MaxH` — `double` narrowed to `tjs_int` by truncation.
    let cur_height = (tt * max_height as f64) as i32;

    let width = src1.width.min(i32::MAX as u32) as i32;
    let height = src1.height.min(i32::MAX as u32) as i32;

    // `CurOmega`: 0 eases with the amplitude envelope, 1 eases out (the
    // frequency runs down to zero), 2 eases in (it runs up from zero).
    let cur_omega = match wave_type {
        1 => max_omega * (time - cur_time) / time,
        2 => max_omega * cur_time / time,
        _ => max_omega * tt,
    };

    // `CurRadStart = -CurOmega * (Height / 2)` — integer division over the
    // full source height, kept as a separate term so the rounding of the
    // sum matches the reference.
    let rad_start = -cur_omega * ((height / 2) as f64);

    // `BlendRatio = CurTime * 255 / Time` (integer division in the C),
    // then the reference's `if(BlendRatio > 255) BlendRatio = 255;` —
    // unreachable because `CurTime <= Time`, kept for parity.
    let mut blend_ratio = ((cur_time * 255.0) / time) as i32;
    if blend_ratio > 255 {
        blend_ratio = 255;
    }

    // `CurBGColor = Blend(BGColor1, BGColor2, BlendRatio)` — the per-
    // channel `a + ((b - a) * opa >> 8)` of `common.h`'s `Blend`, which
    // is `blend_level`.
    let background = blend_level(
        argb_to_rgba(background1),
        argb_to_rgba(background2),
        blend_ratio,
    );

    let Some((x0, y0, x1, y1)) = intersect(dst, region) else {
        return Ok(());
    };

    // `rad = data->Top * CurOmega + CurRadStart`, advanced by one
    // `CurOmega` per scanline (the reference accumulates it the same way).
    let mut rad = (y0 as f64) * cur_omega + rad_start;
    for y in y0..y1 {
        // `d = (tjs_int)(sin(rad) * CurH)` — truncation toward zero.
        let d = (rad.sin() * cur_height as f64) as i32;
        rad += cur_omega;
        for x in x0..x1 {
            let sx = i64::from(x) - i64::from(d);
            let pixel = if (0..i64::from(width)).contains(&sx) && y < height {
                // The reference's blend span `[d, d + Width)`: both sources
                // sampled at `x - d`, cross-faded at `BlendRatio`.
                let sx = sx as i32;
                blend_level(
                    read_pixel(src1, sx, y),
                    read_pixel(src2, sx, y),
                    blend_ratio,
                )
            } else {
                // The strips `[0, d)` / `[Width + d, Width)` the reference
                // fills with `TVPFillARGB`, i.e. a source sample out of
                // bounds.
                background
            };
            write_pixel(dst, x, y, pixel);
        }
    }
    dst.mark_dirty();
    Ok(())
}
