//! The L3 registration/dispatch context and its explicit L2 service handles.

use std::cell::{Cell, RefCell};
use std::ffi::c_char;
use std::marker::PhantomData;

use engine::{Scene, Storage};
use tjs::{
    NativeClassBuilder, NativeInstanceBuilder, NativeStaticMembers, RetainedValue, Tjs2Engine,
    Tjs2ValueId,
};

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
///
/// The scratch/cache fields are the L3 *dispatch* state the binding points
/// at; keeping them here (rather than in a second hidden global) preserves
/// the "exactly one process-wide binding" rule of doc 08 §3.1.
#[derive(Default)]
pub struct CompatServices {
    scene: Option<*mut Scene>,
    storage: Option<*mut Storage>,
    /// Scratch buffers for `Plugins.getList` (see `plugins_native`): the C++
    /// trampoline copies the string array immediately after the callback
    /// returns, so the buffers must outlive the call. Owned here rather than
    /// in a thread-local so the only process-wide state stays the documented
    /// binding.
    linked_array: RefCell<(Vec<*const c_char>, Vec<Vec<u8>>)>,
    /// The cached no-op closure the null-plugin `missing` handler installs as
    /// a property default. It is created once at install time and *duplicated*
    /// per access, so a member read never re-enters the VM.
    ///
    /// Stored as a raw id (rather than a `DetachedValue`) so dropping the host
    /// never dereferences the engine: the engine owns the C++ retention and
    /// frees it when it is destroyed.
    noop_closure: Cell<Tjs2ValueId>,
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

    /// The attached scene graph pointer, if any.
    ///
    /// This hands out the raw pointer rather than a `&Scene`: the services
    /// container does not track the lifetime of the L2 owner, so forming a
    /// shared reference here would be a safe aliasing hazard. Callers
    /// dereference under their own `unsafe` contract (see
    /// [`Self::attach_scene`]).
    pub fn scene(&self) -> Option<*mut Scene> {
        self.scene
    }

    /// The attached storage namespace pointer, if any.
    ///
    /// See [`Self::scene`] for why this returns a raw pointer.
    pub fn storage(&self) -> Option<*mut Storage> {
        self.storage
    }

    /// The mutable `Plugins.getList` scratch buffers.
    pub(crate) fn linked_array_scratch(
        &self,
    ) -> std::cell::RefMut<'_, (Vec<*const c_char>, Vec<Vec<u8>>)> {
        self.linked_array.borrow_mut()
    }

    /// Create and cache the no-op closure the null plugin hands back for
    /// absent member reads/calls. Idempotent; call once the engine is bound.
    pub(crate) fn ensure_noop_closure(&self, engine: &Tjs2Engine) {
        if !self.noop_closure.get().is_null() {
            return;
        }
        if let Ok(RetainedValue::Object(object)) =
            engine.eval_retained("(function(){})", "null_plugin")
        {
            let id = object.raw_id();
            // The engine owns the C++ retention and frees it at destruction;
            // deliberately leak the Rust handle so dropping the host can never
            // touch the engine.
            std::mem::forget(object);
            self.noop_closure.set(id);
        }
    }

    /// The cached no-op closure id, if [`Self::ensure_noop_closure`] created
    /// one.
    pub(crate) fn noop_closure_id(&self) -> Option<Tjs2ValueId> {
        let id = self.noop_closure.get();
        if id.is_null() { None } else { Some(id) }
    }

    /// Forget the cached no-op closure id. Does not touch the engine: the
    /// engine owns the C++ retention and frees it at destruction, so this is
    /// safe to call during teardown even if the engine is already gone.
    pub(crate) fn clear_noop_closure(&self) {
        self.noop_closure.set(std::ptr::null_mut());
    }
}

/// The context native plugins register against and callbacks dispatch
/// through.
///
/// It is deliberately **lifetime-free**: the C ABI hands native callbacks
/// only the raw `tjs2_engine*` and `tjs` has no user-data slot, so the
/// process-wide binding (see the `binding` module) resolves the raw engine
/// back to this context as plain addresses. `CompatCtx` therefore stores two
/// `usize`s — the address of the Rust [`Tjs2Engine`] and of the
/// [`CompatServices`] — and reconstructs references on demand.
///
/// It is **not** a safe value handle:
///
/// * it is `!Send + !Sync` (`PhantomData<*mut ()>`) because the VM is
///   single-threaded;
/// * it is not `Copy`/`Clone`, so it cannot be duplicated into an unbounded
///   lifetime;
/// * the raw accessors ([`Self::engine`], [`Self::services`],
///   [`Self::services_mut`]) are `unsafe`: the caller must uphold the
///   "bound and alive" contract. The safe registration helpers on this type
///   rely on that invariant.
pub struct CompatCtx {
    engine: usize,
    services: usize,
    /// `!Send + !Sync`: the VM must stay on one thread.
    _nosend: PhantomData<*mut ()>,
}

impl CompatCtx {
    /// Build a context from raw addresses.
    ///
    /// # Safety
    /// `engine` must be the address of a live [`Tjs2Engine`] and `services`
    /// the address of a live [`CompatServices`]; both must outlive the
    /// returned handle's use.
    pub(crate) unsafe fn from_raw(engine: usize, services: usize) -> Self {
        Self {
            engine,
            services,
            _nosend: PhantomData,
        }
    }

    /// The VM the compat surface is installed on.
    ///
    /// # Safety
    /// The binding must currently be installed and the engine alive.
    pub unsafe fn engine(&self) -> &Tjs2Engine {
        // SAFETY: the caller upholds the bound/alive contract above.
        unsafe { &*(self.engine as *const Tjs2Engine) }
    }

    /// The L2 services attached by the host.
    ///
    /// # Safety
    /// The binding must currently be installed and the services alive.
    pub unsafe fn services(&self) -> &CompatServices {
        // SAFETY: the caller upholds the bound/alive contract above.
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

    /// Register a static native class, mapping the `tjs` error string
    /// into a [`CompatError`].
    pub fn register_class(&self, builder: &NativeClassBuilder<'_>) -> Result<(), CompatError> {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        unsafe { self.engine() }
            .register_native_class(builder)
            .map_err(CompatError::Register)
    }

    /// Register an instance native class.
    pub fn register_instance(
        &self,
        builder: &NativeInstanceBuilder<'_>,
    ) -> Result<(), CompatError> {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        unsafe { self.engine() }
            .register_native_class_instance(builder)
            .map_err(CompatError::Register)
    }

    /// Attach static (class-level) members to an already-registered class.
    pub fn register_static_members(
        &self,
        members: &NativeStaticMembers<'_>,
    ) -> Result<(), CompatError> {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        unsafe { self.engine() }
            .register_native_static_members(members)
            .map_err(CompatError::Register)
    }
}
