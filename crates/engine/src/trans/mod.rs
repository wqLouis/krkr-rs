//! L2 transition state machine.
//!
//! Models the TVP `Trans` machine (universal/uni, slide, cross-fade, mask,
//! …) as explicit L2 state over the [`crate::layer_ops`] primitives. L3
//! exposes the script `Trans` object.
//!
//! Behavioral reference: the reference transition handlers in
//! `reference/cpp/core/visual/TransIntf.cpp` (`tTVPCrossFadeTransHandler`,
//! `tTVPUniversalTransHandler`, `tTVPScrollTransHandler`) and their providers
//! (`TVPRegisterDefaultTransHandlerProvider`). `main`'s
//! `crates/tvp-natives/src/extrans.rs` only classifies the names; the
//! interpolation lives here.
//!
//! # The state machine
//!
//! A [`Trans`] is a caller-owned value: it stores the transition kind, the
//! duration and the elapsed time, and it composites two source bitmaps into a
//! destination at the current progress. There is no global handler registry,
//! no plugin lookup and no clock. The application calls
//! [`Trans::step`] with the elapsed milliseconds each frame and
//! [`Trans::apply`] once per frame to render.
//!
//! The reference advances a `phase` from `0` to `PhaseMax`; this model keeps
//! both the normalized [`Trans::progress`] (`0.0..=1.0`) and the integer
//! [`Trans::phase`] so the ports of the reference blend tables stay exact.
//!
//! # Kinds
//!
//! | kind | reference provider | options |
//! |---|---|---|
//! | [`TransKind::CrossFade`] | `crossfade` | linear blend src1 → src2 |
//! | [`TransKind::Universal`] | `universal` | `vague`, rule image |
//! | [`TransKind::Mask`] | (`universal` with `vague=0`) | rule image |
//! | [`TransKind::Scroll`] | `scroll` | `from`, `stay` |
//!
//! All of them are `ttExchange` (two sources). A rule image is a
//! [`RuleMask`] tiled over the source, exactly like
//! `iTVPSimpleImageProvider::LoadImage` tiles a smaller rule graphic.
//!
//! # Errors
//!
//! A non-positive/NaN duration, a negative `vague`, a missing or unexpected
//! rule, an inconsistent rule and mismatched source sizes are typed
//! [`TransError`]s; nothing is silently clamped to a fallback.

use crate::layer_ops::{RectI, blit_copy, blit_copy_clipped, intersect, intersect_rect};
use crate::scene::BitmapState;

mod extrans_mosaic;
mod extrans_ripple;
mod extrans_rotate;
mod extrans_turn;
mod extrans_wave;

/// Maximum phase for the cross-fade/universal family (`PhaseMax = 255` in
/// `tTVPCrossFadeTransHandler`).
pub const PHASE_MAX: i32 = 255;

/// Truncated π literal used by the `wave` and `turn` transition tables.
///
/// `f64::consts::PI` differs in the last digits; the truncated value keeps
/// the sine arguments and table entries at their exact values.
pub(crate) const PI_TRUNCATED: f64 = f64::from_bits(0x4009_21FB_5444_2D11);

/// Direction a scroll/slide transition moves from (`tTVPScrollTransFrom`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScrollFrom {
    /// `sttLeft` — the new image slides in from the left.
    Left,
    /// `sttTop` — the new image slides in from the top.
    Top,
    /// `sttRight` — the new image slides in from the right.
    Right,
    /// `sttBottom` — the new image slides in from the bottom.
    Bottom,
}

/// Which layer "stays" during a scroll (`tTVPScrollTransStay`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScrollStay {
    /// `ststNoStay` — both layers move (a push).
    NoStay,
    /// `ststStayDest` — the destination (source 2) is stationary.
    StayDest,
    /// `ststStaySrc` — the source (source 1) is stationary.
    StaySrc,
}

/// The transition algorithm and its parameters.
///
/// The `crossfade`/`universal`/`mask`/`scroll` kinds are the engine's own
/// providers (`TVPRegisterDefaultTransHandlerProvider`); the remaining seven
/// are the `extrans.dll` providers (`ttExchange`/`tutDivisible`), ported from
/// the plugin sources in `sample/extrans` (`wave.cpp`, `mosaic.cpp`,
/// `turn.cpp`, `rotatetrans.cpp`, `ripple.cpp`).
///
/// Their real-valued options are kept as the reference reads them (`double`),
/// so this enum is `PartialEq` but not `Eq`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum TransKind {
    /// `crossfade`: a linear per-pixel blend from source 1 to source 2.
    CrossFade,
    /// `universal`: a rule-image blend whose soft edge is `vague` pixels
    /// wide (the reference's `vague` option, default 64).
    Universal {
        /// Soft-transition width in phase units.
        vague: i32,
    },
    /// A hard-edged rule-image transition (the reference `universal` with
    /// `vague = 0`): the rule's grayscale value selects the source per pixel.
    Mask,
    /// `scroll`: translate both sources across the destination.
    Scroll {
        /// Direction the new image enters from.
        from: ScrollFrom,
        /// Which source stays stationary.
        stay: ScrollStay,
    },
    /// `wave`: a travelling sine-wave displacement between the sources, over
    /// a two-colour background (`wave.cpp`).
    Wave {
        /// Peak displacement in pixels (`maxh`, default 50).
        max_height: i32,
        /// Angular frequency (`maxomega`, default 0.2).
        max_omega: f64,
        /// Colour behind the first source (`bgcolor1`, ARGB).
        background1: i64,
        /// Colour behind the second source (`bgcolor2`, ARGB).
        background2: i64,
        /// Wave shape selector (`wavetype`, default 0).
        wave_type: i32,
    },
    /// `mosaic`: a blocky dissolve into the second source (`mosaic.cpp`).
    Mosaic {
        /// Peak block size in pixels (`maxsize`, default 30).
        max_size: i32,
    },
    /// `turn`: a diamond warp between the sources over a flat background
    /// (`turn.cpp`).
    Turn {
        /// Colour revealed by the warp (`bgcolor`, ARGB).
        background: i64,
    },
    /// `rotatezoom`: rotate and zoom out with an optional twist
    /// (`rotatetrans.cpp`).
    RotateZoom {
        /// Initial zoom factor (`factor`, default 1).
        factor: f64,
        /// Zoom acceleration (`accel`, default 0).
        accel: f64,
        /// Twist in turns (`twist`, default 2).
        twist: f64,
        /// Twist acceleration in turns (`twistaccel`, default -2).
        twist_accel: f64,
        /// Rotation centre; the reference defaults it to the image centre.
        center: Option<(i32, i32)>,
    },
    /// `rotatevanish`: rotate with an accelerating twist while shrinking to a
    /// point (`rotatetrans.cpp`).
    RotateVanish {
        /// Shrink acceleration (`accel`, default 2).
        accel: f64,
        /// Twist in turns (`twist`, default 2).
        twist: f64,
        /// Twist acceleration in turns (`twistaccel`, default 2).
        twist_accel: f64,
        /// Rotation centre; the reference defaults it to the image centre.
        center: Option<(i32, i32)>,
    },
    /// `rotateswap`: a rotating, shrinking swap to the second source over a
    /// flat background (`rotatetrans.cpp`).
    RotateSwap {
        /// Colour revealed by the rotation (`bgcolor`, ARGB).
        background: i64,
        /// Twist in turns (`twist`, default 1).
        twist: f64,
        /// Rotation centre; the reference defaults it to the image centre.
        center: Option<(i32, i32)>,
    },
    /// `ripple`: a radial ripple expanding from a centre (`ripple.cpp`).
    Ripple {
        /// Ripple centre; the reference defaults it to the image centre and
        /// rejects a centre outside the image.
        center: Option<(i32, i32)>,
        /// Ripple map size (`rwidth`, default 128; the reference accepts only
        /// 16, 32, 64 or 128).
        map_width: i32,
        /// Wave roundness (`roundness`, default 1.0; must be positive).
        roundness: f32,
        /// Ripple speed (`speed`, default 6).
        speed: f32,
        /// Peak pixel drift (`maxdrift`, default 24; `0..=127` and smaller
        /// than both source dimensions).
        max_drift: i32,
    },
}

/// A grayscale rule image for [`TransKind::Universal`] / [`TransKind::Mask`].
///
/// The reference's image provider decodes the rule to 8bpp and tiles it when
/// it is smaller than the source; [`RuleMask::at`] does the same.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RuleMask {
    width: u32,
    height: u32,
    gray: Vec<u8>,
}

impl RuleMask {
    /// Build a rule from an explicit `width * height` grayscale buffer.
    ///
    /// # Errors
    ///
    /// Returns [`TransError::InconsistentRule`] for a zero dimension or a
    /// buffer whose length disagrees with `width * height`.
    pub fn new(width: u32, height: u32, gray: Vec<u8>) -> Result<Self, TransError> {
        let expected = u64::from(width) * u64::from(height);
        if width == 0 || height == 0 || expected != gray.len() as u64 {
            return Err(TransError::InconsistentRule {
                width,
                height,
                expected,
                len: gray.len(),
            });
        }
        Ok(Self {
            width,
            height,
            gray,
        })
    }

    /// Build a rule from a tightly packed RGBA8 image, converting each pixel
    /// to its Rec. 601 luma (`(77*r + 150*g + 29*b) >> 8`).
    ///
    /// # Errors
    ///
    /// Returns [`TransError::InconsistentRule`] for a zero dimension or a
    /// buffer whose length is not `width * height * 4`.
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Self, TransError> {
        let expected = u64::from(width) * u64::from(height) * 4;
        if width == 0 || height == 0 || expected != rgba.len() as u64 {
            return Err(TransError::InconsistentRule {
                width,
                height,
                expected,
                len: rgba.len(),
            });
        }
        let gray = rgba
            .chunks_exact(4)
            .map(|px| {
                ((u32::from(px[0]) * 77 + u32::from(px[1]) * 150 + u32::from(px[2]) * 29) >> 8)
                    as u8
            })
            .collect();
        Ok(Self {
            width,
            height,
            gray,
        })
    }

    /// The rule width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The rule height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The raw grayscale buffer, row-major.
    pub fn gray(&self) -> &[u8] {
        &self.gray
    }

    /// The grayscale value at `(x, y)`, tiling the rule when it is smaller
    /// than the source (reference `LoadImage` tiling).
    pub fn at(&self, x: i32, y: i32) -> u8 {
        let w = self.width as i32;
        let h = self.height as i32;
        let tx = x.rem_euclid(w) as usize;
        let ty = y.rem_euclid(h) as usize;
        self.gray[ty * self.width as usize + tx]
    }
}

/// Errors produced by the transition state machine.
#[derive(Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TransError {
    /// A transition duration was not finite and strictly positive.
    #[error("invalid transition duration {duration_ms} ms")]
    InvalidDuration {
        /// The rejected duration.
        duration_ms: f64,
    },
    /// A `universal` soft-edge width was negative.
    #[error("invalid universal vague width {vague}")]
    InvalidVague {
        /// The rejected width.
        vague: i32,
    },
    /// A rule-based transition was built without a rule image.
    #[error("this transition requires a rule image")]
    MissingRule,
    /// A rule image was supplied to a transition that does not use one.
    #[error("this transition does not take a rule image")]
    UnexpectedRule,
    /// A rule image's buffer length disagreed with its dimensions.
    #[error("inconsistent rule {width}x{height}: expected {expected} bytes, got {len}")]
    InconsistentRule {
        /// Rule width in pixels.
        width: u32,
        /// Rule height in pixels.
        height: u32,
        /// Expected byte count (`width * height`, or `* 4` for RGBA input).
        expected: u64,
        /// Actual buffer length.
        len: usize,
    },
    /// The two transition sources had different dimensions.
    #[error("transition source size mismatch: {src1:?} vs {src2:?}")]
    SizeMismatch {
        /// Source 1 `(width, height)`.
        src1: (u32, u32),
        /// Source 2 `(width, height)`.
        src2: (u32, u32),
    },
    /// An `extrans` option is outside the range the reference accepts (its
    /// `TVPThrowExceptionMessage` in `StartTransition`).
    #[error("invalid transition option {option}: {reason}")]
    InvalidOption {
        /// The option name as the script spells it.
        option: &'static str,
        /// Why it was rejected.
        reason: &'static str,
    },
}

/// The result of one [`Trans::step`].
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct TransStep {
    /// Normalized progress in `0.0..=1.0`.
    pub progress: f32,
    /// Integer phase in `0..=phase_max`.
    pub phase: i32,
    /// Whether the duration has elapsed.
    pub completed: bool,
}

/// A caller-owned transition state machine.
///
/// See the [module docs](self) for the lifecycle and kinds.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Trans {
    kind: TransKind,
    duration_ms: f64,
    elapsed_ms: f64,
    phase_max: i32,
    phase: i32,
    rule: Option<RuleMask>,
    completed: bool,
}

impl Trans {
    /// Build a rule-less transition (`crossfade`/`scroll`).
    ///
    /// # Errors
    ///
    /// Returns [`TransError::InvalidDuration`] for a non-positive/NaN
    /// duration, [`TransError::InvalidVague`] for a negative `vague`, or
    /// [`TransError::MissingRule`] when `kind` needs a rule image.
    pub fn new(kind: TransKind, duration_ms: f64) -> Result<Self, TransError> {
        Self::with_rule(kind, duration_ms, None)
    }

    /// Build a transition with an optional rule image.
    ///
    /// # Errors
    ///
    /// As [`Trans::new`], plus [`TransError::UnexpectedRule`] when a rule is
    /// supplied for a kind that does not use one.
    pub fn with_rule(
        kind: TransKind,
        duration_ms: f64,
        rule: Option<RuleMask>,
    ) -> Result<Self, TransError> {
        if !duration_ms.is_finite() || duration_ms <= 0.0 {
            return Err(TransError::InvalidDuration { duration_ms });
        }
        let needs_rule = matches!(kind, TransKind::Universal { .. } | TransKind::Mask);
        if needs_rule && rule.is_none() {
            return Err(TransError::MissingRule);
        }
        if !needs_rule && rule.is_some() {
            return Err(TransError::UnexpectedRule);
        }
        validate_options(kind)?;
        let phase_max = match kind {
            TransKind::Universal { vague } => {
                if vague < 0 {
                    return Err(TransError::InvalidVague { vague });
                }
                PHASE_MAX + vague
            }
            _ => PHASE_MAX,
        };
        Ok(Self {
            kind,
            duration_ms,
            elapsed_ms: 0.0,
            phase_max,
            phase: 0,
            rule,
            completed: false,
        })
    }

    /// A `crossfade` transition.
    ///
    /// # Errors
    ///
    /// Returns [`TransError::InvalidDuration`] for a non-positive/NaN
    /// duration.
    pub fn cross_fade(duration_ms: f64) -> Result<Self, TransError> {
        Self::new(TransKind::CrossFade, duration_ms)
    }

    /// A `universal` transition with a soft rule edge.
    ///
    /// # Errors
    ///
    /// Returns [`TransError::InvalidDuration`] or
    /// [`TransError::InvalidVague`].
    pub fn universal(duration_ms: f64, vague: i32, rule: RuleMask) -> Result<Self, TransError> {
        Self::with_rule(TransKind::Universal { vague }, duration_ms, Some(rule))
    }

    /// A hard-edged `mask` transition.
    ///
    /// # Errors
    ///
    /// Returns [`TransError::InvalidDuration`].
    pub fn mask(duration_ms: f64, rule: RuleMask) -> Result<Self, TransError> {
        Self::with_rule(TransKind::Mask, duration_ms, Some(rule))
    }

    /// A `scroll` transition.
    ///
    /// # Errors
    ///
    /// Returns [`TransError::InvalidDuration`].
    pub fn scroll(
        duration_ms: f64,
        from: ScrollFrom,
        stay: ScrollStay,
    ) -> Result<Self, TransError> {
        Self::new(TransKind::Scroll { from, stay }, duration_ms)
    }

    // -- state -------------------------------------------------------------

    /// The transition kind.
    pub fn kind(&self) -> TransKind {
        self.kind
    }

    /// The configured duration in milliseconds.
    pub fn duration_ms(&self) -> f64 {
        self.duration_ms
    }

    /// Elapsed milliseconds since the start (capped at the duration).
    pub fn elapsed_ms(&self) -> f64 {
        self.elapsed_ms
    }

    /// Normalized progress in `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        (self.elapsed_ms / self.duration_ms).clamp(0.0, 1.0) as f32
    }

    /// The integer phase in `0..=phase_max`.
    pub fn phase(&self) -> i32 {
        self.phase
    }

    /// The phase at completion (`255` for cross-fade/mask/scroll, `255 + vague`
    /// for universal).
    pub fn phase_max(&self) -> i32 {
        self.phase_max
    }

    /// Whether the duration has elapsed.
    pub fn is_complete(&self) -> bool {
        self.completed
    }

    /// The rule image, for rule-based transitions.
    pub fn rule(&self) -> Option<&RuleMask> {
        self.rule.as_ref()
    }

    // -- lifecycle ---------------------------------------------------------

    /// Advance by `dt_ms` and return the new [`TransStep`].
    ///
    /// A non-finite or non-positive `dt_ms` leaves the state unchanged. Once
    /// complete the state stays pinned at `progress = 1.0` and
    /// `phase = phase_max`.
    pub fn step(&mut self, dt_ms: f64) -> TransStep {
        if self.completed {
            return self.view();
        }
        if dt_ms.is_finite() && dt_ms > 0.0 {
            self.elapsed_ms += dt_ms;
        }
        self.settle();
        self.view()
    }

    /// Set the elapsed time from an external tick source and return the new
    /// [`TransStep`].
    ///
    /// Unlike [`Trans::step`], which adds a frame delta, this replaces the
    /// elapsed time with `elapsed_ms`, so a caller-supplied tick can run the
    /// transition forwards or backwards. A non-finite or negative `elapsed_ms`
    /// leaves the state unchanged; a completed transition stays complete.
    pub fn seek_ms(&mut self, elapsed_ms: f64) -> TransStep {
        if self.completed {
            return self.view();
        }
        if elapsed_ms.is_finite() && elapsed_ms >= 0.0 {
            self.elapsed_ms = elapsed_ms;
            self.settle();
        }
        self.view()
    }

    fn settle(&mut self) {
        if self.elapsed_ms >= self.duration_ms {
            self.elapsed_ms = self.duration_ms;
            self.phase = self.phase_max;
            self.completed = true;
        } else {
            // The phase is `elapsed * PhaseMax / duration` with integer
            // truncation, not rounding.
            let phase = self.elapsed_ms * f64::from(self.phase_max) / self.duration_ms;
            self.phase = phase.trunc() as i32;
            self.phase = self.phase.clamp(0, self.phase_max);
        }
    }

    /// Force the transition to completion.
    pub fn complete(&mut self) {
        self.elapsed_ms = self.duration_ms;
        self.phase = self.phase_max;
        self.completed = true;
    }

    /// Return to the start, keeping the kind, duration and rule.
    pub fn reset(&mut self) {
        self.elapsed_ms = 0.0;
        self.phase = 0;
        self.completed = false;
    }

    fn view(&self) -> TransStep {
        TransStep {
            progress: self.progress(),
            phase: self.phase,
            completed: self.completed,
        }
    }

    // -- rendering ---------------------------------------------------------

    /// Composite `src1`/`src2` into `dst` at the current phase, restricted to
    /// `region` (half-open `[x0, y0, x1, y1)`, the reference processing
    /// rectangle).
    ///
    /// Endpoints are exact: at `phase == 0` the destination is a copy of
    /// `src1`, at `phase == phase_max` a copy of `src2`.
    ///
    /// # Errors
    ///
    /// Returns [`TransError::SizeMismatch`] when the sources differ in size.
    pub fn apply(
        &self,
        dst: &mut BitmapState,
        src1: &BitmapState,
        src2: &BitmapState,
        region: RectI,
    ) -> Result<(), TransError> {
        if (src1.width, src1.height) != (src2.width, src2.height) {
            return Err(TransError::SizeMismatch {
                src1: (src1.width, src1.height),
                src2: (src2.width, src2.height),
            });
        }
        if dst.width == 0 || dst.height == 0 {
            return Ok(());
        }
        // Every `ttExchange` provider is `tutDivisible`/`tutDivisibleFade`:
        // the reference short-circuits the two endpoints, so the result at
        // `phase == 0` is source 1 verbatim and at `phase == phase_max`
        // source 2 verbatim. `scroll` is positional and does its own
        // clipping, so it stays outside this.
        if !matches!(self.kind, TransKind::Scroll { .. }) {
            if self.phase <= 0 {
                blit_copy(dst, region.0, region.1, src1, region);
                return Ok(());
            }
            if self.phase >= self.phase_max {
                blit_copy(dst, region.0, region.1, src2, region);
                return Ok(());
            }
        }
        match self.kind {
            TransKind::CrossFade => self.apply_cross_fade(dst, src1, src2, region),
            TransKind::Universal { vague } => {
                self.apply_rule_blend(dst, src1, src2, region, vague)?
            }
            TransKind::Mask => self.apply_rule_blend(dst, src1, src2, region, 0)?,
            TransKind::Scroll { from, stay } => {
                self.apply_scroll(dst, src1, src2, region, from, stay)
            }
            TransKind::Wave {
                max_height,
                max_omega,
                background1,
                background2,
                wave_type,
            } => extrans_wave::apply(
                self,
                dst,
                src1,
                src2,
                region,
                extrans_wave::Params {
                    max_height,
                    max_omega,
                    background1,
                    background2,
                    wave_type,
                },
            )?,
            TransKind::Mosaic { max_size } => {
                extrans_mosaic::apply(self, dst, src1, src2, region, max_size)?
            }
            TransKind::Turn { background } => {
                extrans_turn::apply(self, dst, src1, src2, region, background)?
            }
            TransKind::RotateZoom {
                factor,
                accel,
                twist,
                twist_accel,
                center,
            } => extrans_rotate::apply_zoom(
                self,
                dst,
                src1,
                src2,
                region,
                extrans_rotate::ZoomParams {
                    factor,
                    accel,
                    twist,
                    twist_accel,
                    center,
                },
            )?,
            TransKind::RotateVanish {
                accel,
                twist,
                twist_accel,
                center,
            } => extrans_rotate::apply_vanish(
                self,
                dst,
                src1,
                src2,
                region,
                extrans_rotate::VanishParams {
                    accel,
                    twist,
                    twist_accel,
                    center,
                },
            )?,
            TransKind::RotateSwap {
                background,
                twist,
                center,
            } => extrans_rotate::apply_swap(
                self,
                dst,
                src1,
                src2,
                region,
                extrans_rotate::SwapParams {
                    background,
                    twist,
                    center,
                },
            )?,
            TransKind::Ripple {
                center,
                map_width,
                roundness,
                speed,
                max_drift,
            } => extrans_ripple::apply(
                self,
                dst,
                src1,
                src2,
                region,
                extrans_ripple::Params {
                    center,
                    map_width,
                    roundness,
                    speed,
                    max_drift,
                },
            )?,
        }
        Ok(())
    }

    /// A full-image processing region for `bitmap`.
    pub fn full_region(bitmap: &BitmapState) -> RectI {
        (
            0,
            0,
            bitmap.width.min(i32::MAX as u32) as i32,
            bitmap.height.min(i32::MAX as u32) as i32,
        )
    }

    fn apply_cross_fade(
        &self,
        dst: &mut BitmapState,
        src1: &BitmapState,
        src2: &BitmapState,
        region: RectI,
    ) {
        if self.phase <= 0 {
            blit_copy(dst, region.0, region.1, src1, region);
            return;
        }
        if self.phase >= self.phase_max {
            blit_copy(dst, region.0, region.1, src2, region);
            return;
        }
        let Some((x0, y0, x1, y1)) = intersect(dst, region) else {
            return;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let s1 = read_pixel(src1, x, y);
                let s2 = read_pixel(src2, x, y);
                write_pixel(dst, x, y, blend_level(s1, s2, self.phase));
            }
        }
        dst.mark_dirty();
    }
    fn apply_rule_blend(
        &self,
        dst: &mut BitmapState,
        src1: &BitmapState,
        src2: &BitmapState,
        region: RectI,
        vague: i32,
    ) -> Result<(), TransError> {
        if self.phase <= 0 {
            blit_copy(dst, region.0, region.1, src1, region);
            return Ok(());
        }
        // `Process` short-circuits at the end phase and returns source 2
        // verbatim (the `>> 8` table would leave it one off).
        if self.phase >= self.phase_max {
            blit_copy(dst, region.0, region.1, src2, region);
            return Ok(());
        }
        let rule = self.rule.as_ref().ok_or(TransError::MissingRule)?;
        let table = universal_table(self.phase, vague);
        let Some((x0, y0, x1, y1)) = intersect(dst, region) else {
            return Ok(());
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let opa = i32::from(table[usize::from(rule.at(x, y))]);
                let s1 = read_pixel(src1, x, y);
                let s2 = read_pixel(src2, x, y);
                write_pixel(dst, x, y, blend_level(s1, s2, opa));
            }
        }
        dst.mark_dirty();
        Ok(())
    }

    fn apply_scroll(
        &self,
        dst: &mut BitmapState,
        src1: &BitmapState,
        src2: &BitmapState,
        region: RectI,
        from: ScrollFrom,
        stay: ScrollStay,
    ) {
        use ScrollFrom::{Bottom, Left, Right, Top};
        use ScrollStay::{NoStay, StayDest, StaySrc};

        let w = src1.width.min(i32::MAX as u32) as i32;
        let h = src1.height.min(i32::MAX as u32) as i32;
        let horizontal = matches!(from, Left | Right);
        let extent = if horizontal { w } else { h };
        let phase = (f64::from(self.progress()) * f64::from(extent)).round() as i32;

        let (mut s1l, mut s1t, mut s2l, mut s2t) = (0i32, 0i32, 0i32, 0i32);
        match from {
            Left => match stay {
                NoStay => {
                    s1l = phase;
                    s2l = phase - w;
                }
                StayDest => s2l = phase - w,
                StaySrc => s1l = phase,
            },
            Top => match stay {
                NoStay => {
                    s1t = phase;
                    s2t = phase - h;
                }
                StayDest => s2t = phase - h,
                StaySrc => s1t = phase,
            },
            Right => match stay {
                NoStay => {
                    s1l = -phase;
                    s2l = w - phase;
                }
                StayDest => s2l = w - phase,
                StaySrc => s1l = -phase,
            },
            Bottom => match stay {
                NoStay => {
                    s1t = -phase;
                    s2t = h - phase;
                }
                StayDest => s2t = h - phase,
                StaySrc => s1t = -phase,
            },
        }

        let mut rs1 = (s1l, s1t, w + s1l, h + s1t);
        let mut rs2 = (s2l, s2t, w + s2l, h + s2t);

        // Priority resolution: exactly the reference `Blend` ordering, where
        // each adjustment sees the previous one.
        if stay == StayDest {
            if horizontal {
                if rs2.2 >= rs1.0 && rs2.2 < rs1.2 {
                    rs1.0 = rs2.2;
                }
                if rs2.0 >= rs1.0 && rs2.0 < rs1.2 {
                    rs1.2 = rs2.0;
                }
            } else {
                if rs2.3 >= rs1.1 && rs2.3 < rs1.3 {
                    rs1.1 = rs2.3;
                }
                if rs2.1 >= rs1.1 && rs2.1 < rs1.3 {
                    rs1.3 = rs2.1;
                }
            }
        } else if stay == StaySrc {
            if horizontal {
                if rs1.2 >= rs2.0 && rs1.2 < rs2.2 {
                    rs2.0 = rs1.2;
                }
                if rs1.0 >= rs2.0 && rs1.0 < rs2.2 {
                    rs2.2 = rs1.0;
                }
            } else {
                if rs1.3 >= rs2.1 && rs1.3 < rs2.3 {
                    rs2.1 = rs1.3;
                }
                if rs1.1 >= rs2.1 && rs1.1 < rs2.3 {
                    rs2.3 = rs1.1;
                }
            }
        }

        if let Some(d) = intersect_rect(region, rs1) {
            let src_rect = (d.0 - s1l, d.1 - s1t, d.2 - s1l, d.3 - s1t);
            blit_copy_clipped(dst, d.0, d.1, src1, src_rect, region);
        }
        if let Some(d) = intersect_rect(region, rs2) {
            let src_rect = (d.0 - s2l, d.1 - s2t, d.2 - s2l, d.3 - s2t);
            blit_copy_clipped(dst, d.0, d.1, src2, src_rect, region);
        }
    }
}

/// Validate the `extrans` options that do not depend on the source size.
///
/// The reference rejects bad options in `StartTransition` — that is, while
/// the script is calling `Layer.beginTransition` — so the size-independent
/// half of each provider's checks runs here, at construction. The rest (a
/// centre inside the image, `maxdrift` below both dimensions) can only be
/// checked once the sources are known and stays in the provider's `apply`.
fn validate_options(kind: TransKind) -> Result<(), TransError> {
    let invalid = |option, reason| Err(TransError::InvalidOption { option, reason });
    match kind {
        TransKind::Wave { wave_type, .. } if !(0..=2).contains(&wave_type) => {
            invalid("wavetype", "must be 0, 1 or 2")
        }
        TransKind::Mosaic { max_size } if max_size < 1 => {
            invalid("maxsize", "must be at least one pixel")
        }
        TransKind::Ripple {
            map_width,
            roundness,
            max_drift,
            ..
        } => {
            if !matches!(map_width, 16 | 32 | 64 | 128) {
                return invalid("rwidth", "must be 16, 32, 64 or 128");
            }
            if !roundness.is_finite() || roundness <= 0.0 {
                return invalid("roundness", "must be positive and finite");
            }
            if !(0..=127).contains(&max_drift) {
                return invalid("maxdrift", "must be between 0 and 127");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Read one straight-alpha pixel, or transparent when out of bounds.
///
/// The `extrans` providers (see the submodules) sample through this.
pub(crate) fn read_pixel(bitmap: &BitmapState, x: i32, y: i32) -> [u8; 4] {
    if x < 0 || y < 0 {
        return [0; 4];
    }
    match bitmap.pixel_offset(x as u32, y as u32) {
        Some(i) => [
            bitmap.rgba[i],
            bitmap.rgba[i + 1],
            bitmap.rgba[i + 2],
            bitmap.rgba[i + 3],
        ],
        None => [0; 4],
    }
}

/// Write one straight-alpha pixel.
///
/// The `extrans` providers (see the submodules) store through this.
pub(crate) fn write_pixel(bitmap: &mut BitmapState, x: i32, y: i32, rgba: [u8; 4]) {
    if let Some(i) = bitmap.pixel_offset(x as u32, y as u32) {
        bitmap.rgba[i..i + 4].copy_from_slice(&rgba);
    }
}

/// A script colour (`0xAARRGGBB`, the `extrans` `bgcolor*` options) to
/// straight-alpha RGBA, matching `TVPToActualColor`.
pub(crate) fn argb_to_rgba(color: i64) -> [u8; 4] {
    let value = color as u32;
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    ]
}

/// Per-channel `src1 + (src2 - src1) * opa / 256`, the reference
/// `TVPConstAlphaBlend_SD`/`TVPUnivTransBlend` arithmetic for straight-alpha
/// RGBA. `opa` is `0..=256`.
///
/// The `extrans` providers blend with the same arithmetic (their
/// `TVP_RIPPLE_BLEND`/`TVPLinTransBlend` macros are the same expression).
pub(crate) fn blend_level(s1: [u8; 4], s2: [u8; 4], opa: i32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for c in 0..4 {
        let v = i32::from(s1[c]) + (((i32::from(s2[c]) - i32::from(s1[c])) * opa) >> 8);
        out[c] = v.clamp(0, 255) as u8;
    }
    out
}

/// `TVPInitUnivTransBlendTable_c` (`tvpgl.cpp:4576`): map a rule grayscale
/// value to the source-2 blend opacity for the current `phase`/`vague`.
///
/// `phase` is `0..=255 + vague`; `vague == 0` is the hard threshold used by
/// [`TransKind::Mask`].
fn universal_table(phase: i32, vague: i32) -> [u8; 256] {
    let mut table = [0u8; 256];
    let phasemax = phase;
    let phase0 = phase - vague;
    for (i, slot) in table.iter_mut().enumerate() {
        let i = i as i32;
        let value = if i < phase0 {
            255
        } else if i >= phasemax || vague <= 0 {
            0
        } else {
            255 - ((i - phase0) * 255 / vague)
        };
        *slot = value.clamp(0, 255) as u8;
    }
    table
}
