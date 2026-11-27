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
//! answers no engine id, so a handle keeps such a value in its
//! [`GlobalMembers`] store instead. Every handle has a store; a context handle
//! has its own, keyed by the context's retained id in the scope's store.
//!
//! An answer crosses **out** of the engine as a [`Variant`]: an object the
//! call retained becomes a [`CHandle`], so the next entry reaches the same
//! object; `null` is the null closure, and a byte string becomes the
//! variant's octet.
//!
//! # What remains limited
//!
//! An ordered member walk is not served: [`Dispatch::enum_members`] answers
//! the safe wrapper's sorted, de-duplicated names and gives every member zero
//! flags. A call carries the receiver the executor computed when it is an
//! engine object; a receiver the Rust object model owns has no retained id,
//! so the call is refused rather than run on another object.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::{Rc, Weak};

use crate::vm::runtime::natives::registry::NativeRegistry;
use crate::vm::runtime::number::Number;
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::octet::Octet;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant};
use crate::{CallAnswer, DetachedValue, MemberShape, Tjs2Engine, Tjs2ValueId, TjsValue};

/// The members a handle keeps on the Rust side.
///
/// The engine cannot hold a value of the Rust object model, so a handle may
/// carry those values here. Scalars and engine objects still cross to the
/// engine; only an object the engine cannot address stays in this store. The
/// global scope's store also owns the native class registry: a name the
/// registry publishes is answered from the registry before the engine, and a
/// write or a delete of that name is kept here so the two views do not
/// disagree. A context and every other object have their own store in the same
/// scope and never apply the global registry.
pub struct GlobalMembers {
    members: RefCell<BTreeMap<TjsString, Variant>>,
    removed: RefCell<BTreeSet<TjsString>>,
    natives: Option<NativeRegistry>,
    log: Option<LogSink>,
    parent: Option<Weak<GlobalMembers>>,
    children: RefCell<HashMap<Tjs2ValueId, SharedGlobals>>,
    host: RefCell<Option<Rc<dyn Dispatch>>>,
    hosts: RefCell<HashMap<i64, Rc<dyn Dispatch>>>,
    next_host: Cell<i64>,
}

/// What a refused engine call reports to.
pub type LogSink = Rc<dyn Fn(&str)>;

/// The hidden member an engine anchor carries so a later handle for the same
/// object finds the Rust object it stands for.
const HOST_TOKEN: &str = "__krkr_rust_host";

impl GlobalMembers {
    /// An empty root store with no log sink.
    pub fn new() -> Self {
        Self {
            members: RefCell::new(BTreeMap::new()),
            removed: RefCell::new(BTreeSet::new()),
            natives: Some(NativeRegistry::new()),
            log: None,
            parent: None,
            children: RefCell::new(HashMap::new()),
            host: RefCell::new(None),
            hosts: RefCell::new(HashMap::new()),
            next_host: Cell::new(1),
        }
    }

    /// An empty store with no native class registry.
    fn plain() -> Self {
        Self {
            members: RefCell::new(BTreeMap::new()),
            removed: RefCell::new(BTreeSet::new()),
            natives: None,
            log: None,
            parent: None,
            children: RefCell::new(HashMap::new()),
            host: RefCell::new(None),
            hosts: RefCell::new(HashMap::new()),
            next_host: Cell::new(1),
        }
    }

    /// A store for one object in `root`'s scope, published through the root.
    fn detached_to(root: &SharedGlobals) -> Self {
        Self {
            members: RefCell::new(BTreeMap::new()),
            removed: RefCell::new(BTreeSet::new()),
            natives: None,
            log: root.log.clone(),
            parent: Some(Rc::downgrade(root)),
            children: RefCell::new(HashMap::new()),
            host: RefCell::new(None),
            hosts: RefCell::new(HashMap::new()),
            next_host: Cell::new(1),
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
        self.removed.borrow_mut().remove(&name);
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

    /// The class object the name denotes, when the registry publishes one and
    /// the name has not been deleted.
    fn native(&self, name: &TjsString) -> Option<Variant> {
        if self.removed.borrow().contains(name) {
            return None;
        }
        self.natives.as_ref()?.get(name)
    }

    /// Whether the registry publishes the name.
    fn is_native(&self, name: &TjsString) -> bool {
        self.natives
            .as_ref()
            .is_some_and(|natives| natives.contains(name))
    }

    /// Whether the name was deleted from this store's view.
    fn is_removed(&self, name: &TjsString) -> bool {
        self.removed.borrow().contains(name)
    }

    /// Marks a registry name deleted from this store's view.
    fn mark_removed(&self, name: &TjsString) {
        self.removed.borrow_mut().insert(name.clone());
    }

    /// The registry's class objects, by name, excluding deleted names.
    fn native_entries(&self) -> Vec<(TjsString, Variant)> {
        let removed = self.removed.borrow();
        match &self.natives {
            Some(natives) => {
                let mut entries = Vec::new();
                for name in natives.names() {
                    let name = TjsString::from(name);
                    if removed.contains(&name) {
                        continue;
                    }
                    if let Some(value) = natives.get(&name) {
                        entries.push((name, value));
                    }
                }
                entries
            }
            None => Vec::new(),
        }
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
    /// The store the object's scope shares; child stores are keyed here.
    root: SharedGlobals,
    /// The Rust object this handle stands in front of, when it is a proxy for
    /// one that crossed into the engine.
    host: Option<Rc<dyn Dispatch>>,
    /// Whether this handle is the global scope's own handle. Only that handle
    /// applies the Rust class registry over the engine's global object.
    global: bool,
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
        let globals = Rc::new(GlobalMembers::plain());
        let root = Rc::clone(&globals);
        Self::with_store(engine, value, globals, root, false)
    }

    /// The adapter over `value` for the global scope.
    ///
    /// This is the one handle that applies the global store's class registry
    /// over the engine's global object.
    ///
    /// # Errors
    ///
    /// The id is not an object the engine can answer for: the class-name read
    /// or the validity question failed.
    pub fn with_members(
        engine: Rc<Tjs2Engine>,
        value: DetachedValue,
        globals: SharedGlobals,
    ) -> Result<Self, ObjectError> {
        let root = root_of(&globals);
        Self::with_store(engine, value, globals, root, true)
    }

    /// The adapter over `value`, carrying a non-global object's own members.
    ///
    /// The handle consults its store and the engine, never the global scope's
    /// class registry.
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
        Self::with_store(engine, value, globals, root, false)
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
        Self::with_store(engine, value, globals, root, false)
    }

    /// The adapter over `value` with an explicit store.
    fn with_store(
        engine: Rc<Tjs2Engine>,
        value: DetachedValue,
        mut globals: SharedGlobals,
        root: SharedGlobals,
        global: bool,
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
            && !global
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
            if store_for(&root, id).is_none() {
                globals = child_of(&root, id);
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
            root,
            host,
            global,
            log,
        })
    }

    /// This object's store.
    fn store(&self) -> SharedGlobals {
        self.globals.borrow().clone()
    }

    /// This object's store, preferring the one its scope already published.
    fn entry_store(&self) -> SharedGlobals {
        let current = self.store();
        if Rc::ptr_eq(&current, &self.root) {
            return current;
        }
        store_for(&self.root, self.id()).unwrap_or(current)
    }

    /// This object's store, publishing a store that was only local to this
    /// handle before its first Rust-owned member.
    fn member_store(&self) -> SharedGlobals {
        let current = self.store();
        if Rc::ptr_eq(&current, &self.root) {
            return current;
        }
        let id = self.id();
        if let Some(existing) = store_for(&self.root, id) {
            if !Rc::ptr_eq(&existing, &current) {
                *self.globals.borrow_mut() = Rc::clone(&existing);
                return existing;
            }
            return current;
        }
        self.root
            .children
            .borrow_mut()
            .insert(id, Rc::clone(&current));
        current
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
        let globals = self.entry_store();
        if let Some(value) = globals.get(name) {
            return Ok(value);
        }
        if globals.is_removed(name) {
            return Err(ObjectError::MemberNotFound);
        }
        if self.global
            && let Some(value) = globals.native(name)
        {
            return Ok(value);
        }
        read_member(
            &self.engine,
            self.id(),
            &name.to_string(),
            flags,
            &self.root,
        )
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
            host.prop_set(name, flags, value, objthis)?;
            mirror_member(
                &self.engine,
                self.id(),
                name,
                value,
                &self.root,
                &mut Vec::new(),
            )?;
            return Ok(());
        }
        let globals = self.entry_store();
        // A value the engine cannot address lives in the handle's own store,
        // and so does any write to a name the class registry owns: the store
        // is then the one view of that name.
        let unaddressable = match value {
            Variant::Object(closure) => matches!(
                &closure.object,
                Some(object) if object.engine_id().is_none() || object.is_host_proxy()
            ),
            _ => false,
        };
        if (self.global && globals.is_native(name)) || unaddressable {
            self.member_store().insert(name.clone(), value.clone());
            return Ok(());
        }
        let mut keep = Vec::new();
        let crossing = to_value(&self.engine, value, &mut keep)?;
        let member = name.to_string();
        // The wrapper's own default is plain assignment's flag: a write with
        // no flag asks for exactly that rule, and a write with a flag asks for
        // the flag's rule unchanged.
        let answer = if flags == MemberFlags::NONE {
            self.engine.set_member(self.id(), &member, &crossing)
        } else {
            self.engine
                .set_member_with_flags(self.id(), &member, flags.bits(), &crossing)
        };
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
        let globals = self.entry_store();
        let store = self.member_store();
        let stored = store.remove(name).is_some();
        if self.global && globals.is_native(name) {
            // The class object is this store's value, so deleting the name is
            // this store's answer; the engine's copy is not the view.
            store.mark_removed(name);
            return Ok(());
        }
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
        let globals = self.entry_store();
        let mut extra: BTreeMap<TjsString, Variant> = globals.entries().into_iter().collect();
        if self.global {
            for (name, value) in globals.native_entries() {
                extra.entry(name).or_insert(value);
            }
        }
        for name in names {
            let name = TjsString::from(name);
            if globals.is_removed(&name) {
                continue;
            }
            let value = if let Some(value) = extra.remove(&name) {
                value
            } else if values {
                read_member(
                    &self.engine,
                    self.id(),
                    &name.to_string(),
                    read_flags,
                    &self.root,
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
        let globals = self.entry_store();
        globals
            .get(name)
            .or_else(|| self.global.then(|| globals.native(name)).flatten())
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
        call_answer(&self.engine, answer, &self.root)
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
                Rc::clone(&self.root),
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
