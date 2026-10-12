//! Core TVP native classes, implemented against the L2 API.
//!
//! Each family lives in its own module with a `register(ctx)` entry point;
//! [`register_all`] is the single call the host makes. The classes are thin
//! adapters: they translate TJS values to and from the L2 (`engine`) state and
//! actions and own no engine state themselves.
//!
//! The `tvp-*` crates are the *behavioral* reference only; they are not
//! copied.

pub mod async_trigger;
pub mod cdda_midi;
pub mod clipboard;
pub mod constants;
pub mod debug;
pub mod image_function;
pub mod input;
pub mod kag;
pub mod menu_item;
pub mod plugins;
pub mod rect;
pub mod scripts;
pub mod sound;
pub mod storages;
pub mod system;
pub mod tjsnum;
pub mod tree_owner;
pub mod video;
pub mod visual;

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use crate::{CompatCtx, CompatError, CompatServices};

/// How many native members the startup timing report names.
const NATIVE_TIMING_TOP_N: usize = 10;

/// A native whose accumulated startup dispatch time reaches this makes the
/// report `info` instead of `debug`.
const NATIVE_TIMING_INFO_THRESHOLD: Duration = Duration::from_millis(50);

/// One native member's accumulated dispatch cost while startup ran.
#[derive(Clone, Copy)]
struct NativeTimingEntry {
    name: &'static str,
    calls: u64,
    nanos: u64,
}

/// Per-native dispatch time accumulated while the startup scripts run.
///
/// This is L3 dispatch state reached through [`CompatServices::state`], not a
/// process global. The host opens the window with [`Self::begin`] and closes
/// it with [`Self::end`]; outside it [`Self::is_enabled`] is false and
/// [`NativeTimer`] skips the clock. Each instrumented call pays one dispatch
/// state lookup plus, while the window is open, one `Instant` pair and one
/// linear scan of the recorded members; formatting runs only when the report
/// is emitted.
#[derive(Default)]
struct NativeTiming {
    enabled: Cell<bool>,
    entries: RefCell<Vec<NativeTimingEntry>>,
}

impl NativeTiming {
    /// Clears the counters and starts accumulating.
    fn begin(&self) {
        self.entries.borrow_mut().clear();
        self.enabled.set(true);
    }

    /// Stops accumulating.
    fn end(&self) {
        self.enabled.set(false);
    }

    /// Whether accumulation is on.
    fn is_enabled(&self) -> bool {
        self.enabled.get()
    }

    /// Adds one call's `elapsed` to `name`'s total.
    fn record(&self, name: &'static str, elapsed: Duration) {
        let nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut entries = self.entries.borrow_mut();
        match entries.iter_mut().find(|entry| entry.name == name) {
            Some(entry) => {
                entry.calls = entry.calls.saturating_add(1);
                entry.nanos = entry.nanos.saturating_add(nanos);
            }
            None => entries.push(NativeTimingEntry {
                name,
                calls: 1,
                nanos,
            }),
        }
    }

    /// The report line and whether any member reached
    /// [`NATIVE_TIMING_INFO_THRESHOLD`], or `None` when nothing was recorded.
    ///
    /// This is the only allocation and formatting on the path; it runs once,
    /// after startup.
    fn format_report(&self) -> Option<(bool, String)> {
        let mut entries = self.entries.borrow().clone();
        if entries.is_empty() {
            return None;
        }
        entries.sort_by_key(|b| std::cmp::Reverse(b.nanos));
        let slow = entries
            .iter()
            .any(|entry| entry.nanos >= NATIVE_TIMING_INFO_THRESHOLD.as_nanos() as u64);
        let line = entries
            .iter()
            .take(NATIVE_TIMING_TOP_N)
            .map(|entry| {
                format!(
                    "{}={:.1}ms/{}",
                    entry.name,
                    entry.nanos as f64 / 1_000_000.0,
                    entry.calls
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        Some((slow, line))
    }

    /// Emits the top members by total time once, at `info` when any member
    /// reached [`NATIVE_TIMING_INFO_THRESHOLD`] and at `debug` otherwise.
    fn emit(&self) {
        if let Some((slow, line)) = self.format_report() {
            if slow {
                log::info!("startup: native time (top {NATIVE_TIMING_TOP_N}): {line}");
            } else {
                log::debug!("startup: native time (top {NATIVE_TIMING_TOP_N}): {line}");
            }
        }
    }

    /// The recorded entries, for the unit test that proves attribution.
    #[cfg(test)]
    fn recorded(&self) -> Vec<(&'static str, u64, u64)> {
        self.entries
            .borrow()
            .iter()
            .map(|entry| (entry.name, entry.calls, entry.nanos))
            .collect()
    }
}

/// Times one native callback while startup timing is on.
///
/// [`Self::start`] returns `None` after one dispatch-state lookup and one
/// `Cell` read when the window is closed, so an uninstrumented native is
/// unaffected; when the window is open the `Drop` records the elapsed time.
/// Instrumented natives hold the guard for the duration of the callback, so
/// every return path is counted.
pub(crate) struct NativeTimer<'a> {
    services: &'a CompatServices,
    name: &'static str,
    started: Instant,
}

impl<'a> NativeTimer<'a> {
    /// Starts timing `name`, or returns `None` when startup timing is off.
    pub(crate) fn start(services: &'a CompatServices, name: &'static str) -> Option<Self> {
        if !services.state::<NativeTiming>().is_enabled() {
            return None;
        }
        Some(Self {
            services,
            name,
            started: Instant::now(),
        })
    }
}

impl Drop for NativeTimer<'_> {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        self.services
            .state::<NativeTiming>()
            .record(self.name, elapsed);
    }
}

/// Opens the per-native timing window on the host's dispatch state.
pub(crate) fn begin_native_timing(services: &CompatServices) {
    services.state::<NativeTiming>().begin();
}

/// Closes the window and emits the one report line.
pub(crate) fn end_native_timing(services: &CompatServices) {
    let timing = services.state::<NativeTiming>();
    timing.end();
    timing.emit();
}

/// Register every implemented core native class on the VM.
pub fn register_all(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    constants::register(ctx)?;
    system::register(ctx)?;
    debug::register(ctx)?;
    storages::register(ctx)?;
    scripts::register(ctx)?;
    visual::register(ctx)?;
    input::register(ctx)?;
    sound::register(ctx)?;
    cdda_midi::register(ctx)?;
    async_trigger::register(ctx)?;
    menu_item::register(ctx)?;
    video::register(ctx)?;
    rect::register(ctx)?;
    image_function::register(ctx)?;
    clipboard::register(ctx)?;
    plugins::register(ctx)?;
    tree_owner::register(ctx)?;
    Ok(())
}
