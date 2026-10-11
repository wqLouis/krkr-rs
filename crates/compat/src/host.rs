//! The L3 host: owns the dispatch state and installs the process-wide
//! binding against a VM.
#![allow(unsafe_code)]

use tjs::Tjs2Engine;

use crate::binding;
use crate::ctx::CompatCtx;
use crate::{CompatError, CompatServices, PluginRegistry};

/// Owns the L3 dispatch state ([`CompatServices`] and [`PluginRegistry`]) and
/// Installs the single process-wide binding against a VM.
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

    /// Install the host-decoded video source: the container's
    /// [`VideoSpec`](api::media::VideoSpec) and the frames the host decoded.
    ///
    /// L3 owns no decoder, so `VideoOverlay.open` reads its metadata from here
    /// and the frame body presents these frames at the current position. A
    /// host that has not installed a source makes `open` raise a TJS error
    /// instead of fabricating metadata.
    pub fn set_video_source(
        &self,
        spec: api::media::VideoSpec,
        frames: Vec<api::media::RgbaFrame>,
    ) {
        crate::natives::video::set_source(&self.services, spec, frames);
    }

    /// Install the L1 clipboard backend the `Clipboard` native composes.
    ///
    /// L3 owns no window system, so the host installs the real clipboard
    /// operations here. Any `ClipboardBackend` is accepted; a host with three
    /// plain closures can pass a `(has_format, text, set_text)` tuple,
    /// which implements the trait. Without a backend every `Clipboard` member
    /// raises a typed TJS error instead of fabricating an empty value.
    pub fn set_clipboard_backend<B>(&self, backend: B)
    where
        B: crate::natives::clipboard::ClipboardBackend + 'static,
    {
        crate::natives::clipboard::set_backend(&self.services, Box::new(backend));
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

    /// Drop every value the native dispatch state retains, then remove the
    /// binding.
    ///
    /// The dispatch state holds values retained on the VM whose `Drop`
    /// releases the id on the engine, so this must run while the engine is
    /// still alive. The `vm` host calls it from its own `Drop` before the
    /// engine field is dropped; [`Self::uninstall`] alone leaves the state
    /// map in place until [`Self`] itself is dropped, which can be after the
    /// engine has been destroyed.
    pub fn shutdown(&mut self) {
        self.uninstall();
        // Replacing the box drops the old state map (and every retained value
        // it holds) now, while the engine is alive.
        *self.services = CompatServices::new();
    }
}

impl Drop for CompatHost {
    fn drop(&mut self) {
        self.uninstall();
    }
}

// ---------------------------------------------------------------------------
// Host-facts seam
// ---------------------------------------------------------------------------

/// Host-supplied platform facts the L3 natives cannot derive from the L2
/// services.
///
/// A handful of the reference `System` members describe genuine host facts —
/// the application-data directory and the virtual-desktop geometry — with no
/// L2 counterpart. L3 owns no platform code, so the host installs one closure
/// per fact through [`CompatHost`]; until a fact is installed its member raises
/// a typed TJS error rather than returning a fabricated value.
#[derive(Default)]
pub(crate) struct HostFacts {
    app_data_path: Option<Box<AppDataPathFn>>,
    desktop_rect: Option<Box<DesktopRectFn>>,
}

/// `System.appDataPath` provider signature.
type AppDataPathFn = dyn Fn() -> Result<String, String>;
/// `System.desktop*` provider signature; the tuple is `(left, top, width,
/// height)`.
type DesktopRectFn = dyn Fn() -> Result<(i32, i32, u32, u32), String>;

impl CompatHost {
    /// Install the `System.appDataPath` provider.
    pub fn set_app_data_path(&self, f: impl Fn() -> Result<String, String> + 'static) {
        self.services.state::<HostFacts>().app_data_path = Some(Box::new(f));
    }

    /// Install the `System.desktopLeft`/`desktopTop`/`desktopWidth`/
    /// `desktopHeight` provider. The tuple is `(left, top, width, height)`.
    pub fn set_desktop_rect(&self, f: impl Fn() -> Result<(i32, i32, u32, u32), String> + 'static) {
        self.services.state::<HostFacts>().desktop_rect = Some(Box::new(f));
    }
}

/// Call the installed `System.appDataPath` provider, if any. The dispatch-state
/// borrow is released before the result is returned.
pub(crate) fn host_app_data_path(services: &CompatServices) -> Option<Result<String, String>> {
    services
        .state::<HostFacts>()
        .app_data_path
        .as_ref()
        .map(|f| f())
}

/// Call the installed desktop-rectangle provider, if any.
pub(crate) fn host_desktop_rect(
    services: &CompatServices,
) -> Option<Result<(i32, i32, u32, u32), String>> {
    services
        .state::<HostFacts>()
        .desktop_rect
        .as_ref()
        .map(|f| f())
}
