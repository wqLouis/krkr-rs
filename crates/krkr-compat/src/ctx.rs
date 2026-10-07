//! The L3 registration/dispatch context and its explicit L2 service handles.

use krkr_engine::{Scene, Storage};
use tjs2_sys::{NativeClassBuilder, NativeInstanceBuilder, NativeStaticMembers, Tjs2Engine};

use crate::CompatError;

/// Explicit handles to the L2 services the native classes compose.
///
/// L3 never reaches into L2 state through a global: the host attaches the
/// services it wants exposed and the natives read them through
/// [`CompatCtx::services`]. The set starts minimal — registration itself
/// needs only the VM — and grows as the `tvp-*` families migrate (doc 08 §7).
///
/// The handles are raw pointers rather than borrows because the single
/// process-wide binding (doc 08 §3.1) is lifetime-free: the host owns the
/// services for the lifetime of the VM and the binding only stores addresses.
/// Callers must keep the attached values alive as long as the binding.
#[derive(Default)]
pub struct CompatServices {
    scene: Option<*mut Scene>,
    storage: Option<*mut Storage>,
}

impl CompatServices {
    /// Create an empty service set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the L2 scene graph (state) the visual natives will compose.
    ///
    /// # Safety
    /// `scene` must stay valid for as long as the binding is installed (the
    /// VM's lifetime).
    pub unsafe fn attach_scene(&mut self, scene: *mut Scene) {
        self.scene = Some(scene);
    }

    /// Attach the L2 storage namespace the `Storages` native will compose.
    ///
    /// # Safety
    /// `storage` must stay valid for as long as the binding is installed.
    pub unsafe fn attach_storage(&mut self, storage: *mut Storage) {
        self.storage = Some(storage);
    }

    /// The attached scene graph, if any.
    ///
    /// # Safety
    /// The scene must still be alive (see [`Self::attach_scene`]).
    pub unsafe fn scene(&self) -> Option<&Scene> {
        // SAFETY: the caller upholds the lifetime contract of attach_scene.
        self.scene.map(|p| unsafe { &*p })
    }

    /// The attached storage namespace, if any.
    ///
    /// # Safety
    /// The storage must still be alive (see [`Self::attach_storage`]).
    pub unsafe fn storage(&self) -> Option<&Storage> {
        // SAFETY: the caller upholds the lifetime contract of attach_storage.
        self.storage.map(|p| unsafe { &*p })
    }
}

/// The context native plugins register against and callbacks dispatch
/// through.
///
/// It is deliberately **lifetime-free**: the C ABI hands native callbacks
/// only the raw `tjs2_engine*` and `tjs2-sys` has no user-data slot, so the
/// process-wide binding (see the `binding` module) resolves the raw engine
/// back to this context as plain addresses. `CompatCtx` therefore stores two
/// `usize`s — the address of the Rust [`Tjs2Engine`] and of the
/// [`CompatServices`] — and reconstructs references on demand. It is a copy
/// handle, not an owner.
#[derive(Clone, Copy)]
pub struct CompatCtx {
    engine: usize,
    services: usize,
}

impl CompatCtx {
    /// Build a context from raw addresses.
    ///
    /// # Safety
    /// `engine` must be the address of a live [`Tjs2Engine`] and `services`
    /// the address of a live [`CompatServices`]; both must outlive the
    /// returned handle's use.
    pub(crate) fn from_raw(engine: usize, services: usize) -> Self {
        Self { engine, services }
    }

    /// The VM the compat surface is installed on.
    pub fn engine(&self) -> &Tjs2Engine {
        // SAFETY: the binding only ever stores the address of the live host
        // engine (doc 08 §3.1).
        unsafe { &*(self.engine as *const Tjs2Engine) }
    }

    /// The L2 services attached by the host.
    pub fn services(&self) -> &CompatServices {
        // SAFETY: the binding only ever stores the address of the live
        // `CompatServices` owned by the host.
        unsafe { &*(self.services as *const CompatServices) }
    }

    /// The L2 services, mutably.
    ///
    /// # Safety
    /// The caller must not hold any other reference derived from the same
    /// services for the duration of the returned borrow.
    pub unsafe fn services_mut(&mut self) -> &mut CompatServices {
        // SAFETY: the caller upholds the exclusivity contract above.
        unsafe { &mut *(self.services as *mut CompatServices) }
    }

    /// Register a static native class, mapping the `tjs2-sys` error string
    /// into a [`CompatError`].
    pub fn register_class(&self, builder: &NativeClassBuilder<'_>) -> Result<(), CompatError> {
        self.engine()
            .register_native_class(builder)
            .map_err(CompatError::Register)
    }

    /// Register an instance native class.
    pub fn register_instance(
        &self,
        builder: &NativeInstanceBuilder<'_>,
    ) -> Result<(), CompatError> {
        self.engine()
            .register_native_class_instance(builder)
            .map_err(CompatError::Register)
    }

    /// Attach static (class-level) members to an already-registered class.
    pub fn register_static_members(
        &self,
        members: &NativeStaticMembers<'_>,
    ) -> Result<(), CompatError> {
        self.engine()
            .register_native_static_members(members)
            .map_err(CompatError::Register)
    }
}
