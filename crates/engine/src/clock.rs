//! The frame clock: the engine's own time base.
//!
//! The clock is the value the frame advances and the value the script reads
//! through `System.getTickCount`. It is a plain object the engine owns: no
//! wall clock is read here, and nothing above this layer keeps a second time
//! base of its own.

/// The source of a frame's elapsed time.
///
/// The engine owns the clock it advances; the caller owns where the elapsed
/// time comes from. The windowed loop injects the real time the platform
/// measured, the headless runner injects the frame period it simulates, and a
/// test injects whatever it needs. One source per run: every timeline the
/// frame derives comes from the value this reports.
pub trait FrameTimeSource {
    /// The milliseconds elapsed since the previous frame.
    fn elapsed_ms(&self) -> f64;
}

/// A frame time the caller injects before every frame.
///
/// The value is written from outside the engine: the windowed loop writes
/// the real elapsed time the platform measured, the headless runner writes
/// the frame period it simulates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InjectedFrameTime {
    /// The milliseconds elapsed since the previous frame.
    pub elapsed_ms: f64,
}

impl InjectedFrameTime {
    /// A frame time of `elapsed_ms` milliseconds.
    pub fn new(elapsed_ms: f64) -> Self {
        Self { elapsed_ms }
    }
}

impl FrameTimeSource for InjectedFrameTime {
    fn elapsed_ms(&self) -> f64 {
        self.elapsed_ms
    }
}

/// The engine's frame clock: a millisecond time base the frame advances.
///
/// The clock holds the elapsed milliseconds since the engine started. The
/// frame is the only thing that moves it: [`FrameClock::advance`] adds a
/// frame's delta, and [`FrameClock::set`] puts the clock at an exact value.
/// A reader takes the whole milliseconds ([`FrameClock::now_ms`) or the value
/// with its fractional part ([`FrameClock::now_f64`]).
///
/// The fractional part is kept on purpose: a frame delta shorter than a
/// millisecond must add up over the frames instead of being discarded, so a
/// sixty-frame second of 16.6 ms steps reaches 996 ms rather than 960 ms.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameClock {
    /// The elapsed milliseconds, including the fractional part.
    ms: f64,
}

impl FrameClock {
    /// A clock at zero.
    pub fn new() -> Self {
        Self { ms: 0.0 }
    }

    /// The clock in whole milliseconds.
    ///
    /// The fractional part is not counted here: this is the value the timer
    /// registry, the upward hook and the script see.
    pub fn now_ms(&self) -> u64 {
        self.ms as u64
    }

    /// The clock in milliseconds, including the fractional part.
    pub fn now_f64(&self) -> f64 {
        self.ms
    }

    /// Advances the clock by `dt_ms` and returns the new whole-millisecond
    /// clock.
    ///
    /// A negative delta counts as zero, so the clock never runs backwards.
    /// The accumulated value keeps its fractional part, and the clock
    /// saturates at the integer ceiling instead of wrapping.
    pub fn advance(&mut self, dt_ms: f64) -> u64 {
        self.ms = (self.ms + dt_ms.max(0.0)).min(u64::MAX as f64);
        self.now_ms()
    }

    /// Sets the clock to `now_ms` exactly.
    ///
    /// A frame that carries its own time discards the fractional remainder
    /// the earlier deltas left behind, so the clock is exactly the value
    /// that was set.
    pub fn set(&mut self, now_ms: u64) {
        self.ms = now_ms as f64;
    }
}
