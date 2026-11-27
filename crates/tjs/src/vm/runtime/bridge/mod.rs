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
//! answers no engine id, so it cannot cross and the write or the call is
//! refused.
//!
//! An answer crosses **out** of the engine as a [`Variant`]: an object the
//! read retained becomes a [`CHandle`], so the next entry reaches the same
//! object. An answer the safe surface cannot tell apart from `null` or a byte
//! string is refused rather than guessed.
//!
//! # The refusals
//!
//! The interface has no entry for a named construction, for adding a class
//! name to an existing object, or for an ordered member walk; those entries
//! answer [`ObjectError::CallNotServed`]. The calls the interface does have
//! keep the receiver the value itself carries, so a receiver the executor
//! computed is not crossed.

use std::cell::Cell;
use std::rc::Rc;

use crate::vm::runtime::number::Number;
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::octet::Octet;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant};
use crate::{DetachedValue, MemberShape, Tjs2Engine, Tjs2ValueId, TjsValue};

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
    /// The class names the interface answered for the object, once.
    class_names: Rc<Vec<TjsString>>,
    /// Whether the object was usable, from the last interface answer.
    valid: Rc<Cell<bool>>,
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
            class_names: Rc::new(class_names),
            valid: Rc::new(Cell::new(valid)),
        })
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
            | TjsValue::Object
            | TjsValue::Retained(_) => return Err(ObjectError::CallNotServed),
        }
    }
    Ok(names)
}

/// The engine value a variant marshals to.
///
/// Scalar kinds cross as themselves. An object crosses as a duplicate of its
/// retained id, because the engine consumes the retention it is handed; a
/// `null` object crosses as a retained `null`. A byte string and an object
/// with no engine id are refused.
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
        // The ABI carries the bytes, but the safe value surface has no octet
        // kind; adding one is a change across the workspace, not here.
        Variant::Octet(_) => Err(ObjectError::CallNotServed),
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

/// The variant an engine answer marshals to.
///
/// An object answer is refused: the call that produces it keeps the object in
/// the engine's most-recent-object slot, not in a handle this adapter can own,
/// and the safe value surface cannot tell such an answer from `null` or a byte
/// string.
fn to_variant(answer: TjsValue) -> Result<Variant, ObjectError> {
    match answer {
        TjsValue::Void => Ok(Variant::Void),
        TjsValue::Integer(value) => Ok(Variant::Integer(value)),
        TjsValue::Real(value) => Ok(Variant::Real(value)),
        TjsValue::String(text) => Ok(Variant::String(TjsString::from(text))),
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
) -> Result<Variant, ObjectError> {
    match engine.member_read_exact(id, name, flags.bits()) {
        Ok(MemberShape::Absent) => Err(ObjectError::MemberNotFound),
        Ok(MemberShape::Void) => Ok(Variant::Void),
        Ok(MemberShape::Null) => Ok(Variant::Object(Closure::null())),
        Ok(MemberShape::Object(object)) => {
            let handle = CHandle::new(engine.clone(), object)?;
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
        read_member(&self.engine, self.id(), &name.to_string(), flags)
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.live()?;
        let mut keep = Vec::new();
        let value = to_value(&self.engine, value, &mut keep)?;
        let name = name.to_string();
        // The wrapper's own default is plain assignment's flag: a write with
        // no flag asks for exactly that rule, and a write with a flag asks for
        // the flag's rule unchanged.
        let answer = if flags == MemberFlags::NONE {
            self.engine.set_member(self.id(), &name, &value)
        } else {
            self.engine
                .set_member_with_flags(self.id(), &name, flags.bits(), &value)
        };
        drop(keep);
        match answer {
            Ok(()) => Ok(()),
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
        for name in names {
            let name = TjsString::from(name);
            let value = if values {
                read_member(&self.engine, self.id(), &name.to_string(), read_flags)?
            } else {
                Variant::Void
            };
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
        (*self.class_names).clone()
    }

    fn add_class_name(&self, _name: &TjsString) -> Result<(), ObjectError> {
        // The interface publishes class names as a read; there is no write
        // beside it, so a name cannot be added to an engine object.
        Err(ObjectError::CallNotServed)
    }

    fn engine_id(&self) -> Option<Tjs2ValueId> {
        Some(self.id())
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
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.live()?;
        let mut keep = Vec::new();
        let args = arguments(&self.engine, argv, &mut keep)?;
        // A named call goes through the object's class chain; the value's own
        // call is its default member. The receiver the executor computed is
        // not crossed: the interface has no safe route for it, and the value's
        // own receiver wins in the engine's rule anyway.
        let answer = if name.is_empty() {
            self.engine.call_detached(&self.retained, &args)
        } else {
            self.engine.call_member(self.id(), &name.to_string(), &args)
        };
        drop(keep);
        to_variant(answer.map_err(|_| ObjectError::CallNotServed)?)
    }

    fn create_new(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.live()?;
        if !name.is_empty() {
            // The `NEW` instruction already resolved the class into the value
            // it hands over; a named construction would need a member lookup
            // the interface does not offer as one call.
            return Err(ObjectError::CallNotServed);
        }
        let mut keep = Vec::new();
        let args = arguments(&self.engine, argv, &mut keep)?;
        let object = self
            .engine
            .create_new(self.id(), &args)
            .map_err(|_| ObjectError::CallNotServed)?;
        drop(keep);
        Ok(object_value(CHandle::new(self.engine.clone(), object)?))
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
