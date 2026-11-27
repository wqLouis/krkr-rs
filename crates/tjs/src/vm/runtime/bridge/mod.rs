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
//! answers no engine id, so it cannot cross. A global handle keeps such a value
//! in its [`GlobalMembers`] store instead; any other handle refuses it.
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
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::vm::runtime::natives::registry::NativeRegistry;
use crate::vm::runtime::number::Number;
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::octet::Octet;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant};
use crate::{CallAnswer, DetachedValue, MemberShape, Tjs2Engine, Tjs2ValueId, TjsValue};

/// The members a global handle keeps on the Rust side.
///
/// The engine cannot hold a value of the Rust object model, so a handle created
/// for a global scope may carry those values here. Scalars and engine objects
/// still cross to the engine; only an object the engine cannot address stays
/// in this store. The store also owns the native class registry: a name the
/// registry publishes is answered from the registry before the engine, and a
/// write or a delete of that name is kept here so the two views do not
/// disagree.
pub struct GlobalMembers {
    members: RefCell<BTreeMap<TjsString, Variant>>,
    removed: RefCell<BTreeSet<TjsString>>,
    natives: NativeRegistry,
    log: Option<LogSink>,
}

/// What a refused engine call reports to.
pub type LogSink = Rc<dyn Fn(&str)>;

impl GlobalMembers {
    /// An empty store with no log sink.
    pub fn new() -> Self {
        Self {
            members: RefCell::new(BTreeMap::new()),
            removed: RefCell::new(BTreeSet::new()),
            natives: NativeRegistry::new(),
            log: None,
        }
    }

    /// The same store, reporting a refused engine call to `log`.
    pub fn with_log(mut self, log: impl Fn(&str) + 'static) -> Self {
        self.log = Some(Rc::new(log));
        self
    }

    /// The store's log sink.
    fn log(&self) -> Option<LogSink> {
        self.log.clone()
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

    /// The class object the global name denotes, when the registry publishes
    /// one and the name has not been deleted.
    fn native(&self, name: &TjsString) -> Option<Variant> {
        if self.removed.borrow().contains(name) {
            return None;
        }
        self.natives.get(name)
    }

    /// Whether the registry publishes the global name.
    fn is_native(&self, name: &TjsString) -> bool {
        self.natives.contains(name)
    }

    /// Whether the global name was deleted from the registry's view.
    fn is_removed(&self, name: &TjsString) -> bool {
        self.removed.borrow().contains(name)
    }

    /// Marks a registry name deleted, so the name reads as absent.
    fn mark_removed(&self, name: &TjsString) {
        self.removed.borrow_mut().insert(name.clone());
    }

    /// The registry's class objects, by name, excluding deleted names.
    fn native_entries(&self) -> Vec<(TjsString, Variant)> {
        let removed = self.removed.borrow();
        let mut entries = Vec::new();
        for name in self.natives.names() {
            let name = TjsString::from(name);
            if removed.contains(&name) {
                continue;
            }
            if let Some(value) = self.natives.get(&name) {
                entries.push((name, value));
            }
        }
        entries
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
    /// The members the engine cannot hold, when this handle is a global scope.
    globals: Option<SharedGlobals>,
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
        let id = value.raw_id();
        let names = engine
            .class_names(id)
            .map_err(|_| ObjectError::InvalidObject)?;
        let class_names = names_of(&engine, names)?;
        let valid = engine
            .is_valid(id)
            .map_err(|_| ObjectError::InvalidObject)?;
        Ok(Self {
            retained: Rc::new(value),
            engine,
            class_names: Rc::new(RefCell::new(class_names)),
            valid: Rc::new(Cell::new(valid)),
            globals: None,
            log: None,
        })
    }

    /// The adapter over `value`, carrying the global members the engine cannot
    /// hold.
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
        let log = globals.log();
        let mut handle = Self::new(engine, value)?;
        handle.globals = Some(globals);
        handle.log = log;
        Ok(handle)
    }

    /// The adapter over `value`, reporting refused calls to `log`.
    fn with_log(
        engine: Rc<Tjs2Engine>,
        value: DetachedValue,
        log: Option<LogSink>,
    ) -> Result<Self, ObjectError> {
        let mut handle = Self::new(engine, value)?;
        handle.log = log;
        Ok(handle)
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
    log: Option<LogSink>,
) -> Result<Variant, ObjectError> {
    match answer {
        CallAnswer::Null => Ok(Variant::Object(Closure::null())),
        CallAnswer::Object(object) => Ok(object_value(CHandle::with_log(
            engine.clone(),
            object,
            log,
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
    log: Option<&LogSink>,
) -> Result<Variant, ObjectError> {
    match engine.member_read_exact(id, name, flags.bits()) {
        Ok(MemberShape::Absent) => Err(ObjectError::MemberNotFound),
        Ok(MemberShape::Void) => Ok(Variant::Void),
        Ok(MemberShape::Null) => Ok(Variant::Object(Closure::null())),
        Ok(MemberShape::Object(object)) => {
            let handle = CHandle::with_log(engine.clone(), object, log.cloned())?;
            Ok(object_value(handle))
        }
        Ok(MemberShape::Octet(bytes)) => Ok(Variant::Octet(Octet::from_vec(bytes))),
        Ok(MemberShape::Scalar(value)) => to_variant(value),
        Err(_) => Err(ObjectError::CallNotServed),
    }
}

impl Dispatch for CHandle {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.live()?;
        if let Some(globals) = &self.globals {
            if let Some(value) = globals.get(name) {
                return Ok(value);
            }
            if globals.is_removed(name) {
                return Err(ObjectError::MemberNotFound);
            }
            if let Some(value) = globals.native(name) {
                return Ok(value);
            }
        }
        read_member(
            &self.engine,
            self.id(),
            &name.to_string(),
            flags,
            self.log.as_ref(),
        )
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.live()?;
        // A value the engine cannot address lives in the handle's own store,
        // and so does any write to a name the class registry owns: the store
        // is then the one view of that name.
        if let Some(globals) = &self.globals {
            let unaddressable = match value {
                Variant::Object(closure) => matches!(
                    &closure.object,
                    Some(object) if object.engine_id().is_none()
                ),
                _ => false,
            };
            if globals.is_native(name) || unaddressable {
                globals.insert(name.clone(), value.clone());
                return Ok(());
            }
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
                if let Some(globals) = &self.globals {
                    globals.remove(name);
                }
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
        if let Some(globals) = &self.globals {
            let stored = globals.remove(name).is_some();
            if globals.is_native(name) {
                // The class object is this store's value, so deleting the name
                // is this store's answer; the engine's copy is not the view.
                globals.mark_removed(name);
                return Ok(());
            }
            if stored {
                match self.engine.delete_member(self.id(), &name.to_string()) {
                    Ok(()) => return Ok(()),
                    Err(error) if crate::is_missing_member(&error) => return Ok(()),
                    Err(_) => return Err(ObjectError::CallNotServed),
                }
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
        let mut extra: BTreeMap<TjsString, Variant> = self
            .globals
            .as_ref()
            .map(|globals| globals.entries().into_iter().collect())
            .unwrap_or_default();
        if let Some(globals) = &self.globals {
            for (name, value) in globals.native_entries() {
                extra.entry(name).or_insert(value);
            }
        }
        for name in names {
            let name = TjsString::from(name);
            if self
                .globals
                .as_ref()
                .is_some_and(|globals| globals.is_removed(&name))
            {
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
                    self.log.as_ref(),
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
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        self.engine
            .is_instance_of(self.id(), &class.to_string())
            .map_err(|_| ObjectError::CallNotServed)
    }

    fn class_names(&self) -> Vec<TjsString> {
        self.class_names.borrow().clone()
    }

    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.live()?;
        self.engine
            .add_class_name(self.id(), &name.to_string())
            .map_err(|_| ObjectError::CallNotServed)?;
        self.class_names.borrow_mut().push(name.clone());
        Ok(())
    }

    fn engine_id(&self) -> Option<Tjs2ValueId> {
        Some(self.id())
    }

    fn rust_member(&self, name: &TjsString) -> Option<Variant> {
        let globals = self.globals.as_ref()?;
        globals.get(name).or_else(|| globals.native(name))
    }

    fn is_valid(&self) -> bool {
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
        self.engine.invalidate_retained(self.id()).is_ok()
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.live()?;
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
        call_answer(&self.engine, answer, self.log.clone())
    }

    fn create_new(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.live()?;
        let receiver = receiver_id(objthis)?;
        let mut keep = Vec::new();
        let args = arguments(&self.engine, argv, &mut keep)?;
        let answer = self
            .engine
            .create_new_answer(self.id(), &name.to_string(), &args, receiver);
        drop(keep);
        match answer.map_err(|_| ObjectError::CallNotServed)? {
            CallAnswer::Object(object) => Ok(object_value(CHandle::with_log(
                self.engine.clone(),
                object,
                self.log.clone(),
            )?)),
            CallAnswer::Null | CallAnswer::Scalar(_) => Err(ObjectError::CallNotServed),
        }
    }

    fn operation(
        &self,
        _op: &TjsString,
        _value: &Variant,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
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
        let name = TjsString::from(&Number::Integer(index));
        self.prop_set(&name, flags, value, objthis)
    }
}
