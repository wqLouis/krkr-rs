//! The dispatch adapter over an object that lives in the engine.
//!
//! A bridge: a value the Rust executor manipulates may be an object the
//! engine owns, so the value is reached through the engine's interface and
//! seen as a dispatch object. Each entry is one call across that interface
//! plus the value marshalling, and a shape the interface cannot carry is a
//! typed refusal that names it.
//!
//! # The two directions of the crossing
//!
//! A script value crosses **into** the engine as a [`TjsValue`]: the scalar
//! kinds as themselves, and an object as the engine's retained id. The id is
//! taken from the object itself — [`Dispatch::engine_id`] — never from its
//! address, and a duplicate of it is made for the call because the engine
//! consumes the retention it is handed. An object the Rust object model owns
//! answers no engine id, so the engine cannot hold it and it stays in the store
//! of the scope its object belongs to (see [`unaddressable`]); a context
//! object's store is keyed by the context's retained id in the scope's store.
//!
//! An answer crosses **out** of the engine as a [`Variant`]: an object the
//! call retained becomes a [`CHandle`], so the next entry reaches the same
//! object; `null` is the null closure, and a byte string becomes the
//! variant's octet.
//!
//! # The global scope
//!
//! The object a script's top-level scope is — its `this`, and the object a
//! `var` or a `function` at the top level is declared on — is the scope's
//! [`GlobalObject`], not a handle over the engine's global object. That object
//! owns the scope's members and class objects and asks the engine's global
//! object as one source for a name it does not hold; the resolution order is
//! written once, there. A handle's store is then only what its own object
//! cannot give the engine, and [`scope_global`] is where the scope's object is
//! read from.
//!
//! # What remains limited
//!
//! An ordered member walk is not served: [`Dispatch::enum_members`] answers
//! the safe wrapper's sorted, de-duplicated names and gives every member zero
//! flags. A call carries the receiver the executor computed when it is an
//! engine object; a receiver the Rust object model owns has no retained id,
//! so the call is refused rather than run on another object.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::{Rc, Weak};

use crate::vm::runtime::global::GlobalObject;
use crate::vm::runtime::number::Number;
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::octet::Octet;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant};
use crate::{CallAnswer, DetachedValue, MemberShape, Tjs2Engine, Tjs2ValueId, TjsValue};

/// The values one scope keeps for the objects the engine cannot hold them on.
///
/// The engine cannot hold a value of the Rust object model, so an object's
/// store keeps those values here. Scalars and engine objects still cross to the
/// engine; only an object the engine cannot address stays in a store. The root
/// store of a scope also owns the scope's [`GlobalObject`] and the table that
/// gives a Rust object a stable token, so a later answer for the same engine
/// object finds the Rust object it stands for.
pub struct GlobalMembers {
    members: RefCell<BTreeMap<TjsString, Variant>>,
    log: Option<LogSink>,
    parent: Option<Weak<GlobalMembers>>,
    children: RefCell<HashMap<Tjs2ValueId, SharedGlobals>>,
    host: RefCell<Option<Rc<dyn Dispatch>>>,
    hosts: RefCell<HashMap<i64, Rc<dyn Dispatch>>>,
    next_host: Cell<i64>,
    /// The global object of this scope, once its first script call made it.
    global: RefCell<Option<Rc<GlobalObject>>>,
}

/// What a refused engine call reports to.
pub type LogSink = Rc<dyn Fn(&str)>;

/// The hidden member an engine anchor carries so a later handle for the same
/// object finds the Rust object it stands for.
const HOST_TOKEN: &str = "__krkr_rust_host";

impl GlobalMembers {
    /// An empty store with no log sink.
    pub fn new() -> Self {
        Self {
            members: RefCell::new(BTreeMap::new()),
            log: None,
            parent: None,
            children: RefCell::new(HashMap::new()),
            host: RefCell::new(None),
            hosts: RefCell::new(HashMap::new()),
            next_host: Cell::new(1),
            global: RefCell::new(None),
        }
    }

    /// A store for one object in `root`'s scope, published through the root.
    fn detached_to(root: &SharedGlobals) -> Self {
        Self {
            members: RefCell::new(BTreeMap::new()),
            log: root.log.clone(),
            parent: Some(Rc::downgrade(root)),
            children: RefCell::new(HashMap::new()),
            host: RefCell::new(None),
            hosts: RefCell::new(HashMap::new()),
            next_host: Cell::new(1),
            global: RefCell::new(None),
        }
    }

    /// The same store, reporting a refused engine call to `log`.
    pub fn with_log(mut self, log: impl Fn(&str) + 'static) -> Self {
        self.log = Some(Rc::new(log));
        self
    }

    /// The store's log sink.
    fn log(&self) -> Option<LogSink> {
        self.log.clone().or_else(|| {
            self.parent
                .as_ref()
                .and_then(Weak::upgrade)
                .and_then(|p| p.log())
        })
    }

    /// The object whose members this store stands in front of, when it is the
    /// store for a Rust object that crossed into the engine.
    fn host(&self) -> Option<Rc<dyn Dispatch>> {
        self.host.borrow().clone()
    }

    /// Makes `object` the object this store stands in front of.
    fn set_host(&self, object: Rc<dyn Dispatch>) {
        *self.host.borrow_mut() = Some(object);
    }

    /// Gives `object` a stable token an engine anchor can carry.
    fn register_host(&self, object: Rc<dyn Dispatch>) -> i64 {
        let token = self.next_host.get();
        self.next_host.set(token.wrapping_add(1));
        self.hosts.borrow_mut().insert(token, object);
        token
    }

    /// The object a token names, while it is live.
    fn host_by_token(&self, token: i64) -> Option<Rc<dyn Dispatch>> {
        self.hosts.borrow().get(&token).cloned()
    }

    /// Whether a Rust object has crossed into this scope.
    fn has_hosts(&self) -> bool {
        !self.hosts.borrow().is_empty()
    }

    /// The member the store holds, if any.
    fn get(&self, name: &TjsString) -> Option<Variant> {
        self.members.borrow().get(name).cloned()
    }

    /// Stores a member the engine cannot hold.
    fn insert(&self, name: TjsString, value: Variant) {
        self.members.borrow_mut().insert(name, value);
    }

    /// Removes a stored member.
    fn remove(&self, name: &TjsString) -> Option<Variant> {
        self.members.borrow_mut().remove(name)
    }

    /// The stored members, in name order.
    fn entries(&self) -> Vec<(TjsString, Variant)> {
        self.members
            .borrow()
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}

impl Default for GlobalMembers {
    fn default() -> Self {
        Self::new()
    }
}

/// The shared store a global handle uses.
pub type SharedGlobals = Rc<GlobalMembers>;

/// A fresh global-member store.
pub fn global_members() -> SharedGlobals {
    Rc::new(GlobalMembers::new())
}

/// The root of a store's scope.
fn root_of(store: &SharedGlobals) -> SharedGlobals {
    let mut current = Rc::clone(store);
    loop {
        let parent = current.parent.as_ref().and_then(Weak::upgrade);
        match parent {
            Some(parent) => current = parent,
            None => return current,
        }
    }
}

/// The store one object in `root`'s scope has, creating it when this is the
/// first use.
fn child_of(root: &SharedGlobals, id: Tjs2ValueId) -> SharedGlobals {
    let root = root_of(root);
    if let Some(store) = root.children.borrow().get(&id) {
        return Rc::clone(store);
    }
    let store = Rc::new(GlobalMembers::detached_to(&root));
    root.children.borrow_mut().insert(id, Rc::clone(&store));
    store
}

/// The store one object already has, when it has one.
fn store_for(root: &SharedGlobals, id: Tjs2ValueId) -> Option<SharedGlobals> {
    root_of(root).children.borrow().get(&id).cloned()
}

/// The store a context object keeps within `globals`'s scope.
pub fn context_members(globals: &SharedGlobals, id: Tjs2ValueId) -> SharedGlobals {
    child_of(globals, id)
}

/// The global object the scope's scripts run against, when it has one.
///
/// The object's members are the scope's, so the answer is the same object for as
/// long as the scope lives.
pub fn scope_global(scope: &SharedGlobals) -> Option<Rc<GlobalObject>> {
    let root = root_of(scope);
    root.global.borrow().clone()
}

/// Makes `value`, the engine's global object, the one source behind the scope's
/// global object, and answers that object.
///
/// A scope has one global object: a scope that already has one keeps it, and
/// `value` is released, so a member a script declared in an earlier call is
/// still there. When the scope has none, the object is made with `value` behind
/// it and kept by the scope.
///
/// # Errors
///
/// [`ObjectError::InvalidObject`] when the engine cannot answer for `value`:
/// the class-name read or the validity question failed.
pub fn global_object(
    engine: &Rc<Tjs2Engine>,
    scope: &SharedGlobals,
    value: DetachedValue,
) -> Result<Rc<GlobalObject>, ObjectError> {
    let root = root_of(scope);
    let existing = root.global.borrow().clone();
    if let Some(global) = existing {
        return Ok(global);
    }
    let store = Rc::new(GlobalMembers::new());
    let fallback = CHandle::with_store(Rc::clone(engine), value, store, &root, false)?;
    let global = Rc::new(GlobalObject::over(Some(Rc::new(fallback))));
    root.global.borrow_mut().replace(Rc::clone(&global));
    Ok(global)
}

/// Whether a value has to stay on the Rust side of the crossing.
///
/// The engine addresses a value it can hold: the scalar kinds, the byte
/// strings, the null object, and an object that carries an engine handle. An
/// object of the Rust object model carries none, and an engine anchor's value
/// is the Rust object behind it, so the engine cannot be handed such a value and
/// the scope that owns the object keeps it instead. This is the one rule a write
/// follows, whichever object the write is made on.
pub fn unaddressable(value: &Variant) -> bool {
    match value {
        Variant::Object(closure) => match &closure.object {
            None => false,
            Some(object) => object.engine_id().is_none() || object.is_host_proxy(),
        },
        Variant::Void
        | Variant::Integer(_)
        | Variant::Real(_)
        | Variant::String(_)
        | Variant::Octet(_) => false,
    }
}

/// An engine object, reached through its retained id.
///
/// The handle owns one retention, shared by its clones and released when the
/// last clone drops, so it may be cloned into any number of closures.
#[derive(Clone)]
pub struct CHandle {
    /// The retention that keeps the object alive.
    ///
    /// It is declared before the engine so that it drops first: the retention
    /// releases through the engine, and the engine has to be alive then.
    retained: Rc<DetachedValue>,
    /// The engine the object belongs to.
    engine: Rc<Tjs2Engine>,
    /// The class names the interface answered for the object.
    class_names: Rc<RefCell<Vec<TjsString>>>,
    /// Whether the object was usable, from the last interface answer.
    valid: Rc<Cell<bool>>,
    /// The members the engine cannot hold for this object.
    globals: RefCell<SharedGlobals>,
    /// The scope the object belongs to; the stores of its objects are keyed
    /// there.
    ///
    /// The scope owns its global object, and that object owns the handle that
    /// stands for the engine's global object, so this link is weak: a strong
    /// one would keep the scope and the object alive for each other.
    root: Weak<GlobalMembers>,
    /// The Rust object this handle stands in front of, when it is a proxy for
    /// one that crossed into the engine.
    host: Option<Rc<dyn Dispatch>>,
    /// The sink a refused engine call reports to.
    log: Option<LogSink>,
}

impl std::fmt::Debug for CHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CHandle")
            .field("valid", &self.valid.get())
            .finish()
    }
}

impl CHandle {
    /// The adapter over `value`, whose object belongs to `engine`.
    ///
    /// # Errors
    ///
    /// The id is not an object the engine can answer for: the class-name read
    /// or the validity question failed.
    pub fn new(engine: Rc<Tjs2Engine>, value: DetachedValue) -> Result<Self, ObjectError> {
        let globals = Rc::new(GlobalMembers::new());
        let root = Rc::clone(&globals);
        Self::with_store(engine, value, globals, &root, true)
    }

    /// The global object over `value`, the engine's global object, in
    /// `globals`'s scope.
    ///
    /// The object owns its members and class objects, and `value` is the one
    /// source behind it for a name the object does not hold. It is the object a
    /// caller that reads the global scope through the handle builds; the object
    /// a scope's scripts run against is the scope's own ([`scope_global`]), and
    /// this constructor does not replace it.
    ///
    /// # Errors
    ///
    /// The id is not an object the engine can answer for: the class-name read
    /// or the validity question failed.
    pub fn with_members(
        engine: Rc<Tjs2Engine>,
        value: DetachedValue,
        globals: SharedGlobals,
    ) -> Result<GlobalObject, ObjectError> {
        let root = root_of(&globals);
        let store = Rc::new(GlobalMembers::new());
        let fallback = Self::with_store(engine, value, store, &root, false)?;
        Ok(GlobalObject::over(Some(Rc::new(fallback))))
    }

    /// The adapter over `value`, carrying a non-global object's own members.
    ///
    /// # Errors
    ///
    /// The id is not an object the engine can answer for: the class-name read
    /// or the validity question failed.
    pub fn with_context(
        engine: Rc<Tjs2Engine>,
        value: DetachedValue,
        globals: SharedGlobals,
    ) -> Result<Self, ObjectError> {
        let root = root_of(&globals);
        Self::with_store(engine, value, globals, &root, true)
    }

    /// The adapter over `value`, taking the store its object already has in
    /// `root`'s scope.
    fn with_root(
        engine: Rc<Tjs2Engine>,
        value: DetachedValue,
        root: SharedGlobals,
    ) -> Result<Self, ObjectError> {
        let id = value.raw_id();
        let globals =
            store_for(&root, id).unwrap_or_else(|| Rc::new(GlobalMembers::detached_to(&root)));
        Self::with_store(engine, value, globals, &root, true)
    }

    /// The adapter over `value` with an explicit store.
    ///
    /// `anchor` says whether the object may be a Rust object anchored in the
    /// scope; the handle over the engine's global object is not one.
    fn with_store(
        engine: Rc<Tjs2Engine>,
        value: DetachedValue,
        mut globals: SharedGlobals,
        root: &SharedGlobals,
        anchor: bool,
    ) -> Result<Self, ObjectError> {
        let id = value.raw_id();
        let names = engine
            .class_names(id)
            .map_err(|_| ObjectError::InvalidObject)?;
        let class_names = names_of(&engine, names)?;
        let valid = engine
            .is_valid(id)
            .map_err(|_| ObjectError::InvalidObject)?;
        let mut host = globals.host();
        // A retained id names one value, and the same engine object can be
        // retained under a different id. An anchor carries its Rust object's
        // token, so the later handle finds the same object instead of an
        // empty dictionary.
        let token = if host.is_none()
            && anchor
            && root.has_hosts()
            && class_names
                .iter()
                .any(|name| name.to_string() == "Dictionary")
        {
            host_token(&engine, id)?
        } else {
            None
        };
        if let Some(token) = token
            && let Some(object) = root.host_by_token(token)
        {
            if store_for(root, id).is_none() {
                globals = child_of(root, id);
            }
            globals.set_host(Rc::clone(&object));
            host = Some(object);
        }
        let log = globals.log().or_else(|| root.log());
        Ok(Self {
            retained: Rc::new(value),
            engine,
            class_names: Rc::new(RefCell::new(class_names)),
            valid: Rc::new(Cell::new(valid)),
            globals: RefCell::new(globals),
            root: Rc::downgrade(root),
            host,
            log,
        })
    }

    /// The scope the object belongs to.
    ///
    /// # Errors
    ///
    /// [`ObjectError::InvalidObject`] when the scope is gone: an object that
    /// outlives the scope it was read in has nowhere to keep a value the engine
    /// cannot hold, and no store to publish a child object's members in.
    fn root(&self) -> Result<SharedGlobals, ObjectError> {
        self.root.upgrade().ok_or(ObjectError::InvalidObject)
    }

    /// This object's store.
    fn store(&self) -> SharedGlobals {
        self.globals.borrow().clone()
    }

    /// This object's store, preferring the one its scope already published.
    fn entry_store(&self) -> Result<SharedGlobals, ObjectError> {
        let root = self.root()?;
        let current = self.store();
        if Rc::ptr_eq(&current, &root) {
            return Ok(current);
        }
        Ok(store_for(&root, self.id()).unwrap_or(current))
    }

    /// This object's store, publishing a store that was only local to this
    /// handle before its first Rust-owned member.
    fn member_store(&self) -> Result<SharedGlobals, ObjectError> {
        let root = self.root()?;
        let current = self.store();
        if Rc::ptr_eq(&current, &root) {
            return Ok(current);
        }
        let id = self.id();
        if let Some(existing) = store_for(&root, id) {
            if !Rc::ptr_eq(&existing, &current) {
                *self.globals.borrow_mut() = Rc::clone(&existing);
                return Ok(existing);
            }
            return Ok(current);
        }
        root.children.borrow_mut().insert(id, Rc::clone(&current));
        Ok(current)
    }

    /// The engine the object belongs to.
    pub fn engine(&self) -> &Rc<Tjs2Engine> {
        &self.engine
    }

    /// The retained id.
    pub fn id(&self) -> Tjs2ValueId {
        self.retained.raw_id()
    }

    /// The failure an object that has been invalidated answers with.
    fn live(&self) -> Result<(), ObjectError> {
        if self.valid.get() {
            Ok(())
        } else {
            Err(ObjectError::InvalidObject)
        }
    }
}

/// The names a retained string array holds, in its own order.
fn names_of(engine: &Tjs2Engine, array: DetachedValue) -> Result<Vec<TjsString>, ObjectError> {
    let count = match engine
        .member(array.raw_id(), "count")
        .map_err(|_| ObjectError::CallNotServed)?
    {
        TjsValue::Integer(count) => {
            usize::try_from(count).map_err(|_| ObjectError::CallNotServed)?
        }
        TjsValue::Void
        | TjsValue::Real(_)
        | TjsValue::String(_)
        | TjsValue::Octet(_)
        | TjsValue::Object
        | TjsValue::Retained(_) => return Err(ObjectError::CallNotServed),
    };
    let mut names = Vec::with_capacity(count);
    for index in 0..count {
        match engine
            .member(array.raw_id(), &index.to_string())
            .map_err(|_| ObjectError::CallNotServed)?
        {
            TjsValue::String(name) => names.push(TjsString::from(name)),
            TjsValue::Void
            | TjsValue::Integer(_)
            | TjsValue::Real(_)
            | TjsValue::Octet(_)
            | TjsValue::Object
            | TjsValue::Retained(_) => return Err(ObjectError::CallNotServed),
        }
    }
    Ok(names)
}

/// The engine value a variant marshals to.
///
/// Scalar kinds cross as themselves; an octet's bytes are copied into the
/// owned value. An object crosses as a duplicate of its retained id, because
/// the engine consumes the retention it is handed; a `null` object crosses as
/// a retained `null`. An object with no engine id is refused.
///
/// The retentions the crossing takes are pushed onto `keep`: the engine
/// consumes them when it copies the value, so they must outlive the call, not
/// the marshalling.
fn to_value(
    engine: &Tjs2Engine,
    variant: &Variant,
    keep: &mut Vec<DetachedValue>,
) -> Result<TjsValue, ObjectError> {
    match variant {
        Variant::Void => Ok(TjsValue::Void),
        Variant::Integer(value) => Ok(TjsValue::Integer(*value)),
        Variant::Real(value) => Ok(TjsValue::Real(*value)),
        Variant::String(text) => Ok(TjsValue::String(text.to_string())),
        Variant::Octet(octet) => Ok(TjsValue::Octet(octet.as_slice().to_vec())),
        Variant::Object(closure) => object_to_value(engine, closure, keep),
    }
}

/// The engine value a script object marshals to.
fn object_to_value(
    engine: &Tjs2Engine,
    closure: &Closure,
    keep: &mut Vec<DetachedValue>,
) -> Result<TjsValue, ObjectError> {
    let retained = match &closure.object {
        // `null` is an object with no handle; the engine has a retained form
        // for it, so it crosses as itself.
        None => engine
            .retain_null()
            .map_err(|_| ObjectError::CallNotServed)?,
        Some(object) => {
            let id = object.engine_id().ok_or(ObjectError::CallNotServed)?;
            engine
                .duplicate_retained(id)
                .map_err(|_| ObjectError::CallNotServed)?
        }
    };
    let value = retained_value(&retained);
    keep.push(retained);
    Ok(value)
}

/// The argument a retention crosses the interface as.
///
/// The id is the engine's own opaque handle, and the ABI carries it in the
/// retained-value slot; this is that slot's value, not an address the bridge
/// derived. The retention is consumed by the copy the engine makes; the handle
/// that carries it is dropped after the call, and the release of a consumed id
/// is a no-op.
fn retained_value(value: &DetachedValue) -> TjsValue {
    TjsValue::Retained(value.raw_id().as_ptr() as u64)
}

/// The variant a scalar engine answer marshals to.
///
/// An object never reaches here: the read and the call adopt it through
/// [`CHandle`].
fn to_variant(answer: TjsValue) -> Result<Variant, ObjectError> {
    match answer {
        TjsValue::Void => Ok(Variant::Void),
        TjsValue::Integer(value) => Ok(Variant::Integer(value)),
        TjsValue::Real(value) => Ok(Variant::Real(value)),
        TjsValue::String(text) => Ok(Variant::String(TjsString::from(text))),
        TjsValue::Octet(bytes) => Ok(Variant::Octet(Octet::from_vec(bytes))),
        TjsValue::Object | TjsValue::Retained(_) => Err(ObjectError::CallNotServed),
    }
}

/// The argument list a dispatch call crosses with.
fn arguments(
    engine: &Tjs2Engine,
    argv: Option<&[Variant]>,
    keep: &mut Vec<DetachedValue>,
) -> Result<Vec<TjsValue>, ObjectError> {
    match argv {
        Some(argv) => argv
            .iter()
            .map(|value| to_value(engine, value, keep))
            .collect(),
        None => Ok(Vec::new()),
    }
}

/// The receiver id a call crosses with.
///
/// `None` is no fallback receiver: the engine then uses the value's own
/// receiver, or the value itself. A receiver the Rust object model owns has no
/// retained id, so it cannot cross.
fn receiver_id(objthis: Option<&dyn Dispatch>) -> Result<Option<Tjs2ValueId>, ObjectError> {
    match objthis {
        None => Ok(None),
        Some(receiver) => receiver
            .engine_id()
            .map(Some)
            .ok_or(ObjectError::CallNotServed),
    }
}

/// The variant an engine call answer becomes.
fn call_answer(
    engine: &Rc<Tjs2Engine>,
    answer: CallAnswer,
    root: &SharedGlobals,
) -> Result<Variant, ObjectError> {
    match answer {
        CallAnswer::Null => Ok(Variant::Object(Closure::null())),
        CallAnswer::Object(object) => Ok(object_value(CHandle::with_root(
            engine.clone(),
            object,
            Rc::clone(root),
        )?)),
        CallAnswer::Scalar(value) => to_variant(value),
    }
}

/// The object value a handle becomes.
fn object_value(handle: CHandle) -> Variant {
    let object: Rc<dyn Dispatch> = Rc::new(handle);
    Variant::Object(Closure {
        object: Some(object.clone()),
        objthis: Some(object),
    })
}

/// The member a read answered, as a variant.
fn read_member(
    engine: &Rc<Tjs2Engine>,
    id: Tjs2ValueId,
    name: &str,
    flags: MemberFlags,
    root: &SharedGlobals,
) -> Result<Variant, ObjectError> {
    match engine.member_read_exact(id, name, flags.bits()) {
        Ok(MemberShape::Absent) => Err(ObjectError::MemberNotFound),
        Ok(MemberShape::Void) => Ok(Variant::Void),
        Ok(MemberShape::Null) => Ok(Variant::Object(Closure::null())),
        Ok(MemberShape::Object(object)) => {
            let handle = CHandle::with_root(engine.clone(), object, Rc::clone(root))?;
            Ok(object_value(handle))
        }
        Ok(MemberShape::Octet(bytes)) => Ok(Variant::Octet(Octet::from_vec(bytes))),
        Ok(MemberShape::Scalar(value)) => to_variant(value),
        Err(_) => Err(ObjectError::CallNotServed),
    }
}

/// Retain `object` as a value the engine can hold.
///
/// The object is Rust-owned, so it is given an engine dictionary as its anchor
/// and the object itself becomes the dictionary's store host. The dictionary's
/// members are a mirror for the engine; code that reaches the anchor through
/// the Rust bridge sees the object itself, so a class, a function or a
/// dictionary keeps its identity after the call that produced it.
pub fn retain_host_object(
    engine: &Tjs2Engine,
    object: &Rc<dyn Dispatch>,
    globals: &SharedGlobals,
) -> Result<DetachedValue, ObjectError> {
    let anchor = engine
        .new_dictionary()
        .map_err(|_| ObjectError::CallNotServed)?;
    let id = anchor.raw_id();
    child_of(globals, id).set_host(Rc::clone(object));
    mirror_object(engine, id, object, globals, &mut Vec::new())?;
    let token = root_of(globals).register_host(Rc::clone(object));
    engine
        .set_member_with_flags(
            id,
            HOST_TOKEN,
            (MemberFlags::HIDDEN | MemberFlags::ENSURE).bits(),
            &TjsValue::Integer(token),
        )
        .map_err(|_| ObjectError::CallNotServed)?;
    Ok(anchor)
}

/// The Rust object token an engine anchor carries, when it carries one.
fn host_token(engine: &Tjs2Engine, id: Tjs2ValueId) -> Result<Option<i64>, ObjectError> {
    match engine
        .member_ignore_prop(id, HOST_TOKEN)
        .map_err(|_refused| ObjectError::InvalidObject)?
    {
        TjsValue::Integer(token) => Ok(Some(token)),
        TjsValue::Void
        | TjsValue::Real(_)
        | TjsValue::String(_)
        | TjsValue::Octet(_)
        | TjsValue::Object
        | TjsValue::Retained(_) => Ok(None),
    }
}

/// Copies a Rust object's members onto its engine anchor.
fn mirror_object(
    engine: &Tjs2Engine,
    anchor: Tjs2ValueId,
    object: &Rc<dyn Dispatch>,
    globals: &SharedGlobals,
    stack: &mut Vec<*const ()>,
) -> Result<(), ObjectError> {
    let pointer = Rc::as_ptr(object) as *const ();
    if stack.contains(&pointer) {
        return Ok(());
    }
    stack.push(pointer);
    let mut members = Vec::new();
    object
        .enum_members(MemberFlags::NONE, &mut |name, _flags, value| {
            members.push((name.clone(), value.clone()));
            true
        })
        .map_err(|_| ObjectError::CallNotServed)?;
    for (name, value) in members {
        mirror_member(engine, anchor, &name, &value, globals, stack)?;
    }
    stack.pop();
    Ok(())
}

/// Copies one Rust member's value onto an engine anchor.
fn mirror_member(
    engine: &Tjs2Engine,
    anchor: Tjs2ValueId,
    name: &TjsString,
    value: &Variant,
    globals: &SharedGlobals,
    stack: &mut Vec<*const ()>,
) -> Result<(), ObjectError> {
    let member = name.to_string();
    match value {
        Variant::Object(closure) => match &closure.object {
            None => {
                let null = engine
                    .retain_null()
                    .map_err(|_| ObjectError::CallNotServed)?;
                set_retained_member(engine, anchor, &member, null)
            }
            Some(object) => {
                if let Some(id) = object.engine_id() {
                    let duplicate = engine
                        .duplicate_retained(id)
                        .map_err(|_| ObjectError::CallNotServed)?;
                    set_retained_member(engine, anchor, &member, duplicate)
                } else {
                    let nested = engine
                        .new_dictionary()
                        .map_err(|_| ObjectError::CallNotServed)?;
                    let nested_id = nested.raw_id();
                    child_of(globals, nested_id).set_host(Rc::clone(object));
                    mirror_object(engine, nested_id, object, globals, stack)?;
                    set_retained_member(engine, anchor, &member, nested)
                }
            }
        },
        _ => {
            let mut keep = Vec::new();
            let crossing = to_value(engine, value, &mut keep)?;
            let answer = engine.set_member(anchor, &member, &crossing);
            drop(keep);
            answer.map_err(|_| ObjectError::CallNotServed)
        }
    }
}

/// Sets an engine member from a retention, consuming the retention.
fn set_retained_member(
    engine: &Tjs2Engine,
    anchor: Tjs2ValueId,
    name: &str,
    value: DetachedValue,
) -> Result<(), ObjectError> {
    let id = value.raw_id();
    let crossing = TjsValue::Retained(id.as_ptr() as u64);
    let answer = engine.set_member(anchor, name, &crossing);
    drop(value);
    answer.map_err(|_| ObjectError::CallNotServed)
}

impl Dispatch for CHandle {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.live()?;
        if let Some(host) = &self.host {
            return host.prop_get(name, flags, objthis);
        }
        let globals = self.entry_store()?;
        if let Some(value) = globals.get(name) {
            return Ok(value);
        }
        let root = self.root()?;
        read_member(&self.engine, self.id(), &name.to_string(), flags, &root)
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.live()?;
        if let Some(host) = &self.host {
            let root = self.root()?;
            host.prop_set(name, flags, value, objthis)?;
            mirror_member(&self.engine, self.id(), name, value, &root, &mut Vec::new())?;
            return Ok(());
        }
        // A value the engine cannot address lives in the handle's own store.
        if unaddressable(value) {
            self.member_store()?.insert(name.clone(), value.clone());
            return Ok(());
        }
        let mut keep = Vec::new();
        let crossing = to_value(&self.engine, value, &mut keep)?;
        let member = name.to_string();
        // The write carries the instruction's own flags: a write with no flag
        // is the plain assignment rule, which refuses a name the object does
        // not hold, and a write with a flag asks for that flag's rule.
        let answer = self
            .engine
            .set_member_with_flags(self.id(), &member, flags.bits(), &crossing);
        drop(keep);
        match answer {
            Ok(()) => {
                self.store().remove(name);
                Ok(())
            }
            Err(error) if crate::is_missing_member(&error) => Err(ObjectError::MemberMustExist),
            Err(_) => Err(ObjectError::CallNotServed),
        }
    }

    fn delete_member(
        &self,
        name: &TjsString,
        _flags: MemberFlags,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.live()?;
        if let Some(host) = &self.host {
            let answer = host.delete_member(name, _flags, _objthis);
            match self.engine.delete_member(self.id(), &name.to_string()) {
                Ok(()) => {}
                Err(error) if crate::is_missing_member(&error) => {}
                Err(_) => return Err(ObjectError::CallNotServed),
            }
            return answer;
        }
        let store = self.member_store()?;
        let stored = store.remove(name).is_some();
        if stored {
            match self.engine.delete_member(self.id(), &name.to_string()) {
                Ok(()) => return Ok(()),
                Err(error) if crate::is_missing_member(&error) => return Ok(()),
                Err(_) => return Err(ObjectError::CallNotServed),
            }
        }
        match self.engine.delete_member(self.id(), &name.to_string()) {
            Ok(()) => Ok(()),
            Err(error) if crate::is_missing_member(&error) => Err(ObjectError::MemberNotFound),
            Err(_) => Err(ObjectError::CallNotServed),
        }
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        self.live()?;
        if let Some(host) = &self.host {
            return host.enum_members(flags, callback);
        }
        // The safe wrapper sorts and de-duplicates the names, so this walk is
        // not the object's own order, and the interface publishes no member
        // flags: the callback receives zero for every member.
        let names = self
            .engine
            .enum_members(self.id())
            .map_err(|_| ObjectError::CallNotServed)?;
        let read_flags = if flags.contains(MemberFlags::IGNORE_PROPERTY) {
            MemberFlags::IGNORE_PROPERTY
        } else {
            MemberFlags::NONE
        };
        let values = !flags.contains(MemberFlags::ENUM_NO_VALUE);
        let globals = self.entry_store()?;
        let root = self.root()?;
        let mut extra: BTreeMap<TjsString, Variant> = globals.entries().into_iter().collect();
        for name in names {
            let name = TjsString::from(name);
            let value = if let Some(value) = extra.remove(&name) {
                value
            } else if values {
                read_member(
                    &self.engine,
                    self.id(),
                    &name.to_string(),
                    read_flags,
                    &root,
                )?
            } else {
                Variant::Void
            };
            if !callback(&name, 0, &value) {
                return Ok(());
            }
        }
        for (name, value) in extra {
            if !callback(&name, 0, &value) {
                break;
            }
        }
        Ok(())
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        if let Some(host) = &self.host {
            return host.is_instance_of(class, objthis);
        }
        self.engine
            .is_instance_of(self.id(), &class.to_string())
            .map_err(|_| ObjectError::CallNotServed)
    }

    fn class_names(&self) -> Vec<TjsString> {
        match &self.host {
            Some(host) => host.class_names(),
            None => self.class_names.borrow().clone(),
        }
    }

    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.live()?;
        if let Some(host) = &self.host {
            return host.add_class_name(name);
        }
        self.engine
            .add_class_name(self.id(), &name.to_string())
            .map_err(|_| ObjectError::CallNotServed)?;
        self.class_names.borrow_mut().push(name.clone());
        Ok(())
    }

    fn engine_id(&self) -> Option<Tjs2ValueId> {
        Some(self.id())
    }

    fn is_host_proxy(&self) -> bool {
        self.host.is_some()
    }

    fn rust_member(&self, name: &TjsString) -> Option<Variant> {
        if let Some(host) = &self.host {
            return host.rust_member(name);
        }
        // A handle whose scope is gone has no store left to read; the object's
        // own call entry reports the refusal it finds.
        let Ok(globals) = self.entry_store() else {
            return None;
        };
        globals.get(name)
    }

    fn is_valid(&self) -> bool {
        if let Some(host) = &self.host {
            return host.is_valid() && self.valid.get();
        }
        let Ok(valid) = self.engine.is_valid(self.id()) else {
            // The question could not be asked; the last answer the interface
            // gave is the one the handle keeps.
            return self.valid.get();
        };
        self.valid.set(valid);
        valid
    }

    fn invalidate(&self) -> bool {
        if !self.valid.get() {
            return false;
        }
        // The handle is unusable after the attempt either way: an
        // invalidation whose `finalize` raised leaves nothing this adapter
        // can call.
        self.valid.set(false);
        let host_changed = self.host.as_ref().is_some_and(|host| host.invalidate());
        let engine_changed = self.engine.invalidate_retained(self.id()).is_ok();
        host_changed || engine_changed
    }

    fn context(&self) -> Option<&crate::vm::runtime::exec::ContextObject> {
        self.host.as_ref().and_then(|host| host.context())
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.live()?;
        if let Some(host) = &self.host {
            return host.func_call(name, argv, objthis);
        }
        let receiver = receiver_id(objthis)?;
        let mut keep = Vec::new();
        let args = arguments(&self.engine, argv, &mut keep)?;
        // A named call goes through the object's class chain; the value's own
        // call is its default member. The computed receiver crosses as its
        // retained id, so the engine's own receiver preference still wins.
        let answer = if name.is_empty() {
            self.engine.call_value_answer(self.id(), &args, receiver)
        } else {
            self.engine
                .call_member_answer(self.id(), &name.to_string(), &args, receiver)
        };
        drop(keep);
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => {
                if let Some(log) = &self.log {
                    log(&error);
                }
                return Err(ObjectError::CallNotServed);
            }
        };
        call_answer(&self.engine, answer, &self.root()?)
    }

    fn create_new(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.live()?;
        if let Some(host) = &self.host {
            return host.create_new(name, argv, objthis);
        }
        let receiver = receiver_id(objthis)?;
        let mut keep = Vec::new();
        let args = arguments(&self.engine, argv, &mut keep)?;
        let answer = self
            .engine
            .create_new_answer(self.id(), &name.to_string(), &args, receiver);
        drop(keep);
        match answer.map_err(|_| ObjectError::CallNotServed)? {
            CallAnswer::Object(object) => Ok(object_value(CHandle::with_root(
                self.engine.clone(),
                object,
                self.root()?,
            )?)),
            CallAnswer::Null | CallAnswer::Scalar(_) => Err(ObjectError::CallNotServed),
        }
    }

    fn operation(
        &self,
        op: &TjsString,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        if let Some(host) = &self.host {
            return host.operation(op, value, objthis);
        }
        // The interface has no operator entry: not served, so the executor
        // applies the read-modify-write, which is the engine's own base rule.
        Ok(false)
    }

    fn prop_get_by_num(
        &self,
        index: i64,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        if let Some(host) = &self.host {
            return host.prop_get_by_num(index, flags, objthis);
        }
        let name = TjsString::from(&Number::Integer(index));
        self.prop_get(&name, flags, objthis)
    }

    fn prop_set_by_num(
        &self,
        index: i64,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        if let Some(host) = &self.host {
            return host.prop_set_by_num(index, flags, value, objthis);
        }
        let name = TjsString::from(&Number::Integer(index));
        self.prop_set(&name, flags, value, objthis)
    }
}
