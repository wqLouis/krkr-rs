//! `mosaic` — the `extrans.dll` rectangular-mosaic dissolve.
//!
//! Port of `tTVPMosaicTransHandler` and
//! `tTVPMosaicTransHandlerProvider` (`sample/extrans/mosaic.cpp`). The
//! frame is cut into a square block grid; every block is painted with one
//! colour sampled from the block centre of both sources and blended from
//! source 1 to source 2 as the phase advances. The block size follows a
//! triangle wave between 2 pixels and `max_size`: it peaks at `max_size`
//! in the middle of the transition and shrinks back to 2 pixels towards
//! both ends, so the picture breaks into coarse tiles mid-transition and
//! settles into fine ones as it finishes.
//!
//! # Mapping decisions
//!
//! * **Clock.** The reference drives the handler from a millisecond tick
//!   (`StartProcess(tick)`, `Time`, `HalfTime = Time / 2`); this port runs
//!   the same integer formulas on the L2 phase clock: `Time` is
//!   [`Trans::phase_max`] and `CurTime` is [`Trans::phase`]. `mosaic` is
//!   never `universal`, so [`Trans::new`] pins `phase_max` to
//!   [`PHASE_MAX`](super::PHASE_MAX) (`255`) and `HalfTime` is
//!   `255 / 2 = 127` — never zero.
//!   `BlendRatio = CurTime * 255 / Time` therefore collapses to `phase`
//!   exactly, and `CurBlockSize` keeps the reference's truncating integer
//!   division, quantised to the `1/255` phase steps instead of
//!   milliseconds.
//! * **Destination offset.** The reference's `destxofs = DestLeft - Left`
//!   shifts the painting into a separate destination scan line provider;
//!   L2 composites `dst`, `src1` and `src2` in one coordinate space, so
//!   the offset is zero and the processing rectangle is `region` clipped
//!   to `dst` (exactly the `data->Left/Top/Width/Height` rectangle the
//!   reference clips `FILL_ONE` against).
//! * **Colour.** Each block takes the colour of a *single* sample at the
//!   unclipped block centre (`mx = l + (bs >> 1)`, clamped to the image) —
//!   the reference deliberately does not average the block (see the note
//!   under `FILL_ONE`). The blend is the reference `Blend` arithmetic,
//!   [`blend_level`], on the straight-alpha RGBA buffer; the reference's
//!   caveat that the handler mishandles an alpha-carrying destination
//!   applies verbatim.
//!
//! # Errors
//!
//! [`TransError::InvalidOption`] for a `max_size` the reference grid
//! arithmetic cannot run with (see [`MosaicGrid::new`]); nothing is
//! clamped silently.

use super::{Trans, TransError, blend_level, read_pixel, write_pixel};
use crate::layer_ops::{RectI, intersect};
use crate::scene::BitmapState;

/// Render one `mosaic` frame of the transition into `dst`.
///
/// The endpoints are already handled by [`Trans::apply`] (it copies `src1`
/// at `phase <= 0` and `src2` at `phase >= phase_max` before dispatching),
/// so this only ever runs for `0 < phase < phase_max`.
///
/// # Parameters
///
/// * `trans` — the transition state; only [`Trans::phase`] and
///   [`Trans::phase_max`] are read, the L2 equivalent of the reference
///   `StartProcess(tick)` clock (`CurTime`/`Time`).
/// * `dst` — destination bitmap, written block by block and marked dirty
///   afterwards; only the intersection of `region` with its bounds is
///   painted, the reference's `DestLeft`/`DestTop` offset is zero here.
/// * `src1` — first source (the image the transition starts from); its
///   dimensions define the block grid (`Width`/`Height` of the reference
///   handler) and clamp the block-centre sample.
/// * `src2` — second source (the image the transition dissolves into),
///   same size as `src1` (checked by [`Trans::apply`]).
/// * `region` — half-open processing rectangle `[x0, y0, x1, y1)` the
///   reference `Process` receives as `data->Left/Top/Width/Height`.
/// * `max_size` — `maxsize` option: the peak block size in pixels
///   (default 30), reached at the middle of the transition.
///
/// # Errors
///
/// [`TransError::InvalidOption`] for a `max_size` below one pixel (the
/// grid math divides by the current block size) or one whose peak block
/// size does not fit the reference's 32-bit `tjs_int` grid arithmetic.
pub(crate) fn apply(
    trans: &Trans,
    dst: &mut BitmapState,
    src1: &BitmapState,
    src2: &BitmapState,
    region: RectI,
    max_size: i32,
) -> Result<(), TransError> {
    let grid = MosaicGrid::new(trans, src1, max_size)?;

    // The rectangle to paint: `region` clipped to `dst` plays the role of
    // the reference `data->Left/Top/Width/Height` (its `destxofs`/`destyofs`
    // cancel in L2's single coordinate space). An empty rectangle means
    // nothing to paint, exactly as a `Process` call with an empty region.
    let Some(rect @ (left, top, right, bottom)) = intersect(dst, region) else {
        return Ok(());
    };

    // 1: the range of mosaic blocks that intersects the rectangle.
    let bs = grid.block;
    let startx = (left - grid.ofs_x) / bs;
    let starty = (top - grid.ofs_y) / bs;
    let endx = (right - 1 - grid.ofs_x) / bs;
    let endy = (bottom - 1 - grid.ofs_y) / bs;

    // 2: the border blocks are painted through the block ∩ rectangle
    //    intersection (`FILL_ONE`), 3: every interior block lies fully
    //    inside the rectangle, so it is filled without a clip check.
    //    Top row.
    let mut y = starty;
    for x in startx..=endx {
        grid.fill_edge(dst, src1, src2, rect, x, y);
    }
    y += 1;

    // Middle rows.
    while y < endy {
        grid.fill_edge(dst, src1, src2, rect, startx, y);
        for x in (startx + 1)..endx {
            grid.fill_block(dst, src1, src2, x, y);
        }
        grid.fill_edge(dst, src1, src2, rect, endx, y);
        y += 1;
    }

    // Bottom row: the middle loop stops at `y == endy`, so the reference's
    // `if(y <= endy)` paints exactly that row — and only for a single-row
    // rectangle (`starty == endy`) does it skip the extra pass.
    if y <= endy {
        for x in startx..=endx {
            grid.fill_edge(dst, src1, src2, rect, x, y);
        }
    }

    dst.mark_dirty();
    Ok(())
}

/// The per-frame mosaic parameters `StartProcess` computes in the
/// reference (`mosaic.cpp`), plus the two block-fill paths of `Process`.
#[derive(Clone, Copy)]
struct MosaicGrid {
    /// `CurBlockSize`: the current square block size in pixels, `>= 1`.
    block: i32,
    /// `CurOfsX`: the grid's x offset, always `<= 0`, chosen so that a
    /// block centre lands on the image centre.
    ofs_x: i32,
    /// `CurOfsY`: the grid's y offset, always `<= 0`.
    ofs_y: i32,
    /// `BlendRatio`: the frame's source 1 → source 2 blend level in
    /// `0..=255`.
    ratio: i32,
}

impl MosaicGrid {
    /// The reference `StartProcess` parameter computation, driven by the
    /// phase clock instead of `tick`:
    ///
    /// * `t` is the triangle wave of the elapsed time (`phase` below the
    ///   halfway point, `phase_max - phase` above it, i.e. `Time - t`),
    ///   which the reference clamps at `0` — here `0 <= phase <=
    ///   phase_max` is an invariant of `Trans`, so both of its clamps
    ///   (`CurTime > Time`, `t < 0`) cannot trigger.
    /// * `block = (max_size - 2) * t / HalfTime + 2`, computed in 64-bit
    ///   like the reference's `tjs_int64 t` and truncated towards zero.
    /// * `ratio = CurTime * 255 / Time`, `<= 255` because `phase <=
    ///   phase_max` (the reference's own `> 255` clamp never fires once
    ///   `CurTime` is capped).
    /// * the offsets centre a block on the image, matching
    ///   `CurOfsX = (Width - bs) / 2 - (Width / 2 / bs * bs)` followed by
    ///   `if(CurOfsX > 0) CurOfsX -= bs`.
    ///
    /// # Parameters
    ///
    /// * `trans` — the transition state, for `phase`/`phase_max`.
    /// * `src1` — first source; its dimensions define the grid.
    /// * `max_size` — `maxsize` option, the peak block size in pixels.
    ///
    /// # Errors
    ///
    /// [`TransError::InvalidOption`] (`option: "maxsize"`) when
    /// `max_size < 1` would make the reference divide by a non-positive
    /// `CurBlockSize`, or when the peak block size of the triangle wave
    /// overflows the reference's 32-bit `tjs_int` grid arithmetic.
    fn new(trans: &Trans, src1: &BitmapState, max_size: i32) -> Result<Self, TransError> {
        if max_size < 1 {
            // The reference reads `maxsize` unchecked and later evaluates
            // `(MaxBlockSize-2) * t / HalfTime + 2` and `x /= CurBlockSize`;
            // a value below 1 yields a block size of 0 or less (division by
            // zero / inverted grid), so there is no runnable configuration
            // to fall back to.
            return Err(TransError::InvalidOption {
                option: "maxsize",
                reason: "block size must be at least 1 pixel",
            });
        }

        let phase = trans.phase();
        let phase_max = trans.phase_max();
        // `HalfTime = Time / 2`; `mosaic` is never `universal`, so
        // `phase_max == 255` (`Trans::new`) and `half == 127 != 0`.
        let half = phase_max / 2;
        // The largest reachable `t` is `phase_max - half` (at `phase ==
        // half`); `block` grows monotonically with `t` for `max_size >= 2`
        // and stays in `1..=2` for `max_size == 1`, so this is the peak of
        // the whole transition.
        let peak = (i64::from(max_size) - 2) * i64::from(phase_max - half) / i64::from(half) + 2;
        if peak > i64::from(i32::MAX) {
            return Err(TransError::InvalidOption {
                option: "maxsize",
                reason: "peak block size exceeds the 32-bit pixel grid",
            });
        }

        // `t`: the reference triangle `if(t >= HalfTime) t = Time - t;`.
        let t = if phase >= half {
            phase_max - phase
        } else {
            phase
        };
        // `(MaxBlockSize-2) * t / HalfTime + 2` in `tjs_int64`, then narrowed
        // to `tjs_int` — in range by the `peak` check above and `>= 1`
        // because `t <= phase_max - half` and `max_size >= 1`.
        let block = ((i64::from(max_size) - 2) * i64::from(t) / i64::from(half) + 2) as i32;
        // `BlendRatio = CurTime * 255 / Time` (truncating).
        let ratio = (i64::from(phase) * 255 / i64::from(phase_max)) as i32;

        let width = src1.width.min(i32::MAX as u32) as i32;
        let height = src1.height.min(i32::MAX as u32) as i32;
        let x = width / 2 / block * block;
        let y = height / 2 / block * block;
        let mut ofs_x = (width - block) / 2 - x;
        let mut ofs_y = (height - block) / 2 - y;
        if ofs_x > 0 {
            ofs_x -= block;
        }
        if ofs_y > 0 {
            ofs_y -= block;
        }

        Ok(Self {
            block,
            ofs_x,
            ofs_y,
            ratio,
        })
    }

    /// The shared block sample: blend both sources at the block centre
    /// `(l + (bs >> 1), t + (bs >> 1))`, clamped to the image bounds like
    /// the reference's `mx`/`my`. The centre is taken from the
    /// *unclipped* block corner, so a partially visible border block keeps
    /// the colour of its true centre.
    ///
    /// # Parameters
    ///
    /// * `src1` / `src2` — the two sources, sampled at the same point.
    /// * `l` / `t` — the unclipped top-left corner of the block.
    fn sample(&self, src1: &BitmapState, src2: &BitmapState, l: i32, t: i32) -> [u8; 4] {
        let centre = self.block >> 1;
        let mut mx = l + centre;
        let mut my = t + centre;
        let width = src1.width.min(i32::MAX as u32) as i32;
        let height = src1.height.min(i32::MAX as u32) as i32;
        if mx < 0 {
            mx = 0;
        }
        if my < 0 {
            my = 0;
        }
        if mx >= width {
            mx = width - 1;
        }
        if my >= height {
            my = height - 1;
        }
        blend_level(
            read_pixel(src1, mx, my),
            read_pixel(src2, mx, my),
            self.ratio,
        )
    }

    /// `FILL_ONE`: fill the intersection of block `(x, y)` with `rect`
    /// (the reference `Clip(l, r, …) && Clip(t, b, …)` guard) with the
    /// block's centre sample. Used for the border rows/columns, where a
    /// block may stick out of the rectangle.
    ///
    /// # Parameters
    ///
    /// * `dst` — destination bitmap to paint into.
    /// * `src1` / `src2` — the two sources to sample.
    /// * `rect` — the clipped processing rectangle, in `dst` coordinates.
    /// * `x` / `y` — the block's grid indices.
    fn fill_edge(
        &self,
        dst: &mut BitmapState,
        src1: &BitmapState,
        src2: &BitmapState,
        rect: RectI,
        x: i32,
        y: i32,
    ) {
        let (left, top, right, bottom) = rect;
        let l = x * self.block + self.ofs_x;
        let t = y * self.block + self.ofs_y;
        let cl = l.max(left);
        let cr = (l + self.block).min(right);
        let ct = t.max(top);
        let cb = (t + self.block).min(bottom);
        if cl >= cr || ct >= cb {
            return;
        }
        let color = self.sample(src1, src2, l, t);
        for py in ct..cb {
            for px in cl..cr {
                write_pixel(dst, px, py, color);
            }
        }
    }

    /// The interior path of `Process` (`FILL_LINE(destp, bs, bs, d)`):
    /// fill the whole block `(x, y)` without a clip check — interior blocks
    /// lie fully inside the rectangle by construction of `startx`/`endx`
    /// and `starty`/`endy`, so no pixel can escape `dst`.
    ///
    /// # Parameters
    ///
    /// * `dst` — destination bitmap to paint into.
    /// * `src1` / `src2` — the two sources to sample.
    /// * `x` / `y` — the block's grid indices.
    fn fill_block(
        &self,
        dst: &mut BitmapState,
        src1: &BitmapState,
        src2: &BitmapState,
        x: i32,
        y: i32,
    ) {
        let l = x * self.block + self.ofs_x;
        let t = y * self.block + self.ofs_y;
        let color = self.sample(src1, src2, l, t);
        for py in t..t + self.block {
            for px in l..l + self.block {
                write_pixel(dst, px, py, color);
            }
        }
    }
}
