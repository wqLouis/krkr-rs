//! L2 timer registry.
//!
//! Timers are explicit L2 state driven by the L1 clock: the host passes the
//! current time each frame and L2 reports which timers are due; L3 owns the
//! script-facing `Timer` object and dispatches `onTimer`.
//!
//! Behavioral reference: the L3 `Timer` in `compat/src/natives/visual.rs`
//! and `main`'s `crates/tvp-visual`.
//!
//! The registry has no clock of its own: [`TimerState::advance`] takes
//! `now_ms` from the L1 clock, so time is injected and tests are
//! deterministic. It holds no global (D9) and reports failures as typed
//! errors rather than silently substituting defaults (D11).

use std::collections::BTreeMap;

/// Handle to a timer created by [`TimerState::create`].
///
/// The inner value is stable for the lifetime of the timer and is what the
/// script-facing `Timer.id` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct TimerId(pub u64);

impl TimerId {
    /// The raw identifier.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// How a timer behaves once it fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimerMode {
    /// Fire once, then disable itself.
    OneShot,
    /// Re-arm and fire again every interval.
    Repeat,
}

/// A timer's configurable state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TimerConfig {
    /// Interval between fires in milliseconds; must be at least 1.
    pub interval_ms: u64,
    /// Whether the timer fires.
    pub enabled: bool,
    /// Fire-once or repeating.
    pub mode: TimerMode,
    /// Reference `Capacity` (stored; one callback is delivered per interval).
    pub capacity: u32,
}

impl TimerConfig {
    /// Create a timer configuration from its four fields.
    pub const fn new(interval_ms: u64, enabled: bool, mode: TimerMode, capacity: u32) -> Self {
        Self {
            interval_ms,
            enabled,
            mode,
            capacity,
        }
    }
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            enabled: false,
            mode: TimerMode::Repeat,
            capacity: 1,
        }
    }
}

/// A timer that fired during one [`TimerState::advance`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TimerFire {
    /// The timer that fired.
    pub id: TimerId,
    /// Cumulative number of times the timer has fired, after this advance.
    pub count: u64,
}

impl TimerFire {
    /// Create a fire record for `id` at cumulative `count`.
    pub const fn new(id: TimerId, count: u64) -> Self {
        Self { id, count }
    }
}

/// Typed timer-registry failures (D11: no silent fallbacks).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TimerError {
    /// The id is not registered.
    #[error("unknown timer {0:?}")]
    UnknownTimer(TimerId),
    /// `interval_ms` was zero.
    #[error("timer interval must be at least 1 ms")]
    InvalidInterval,
    /// `capacity` was zero.
    #[error("timer capacity must be at least 1")]
    InvalidCapacity,
}

/// One timer's internal state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimerEntry {
    config: TimerConfig,
    /// Time of the next fire in the L1 clock's domain.
    next_fire_ms: u64,
    /// Cumulative fire count.
    count: u64,
}

/// The timer registry.
///
/// Timers are keyed by a monotonic [`TimerId`]; iteration is ordered so the
/// due list is deterministic. The registry owns no global and no clock.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct TimerState {
    timers: BTreeMap<TimerId, TimerEntry>,
    next_id: u64,
}

impl TimerState {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a timer with the given config and return its id.
    ///
    /// # Errors
    ///
    /// [`TimerError::InvalidInterval`] if `interval_ms == 0`, or
    /// [`TimerError::InvalidCapacity`] if `capacity == 0`.
    pub fn create(&mut self, config: TimerConfig) -> Result<TimerId, TimerError> {
        if config.interval_ms == 0 {
            return Err(TimerError::InvalidInterval);
        }
        if config.capacity == 0 {
            return Err(TimerError::InvalidCapacity);
        }
        let id = TimerId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.timers.insert(
            id,
            TimerEntry {
                config,
                // A disabled timer has no meaningful deadline until it is
                // enabled through `set_enabled`; an enabled one is due on the
                // first `advance` after creation.
                next_fire_ms: 0,
                count: 0,
            },
        );
        Ok(id)
    }

    /// Remove a timer.
    ///
    /// # Errors
    ///
    /// [`TimerError::UnknownTimer`] if `id` does not exist.
    pub fn remove(&mut self, id: TimerId) -> Result<(), TimerError> {
        self.timers
            .remove(&id)
            .map(|_| ())
            .ok_or(TimerError::UnknownTimer(id))
    }

    /// Whether `id` is registered.
    pub fn contains(&self, id: TimerId) -> bool {
        self.timers.contains_key(&id)
    }

    /// The number of registered timers.
    pub fn len(&self) -> usize {
        self.timers.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }

    /// The config of `id`, if registered.
    pub fn config(&self, id: TimerId) -> Option<TimerConfig> {
        self.timers.get(&id).map(|entry| entry.config)
    }

    /// `id`'s interval in milliseconds, if registered.
    pub fn interval_ms(&self, id: TimerId) -> Option<u64> {
        self.timers.get(&id).map(|entry| entry.config.interval_ms)
    }

    /// Whether `id` is enabled, if registered.
    pub fn is_enabled(&self, id: TimerId) -> Option<bool> {
        self.timers.get(&id).map(|entry| entry.config.enabled)
    }

    /// `id`'s mode, if registered.
    pub fn mode(&self, id: TimerId) -> Option<TimerMode> {
        self.timers.get(&id).map(|entry| entry.config.mode)
    }

    /// `id`'s capacity, if registered.
    pub fn capacity(&self, id: TimerId) -> Option<u32> {
        self.timers.get(&id).map(|entry| entry.config.capacity)
    }

    /// `id`'s cumulative fire count, if registered.
    pub fn count(&self, id: TimerId) -> Option<u64> {
        self.timers.get(&id).map(|entry| entry.count)
    }

    /// `id`'s next-fire time, if registered.
    pub fn next_fire_ms(&self, id: TimerId) -> Option<u64> {
        self.timers.get(&id).map(|entry| entry.next_fire_ms)
    }

    /// Set `id`'s interval, re-arming from `now_ms` when it is enabled.
    ///
    /// # Errors
    ///
    /// [`TimerError::UnknownTimer`] or [`TimerError::InvalidInterval`].
    pub fn set_interval(
        &mut self,
        id: TimerId,
        interval_ms: u64,
        now_ms: u64,
    ) -> Result<(), TimerError> {
        if interval_ms == 0 {
            return Err(TimerError::InvalidInterval);
        }
        let entry = self
            .timers
            .get_mut(&id)
            .ok_or(TimerError::UnknownTimer(id))?;
        entry.config.interval_ms = interval_ms;
        if entry.config.enabled {
            entry.next_fire_ms = now_ms.saturating_add(interval_ms);
        }
        Ok(())
    }

    /// Enable or disable `id`, re-arming from `now_ms` when enabling.
    ///
    /// # Errors
    ///
    /// [`TimerError::UnknownTimer`] if `id` does not exist.
    pub fn set_enabled(
        &mut self,
        id: TimerId,
        enabled: bool,
        now_ms: u64,
    ) -> Result<(), TimerError> {
        let entry = self
            .timers
            .get_mut(&id)
            .ok_or(TimerError::UnknownTimer(id))?;
        entry.config.enabled = enabled;
        if enabled {
            entry.next_fire_ms = now_ms.saturating_add(entry.config.interval_ms);
        }
        Ok(())
    }

    /// Set `id`'s capacity.
    ///
    /// # Errors
    ///
    /// [`TimerError::UnknownTimer`] or [`TimerError::InvalidCapacity`].
    pub fn set_capacity(&mut self, id: TimerId, capacity: u32) -> Result<(), TimerError> {
        if capacity == 0 {
            return Err(TimerError::InvalidCapacity);
        }
        let entry = self
            .timers
            .get_mut(&id)
            .ok_or(TimerError::UnknownTimer(id))?;
        entry.config.capacity = capacity;
        Ok(())
    }

    /// Set `id`'s mode.
    ///
    /// # Errors
    ///
    /// [`TimerError::UnknownTimer`] if `id` does not exist.
    pub fn set_mode(&mut self, id: TimerId, mode: TimerMode) -> Result<(), TimerError> {
        let entry = self
            .timers
            .get_mut(&id)
            .ok_or(TimerError::UnknownTimer(id))?;
        entry.config.mode = mode;
        Ok(())
    }

    /// Advance the registry to `now_ms`, firing every due timer once.
    ///
    /// A fired repeating timer is re-armed from `now_ms` (so a late frame does
    /// not burst catch-up fires, matching the reference); a one-shot timer is
    /// disabled. Returns the fired timers in id order.
    pub fn advance(&mut self, now_ms: u64) -> Vec<TimerFire> {
        let mut fired = Vec::new();
        for (&id, entry) in self.timers.iter_mut() {
            if !entry.config.enabled || entry.next_fire_ms > now_ms {
                continue;
            }
            entry.count = entry.count.saturating_add(1);
            fired.push(TimerFire {
                id,
                count: entry.count,
            });
            match entry.config.mode {
                TimerMode::OneShot => entry.config.enabled = false,
                TimerMode::Repeat => {
                    entry.next_fire_ms = now_ms.saturating_add(entry.config.interval_ms);
                }
            }
        }
        fired
    }

    /// Whether any registered timer is enabled.
    pub fn any_enabled(&self) -> bool {
        self.timers.values().any(|entry| entry.config.enabled)
    }
}
