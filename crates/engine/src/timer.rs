//! L2 timer registry — remember when to wake up, then call back.
//!
//! Timers are explicit L2 state driven by the L1 clock: the host passes the
//! current time each frame to [`TimerState::advance`], which invokes the due
//! callbacks in creation order. compat owns the script-facing `Timer` object
//! and registers its dispatch closure as the callback; whether a timer is
//! armed *at all* is compat's decision (the reference's "armed and enabled yet never
//! posts" shapes — a negative `Capacity`, a non-positive `interval` — are
//! expressed by L3 simply not arming the timer).
//!
//! The registry is **repeating by construction**: a timer created through
//! [`TimerState::create_interval`] wakes its callback every `interval_ms`,
//! first one interval from the injected now, and keeps re-arming until it is
//! [`TimerState::remove`]d or [`TimerState::set_enabled`]`(false)`. There is
//! no one-shot mode, no fire budget and no suppression flag — L2's only job
//! is "wait, then call".
//!
//! # Time injection
//!
//! The registry has no clock of its own and reports failures as typed
//! errors rather than silently substituting defaults. Two entry points
//! inject time:
//!
//! * [`TimerState::advance`] takes `now_ms` from the L1 clock and *delivers*
//!   — this is the frame's event-pump step;
//! * assigning [`TimerState::now_ms`] injects the current time *without*
//!   delivering.
//!   L3's property setters write it before (re-)arming so a schedule write
//!   between two polls lands on the write's own timestamp, exactly like the
//!   reference deriving `NextTick` from `GetTickCount()` at
//!   `SetEnabled`/`SetInterval` time.
//!
//! Both keep the registry fully deterministic for tests: time only ever
//! enters through these two paths.
//!
//! # Re-entrancy
//!
//! Callbacks run *inside* [`TimerState::advance`], yet they may re-enter the
//! registry — creating timers, changing intervals, removing themselves —
//! through the same handle the host attached (L3's `attach_timer` pointer,
//! or a test's raw pointer). The delivery is therefore structured as:
//!
//! 1. **Collect first.** Every due timer is collected in creation order and
//!    re-armed from `now_ms` *before* any callback runs, so no phase iterates
//!    a collection it is mutating, a late frame fires exactly once (no
//!    catch-up burst), and a nested `advance` can never deliver the same
//!    timer twice in one round.
//! 2. **Then invoke.** For each collected timer the callback box is *taken
//!    out* of the entry before the call, so the entry is plain registry
//!    state while the callback runs. A collected timer fires only if its
//!    schedule is untouched since collection: any [`TimerState::set_interval`]
//!    or [`TimerState::set_enabled`] (the reference's `CancelEvents`) bumps
//!    the entry's schedule generation, and a removed timer simply has no
//!    entry left — both cancel the pending fire, like the reference cancels
//!    an already-queued `onTimer`.
//! 3. **Restore afterwards.** The taken callback is put back once the call
//!    returns — including when the callback *panics*: unwinding runs the
//!    restore guard, so the registry is left consistent (the timer keeps
//!    firing on later advances) and only the panic propagates.
//!
//! The registry is single-threaded: a callback's re-entrant borrow of the
//! registry is created and dropped entirely within one callback invocation,
//! strictly sequential with [`TimerState::advance`]'s own accesses (the
//! documented attach-handle contract).

use std::collections::BTreeMap;

/// Handle to a timer created by [`TimerState::create_interval`].
///
/// The inner value is stable for the lifetime of the timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct TimerId(pub u64);

impl TimerId {
    /// The raw identifier.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// The callback a registered timer invokes when it fires.
///
/// It receives the firing timer's own [`TimerId`] and may re-enter the
/// registry (see the module docs): create timers, change intervals, remove
/// itself — all through the handle the host attached.
pub type TimerCallback = Box<dyn FnMut(TimerId) + 'static>;

/// Typed timer-registry failures (no silent fallbacks).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TimerError {
    /// The id is not registered.
    #[error("unknown timer {0:?}")]
    UnknownTimer(TimerId),
    /// `interval_ms` was zero.
    ///
    /// `0` is not representable in L2 (a zero interval would be due
    /// immediately, every advance). L3 maps a script `interval <= 0` onto
    /// *not arming* the timer instead of ever passing it on here.
    #[error("timer interval must be at least 1 ms")]
    InvalidInterval,
}

/// One timer's internal state.
struct TimerEntry {
    /// Interval between fires; always at least 1 ms (validated when the
    /// timer is created and on every [`TimerState::set_interval`]).
    interval_ms: u64,
    /// `Some(deadline)` = armed and due at that time; `None` = disarmed —
    /// [`TimerState::set_enabled`]`(false)` drops the deadline outright, and
    /// the next enable re-arms from the then-injected now.
    deadline_ms: Option<u64>,
    /// The schedule generation: bumped on every schedule mutation
    /// ([`TimerState::set_interval`], [`TimerState::set_enabled`]) *and*
    /// when [`TimerState::advance`] collects the timer as due. A collected
    /// timer only fires if its generation is still the collected one, so a
    /// mutation between collection and delivery cancels the pending fire.
    generation: u64,
    /// The callback. `None` only while the callback itself is running (it
    /// is taken out for the invocation and put back by the restore guard,
    /// also when the callback unwinds).
    callback: Option<TimerCallback>,
}

/// The timer registry.
///
/// Timers are keyed by a monotonic [`TimerId`]; iteration is ordered, so
/// due callbacks are delivered in creation order. The registry owns no
/// global and no clock: time enters only through
/// [`TimerState::now_ms`] / [`TimerState::advance`].
#[derive(Default)]
#[non_exhaustive]
pub struct TimerState {
    /// Keyed by the monotonic id, which equals creation order — the map's
    /// ordered iteration is what makes delivery deterministic.
    timers: BTreeMap<TimerId, TimerEntry>,
    /// The next id to hand out.
    next_id: u64,
    /// The most recently injected time: the last `advance(now_ms)`, or the
    /// last write to this field. Every (re-)arm point is measured from here.
    pub now_ms: u64,
}

impl TimerState {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `callback` to wake every `interval_ms`, first fire one
    /// interval from the injected now ([`TimerState::now_ms`] or the last
    /// [`TimerState::advance`]).
    ///
    /// The timer is created **armed**: it fires at `now + interval_ms` and
    /// re-arms every `interval_ms` until removed or disabled. Disarm it
    /// immediately with [`TimerState::set_enabled`]`(false)` if the owner
    /// needs a scheduled-but-held timer.
    ///
    /// # Errors
    ///
    /// [`TimerError::InvalidInterval`] if `interval_ms == 0`.
    pub fn create_interval(
        &mut self,
        interval_ms: u64,
        callback: TimerCallback,
    ) -> Result<TimerId, TimerError> {
        if interval_ms == 0 {
            return Err(TimerError::InvalidInterval);
        }
        let id = TimerId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.timers.insert(
            id,
            TimerEntry {
                interval_ms,
                deadline_ms: Some(self.now_ms.saturating_add(interval_ms)),
                generation: 0,
                callback: Some(callback),
            },
        );
        Ok(id)
    }

    /// Remove a timer, dropping its callback. It never fires again.
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

    /// Set `id`'s interval in milliseconds (at least 1).
    ///
    /// On an armed timer this re-arms it from the injected now — a pending
    /// fire is cancelled, matching the reference's `SetInterval`, which
    /// cancels the queued events and re-derives `NextTick`. A disarmed timer
    /// just stores the interval for the next enable; L3 pushes its current
    /// interval again on every arming, so the stored value only matters
    /// while the timer is armed.
    ///
    /// # Errors
    ///
    /// [`TimerError::UnknownTimer`] if `id` does not exist, or
    /// [`TimerError::InvalidInterval`] if `interval_ms == 0`.
    pub fn set_interval(&mut self, id: TimerId, interval_ms: u64) -> Result<(), TimerError> {
        if interval_ms == 0 {
            return Err(TimerError::InvalidInterval);
        }
        let entry = self
            .timers
            .get_mut(&id)
            .ok_or(TimerError::UnknownTimer(id))?;
        entry.interval_ms = interval_ms;
        entry.generation = entry.generation.wrapping_add(1);
        if entry.deadline_ms.is_some() {
            entry.deadline_ms = Some(self.now_ms.saturating_add(interval_ms));
        }
        Ok(())
    }

    /// Arm or disarm `id`.
    ///
    /// Enabling arms the timer at `now + interval_ms`, measured from the
    /// injected now — write [`TimerState::now_ms`] first when arming outside
    /// [`TimerState::advance`]. Disabling drops the pending deadline, and
    /// the next enable re-arms from then. Both cancel a fire that a running
    /// [`TimerState::advance`] has collected but not yet delivered.
    ///
    /// # Errors
    ///
    /// [`TimerError::UnknownTimer`] if `id` does not exist.
    pub fn set_enabled(&mut self, id: TimerId, enabled: bool) -> Result<(), TimerError> {
        let entry = self
            .timers
            .get_mut(&id)
            .ok_or(TimerError::UnknownTimer(id))?;
        entry.generation = entry.generation.wrapping_add(1);
        entry.deadline_ms = if enabled {
            Some(self.now_ms.saturating_add(entry.interval_ms))
        } else {
            None
        };
        Ok(())
    }

    /// Advance the registry to `now_ms`, invoking every due callback once,
    /// in creation order; returns the number of callbacks invoked.
    ///
    /// A due timer fires at most once per advance and is re-armed from
    /// `now_ms`, so a late frame does not burst catch-up fires. A timer whose
    /// schedule was mutated between collection and delivery (by a peer
    /// callback or the timer itself) is skipped — the mutation cancelled its
    /// pending fire — and a timer removed mid-round has no entry left to
    /// invoke. Early-returns `0` when nothing is registered or nothing is
    /// armed.
    ///
    /// Re-entrancy: callbacks run inside this method and may mutate the
    /// registry (see the module docs); a panicking callback propagates its
    /// panic after the registry has been restored to a consistent state.
    pub fn advance(&mut self, now_ms: u64) -> usize {
        self.now_ms = now_ms;
        if self.timers.is_empty() {
            return 0;
        }
        // Phase 1 — collect due timers in creation order and re-arm them
        // from `now_ms` before any callback can mutate the registry.
        let mut due: Vec<(TimerId, u64)> = Vec::new();
        for (&id, entry) in self.timers.iter_mut() {
            let Some(deadline) = entry.deadline_ms else {
                continue;
            };
            if deadline > now_ms {
                continue;
            }
            entry.generation = entry.generation.wrapping_add(1);
            entry.deadline_ms = Some(now_ms.saturating_add(entry.interval_ms));
            due.push((id, entry.generation));
        }
        if due.is_empty() {
            return 0;
        }
        // Phase 2 — invoke. The callback is taken out of its entry for the
        // call, so the callback may freely mutate the registry; the guard
        // puts it back afterwards (also on unwind).
        let mut invoked = 0usize;
        for (id, generation) in due {
            let callback = match self.timers.get_mut(&id) {
                Some(entry) if entry.generation == generation => entry.callback.take(),
                // Removed, or rescheduled since collection: the schedule
                // write cancelled this round's fire.
                _ => continue,
            };
            let Some(callback) = callback else {
                continue;
            };
            let mut restore = RestoreCallback {
                state: self,
                id,
                callback: Some(callback),
            };
            if let Some(callback) = restore.callback.as_mut() {
                callback(id);
                invoked += 1;
            }
        }
        invoked
    }
}

/// Puts a taken [`TimerEntry::callback`] back into its entry once the
/// invocation ends — including when the callback panics and unwinds, so a
/// panic leaves the registry with every timer still able to fire.
struct RestoreCallback<'a> {
    state: &'a mut TimerState,
    id: TimerId,
    callback: Option<TimerCallback>,
}

impl Drop for RestoreCallback<'_> {
    fn drop(&mut self) {
        if let Some(callback) = self.callback.take()
            && let Some(entry) = self.state.timers.get_mut(&self.id)
            && entry.callback.is_none()
        {
            entry.callback = Some(callback);
        }
        // If the entry is gone the timer was removed mid-flight: the
        // entry's drop already ran, and this callback goes away with the
        // guard. (An entry that somehow regained a callback keeps it.)
    }
}
