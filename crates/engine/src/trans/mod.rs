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
use crate::scene::{BitmapState, Rect, TransitionFrame, TransitionSource};

mod effect;
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
            .as_chunks::<4>()
            .0
            .iter()
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
    /// The tick the transition started at, when the caller drives it from a
    /// clock.
    start_tick: Option<u64>,
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
            start_tick: None,
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

    /// Records the tick the transition starts at.
    ///
    /// The transition's elapsed time is the difference between the clock's
    /// value and this tick, so the phase follows the clock instead of an
    /// accumulated frame delta. A transition that is not started at a tick
    /// takes the tick of its first advance as its start.
    pub fn start_at(&mut self, tick: u64) {
        self.start_tick = Some(tick);
    }

    /// Advances the transition to the absolute tick `tick` and returns the new
    /// [`TransStep`].
    ///
    /// The elapsed time is the difference between `tick` and the tick the
    /// transition started at ([`Trans::start_at`]), so the phase is a function
    /// of the clock rather than of the frames that happened to run. A tick
    /// before the start counts as no elapsed time.
    pub fn advance_to(&mut self, tick: u64) -> TransStep {
        let start = *self.start_tick.get_or_insert(tick);
        self.seek_ms(tick.saturating_sub(start) as f64)
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
        self.start_tick = None;
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
        (0, 0, dimension(bitmap.width), dimension(bitmap.height))
    }

    /// The frame `src1`/`src2` composite into at the current phase, for a host
    /// that draws the projected scene instead of blending a display bitmap.
    ///
    /// Only the kinds that are a placement or a fade of the two sources return
    /// a frame — `crossfade` and `scroll`. The rule-based and warp kinds
    /// (`universal`, `mask`, `wave`, `mosaic`, `turn`, the `rotate*` family and
    /// `ripple`) need a per-pixel rule or coordinate warp, so they return
    /// `None` and [`Trans::apply`] blends them.
    ///
    /// `size` is the shared source size and the frame's own pixel space;
    /// `covers_layer` records whether the frame stands in for the layer's whole
    /// subtree (drawn over the layer's rect) rather than for its own image.
    /// `None` also covers a zero-sized frame.
    pub fn transition_frame(
        &self,
        src1: u32,
        src2: u32,
        size: (u32, u32),
        covers_layer: bool,
    ) -> Option<TransitionFrame> {
        if size.0 == 0 || size.1 == 0 {
            return None;
        }
        let full = Rect::new(0, 0, size.0, size.1);
        match self.kind {
            TransKind::CrossFade => {
                // The endpoints are exact copies of one source, so the other
                // source draws nothing there.
                let (start, end) = if self.phase <= 0 {
                    (1.0, 0.0)
                } else if self.phase >= self.phase_max {
                    (0.0, 1.0)
                } else {
                    // `blend_level` weights the fade by `phase >> 8`.
                    (1.0, self.phase as f32 / 256.0)
                };
                Some(TransitionFrame::new(
                    size,
                    covers_layer,
                    TransitionSource::new(src1, full, full, start),
                    TransitionSource::new(src2, full, full, end),
                ))
            }
            TransKind::Scroll { from, stay } => {
                let (w, h) = (dimension(size.0), dimension(size.1));
                let (first, second) = scroll_placements(self.progress(), (w, h), from, stay);
                let frame = (0, 0, w, h);
                // A source outside the frame contributes no pixel; it keeps a
                // zero rectangle, which the projection draws nothing for.
                let (dst1, src_rect1) = scroll_source_rects(frame, first).unwrap_or_default();
                let (dst2, src_rect2) = scroll_source_rects(frame, second).unwrap_or_default();
                Some(TransitionFrame::new(
                    size,
                    covers_layer,
                    TransitionSource::new(src1, src_rect1, dst1, 1.0),
                    TransitionSource::new(src2, src_rect2, dst2, 1.0),
                ))
            }
            TransKind::Universal { .. }
            | TransKind::Mask
            | TransKind::Wave { .. }
            | TransKind::Mosaic { .. }
            | TransKind::Turn { .. }
            | TransKind::RotateZoom { .. }
            | TransKind::RotateVanish { .. }
            | TransKind::RotateSwap { .. }
            | TransKind::Ripple { .. } => None,
        }
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
        // One weight for the whole rectangle: the phase.
        for y in y0..y1 {
            let row = BlendRow { x0, x1, y };
            blend_row(dst, src1, src2, row, self.phase);
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
        // Each pixel takes its own weight from the rule image.
        for y in y0..y1 {
            let row = BlendRow { x0, x1, y };
            blend_rule_row(dst, src1, src2, row, &table, rule);
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
        let w = dimension(src1.width);
        let h = dimension(src1.height);
        let (first, second) = scroll_placements(self.progress(), (w, h), from, stay);
        for (placement, source) in [(first, src1), (second, src2)] {
            if let Some(d) = intersect_rect(region, placement.rect) {
                let src_rect = (
                    d.0 - placement.left,
                    d.1 - placement.top,
                    d.2 - placement.left,
                    d.3 - placement.top,
                );
                blit_copy_clipped(dst, d.0, d.1, source, src_rect, region);
            }
        }
    }
}

/// One source of a `scroll` frame: the offset it is translated by and the
/// rectangle it occupies in the frame before clipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrollPlacement {
    /// Left offset of the source inside the frame.
    pub(crate) left: i32,
    /// Top offset of the source inside the frame.
    pub(crate) top: i32,
    /// Rectangle the source occupies, in frame pixels.
    pub(crate) rect: RectI,
}

/// The two source placements of a `scroll` frame at `progress`, after the
/// `stay` priority resolution.
fn scroll_placements(
    progress: f32,
    size: (i32, i32),
    from: ScrollFrom,
    stay: ScrollStay,
) -> (ScrollPlacement, ScrollPlacement) {
    use ScrollFrom::{Bottom, Left, Right, Top};
    use ScrollStay::{NoStay, StayDest, StaySrc};

    let (w, h) = size;
    let horizontal = matches!(from, Left | Right);
    let extent = if horizontal { w } else { h };
    let phase = (f64::from(progress) * f64::from(extent)).round() as i32;

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

    // Priority resolution: exactly the reference `Blend` ordering, where each
    // adjustment sees the previous one.
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

    (
        ScrollPlacement {
            left: s1l,
            top: s1t,
            rect: rs1,
        },
        ScrollPlacement {
            left: s2l,
            top: s2t,
            rect: rs2,
        },
    )
}

/// The destination and source rectangles one `scroll` source occupies inside
/// `frame`, after clipping to the frame; `None` when it lies outside.
fn scroll_source_rects(frame: RectI, placement: ScrollPlacement) -> Option<(Rect, Rect)> {
    let d = intersect_rect(frame, placement.rect)?;
    let width = (d.2 - d.0) as u32;
    let height = (d.3 - d.1) as u32;
    Some((
        Rect::new(d.0, d.1, width, height),
        Rect::new(d.0 - placement.left, d.1 - placement.top, width, height),
    ))
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
    match bitmap
        .pixel_offset(x as u32, y as u32)
        .and_then(|i| bitmap.rgba.get(i..i + 4))
    {
        Some(pixel) => [pixel[0], pixel[1], pixel[2], pixel[3]],
        None => [0; 4],
    }
}

/// Write one straight-alpha pixel.
///
/// The `extrans` providers (see the submodules) store through this.
pub(crate) fn write_pixel(bitmap: &mut BitmapState, x: i32, y: i32, rgba: [u8; 4]) {
    if let Some(i) = bitmap.pixel_offset(x as u32, y as u32)
        && let Some(pixel) = bitmap.rgba.get_mut(i..i + 4)
    {
        pixel.copy_from_slice(&rgba);
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
    [
        blend_channel(s1[0], s2[0], opa),
        blend_channel(s1[1], s2[1], opa),
        blend_channel(s1[2], s2[2], opa),
        blend_channel(s1[3], s2[3], opa),
    ]
}

/// One channel of [`blend_level`]: `src1 + (src2 - src1) * opa / 256`, clamped
/// to the byte range.
///
/// The row blend and the per-pixel blend share this expression, so the two
/// cannot drift apart.
#[inline(always)]
fn blend_channel(s1: u8, s2: u8, opa: i32) -> u8 {
    let s1 = i32::from(s1);
    let s2 = i32::from(s2);
    let value = s1 + (((s2 - s1) * opa) >> 8);
    value.clamp(0, 255) as u8
}

/// A bitmap dimension as the `i32` the pixel coordinates use.
fn dimension(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}

/// One row of a blend: the columns `[x0, x1)` and the row `y`.
#[derive(Clone, Copy, Debug)]
struct BlendRow {
    /// The first column of the row.
    x0: i32,
    /// The column after the last one.
    x1: i32,
    /// The row itself.
    y: i32,
}

/// The bytes of one bitmap row, restricted to the columns `[x0, x1)`.
///
/// `None` when the row holds no readable pixel: a pixel read outside the
/// bitmap is transparent.
fn row_bytes(bitmap: &BitmapState, row: BlendRow) -> Option<&[u8]> {
    if row.y < 0 || row.y >= dimension(bitmap.height) {
        return None;
    }
    let start = row.x0.max(0);
    let end = row.x1.min(dimension(bitmap.width));
    if start >= end {
        return Some(&[]);
    }
    let offset = bitmap.pixel_offset(start as u32, row.y as u32)?;
    let len = ((end - start) as usize).checked_mul(4)?;
    let last = offset.checked_add(len)?;
    bitmap.rgba.get(offset..last)
}

/// The destination's bytes for the columns `[x0, x1)` of row `y`.
fn dst_row_bytes(dst: &mut BitmapState, row: BlendRow) -> Option<&mut [u8]> {
    let start = row.x0.max(0);
    let end = row.x1.min(dimension(dst.width));
    if start >= end {
        return None;
    }
    let offset = dst.pixel_offset(start as u32, row.y as u32)?;
    let len = ((end - start) as usize).checked_mul(4)?;
    let last = offset.checked_add(len)?;
    dst.rgba.get_mut(offset..last)
}

/// The rule's grayscale row for `y`, the row the blend reads.
fn rule_row(rule: &RuleMask, y: i32) -> &[u8] {
    let width = rule.width as usize;
    let height = dimension(rule.height);
    let row = y.rem_euclid(height) as usize;
    &rule.gray[row * width..(row + 1) * width]
}

/// Blends one row of the destination with the two sources and one weight for
/// the whole row.
fn blend_row(
    dst: &mut BitmapState,
    src1: &BitmapState,
    src2: &BitmapState,
    row: BlendRow,
    opa: i32,
) {
    let Some(dst_row) = dst_row_bytes(dst, row) else {
        return;
    };
    let first = row_bytes(src1, row);
    let second = row_bytes(src2, row);
    match (first, second) {
        (Some(a), Some(b)) if a.len() == dst_row.len() && b.len() == dst_row.len() => {
            blend_pixels(dst_row, a, b, opa);
        }
        _ => blend_pixels_sparse(dst_row, src1, src2, row, opa),
    }
}

/// Blends the pixels of one row: `dst = src1 + (src2 - src1) * opa / 256`,
/// clamped per channel.
///
/// The rows hold whole pixels, so every pixel of the row is blended.
fn blend_pixels(dst: &mut [u8], src1: &[u8], src2: &[u8], opa: i32) {
    for (d, (a, b)) in dst.as_chunks_mut::<4>().0.iter_mut().zip(
        src1.as_chunks::<4>()
            .0
            .iter()
            .zip(src2.as_chunks::<4>().0.iter()),
    ) {
        d[0] = blend_channel(a[0], b[0], opa);
        d[1] = blend_channel(a[1], b[1], opa);
        d[2] = blend_channel(a[2], b[2], opa);
        d[3] = blend_channel(a[3], b[3], opa);
    }
}

/// Blends the pixels of one row whose sources do not cover every column: a
/// column outside a source reads as transparent.
fn blend_pixels_sparse(
    dst: &mut [u8],
    src1: &BitmapState,
    src2: &BitmapState,
    row: BlendRow,
    opa: i32,
) {
    for (index, pixel) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = row.x0.max(0) + index as i32;
        let blended = blend_level(read_pixel(src1, x, row.y), read_pixel(src2, x, row.y), opa);
        pixel[0] = blended[0];
        pixel[1] = blended[1];
        pixel[2] = blended[2];
        pixel[3] = blended[3];
    }
}

/// Blends one row of the destination with the two sources and the weight
/// each pixel takes from the rule image.
fn blend_rule_row(
    dst: &mut BitmapState,
    src1: &BitmapState,
    src2: &BitmapState,
    row: BlendRow,
    table: &[u8; 256],
    rule: &RuleMask,
) {
    let Some(dst_row) = dst_row_bytes(dst, row) else {
        return;
    };
    let first = row_bytes(src1, row);
    let second = row_bytes(src2, row);
    let gray_row = rule_row(rule, row.y);
    let width = gray_row.len();
    let mut column = row.x0.max(0).rem_euclid(dimension(rule.width)) as usize;
    match (first, second) {
        (Some(a), Some(b)) if a.len() == dst_row.len() && b.len() == dst_row.len() => {
            for (d, (left, right)) in dst_row
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()))
            {
                let opa = i32::from(table[usize::from(gray_row[column])]);
                column += 1;
                if column == width {
                    column = 0;
                }
                if opa == 0 {
                    // A zero weight leaves the first source untouched.
                    d[0] = left[0];
                    d[1] = left[1];
                    d[2] = left[2];
                    d[3] = left[3];
                    continue;
                }
                d[0] = blend_channel(left[0], right[0], opa);
                d[1] = blend_channel(left[1], right[1], opa);
                d[2] = blend_channel(left[2], right[2], opa);
                d[3] = blend_channel(left[3], right[3], opa);
            }
        }
        _ => blend_rule_pixels_sparse(dst_row, src1, src2, row, table, rule),
    }
}

/// Blends the pixels of one row whose sources do not cover every column: a
/// column outside a source reads as transparent.
fn blend_rule_pixels_sparse(
    dst: &mut [u8],
    src1: &BitmapState,
    src2: &BitmapState,
    row: BlendRow,
    table: &[u8; 256],
    rule: &RuleMask,
) {
    for (index, pixel) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = row.x0.max(0) + index as i32;
        let opa = i32::from(table[usize::from(rule.at(x, row.y))]);
        let blended = blend_level(read_pixel(src1, x, row.y), read_pixel(src2, x, row.y), opa);
        pixel[0] = blended[0];
        pixel[1] = blended[1];
        pixel[2] = blended[2];
        pixel[3] = blended[3];
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixels the blend tests read: the channel extremes, the alpha
    /// extremes and a mid-tone.
    const PIXELS: [[u8; 4]; 6] = [
        [0, 0, 0, 0],
        [255, 255, 255, 255],
        [0, 255, 0, 255],
        [255, 0, 255, 0],
        [1, 254, 128, 64],
        [200, 100, 50, 255],
    ];

    /// The two source rows the blend tests read: the pixels and their
    /// mirror image, so every channel pair of the two sources appears.
    fn source_row() -> (Vec<u8>, Vec<u8>) {
        let mut first = Vec::new();
        let mut second = Vec::new();
        for pixel in PIXELS {
            first.extend_from_slice(&pixel);
            second.extend_from_slice(&[pixel[3], pixel[2], pixel[1], pixel[0]]);
        }
        (first, second)
    }

    /// One pixel of a row buffer.
    fn pixel(row: &[u8], index: usize) -> [u8; 4] {
        [
            row[index * 4],
            row[index * 4 + 1],
            row[index * 4 + 2],
            row[index * 4 + 3],
        ]
    }

    /// The row blend and the per-pixel blend must write the same bytes.
    ///
    /// The blend is the per-channel `src1 + (src2 - src1) * opa / 256` the
    /// reference applies to a whole row at a time
    /// (`reference/cpp/core/visual/tvpgl.cpp:4387-4399`).
    #[test]
    fn the_row_blend_matches_the_per_pixel_blend() {
        let (first, second) = source_row();
        for opa in 0..=256 {
            let mut row = vec![0u8; first.len()];
            blend_pixels(&mut row, &first, &second, opa);
            let mut per_pixel = vec![0u8; first.len()];
            for (index, out) in per_pixel.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let blended = blend_level(pixel(&first, index), pixel(&second, index), opa);
                out.copy_from_slice(&blended);
            }
            assert_eq!(row, per_pixel, "weight {opa}");
        }
    }

    /// The rule row blend and the per-pixel rule blend must write the same
    /// bytes.
    ///
    /// The rule supplies the weight of every pixel
    /// (`reference/cpp/core/visual/tvpgl.cpp:4624-4636`) and the table maps
    /// the rule's grayscale value to that weight
    /// (`reference/cpp/core/visual/tvpgl.cpp:4576-4595`).
    #[test]
    fn the_rule_row_blend_matches_the_per_pixel_blend() {
        let (first, second) = source_row();
        let width = PIXELS.len() as u32;
        let height = 3;
        // A ramp that reaches the weights of zero and of the whole range.
        let gray: Vec<u8> = (0..(width * height)).map(|i| (i * 14) as u8).collect();
        let rule = RuleMask::new(width, height, gray).expect("rule");
        let table = universal_table(120, 64);
        let mut first_image = Vec::new();
        let mut second_image = Vec::new();
        for _ in 0..height {
            first_image.extend_from_slice(&first);
            second_image.extend_from_slice(&second);
        }
        let size = first_image.len();
        let src1 = BitmapState::new(width, height, first_image);
        let src2 = BitmapState::new(width, height, second_image);
        let mut dst = BitmapState::new(width, height, vec![0u8; size]);
        for y in 0..height as i32 {
            let row = BlendRow {
                x0: 0,
                x1: width as i32,
                y,
            };
            blend_rule_row(&mut dst, &src1, &src2, row, &table, &rule);
        }
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let opa = i32::from(table[usize::from(rule.at(x, y))]);
                let expected =
                    blend_level(pixel(&first, x as usize), pixel(&second, x as usize), opa);
                let index = (y as usize) * width as usize + x as usize;
                assert_eq!(
                    pixel(&dst.rgba, index),
                    expected,
                    "pixel ({x},{y}) with weight {opa}"
                );
            }
        }
    }

    /// A row whose sources do not cover every column blends the covered
    /// columns and leaves the rest transparent, the way a per-pixel read
    /// outside a bitmap does.
    ///
    /// The blend of the covered columns is the per-channel arithmetic above
    /// (`reference/cpp/core/visual/tvpgl.cpp:4387-4399`).
    #[test]
    fn a_row_without_full_sources_blends_what_it_covers() {
        let src1 = BitmapState::new(2, 1, vec![10, 20, 30, 40, 50, 60, 70, 80]);
        let src2 = BitmapState::new(1, 1, vec![90, 100, 110, 120]);
        let mut dst = BitmapState::new(3, 1, vec![0u8; 12]);
        let row = BlendRow { x0: 0, x1: 3, y: 0 };
        blend_row(&mut dst, &src1, &src2, row, 128);
        // The first column blends both sources, the second only the first
        // one (the second source reads as transparent there) and the third
        // reads as transparent in both.
        assert_eq!(&dst.rgba[0..4], &[50, 60, 70, 80]);
        assert_eq!(&dst.rgba[4..8], &[25, 30, 35, 40]);
        assert_eq!(&dst.rgba[8..12], &[0, 0, 0, 0]);
    }
}
