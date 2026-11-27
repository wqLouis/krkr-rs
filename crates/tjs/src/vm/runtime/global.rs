//! The global object: a scope's members in front of the engine's.
//!
//! A script's top-level scope is one object. A `var` the script declares and a
//! `function` it declares are that object's members, the class names it reads
//! are members too, and the object a script calls `this` at the top level is
//! this object.
//!
//! # The resolution order
//!
//! The object answers a name from three places, in this order:
//!
//! 1. its own members — everything a script or the host wrote;
//! 2. the class objects its registry publishes — `Math`, `Exception`,
//!    `RegExp`, `Date`;
//! 3. the engine's global object, for a name that is neither of those.
//!
//! That is the whole rule, and it is the only place it is written: a name the
//! object does not hold and the registry does not publish is asked of the
//! engine's global object as one source, not merged with it. A name none of the
//! three holds is absent — a different answer from a member that holds the
//! empty value, and the one `typeof` reports as `undefined`.
//!
//! A class name a script deletes stays deleted: the registry does not come back
//! through a deletion, and neither does the engine's copy of the name.
//!
//! # Where a write goes
//!
//! A write stays on this object in two cases:
//!
//! * the value cannot cross into the engine — an object of the Rust object
//!   model has no engine handle, so the engine has nowhere to put it
//!   ([`crate::vm::runtime::bridge::unaddressable`]);
//! * the name is a class name this object publishes, so the write installs a
//!   member that shadows the class object.
//!
//! Every other value the engine can hold, so the engine holds it: the engine's
//! own code reads its global object directly, and a value kept only here would
//! be invisible to it. A write to a name the engine holds drops this object's
//! member of that name, so the two views cannot answer different values.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;
use std::rc::Rc;

use super::bridge::{CHandle, unaddressable};
use super::natives::registry::NativeRegistry;
use super::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use super::string::TjsString;
use super::variant::Variant;
use crate::Tjs2ValueId;

/// The failure an absent name is reported as.
///
/// A read that required the member answers [`ObjectError::MemberMustExist`],
/// and one that did not answers [`ObjectError::MemberNotFound`]; both mean the
/// name is not a member at all.
fn absent(flags: MemberFlags) -> ObjectError {
    if flags.contains(MemberFlags::MUST_EXIST) {
        ObjectError::MemberMustExist
    } else {
        ObjectError::MemberNotFound
    }
}

/// A scope's global object.
///
/// The members a script and the host declare live in a [`CustomObject`], the
/// class objects live in the registry in front of it, and the engine's global
/// object sits behind both as the one source this object does not own. A script
/// that writes a class name shadows it, and one that deletes it leaves the name
/// absent.
pub struct GlobalObject {
    /// The members the script and the host declared.
    members: CustomObject,
    /// The class objects the object publishes.
    natives: NativeRegistry,
    /// The class names a script deleted.
    removed: RefCell<BTreeSet<TjsString>>,
    /// The engine's global object, asked for a name this object does not hold.
    ///
    /// It is also the object's engine identity: the script-visible global is
    /// the engine's global, with these members in front of it.
    fallback: Option<Rc<CHandle>>,
}

impl GlobalObject {
    /// A global with the registry's classes, no declared member and nothing
    /// behind it.
    pub fn new() -> Self {
        Self::over(None)
    }

    /// A global whose unheld names are asked of `fallback`.
    ///
    /// `fallback` is the engine's global object, which is the one source a name
    /// this object does not hold is asked of.
    pub fn over(fallback: Option<Rc<CHandle>>) -> Self {
        Self {
            members: CustomObject::new(Vec::new()),
            natives: NativeRegistry::over(fallback.clone()),
            removed: RefCell::new(BTreeSet::new()),
            fallback,
        }
    }

    /// Whether the registry publishes the name and the script has not deleted
    /// it.
    fn is_published(&self, name: &TjsString) -> bool {
        !self.removed.borrow().contains(name) && self.natives.contains(name)
    }

    /// The class object the name denotes, when the object publishes it.
    fn published(&self, name: &TjsString) -> Option<Variant> {
        if !self.is_published(name) {
            return None;
        }
        self.natives.get(name)
    }

    /// The class names the object publishes, with their class objects.
    ///
    /// # Errors
    ///
    /// [`ObjectError::CallNotServed`] when a name the registry lists does not
    /// resolve, which its own key list makes impossible; the walk reports it
    /// rather than leaving the name out.
    fn published_entries(
        &self,
        flags: MemberFlags,
    ) -> Result<Vec<(TjsString, Variant)>, ObjectError> {
        let mut entries = Vec::new();
        for published in self.natives.names() {
            let name = TjsString::from(published);
            if !self.is_published(&name) {
                continue;
            }
            let value = if flags.contains(MemberFlags::ENUM_NO_VALUE) {
                Variant::Void
            } else {
                match self.natives.get(&name) {
                    Some(value) => value,
                    None => return Err(ObjectError::CallNotServed),
                }
            };
            entries.push((name, value));
        }
        Ok(entries)
    }

    /// Drops this object's member of `name`, when it holds one.
    ///
    /// # Errors
    ///
    /// [`ObjectError::InvalidObject`] when the object has been invalidated; a
    /// name the object does not hold is not a failure, because the caller is
    /// removing a member the engine now holds.
    fn drop_member(&self, name: &TjsString) -> Result<(), ObjectError> {
        match self.members.delete_member(name, MemberFlags::NONE, None) {
            Ok(()) | Err(ObjectError::MemberNotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Default for GlobalObject {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatch for GlobalObject {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        // a class name is a member of this object, so a read that creates the
        // member it names finds it instead of shadowing the class object with an
        // empty member
        if flags.contains(MemberFlags::ENSURE)
            && let Some(value) = self.published(name)
        {
            return Ok(value);
        }
        match self.members.prop_get(name, flags, objthis) {
            Ok(value) => return Ok(value),
            Err(ObjectError::MemberNotFound | ObjectError::MemberMustExist) => {}
            Err(error) => return Err(error),
        }
        if let Some(value) = self.published(name) {
            return Ok(value);
        }
        if let Some(fallback) = &self.fallback {
            match fallback.prop_get(name, flags, objthis) {
                Ok(value) => return Ok(value),
                Err(ObjectError::MemberNotFound | ObjectError::MemberMustExist) => {}
                Err(error) => return Err(error),
            }
        }
        Err(absent(flags))
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        // the object is the one every source is read through, so a write it
        // cannot take is refused before any source changes
        if !self.members.is_valid() {
            return Err(ObjectError::InvalidObject);
        }
        // a name this object answers is a member the write reaches, so a write
        // that does not ask for the member to be created finds it: the class
        // object a published name stands for becomes its first value
        if self.is_published(name) {
            if let Some(class) = self.published(name) {
                self.members
                    .prop_set(name, MemberFlags::ENSURE, &class, objthis)?;
            }
            return self.members.prop_set(name, flags, value, objthis);
        }
        let Some(fallback) = &self.fallback else {
            return self.members.prop_set(name, flags, value, objthis);
        };
        // a value the engine cannot address has no home but this object
        if unaddressable(value) {
            return self.members.prop_set(name, flags, value, objthis);
        }
        fallback.prop_set(name, flags, value, objthis)?;
        self.drop_member(name)
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        let published = self.is_published(name);
        let stored = match self.members.delete_member(name, flags, objthis) {
            Ok(()) => true,
            Err(ObjectError::MemberNotFound | ObjectError::MemberMustExist) => false,
            Err(error) => return Err(error),
        };
        if published {
            self.removed.borrow_mut().insert(name.clone());
        }
        let engine = match &self.fallback {
            Some(fallback) => match fallback.delete_member(name, flags, objthis) {
                Ok(()) => true,
                Err(ObjectError::MemberNotFound | ObjectError::MemberMustExist) => false,
                Err(error) => return Err(error),
            },
            None => false,
        };
        if published || stored || engine {
            return Ok(());
        }
        Err(absent(flags))
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        let mut seen: BTreeSet<TjsString> = BTreeSet::new();
        let mut stopped = false;
        self.members
            .enum_members(flags, &mut |name, member_flags, value| {
                seen.insert(name.clone());
                let keep = callback(name, member_flags, value);
                stopped = !keep;
                keep
            })?;
        if stopped {
            return Ok(());
        }
        for (name, value) in self.published_entries(flags)? {
            if seen.contains(&name) {
                continue;
            }
            if !callback(&name, 0, &value) {
                return Ok(());
            }
        }
        let Some(fallback) = &self.fallback else {
            return Ok(());
        };
        // the engine's names, without the ones this object has already
        // answered and without the class names a script deleted
        fallback.enum_members(flags, &mut |name, member_flags, value| {
            if seen.contains(name) || self.removed.borrow().contains(name) {
                return true;
            }
            callback(name, member_flags, value)
        })
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        self.members.is_instance_of(class, objthis)
    }

    fn class_names(&self) -> Vec<TjsString> {
        self.members.class_names()
    }

    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.members.add_class_name(name)
    }

    /// The engine's retained id for the global object, when there is one behind
    /// this object.
    ///
    /// The script-visible global is the engine's global with these members in
    /// front of it, so a value that holds this object can cross into an engine
    /// call as that object.
    fn engine_id(&self) -> Option<Tjs2ValueId> {
        self.fallback.as_ref().map(|fallback| fallback.id())
    }

    /// The member a named call reaches through the scope's own store.
    ///
    /// The frame's `this` view asks this before its call entry, so a function a
    /// script declared at the top level is called by the value this answers. Only
    /// a value whose call can run — an object — is answered here: a name of a
    /// value that is not callable is left to the call entry, which reports the
    /// name with the refusal.
    fn rust_member(&self, name: &TjsString) -> Option<Variant> {
        let value = match self.members.prop_get(name, MemberFlags::NONE, None) {
            Ok(value) => value,
            Err(
                ObjectError::MemberNotFound
                | ObjectError::MemberMustExist
                | ObjectError::CallNotServed
                | ObjectError::ParameterCount
                | ObjectError::NotImplemented
                | ObjectError::InvalidObject,
            ) => return None,
        };
        match value {
            Variant::Object(_) => Some(value),
            Variant::Void
            | Variant::Integer(_)
            | Variant::Real(_)
            | Variant::String(_)
            | Variant::Octet(_) => None,
        }
    }

    /// The member a named call reaches, resolved through this object's own
    /// order.
    ///
    /// A named call reads the name and calls the value it reached, so the lookup
    /// is [`Self::prop_get`]: the object's own members, then the class objects,
    /// then the engine's global object. The executor calls the value, which is
    /// how a `function` a script declared at the top level is callable as
    /// `this.f(...)`. A lookup that fails travels as the failure it is, so a
    /// member that takes another number of arguments is not reported as a name
    /// that does not exist.
    fn func_member(
        &self,
        name: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Option<Result<Variant, ObjectError>> {
        Some(self.prop_get(name, MemberFlags::NONE, objthis))
    }

    fn is_valid(&self) -> bool {
        self.members.is_valid()
    }

    fn invalidate(&self) -> bool {
        self.members.invalidate()
    }

    /// A call that names no member is refused: the global object is not a
    /// function, and a named call is the member's own ([`Self::func_member`]).
    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        if name.is_empty() {
            return Err(ObjectError::CallNotServed);
        }
        match &self.fallback {
            Some(fallback) => fallback.func_call(name, argv, objthis),
            None => Err(ObjectError::CallNotServed),
        }
    }
}

impl fmt::Debug for GlobalObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GlobalObject")
            .field("members", &self.members.len())
            .field("natives", &self.natives.names().count())
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}
