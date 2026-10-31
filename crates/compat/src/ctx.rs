//! The L3 registration/dispatch context and its explicit L2 service handles.
#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::ffi::c_char;
use std::marker::PhantomData;

use crate::engine_api::InputState;
use crate::engine_api::TimerState;
use crate::engine_api::{DecodePool, Scene, Storage};
use crate::engine_api::{FrameClock, FrameTimeSource};
use tjs::{
    NativeClassBuilder, NativeInstanceBuilder, NativeInstanceMembers, NativeStaticMembers,
    RetainedValue, Tjs2Engine, Tjs2ValueId,
};

use crate::CompatError;

/// One per-family dispatch-state cell. The map's `RefCell` borrow is released
/// before the cell is borrowed, so two different families' states can be
/// borrowed at once (nested access). The box keeps each cell's address stable
/// for the whole `CompatServices` lifetime.
type StateCell = RefCell<Box<dyn std::any::Any>>;
/// The per-family dispatch-state map, keyed by [`std::any::TypeId`].
type StateMap = RefCell<crate::collections::Map<std::any::TypeId, Box<StateCell>>>;

/// Explicit handles to the L2 services the native classes compose.
///
/// L3 never reaches into L2 state through a global: the host attaches the
/// services it wants exposed and the natives read them through
/// [`CompatCtx::services`]. The set starts minimal — registration itself
/// needs only the VM.
///
/// The handles are raw pointers rather than borrows because the single
/// process-wide binding is lifetime-free: the host owns the
/// services for the lifetime of the VM and the binding only stores addresses.
/// Callers must keep the attached values alive as long as the binding.
///
/// The scratch/cache fields are the L3 *dispatch* state the binding points
/// at; keeping them here (rather than in a second hidden global) preserves
/// the "exactly one process-wide binding" rule.
#[derive(Default)]
pub struct CompatServices {
    scene: Option<*mut Scene>,
    storage: Option<*mut Storage>,
    input: Option<*mut InputState>,
    timer: Option<*mut TimerState>,
    /// The L2 frame clock the timers, the sound poll and the transitions read.
    ///
    /// `None` when the host attached no clock: those steps raise instead of
    /// pacing themselves against no time at all.
    clock: Option<*mut FrameClock>,
    /// The clock source the script's tick count reads on demand.
    ///
    /// It is the same source the engine paces the frames from, so the script's
    /// clock and the frame's clock are one base read at two moments. `None`
    /// when the host attached none: the tick count then reads the frame clock,
    /// which is the whole time such a host has.
    frame_time: Option<*const (dyn FrameTimeSource + 'static)>,
    /// The L2 image decode pool the visual natives submit decodes to.
    ///
    /// `None` when the host attached no pool: the loads then decode
    /// synchronously, exactly as they did before the pool existed.
    decode_pool: Option<*mut DecodePool>,
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
    /// Per-family L3 dispatch state, keyed by type. Natives that need state
    /// that outlives a callback (the `System` command line, the `Debug` log
    /// ring, timer/sound/input registries, ...) stash it here instead of in a
    /// process global, so the single documented binding stays the only
    /// process-wide state.
    states: StateMap,
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

    /// Attach the L2 generic input state the input natives will read.
    ///
    /// # Safety
    /// `input` must stay valid for as long as the binding is installed (the
    /// VM's lifetime).
    pub unsafe fn attach_input(&mut self, input: *mut InputState) {
        self.input = Some(input);
    }

    /// The attached L2 input state pointer, if any.
    ///
    /// See [`Self::scene`] for why this returns a raw pointer.
    pub fn input(&self) -> Option<*mut InputState> {
        self.input
    }

    /// Attach the L2 timer registry the `Timer` native will compose.
    ///
    /// # Safety
    /// `timer` must stay valid for as long as the binding is installed.
    pub unsafe fn attach_timer(&mut self, timer: *mut TimerState) {
        self.timer = Some(timer);
    }

    /// The attached L2 timer registry pointer, if any.
    ///
    /// See [`Self::scene`] for why this returns a raw pointer.
    pub fn timer(&self) -> Option<*mut TimerState> {
        self.timer
    }

    /// Attach the L2 frame clock the `System` native reads.
    ///
    /// # Safety
    /// `clock` must stay valid for as long as the binding is installed.
    pub unsafe fn attach_clock(&mut self, clock: *mut FrameClock) {
        self.clock = Some(clock);
    }

    /// The attached L2 frame clock pointer, if any.
    ///
    /// See [`Self::scene`] for why this returns a raw pointer.
    pub fn clock(&self) -> Option<*mut FrameClock> {
        self.clock
    }

    /// Attach the clock source the composition root paces the frames from.
    ///
    /// This is the same source the engine reads the frame's elapsed time from;
    /// the `System.getTickCount` native reads it on demand so the script's
    /// clock moves inside a frame, where the frame clock only moves at a frame
    /// boundary.
    ///
    /// # Safety
    /// `source` must stay valid for as long as the binding is installed, and it
    /// must be the source the host is stepped with: a different one would make
    /// the script's time and the frame's time two bases.
    pub unsafe fn attach_frame_time(&mut self, source: *const (dyn FrameTimeSource + 'static)) {
        if source.is_null() {
            self.frame_time = None;
        } else {
            self.frame_time = Some(source);
        }
    }

    /// The attached frame-time source, if any.
    ///
    /// See [`Self::scene`] for why this returns a raw pointer.
    pub fn frame_time(&self) -> Option<*const (dyn FrameTimeSource + 'static)> {
        self.frame_time
    }

    /// Attach the L2 image decode pool the visual natives submit decodes to.
    ///
    /// # Safety
    /// `pool` must stay valid for as long as the binding is installed, and
    /// the pool must not be replaced while the binding is installed: the
    /// natives hold the address of the pool the host attached.
    pub unsafe fn attach_decode_pool(&mut self, pool: *mut DecodePool) {
        self.decode_pool = if pool.is_null() { None } else { Some(pool) };
    }

    /// The attached L2 image decode pool pointer, if any.
    ///
    /// See [`Self::scene`] for why this returns a raw pointer.
    pub fn decode_pool(&self) -> Option<*mut DecodePool> {
        self.decode_pool
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
        self.noop_closure.set(tjs::Tjs2ValueId::null());
    }

    /// Borrow (creating if absent) the per-family dispatch state of type `T`.
    ///
    /// This is the extension point that keeps native-family state out of
    /// process globals: a family defines its own state type, defaults it, and
    /// reads it back here. `T` is keyed by [`std::any::TypeId`].
    pub fn state<T: Default + 'static>(&self) -> std::cell::RefMut<'_, T> {
        use std::any::{Any, TypeId};
        use std::cell::{RefCell, RefMut};
        // Ensure the entry exists, then take a stable pointer to its cell. Each
        // entry is boxed and never removed or replaced, so its address stays
        // valid for the whole `&self` lifetime and the map borrow can be
        // released before the entry is borrowed. That lets one state be borrowed
        // while another already is (nested access); the previous single-`RefCell`
        // map panicked with `RefCell already borrowed`.
        let cell: *const RefCell<Box<dyn Any>> = {
            let mut map = self.states.borrow_mut();
            let entry = map
                .entry(TypeId::of::<T>())
                .or_insert_with(|| Box::new(RefCell::new(Box::new(T::default()) as Box<dyn Any>)));
            &**entry
        };
        // SAFETY: entries are boxed (stable address) and never removed or
        // replaced, so `cell` remains valid while `&self` is alive. The borrow
        // flags are still enforced by the entry's `RefCell`, so aliasing `&mut T`
        // of the same type panics exactly as it must.
        let cell: &RefCell<Box<dyn Any>> = unsafe { &*cell };
        RefMut::map(cell.borrow_mut(), |any| {
            any.downcast_mut::<T>().expect("state is keyed by TypeId")
        })
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
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Register`] when the VM rejects the class — a
    /// duplicate class name, for instance. The registration is atomic: a
    /// rejected class leaves nothing behind.
    pub fn register_class(&self, builder: &NativeClassBuilder<'_>) -> Result<(), CompatError> {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        unsafe { self.engine() }
            .register_native_class(builder)
            .map_err(CompatError::Register)
    }

    /// Register an instance native class.
    ///
    /// # Where the members land (override semantics)
    ///
    /// The members are registered **on the class** (`RegisterNCM`, the
    /// reference `TJS_BEGIN_NATIVE_MEMBERS` shape) — this call never copies
    /// them onto live objects. Two instance shapes then coexist:
    ///
    /// * `new Timer()` (the native class creates the object): the class's
    ///   `FuncCall` copies every non-static member onto the new object as an
    ///   own property (reference `tjsNative.cpp:305-375`).
    /// * `class OnceTimer extends Timer { function onTimer() {...} }` (a
    ///   script subclass creates the object): object creation registers the
    ///   **script** members as own properties (reference `VM_REGMEMBER`,
    ///   `tjsInterCodeGen.cpp:704-715`) *after* the extends chain is wired,
    ///   while the native members stay reachable through the class chain.
    ///
    /// Either way a script-defined member shadows the native default. That
    /// is why every dispatch seam in this crate resolves the member **on the
    /// object** (`Tjs2Engine::call_member`, e.g. `Timer.onTimer`,
    /// `AsyncTrigger.onFire`, `MenuItem.onClick`,
    /// `Layer.onTransitionCompleted`) instead of invoking our native
    /// callback directly — verified by `tests/extend_dispatch.rs`.
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Register`] when the VM rejects the class — a
    /// duplicate class name, for instance.
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
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Register`] when the VM rejects the members or
    /// the class does not exist.
    pub fn register_static_members(
        &self,
        members: &NativeStaticMembers<'_>,
    ) -> Result<(), CompatError> {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        unsafe { self.engine() }
            .register_native_static_members(members)
            .map_err(CompatError::Register)
    }

    /// Attach instance members (no native payload) to an already-existing
    /// class, including a TJS2 builtin such as `Array` / `Dictionary`
    /// (reference `NCB_ATTACH_CLASS` / `NCB_ATTACH_FUNCTION`).
    ///
    /// Like [`Self::register_instance`], the members land on the class (or,
    /// for a builtin, the class object looked up on the VM global), so a
    /// script-defined member of an instance or subclass still shadows them.
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Register`] when the VM rejects the members or
    /// the class does not exist.
    pub fn register_instance_members(
        &self,
        members: &NativeInstanceMembers<'_>,
    ) -> Result<(), CompatError> {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        unsafe { self.engine() }
            .register_native_instance_members(members)
            .map_err(CompatError::Register)
    }

    /// Delete a named member from a retained object, mapping the `tjs` error
    /// string into a [`CompatError`].
    ///
    /// Registration publishes class-name globals but has no way to undo a
    /// member write; this is the removal path for the reference's
    /// `NCB_REGISTER_SUBCLASS_DELAY(X)` shape, whose class is published only
    /// for the duration of the delay (reference
    /// `cpp/core/plugin/ncbind.hpp:1963-1970`). An absent member and a
    /// non-object id are reported as [`CompatError::Engine`].
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Engine`] when the member is absent or the id
    /// does not name an object.
    pub fn delete_member(&self, id: Tjs2ValueId, membername: &str) -> Result<(), CompatError> {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        unsafe { self.engine() }
            .delete_member(id, membername)
            .map_err(CompatError::Engine)
    }
}
