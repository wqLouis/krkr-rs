//! `ripple` — the radial ripple exchange transition of `extrans.dll`.
//!
//! A ripple expands from a centre, every destination pixel is resampled at
//! a displaced coordinate taken from a precomputed direction/drift map, and
//! the two sources are blended there at the current ratio.
//!
//! # The algorithm
//!
//! `tTVPRippleTable::MakeTable` precomputes two tables per parameter set:
//!
//! * **`DisplaceMap`** covers one mirrored quadrant (the caller folds the
//!   pixel's offset from the centre into `|x − cx|`/`|y − cy|` half-pairs, so
//!   one quadrant serves all four) and stores one `tjs_uint16` per pixel:
//!   `dist * TVP_RIPPLE_DIR_PREC + dir`, where `dist` is the distance from
//!   the centre masked to `RippleWidth − 1` and `dir = atan(xx / yy) /
//!   (π/2) * TVP_RIPPLE_DIR_PREC` encodes the direction within the quadrant
//!   (32 directions, `TVP_RIPPLE_DIR_PREC`).
//! * **`DriftMap`** stores the packed signed displacement `(dx, dy)` for each
//!   `(drift, wave-slot, direction)`: the [`wave_form`] profile (a
//!   squared, slightly skewed sine, always ≥ 0) scaled by a fixed-point
//!   drift amplitude and rotated by 1/1024
//!   `sin`/`cos` tables. Every wave slot is written twice, at `slot` and
//!   `slot + RippleWidth`, so a lookup of `dist + phase` needs no mask —
//!   that duplication is the `* 2` stride.
//!
//! Per frame (`StartProcess`) three scalars are derived from the elapsed
//! time: `BlendRatio = CurTime * 255 / Time` (integer division), the wave
//! window `Phase = RippleWidth − (speed · elapsed · RippleWidth / 2000π mod
//! RippleWidth) − 1` (reversed, so the rings travel outward with `speed`),
//! and the amplitude `Drift = sin(π · CurTime / Time) · maxdrift ·
//! TVP_RIPPLE_DRIFT_PREC`, clamped below the table — the ripple swells to
//! its peak mid-transition and settles at both ends.
//!
//! `Process` then walks the scanlines: each pixel reads its mirrored
//! quadrant entry, folds it through the drift slice at `(Drift, Phase)`, and
//! samples **both** sources at the displaced coordinate. Sign conventions
//! are fixed: right/below the centre sample at `x − dx` /
//! `y − dy`, left/above at `x + dx` / `y + dy` — since `cos`/`sin` of the
//! quadrant are positive, everything converges on the centre. Samples that
//! would leave the image are reflected once (`TVP_RIPPLE_TURN_BORDER`), so a
//! sample never leaves the image.
//!
//! A `DriftCarePixels` border band would be enough to cover the danger zone
//! (the band is sized for the `maxdrift` displacement), but it does not
//! cover the case where the centre sits within `DriftCarePixels` of an
//! edge: the "fast" path then computes `x + dx` past the right edge (or
//! `x - dx` below zero) and would read past the source. Reflecting
//! everywhere avoids that overrun.
//!
//! # Table memoisation
//!
//! The table is a pure, deterministic function of its seven constructor
//! arguments, so a single-entry memo lives in a [`OnceLock`] here: the first
//! frame builds it, later frames with the same parameters reuse it. L2 still
//! keeps no hidden *mutable* state — the memo is an immutable value behind
//! `OnceLock` and only ever depends on its key. When a differently
//! parameterised ripple arrives (four cached tables are kept elsewhere),
//! the table is rebuilt and used locally for that call; alternating between
//! such parameter sets therefore rebuilds each time, a deliberate trade of
//! a little CPU against keeping the memo init-once and race-free.
//!
//! # Deviations
//!
//! * **Alpha**: `TVP_RIPPLE_BLEND` computes the RGB lanes and then writes
//!   the destination alpha as `0` (the effect cannot process
//!   transparency). On this engine's straight-alpha
//!   bitmaps all four channels are blended through [`blend_level`],
//!   which is the same `s1 + ((s2 − s1) * ratio >> 8)` per-channel
//!   arithmetic with alpha carried along instead of dropped.
//! * **`maxdrift = 0`**: `Drift` is clamped to
//!   `MaxDrift * TVP_RIPPLE_DRIFT_PREC − 1 = −1` and then indexed *before*
//!   its (zero-length) drift table. The amplitude is kept at `0`
//!   (a still ripple, `dx = dy = 0`) and allocates one zeroed drift slice.
//! * **Map size**: `MapWidth = CenterX < (Width >> 1) ? Width − CenterX :
//!   CenterX` overruns by one element when the centre sits exactly on
//!   `width >> 1` of an odd-sized image — the default centre for odd sizes —
//!   because the "forward" half then measures `width − centre > centre`.
//!   This model uses `max(centre, size − centre)`, the largest quadrant the
//!   algorithm describes; the two agree everywhere else.
//! * **`M_PI`**: the plugin's own fallback
//!   `3.14159263589793238462` under `#ifndef M_PI` is a typo'd π,
//!   ≈ +5.4e-10 off the real one. This model uses
//!   `std::f64::consts::PI`, what `M_PI` resolves to on modern toolchains;
//!   the two constants can disagree only at exact rounding boundaries (a
//!   direction code off by one on rare pixels), never in the formulas.
//! * Types follow the script's (`double` → `f64`, `float` → `f32`,
//!   `tjs_int` → `i32`), including C's promotions at calls: `atan`, `sqrt`
//!   and `sin` run in `f64` on `f32` operands, and the fixed-point rounding
//!   casts keep their `± 0.5` truncation.

use std::f64::consts::PI;
use std::sync::OnceLock;

use super::{Trans, TransError, blend_level, read_pixel, write_pixel};
use crate::layer_ops::{RectI, intersect, intersect_rect};
use crate::scene::BitmapState;

/// `TVP_RIPPLE_DIR_PREC`: how many directions one quadrant is split into.
const DIR_PREC: usize = 32;

/// `TVP_RIPPLE_DRIFT_PREC`: drift-table entries per pixel of amplitude (the
/// sub-pixel drift resolution).
const DRIFT_PREC: i32 = 4;

/// The `ripple` options of `StartTransition`, resolved against the
/// source size in [`apply`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Params {
    /// Ripple centre; it defaults to the image centre and a centre outside
    /// the image is rejected.
    pub center: Option<(i32, i32)>,
    /// Ripple map size (`rwidth`, default 128; only 16, 32, 64 or 128 are
    /// accepted).
    pub map_width: i32,
    /// Wave roundness (`roundness`, default 1.0; must be positive).
    pub roundness: f32,
    /// Ripple speed (`speed`, default 6).
    pub speed: f32,
    /// Peak pixel drift (`maxdrift`, default 24; `0..=127` and smaller than
    /// both source dimensions).
    pub max_drift: i32,
}

/// Render one `ripple` frame of `trans` into `dst`, restricted to `region`.
///
/// The caller (`Trans::apply`) has already copied source 1 at `phase <= 0`
/// and source 2 at `phase >= phase_max`, so this only ever runs mid-flight —
/// exactly where `Process` runs, with `BlendRatio`,
/// `Phase` and `Drift` recomputed from the elapsed time here (the
/// equivalent of `StartProcess`).
///
/// # Errors
///
/// Returns [`TransError::InvalidOption`] for every option rejected in
/// `StartTransition`, never a silent clamp: a `rwidth`
/// outside `16 | 32 | 64 | 128`, a non-positive (or non-finite) `roundness`,
/// a `maxdrift` outside `0..=127` or not smaller than both source
/// dimensions, and a centre outside the source image.
pub(crate) fn apply(
    trans: &Trans,
    dst: &mut BitmapState,
    src1: &BitmapState,
    src2: &BitmapState,
    region: RectI,
    params: Params,
) -> Result<(), TransError> {
    let src_width = src1.width;
    let src_height = src1.height;
    let width = src_width as i32;
    let height = src_height as i32;

    // -- StartTransition validation ------------------------------------
    let (center_x, center_y) = params.center.unwrap_or((width >> 1, height >> 1));
    if center_x < 0 || center_x as u32 >= src_width {
        return Err(TransError::InvalidOption {
            option: "centerx",
            reason: "centerx must be inside the source image",
        });
    }
    if center_y < 0 || center_y as u32 >= src_height {
        return Err(TransError::InvalidOption {
            option: "centery",
            reason: "centery must be inside the source image",
        });
    }
    if !matches!(params.map_width, 16 | 32 | 64 | 128) {
        return Err(TransError::InvalidOption {
            option: "rwidth",
            reason: "rwidth must be 16, 32, 64 or 128",
        });
    }
    if params.roundness <= 0.0 || !params.roundness.is_finite() {
        return Err(TransError::InvalidOption {
            option: "roundness",
            reason: "roundness must be a positive finite number",
        });
    }
    if params.max_drift < 0 || params.max_drift >= 128 {
        return Err(TransError::InvalidOption {
            option: "maxdrift",
            reason: "maxdrift must be within 0..=127",
        });
    }
    if params.max_drift as u32 >= src_width || params.max_drift as u32 >= src_height {
        return Err(TransError::InvalidOption {
            option: "maxdrift",
            reason: "maxdrift must be smaller than both source dimensions",
        });
    }

    // -- the memoised tTVPRippleTable ----------------------------------
    let key = TableKey {
        width: src_width,
        height: src_height,
        center_x,
        center_y,
        ripple_width: params.map_width,
        roundness_bits: params.roundness.to_bits(),
        max_drift: params.max_drift,
    };
    let handle = table_for(key);
    let table = handle.get();

    // -- StartProcess scalars ------------------------------------------
    let cur_time = trans.elapsed_ms();
    let time = trans.duration_ms();

    // `BlendRatio = CurTime * 255 / Time`, integer division, capped at 255.
    let mut blend_ratio = ((cur_time * 255.0) / time) as i32;
    if blend_ratio > 255 {
        blend_ratio = 255;
    }

    // `Phase = (speed · elapsed · RippleWidth / 2000π) mod RippleWidth`,
    // floored toward zero by the C `(int)` cast, floored at 0, then reversed.
    let ripple_width = params.map_width;
    let phase_value = (f64::from(params.speed)
        * ((1.0 / (PI * 2.0)) * (1.0 / 1000.0))
        * cur_time
        * f64::from(ripple_width)) as i32;
    let mut phase_index = phase_value % ripple_width;
    if phase_index < 0 {
        phase_index = 0;
    }
    let ripple_phase = (ripple_width - phase_index - 1) as usize;

    // `Drift = (int)(sin(π · CurTime / Time) · MaxDrift · DRIFT_PREC)`
    // (the multiplication is in `float`), clamped into the table. The
    // `MaxDrift == 0` clamp would yield `-1`; see the module
    // docs — a still ripple is kept at `0`.
    let swell = ((PI * cur_time / time).sin()) as f32;
    let drift_value = (swell * params.max_drift as f32 * DRIFT_PREC as f32) as i32;
    let drift_limit = params.max_drift * DRIFT_PREC;
    let drift = if drift_value < 0 {
        0
    } else if drift_value >= drift_limit {
        (drift_limit - 1).max(0)
    } else {
        drift_value
    };

    // -- scanlines -----------------------------------------------------
    // Nothing is processed outside the shared source rectangle;
    // clipping here keeps the quadrant indices inside the mirrored map even
    // when the destination bitmap is larger than the sources.
    let image = (0, 0, width, height);
    let Some((x0, y0, x1, y1)) =
        intersect(dst, region).and_then(|rect| intersect_rect(rect, image))
    else {
        return Ok(());
    };
    for y in y0..y1 {
        let map_y = if y < center_y {
            (center_y - y - 1) as usize
        } else {
            (y - center_y) as usize
        };
        for x in x0..x1 {
            let map_x = if x < center_x {
                (center_x - x - 1) as usize
            } else {
                (x - center_x) as usize
            };
            // GetDisplaceMap(x, y)[…] through GetDriftMap(Drift, Phase).
            let code = table.displace_at(map_x, map_y);
            let packed = table.drift_at(drift as usize, ripple_phase, code);
            let dx = i32::from((packed >> 8) as u8 as i8);
            let dy = i32::from((packed & 0xff) as u8 as i8);
            let mut sx = if x < center_x { x + dx } else { x - dx };
            let mut sy = if y < center_y { y + dy } else { y - dy };
            turn_border(&mut sx, &mut sy, width, height);
            let before = read_pixel(src1, sx, sy);
            let after = read_pixel(src2, sx, sy);
            write_pixel(dst, x, y, blend_level(before, after, blend_ratio));
        }
    }
    dst.mark_dirty();
    Ok(())
}

/// `TVP_RIPPLE_TURN_BORDER`: reflect a sample coordinate once at each edge,
/// folding it back into the image (the `_e` transforms).
fn turn_border(x: &mut i32, y: &mut i32, width: i32, height: i32) {
    if *x < 0 {
        *x = -*x;
    }
    if *y < 0 {
        *y = -*y;
    }
    if *x >= width {
        *x = width - 1 - (*x - width);
    }
    if *y >= height {
        *y = height - 1 - (*y - height);
    }
}

/// The identity of one ripple table: every constructor argument, with
/// `roundness` kept as its bit pattern so the key stays `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TableKey {
    /// Transition image width (table width).
    width: u32,
    /// Transition image height (table height).
    height: u32,
    /// Ripple centre X (`CenterX`).
    center_x: i32,
    /// Ripple centre Y (`CenterY`).
    center_y: i32,
    /// Wave length in pixels (`RippleWidth`).
    ripple_width: i32,
    /// `Roundness` as `f32::to_bits()`.
    roundness_bits: u32,
    /// Peak drift in pixels (`MaxDrift`).
    max_drift: i32,
}

/// The precomputed `tTVPRippleTable`: a mirrored displacement quadrant plus
/// the packed `(dx, dy)` drift slices.
#[derive(Debug, Clone)]
struct RippleTable {
    /// `MapWidth` — column stride of the mirrored quadrant.
    map_width: usize,
    /// `DisplaceMap`, row-major over the quadrant: `dist * DIR_PREC + dir`
    /// per pixel.
    displace: Vec<u16>,
    /// `DriftMap`, laid out as
    /// `drift_index * drift_stride + slot * DIR_PREC + dir`, each entry
    /// packing `dx` in the high byte and `dy` in the low byte (signed).
    drift: Vec<u16>,
    /// Entries in one drift slice (`RippleWidth * 2 * TVP_RIPPLE_DIR_PREC`;
    /// the `* 2` is the duplicated wave window).
    drift_stride: usize,
}

impl RippleTable {
    /// `GetDisplaceMap(x, y)` → the direction/distance code of one quadrant
    /// pixel; `x`/`y` are already folded toward the centre.
    fn displace_at(&self, x: usize, y: usize) -> u16 {
        self.displace[x + y * self.map_width]
    }

    /// `GetDriftMap(Drift, Phase)[code]` → the packed displacement for a
    /// direction code at the current drift amplitude and wave window.
    fn drift_at(&self, drift_index: usize, phase: usize, code: u16) -> u16 {
        self.drift[drift_index * self.drift_stride + phase * DIR_PREC + usize::from(code)]
    }
}

/// The single-entry memoised table (see the module docs).
static TABLE_CACHE: OnceLock<(TableKey, RippleTable)> = OnceLock::new();

/// A table handle handed out by [`table_for`].
enum TableRef<'a> {
    /// The memoised table; lives as long as the process.
    Shared(&'a RippleTable),
    /// A table rebuilt because the memo holds a different parameter set (or
    /// lost the initialisation race to one).
    Local(RippleTable),
}

impl TableRef<'_> {
    /// The table to render with.
    fn get(&self) -> &RippleTable {
        match self {
            TableRef::Shared(table) => table,
            TableRef::Local(table) => table,
        }
    }
}

/// The memoised `tTVPRippleTable` for `key`, built on first use.
fn table_for(key: TableKey) -> TableRef<'static> {
    if let Some((cached, table)) = TABLE_CACHE.get()
        && *cached == key
    {
        return TableRef::Shared(table);
    }
    match TABLE_CACHE.set((key, build_table(key))) {
        Ok(()) => TableRef::Shared(&TABLE_CACHE.get().expect("table cache initialised above").1),
        Err((_, table)) => {
            // Someone initialised the cache first; share their table only if
            // it answers this key, otherwise keep ours for this call.
            if let Some((cached, table)) = TABLE_CACHE.get()
                && *cached == key
            {
                return TableRef::Shared(table);
            }
            TableRef::Local(table)
        }
    }
}

/// The displacement quadrant, the wave profile, the 1/1024 `sin`/`cos`
/// tables and every drift slice, in the script's types and operation order.
fn build_table(key: TableKey) -> RippleTable {
    let width = key.width as i32;
    let height = key.height as i32;
    let center_x = key.center_x;
    let center_y = key.center_y;
    let roundness = f32::from_bits(key.roundness_bits);
    let max_drift = key.max_drift;
    let ripple_width = key.ripple_width as usize;

    // `MapWidth`/`MapHeight`: the larger half the centre splits the image
    // into (see the module docs for the off-by-one).
    let map_width = center_x.max(width - center_x) as usize;
    let map_height = center_y.max(height - center_y) as usize;

    // `DisplaceMap`: one mirrored quadrant, `dist * DIR_PREC + dir` per
    // pixel; `dist` wraps by the ripple width via its power-of-two mask.
    let ripple_mask = key.ripple_width - 1;
    let mut displace = Vec::with_capacity(map_width * map_height);
    for my in 0..map_height {
        let yy = (my as f32 + 0.5) * roundness;
        let fac = (1.0 / f64::from(yy)) as f32;
        for mx in 0..map_width {
            let xx = mx as f32 + 0.5;
            let dir = (f64::from(xx * fac).atan() * ((1.0 / (PI / 2.0)) * DIR_PREC as f64)) as i32;
            let dist = (f64::from(xx * xx + yy * yy).sqrt() as i32) & ripple_mask;
            displace.push((dist * DIR_PREC as i32 + dir) as u16);
        }
    }

    // `DriftMap`: `maxdrift * DRIFT_PREC` slices of `RippleWidth * 2 *
    // DIR_PREC` entries, always at least one (zeroed) slice so `maxdrift =
    // 0` has something to index.
    let drift_slices = (max_drift * DRIFT_PREC).max(1) as usize;
    let drift_stride = ripple_width * 2 * DIR_PREC;
    let mut drift = vec![0u16; drift_slices * drift_stride];

    // The wave profile, one entry per ripple-width pixel.
    let rcp_rw = (1.0 / (ripple_width as f64)) as f32;
    let mut ripple_form = Vec::with_capacity(ripple_width);
    for w in 0..ripple_width {
        let rad = (f64::from((w as f32) * rcp_rw) * (PI * -2.0)) as f32;
        let mut s = wave_form(rad);
        s = s.clamp(-1.0, 1.0);
        s = (f64::from(s) * 2048.0) as f32;
        ripple_form.push(round_fixed(s));
    }

    // 1/1024-precision `cos`/`sin` of one quadrant's directions.
    let mut cos_table = [0i32; DIR_PREC];
    let mut sin_table = [0i32; DIR_PREC];
    for w in 0..DIR_PREC {
        let fdir = (PI * 0.5
            - f64::from((w as f32) + 0.5) * ((1.0 / (DIR_PREC as f64)) * (PI / 2.0)))
            as f32;
        cos_table[w] = round_fixed((f64::from(fdir).cos() * 2048.0) as f32);
        sin_table[w] = round_fixed((f64::from(fdir).sin() * 2048.0) as f32);
    }

    // Every drift slice, in fixed point (computing these in float is
    // terribly slow).
    for drift_index in 0..(max_drift * DRIFT_PREC) as usize {
        let fdrift = ((drift_index as i32) << 10) / DRIFT_PREC; // 8.10
        let base = drift_index * drift_stride;
        for (w, form) in ripple_form.iter().enumerate() {
            let fd = (form * fdrift) >> 10; // 8.11
            let slot = base + w * DIR_PREC;
            for dir in 0..DIR_PREC {
                let xd = (cos_table[dir] * fd) >> 11; // 8.11
                let yd = (sin_table[dir] * fd) >> 11; // 8.11
                // Packed as `(char)(xd >> 11) << 8 | (char)(yd >> 11)`.
                let packed = ((((xd >> 11) as i8) as i32) << 8) + (((yd >> 11) as i8) as i32);
                let packed = packed as u16;
                drift[slot + dir] = packed;
                drift[slot + ripple_width * DIR_PREC + dir] = packed;
            }
        }
    }

    RippleTable {
        map_width,
        displace,
        drift,
        drift_stride,
    }
}

/// The wave profile — a sine nudged by a
/// detuned second harmonic, normalised to ≈ 1, then squared (`s *= s`), so
/// the result is never negative.
fn wave_form(rad: f32) -> f32 {
    let inner = (f64::from(rad).sin() + f64::from(rad * 2.0 - 2.0).sin() * 0.2) / 1.19;
    let s = inner as f32;
    s * s
}

/// `(tjs_int32)(v < 0 ? v - 0.5 : v + 0.5)` — the rounding cast:
/// truncate a `± 0.5` shift, i.e. round half away from zero.
fn round_fixed(v: f32) -> i32 {
    if v < 0.0 {
        (f64::from(v) - 0.5) as i32
    } else {
        (f64::from(v) + 0.5) as i32
    }
}
