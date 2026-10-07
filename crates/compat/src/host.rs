//! The L3 host: owns the dispatch state and installs the process-wide
//! binding against a VM.

use tjs::Tjs2Engine;

use crate::binding;
use crate::ctx::CompatCtx;
use crate::{CompatError, CompatServices, PluginRegistry};

/// Owns the L3 dispatch state ([`CompatServices`] and [`PluginRegistry`]) and
/// installs the single process-wide binding (doc 08 §3.1) against a VM.
///
/// The two state values are boxed so their addresses stay stable when the
/// host is moved; the binding stores raw addresses and therefore relies on
/// that stability. [`CompatHost::install`] registers the built-in compat
/// classes and then binds the engine; [`Drop`] always removes the binding.
pub struct CompatHost {
    services: Box<CompatServices>,
    registry: Box<PluginRegistry>,
    engine: usize,
    services_ptr: usize,
    bound: bool,
}

impl Default for CompatHost {
    fn default() -> Self {
        Self::new()
    }
}

impl CompatHost {
    /// A host with an empty registry.
    pub fn new() -> Self {
        Self::with_registry(PluginRegistry::builtin())
    }

    /// A host with the given registry.
    pub fn with_registry(registry: PluginRegistry) -> Self {
        Self {
            services: Box::new(CompatServices::new()),
            registry: Box::new(registry),
            engine: 0,
            services_ptr: 0,
            bound: false,
        }
    }

    /// The L2 services the natives compose.
    pub fn services(&self) -> &CompatServices {
        &self.services
    }

    /// The L2 services, mutably (attach scene/storage before installing).
    pub fn services_mut(&mut self) -> &mut CompatServices {
        &mut self.services
    }

    /// The plugin registry.
    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    /// The plugin registry, mutably (add built-ins before installing).
    pub fn registry_mut(&mut self) -> &mut PluginRegistry {
        &mut self.registry
    }

    /// Whether the binding is currently installed.
    pub fn is_bound(&self) -> bool {
        self.bound
    }

    /// A dispatch context handle for the bound VM.
    ///
    /// Returns [`CompatError::Engine`] before [`Self::install`], so the
    /// handle can never dereference the null addresses of an unbound host.
    pub fn ctx(&self) -> Result<CompatCtx, CompatError> {
        if !self.bound {
            return Err(CompatError::Engine("compat host is not installed".into()));
        }
        // SAFETY: `bound` guarantees both stored addresses are live.
        Ok(unsafe { CompatCtx::from_raw(self.engine, self.services_ptr) })
    }

    /// Register the built-in compat classes and install the binding.
    ///
    /// Fails if a binding is already installed (there is exactly one VM).
    ///
    /// The binding stores the **address** of `engine`, so `engine` must stay
    /// at a stable address for as long as the binding is installed: keep it
    /// behind a `Box` (as [`crate::Host`] and `vm`'s host do) and do not move
    /// the value afterwards.
    pub fn install(&mut self, engine: &Tjs2Engine) -> Result<(), CompatError> {
        if self.bound {
            return Err(CompatError::Register(
                "compat host is already installed".into(),
            ));
        }
        let engine_addr = engine as *const Tjs2Engine as usize;
        let services_ptr = &mut *self.services as *mut CompatServices as usize;
        // SAFETY: the engine and the boxed services outlive this scope (and
        // the binding stores their stable addresses only after success).
        let mut ctx = unsafe { CompatCtx::from_raw(engine_addr, services_ptr) };
        // Register the native surface first, then bind: registration does
        // not need the binding, and a failure leaves no half-installed table.
        crate::install(&mut ctx)?;
        binding::install(engine, services_ptr as *mut CompatServices, &self.registry)?;
        // Create the null-plugin no-op closure once, before any native
        // callback can run, so the `missing` handler never re-enters the VM.
        self.services.ensure_noop_closure(engine);
        self.engine = engine_addr;
        self.services_ptr = services_ptr;
        self.bound = true;
        Ok(())
    }

    /// Remove the binding (idempotent).
    pub fn uninstall(&mut self) {
        if self.bound {
            binding::uninstall();
            self.bound = false;
        }
        // Drop the per-install no-op cache id so a later re-install against a
        // different engine creates a fresh one.
        self.services.clear_noop_closure();
    }
}

impl Drop for CompatHost {
    fn drop(&mut self) {
        self.uninstall();
    }
}
