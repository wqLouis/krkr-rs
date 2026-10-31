//! The frame clock: the engine's own time base.
//!
//! The clock is the value the frame advances and the value the script reads
//! through `System.getTickCount`. It is a plain object the engine owns: no
//! wall clock is read here, and nothing above this layer keeps a second time
//! base of its own.

/// The source of the frame's time.
///
/// The engine owns the clock it advances; the caller owns where the time comes
/// from. The windowed loop injects the real time the platform measured, the
/// headless runner injects the time it measured between its frames, and a test
/// injects whatever it needs. One source per run: every timeline the frame
/// derives comes from the value this reports.
///
/// The source is read at **two** moments, and both readings are the same base:
/// the frame reads [`FrameTimeSource::elapsed_ms`] at each boundary and adds it
/// to the clock, and the script's tick count reads
/// [`FrameTimeSource::now_ms`] when the script asks. A source that measures
/// real time therefore moves the script's clock inside a frame; a source that
/// injects a fixed frame time has no reading of its own, and the tick count
/// stays the frame clock.
pub trait FrameTimeSource {
    /// The milliseconds elapsed since the previous frame.
    ///
    /// The engine adds this to its clock when the frame starts.
    fn elapsed_ms(&self) -> f64;

    /// The source's current reading, in milliseconds on the same base as
    /// [`FrameTimeSource::elapsed_ms`].
    ///
    /// This is read on demand — once per `System.getTickCount` call and nowhere
    /// else — so it must answer with the clock's value *now* and not with a
    /// cached frame boundary. A source that measures real time answers with the
    /// time elapsed since the run started; [`InjectedFrameTime`] answers with
    /// the frame time it was given, because a simulated frame has no clock
    /// behind it.
    fn now_ms(&self) -> f64;
}

/// A frame time the caller injects before every frame.
///
/// The value is written from outside the engine: a test injects the frame
/// period it simulates. A host that measures real time owns its own source
/// instead, so that its reading can move inside a frame.
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

    /// The injected frame time itself.
    ///
    /// A simulated frame has no clock of its own: there is no moment between
    /// two reads, so the reading is the frame time the caller injected. A
    /// reader that has the frame clock takes the later of the two, so an
    /// injected run's tick count stays the frame clock and a test remains
    /// deterministic.
    fn now_ms(&self) -> f64 {
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
