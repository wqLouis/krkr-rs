//! The one process-wide engine → context binding (doc 08 §3.1).
//!
//! The C++ ABI hands native callbacks only the raw `tjs2_engine*`, and
//! `tjs2-sys` exposes **no user-data slot**. The old `tvp-*` code worked
//! around this with process-global contexts; the target architecture keeps
//! exactly **one** process-wide binding — this table — and documents it.
//!
//! It is L3 (never L1), it holds *dispatch* state (the Rust [`Tjs2Engine`],
//! the L2 [`CompatServices`] and the [`PluginRegistry`]) and **never game
//! state**. The host installs it once, before any native callback can run,
//! and clears it before dropping the engine; the threaded services it points
//! at stay alive for the whole VM lifetime.
//!
//! The table stores plain `usize` addresses so it needs no `unsafe impl
//! Send`/`Sync`: the addresses are only dereferenced on the single VM thread.

use std::sync::{LazyLock, Mutex, MutexGuard};

use tjs2_sys::{Engine, Tjs2Engine};

use crate::{CompatCtx, CompatError, CompatServices, PluginRegistry};

struct Binding {
    /// The C++ engine pointer native callbacks receive (`Tjs2Engine::raw`).
    engine_raw: usize,
    /// The address of the Rust host engine (for safe method calls).
    engine: usize,
    /// The address of the host-owned L2 services.
    services: usize,
    /// The address of the host-owned plugin registry.
    registry: usize,
}

static BINDING: LazyLock<Mutex<Option<Binding>>> = LazyLock::new(|| Mutex::new(None));

fn lock() -> MutexGuard<'static, Option<Binding>> {
    BINDING.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A resolved binding, copied out of the table so no lock is held while a
/// native callback runs.
#[derive(Clone, Copy)]
pub(crate) struct Resolved {
    engine: usize,
    services: usize,
    registry: usize,
}

impl Resolved {
    /// The Rust host engine.
    pub(crate) fn engine(&self) -> &'static Tjs2Engine {
        // SAFETY: install only stores the address of a live host engine that
        // outlives the binding.
        unsafe { &*(self.engine as *const Tjs2Engine) }
    }

    /// The host-owned plugin registry.
    pub(crate) fn registry(&self) -> &'static PluginRegistry {
        // SAFETY: install only stores the address of a live registry that
        // outlives the binding.
        unsafe { &*(self.registry as *const PluginRegistry) }
    }

    /// A context handle for registration and native dispatch.
    pub(crate) fn ctx(&self) -> CompatCtx {
        CompatCtx::from_raw(self.engine, self.services)
    }
}

/// Install the process-wide binding.
///
/// Fails if a binding is already installed: there is exactly one VM.
pub(crate) fn install(
    engine: &Tjs2Engine,
    services: *mut CompatServices,
    registry: &PluginRegistry,
) -> Result<(), CompatError> {
    let mut slot = lock();
    if slot.is_some() {
        return Err(CompatError::Register(
            "compat binding already installed".into(),
        ));
    }
    *slot = Some(Binding {
        engine_raw: engine.raw() as usize,
        engine: engine as *const Tjs2Engine as usize,
        services: services as usize,
        registry: registry as *const PluginRegistry as usize,
    });
    Ok(())
}

/// Remove the binding (called before the engine is dropped).
pub(crate) fn uninstall() {
    *lock() = None;
}

/// Resolve the binding for the C++ engine pointer a callback received.
pub(crate) fn resolve(engine: *mut Engine) -> Option<Resolved> {
    let slot = lock();
    slot.as_ref().and_then(|binding| {
        if binding.engine_raw == engine as usize {
            Some(Resolved {
                engine: binding.engine,
                services: binding.services,
                registry: binding.registry,
            })
        } else {
            None
        }
    })
}
