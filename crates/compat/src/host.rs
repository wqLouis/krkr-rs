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

    /// Install the host image codec the `Bitmap` natives compose.
    ///
    /// `decode` backs `Bitmap.load`/`Bitmap.loadAsync`, `probe` backs
    /// `Bitmap.loadHeader` and `save` backs `Bitmap.save`. Only the L4 host
    /// links the L1 `media` crate; the integration tests install a synthetic
    /// codec here. A member whose closure is absent raises a typed error
    /// instead of fabricating a result.
    ///
    /// The three closures are owned by the host's dispatch state and live as
    /// long as the host does; they are called from the VM's single thread.
    pub fn set_image_codec(
        &self,
        decode: impl Fn(&[u8], Option<&str>) -> Result<api::media::DecodedImage, String> + 'static,
        probe: impl Fn(&[u8], Option<&str>) -> Result<api::media::ImageInfo, String> + 'static,
        save: impl Fn(&str, &str, u32, u32, &[u8]) -> Result<(), String> + 'static,
    ) {
        self.services()
            .state::<crate::resource::ResourceResolver>()
            .set_codec(decode, probe, save);
    }

    /// Install the host's system-font directories on the shared text engine.
    ///
    /// The directories are scanned lazily, the first time a face is resolved
    /// from the host; install them before the game's startup script runs so
    /// no face has been resolved yet.
    pub fn set_system_font_dirs(&self, dirs: Vec<std::path::PathBuf>) {
        self.services()
            .state::<crate::resource::ResourceResolver>()
            .engine
            .set_system_font_dirs(dirs);
    }

    /// The plugin registry.
    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    /// Whether the binding is currently installed.
    pub fn is_bound(&self) -> bool {
        self.bound
    }

    /// The number of live `MenuItem` nodes the engine retains.
    ///
    /// A host diagnostic for the native retention cycles: each live item's
    /// destroy callback removes its node, so a count that grows across
    /// create/drop cycles is a leak.
    pub fn menu_node_count(&self) -> usize {
        crate::natives::menu_item::node_count(self.services.as_ref())
    }

    /// The number of live `Timer` bindings the engine retains.
    pub fn timer_count(&self) -> usize {
        crate::natives::visual::timer_count(self.services.as_ref())
    }

    /// The number of live `WaveSoundBuffer` objects the engine retains.
    pub fn sound_object_count(&self) -> usize {
        crate::natives::sound::object_count(self.services.as_ref())
    }

    /// The number of live `AsyncTrigger` bindings the engine retains.
    pub fn async_trigger_count(&self) -> usize {
        crate::natives::async_trigger::trigger_count(self.services.as_ref())
    }

    /// Opens the per-native dispatch-timing window (the startup sequence).
    ///
    /// Call before the startup scripts run; pair with
    /// [`Self::end_native_timing`]. The counters live in the host's dispatch
    /// state and the window is closed again before the first frame, so the
    /// frame path is never timed.
    pub fn begin_native_timing(&self) {
        crate::natives::begin_native_timing(self.services.as_ref());
    }

    /// Completes the plugin work that needed an L2 service not yet attached
    /// when [`Self::install`] ran.
    ///
    /// The `xp3filter` plugin reads `xp3filter.tjs` through the storage while
    /// it registers; a composition root that attaches the storage after
    /// installing leaves that read pending until here. The composition root
    /// calls this before the startup scripts; a read failure is returned so
    /// the caller can stop instead of running unfiltered. Idempotent and a
    /// no-op once the work has run (including when registration already
    /// completed it).
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Engine`] when the game directory of the attached
    /// storage is not valid UTF-8, or when the deferred plugin read fails.
    /// The host is not installed: the call is a no-op that reports success.
    pub fn activate_deferred_plugins(&self) -> Result<(), CompatError> {
        if !self.bound {
            return Ok(());
        }
        // SAFETY: `bound` guarantees the stored engine/services addresses are
        // live.
        let engine = unsafe { &*(self.engine as *const Tjs2Engine) };
        // A host that attached the storage after `install` gets the stream
        // data directory here; re-applying it is idempotent.
        self.apply_stream_data_dir(engine)?;
        crate::natives::plugins::activate_deferred(engine, self.services.as_ref())
    }

    /// Points the VM's stream factories at the attached storage's project
    /// directory, so a relative save name resolves under the game path rather
    /// than at the process working directory. No-op when no storage is
    /// attached.
    fn apply_stream_data_dir(&self, engine: &Tjs2Engine) -> Result<(), CompatError> {
        let Some(storage) = self.services.storage() else {
            return Ok(());
        };
        // SAFETY: `attach_storage` requires the owner to outlive the binding.
        let app_dir = unsafe { &*storage }.app_dir();
        let Some(app_dir) = app_dir.to_str() else {
            return Err(CompatError::Engine(
                "stream data dir is not valid UTF-8".to_string(),
            ));
        };
        engine
            .set_data_dir(app_dir)
            .map_err(|error| CompatError::Engine(error.to_string()))
    }

    /// Closes the per-native dispatch-timing window and emits the one report
    /// line: the top members by total milliseconds with call counts, at `info`
    /// when a single native reached the documented threshold and at `debug`
    /// otherwise.
    pub fn end_native_timing(&self) {
        crate::natives::end_native_timing(self.services.as_ref());
    }

    /// A dispatch context handle for the bound VM.
    ///
    /// The handle borrows nothing: it carries the addresses of the bound
    /// engine and of the host's service set, both of which the host keeps
    /// alive until it is uninstalled.
    ///
    /// # Errors
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
    /// behind a `Box` (as the `app` crate's host and `vm`'s host do) and do
    /// not move the value afterwards.
    ///
    /// When a storage is already attached, its game directory becomes the
    /// VM's stream data directory, so a relative save name resolves there
    /// instead of at the process working directory. A host that attaches the
    /// storage afterwards gets the same setting from
    /// [`Self::activate_deferred_plugins`].
    ///
    /// The host keeps the engine alive for the whole binding: the engine must
    /// outlive the host, or the binding must be removed with [`Self::shutdown`]
    /// first.
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Register`] when the host is already installed or
    /// when a native class cannot be registered, and [`CompatError::Engine`]
    /// when the attached storage's game directory is not valid UTF-8. A
    /// failure leaves no binding installed.
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
        // Relative stream names resolve against the game directory before any
        // script can write one. The storage must already be attached; undo
        // the binding when the directory cannot be represented.
        if let Err(error) = self.apply_stream_data_dir(engine) {
            binding::uninstall();
            return Err(error);
        }
        self.engine = engine_addr;
        self.services_ptr = services_ptr;
        self.bound = true;
        Ok(())
    }

    /// Remove the binding (idempotent).
    ///
    /// Crate-internal: the public teardown is [`Self::shutdown`], which also
    /// drops the dispatch state while the engine is still alive. A host that
    /// wants a registry other than the built-in one builds it first and hands
    /// it to [`Self::with_registry`].
    pub(crate) fn uninstall(&mut self) {
        if self.bound {
            binding::uninstall();
            self.bound = false;
        }
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
/// the virtual-desktop geometry — with no L2 counterpart. L3 owns no platform
/// code, so the host installs one closure per fact through [`CompatHost`];
/// until a fact is installed its member raises a typed TJS error rather than
/// returning a fabricated value.
#[derive(Default)]
pub(crate) struct HostFacts {
    desktop_rect: Option<Box<DesktopRectFn>>,
}

/// `System.desktop*` provider signature; the tuple is `(left, top, width,
/// height)`.
type DesktopRectFn = dyn Fn() -> Result<(i32, i32, u32, u32), String>;

impl CompatHost {
    /// Install the `System.desktopLeft`/`desktopTop`/`desktopWidth`/
    /// `desktopHeight` provider. The tuple is `(left, top, width, height)`.
    pub fn set_desktop_rect(&self, f: impl Fn() -> Result<(i32, i32, u32, u32), String> + 'static) {
        self.services.state::<HostFacts>().desktop_rect = Some(Box::new(f));
    }
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
