//! Interpolation primitives for the motion runtime.
//!
//! The reference plugin keeps a per-variable keyframe queue
//! (`Player::VariableAnimatorState`) and steps it with a frame delta,
//! applying an ease weight in `[0, 1]`. Timelines reuse the same shape for
//! their blend animators. This module provides the shared state machine and
//! the easing helpers; it composes with the engine timer registry by taking an
//! explicit elapsed-time delta rather than reading a clock.

/// One queued keyframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Keyframe {
    /// The target value.
    pub(crate) value: f32,
    /// The transition duration in frames (`0` means snap).
    pub(crate) duration: f32,
    /// The ease weight (`1.0` is linear).
    pub(crate) weight: f32,
}

/// A queue-driven scalar animator.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Animator {
    queue: std::collections::VecDeque<Keyframe>,
    /// Whether a transition is currently running.
    pub(crate) active: bool,
    /// The value most recently exposed to the script surface.
    pub(crate) current: f32,
    start: f32,
    target: f32,
    progress: f32,
    duration: f32,
    weight: f32,
}

impl Animator {
    /// A settled animator at `value`.
    pub(crate) fn at(value: f32) -> Self {
        Self {
            queue: std::collections::VecDeque::new(),
            active: false,
            current: value,
            start: value,
            target: value,
            progress: 1.0,
            duration: 0.0,
            weight: 1.0,
        }
    }

    /// The current value.
    pub(crate) fn value(&self) -> f32 {
        self.current
    }

    /// Whether a transition is queued or running.
    pub(crate) fn is_active(&self) -> bool {
        self.active || !self.queue.is_empty()
    }

    /// Snap directly to `value`, clearing any transition.
    pub(crate) fn set(&mut self, value: f32) {
        self.queue.clear();
        self.active = false;
        self.current = value;
        self.start = value;
        self.target = value;
        self.progress = 1.0;
        self.duration = 0.0;
        self.weight = 1.0;
    }

    /// Queue a transition toward `value` over `duration` frames.
    ///
    /// A non-positive duration snaps immediately, matching the reference
    /// `queueControllerStateLikeBinary` fast path. Otherwise the keyframe is
    /// appended; an idle animator starts from its current value.
    pub(crate) fn transition_to(&mut self, value: f32, duration: f32, weight: f32) {
        if duration <= 0.0 {
            self.set(value);
            return;
        }
        if !self.active && self.queue.is_empty() {
            self.start = self.current;
        }
        self.queue.push_back(Keyframe {
            value,
            duration,
            weight,
        });
        if !self.active {
            self.begin_next();
        }
    }

    fn begin_next(&mut self) {
        let Some(next) = self.queue.pop_front() else {
            self.active = false;
            return;
        };
        self.active = true;
        self.start = self.current;
        self.target = next.value;
        self.progress = 0.0;
        self.duration = next.duration.max(f32::EPSILON);
        self.weight = next.weight;
    }

    /// Advance the transition by `dt` frames.
    pub(crate) fn step(&mut self, dt: f32) {
        if !self.active {
            return;
        }
        let step = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        if self.duration <= 0.0 {
            self.current = self.target;
            self.progress = 1.0;
            self.active = false;
            if !self.queue.is_empty() {
                self.begin_next();
            }
            return;
        }
        self.progress = (self.progress + step / self.duration).min(1.0);
        let eased = apply_ease(self.progress, self.weight);
        self.current = self.start + (self.target - self.start) * eased;
        if self.progress >= 1.0 {
            self.current = self.target;
            self.active = false;
            if !self.queue.is_empty() {
                self.begin_next();
            }
        }
    }
}

/// Apply the reference ease weight to a normalized progress.
///
/// `weight > 1` eases in (slow start), `weight < 1` eases out (fast start),
/// and `1.0` is linear. The curve is the smooth-step `p^weight` blend the
/// reference approximates with its controller animators.
pub(crate) fn apply_ease(progress: f32, weight: f32) -> f32 {
    let p = progress.clamp(0.0, 1.0);
    if !weight.is_finite() || (weight - 1.0).abs() < f32::EPSILON {
        return p;
    }
    p.powf(weight)
}

/// Convert the script `easing` argument to the reference animator weight.
///
/// `ease > 0` maps to `ease + 1`, `ease < 0` to `1 / (1 - ease)`, and `0`
/// to linear — exactly `variableEaseWeightLike_0x671228`.
pub(crate) fn ease_weight(ease: f64) -> f32 {
    if ease > 0.0 {
        (ease + 1.0) as f32
    } else if ease < 0.0 {
        (1.0 / (1.0 - ease)) as f32
    } else {
        1.0
    }
}

/// A color animator that tracks four bytes plus a packed value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ColorAnimator {
    /// The packed `0xAARRGGBB` value.
    pub(crate) packed: u32,
    /// The four RGBA bytes (reference `_emoteColorState.rgbaBytes`).
    pub(crate) bytes: [f32; 4],
}

impl Default for ColorAnimator {
    fn default() -> Self {
        Self {
            packed: 0xFFFF_FFFF,
            bytes: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

impl ColorAnimator {
    /// Parse a packed `0xAARRGGBB` color into the byte vector.
    pub(crate) fn from_packed(packed: u32) -> Self {
        Self {
            packed,
            bytes: [
                ((packed >> 16) & 0xFF) as f32 / 255.0,
                ((packed >> 8) & 0xFF) as f32 / 255.0,
                (packed & 0xFF) as f32 / 255.0,
                ((packed >> 24) & 0xFF) as f32 / 255.0,
            ],
        }
    }
}
