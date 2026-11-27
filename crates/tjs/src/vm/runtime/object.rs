//! The member surface of an object, and the custom object that holds members.
//!
//! A script object is reached through one surface: read a member, write one,
//! delete one, enumerate them, ask whether it belongs to a class, list its
//! class names, and invalidate it. [`Dispatch`] is that surface in Rust terms,
//! and [`CustomObject`] is the object every script dictionary and every plain
//! object is: a member map with a value per member.
//!
//! # The member record
//!
//! A member holds a value. A member can also carry a **property**: a getter
//! and a setter that stand in for the value, where the value is what the
//! member reads as when the property is not invoked. A read invokes the
//! getter unless the flags ignore properties; a write invokes the setter the
//! same way, and a property that answers "not served" falls back to the
//! member's value, which is also what a member without a property simply
//! holds.
//!
//! # The flags
//!
//! [`MemberFlags`] carries what the surface is asked to do: create a member
//! that is not there, require one that is, ignore a member's property, mark a
//! member hidden or static, and skip values while enumerating. The rules the
//! flags select are in [`CustomObject`]'s implementations.
//!
//! # The property seam
//!
//! A property's getter and setter are [`PropertyCall`] trait objects. The
//! object model knows only that a read answers a value and a write takes one;
//! the interpreter implements the trait in terms of a script closure. That is
//! the one seam this part leaves open, and it is why a member whose *value* is
//! an object is not served by that object's default member here: a property is
//! a member record carrying calls, never a callable object.

use std::cell::{Cell, RefCell};
use std::ops::BitOr;
use std::rc::Rc;

use super::member::MemberMap;
use super::number::Number;
use super::string::TjsString;
use super::variant::Variant;
use crate::Tjs2ValueId;

/// What a member surface call is asked to do.
///
/// The bits are the ones the engine's surface uses, so a caller mirroring an
/// engine flag value can build one with [`MemberFlags::from_bits`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemberFlags(u32);

impl MemberFlags {
    /// No flag: a plain call.
    pub const NONE: Self = Self(0);

    /// Create the member when it is not there yet.
    pub const ENSURE: Self = Self(0x0000_0200);

    /// The member must exist; the call fails when it does not.
    pub const MUST_EXIST: Self = Self(0x0000_0400);

    /// Read or write the member's value without invoking its property.
    pub const IGNORE_PROPERTY: Self = Self(0x0000_0800);

    /// The member is hidden.
    pub const HIDDEN: Self = Self(0x0000_1000);

    /// The member is static.
    pub const STATIC: Self = Self(0x0001_0000);

    /// Enumerating does not retrieve the members' values.
    pub const ENUM_NO_VALUE: Self = Self(0x0010_0000);

    /// The flags the given bits name.
    pub fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// The bits.
    pub fn bits(self) -> u32 {
        self.0
    }

    /// Whether every flag of `other` is set.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for MemberFlags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// The failure of a member surface call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ObjectError {
    /// The name names no member.
    #[error("the member does not exist")]
    MemberNotFound,

    /// The name names no member, and the caller required one.
    #[error("the member must exist")]
    MemberMustExist,

    /// The object cannot serve the call: a property answered that it does not
    /// have the read or the write, or a value was asked to be invoked or
    /// constructed and it is not a function or a class.
    #[error("the object cannot serve the call")]
    CallNotServed,

    /// The object has been invalidated.
    #[error("the object is invalid")]
    InvalidObject,
}

/// The call a property member makes for a read or a write.
///
/// The interpreter implements this in terms of a script closure; the object
/// model knows only that a read answers a value and a write takes one.
pub trait PropertyCall {
    /// Calls the property.
    ///
    /// `value` is what a write stores and `None` for a read; `objthis` is the
    /// receiver the member was reached on. A read answers the value the
    /// property produced, and a write's answer is not used. A call the
    /// property cannot serve answers [`ObjectError::CallNotServed`], and the
    /// member's stored value stands in for it; any other error travels to the
    /// caller unchanged.
    fn call(
        &self,
        value: Option<&Variant>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError>;
}

/// A property member's calls; either of them may be absent.
#[derive(Clone, Default)]
pub struct Property {
    /// The getter, called by a read.
    getter: Option<Rc<dyn PropertyCall>>,
    /// The setter, called by a write.
    setter: Option<Rc<dyn PropertyCall>>,
}

impl Property {
    /// A property with the given getter and setter.
    pub fn new(getter: Option<Rc<dyn PropertyCall>>, setter: Option<Rc<dyn PropertyCall>>) -> Self {
        Self { getter, setter }
    }

    /// The value a read answers: the getter's, or the member's when the
    /// getter is absent or answers that it cannot serve the read.
    fn read(
        &self,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let Some(getter) = &self.getter else {
            return Ok(value.clone());
        };
        match getter.call(None, objthis) {
            Ok(answer) => Ok(answer),
            Err(ObjectError::CallNotServed) => Ok(value.clone()),
            Err(error) => Err(error),
        }
    }

    /// Whether a write goes to the setter.
    ///
    /// Answers `Ok(true)` when the setter took the write, `Ok(false)` when the
    /// member's value takes it, and the setter's error otherwise.
    fn write(&self, value: &Variant, objthis: Option<&dyn Dispatch>) -> Result<bool, ObjectError> {
        let Some(setter) = &self.setter else {
            return Ok(false);
        };
        match setter.call(Some(value), objthis) {
            Ok(_) => Ok(true),
            Err(ObjectError::CallNotServed) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// One member of a [`CustomObject`].
#[derive(Clone)]
struct Member {
    /// The value the member holds, and what it reads as when its property is
    /// not invoked.
    value: Variant,
    /// The property's calls, when the member is a property.
    property: Option<Property>,
    /// Whether the member is hidden.
    hidden: bool,
    /// Whether the member is static.
    is_static: bool,
}

impl Member {
    /// A member that holds a value and nothing else.
    fn value(value: Variant) -> Self {
        Self {
            value,
            property: None,
            hidden: false,
            is_static: false,
        }
    }

    /// The flags an enumeration reports for the member.
    fn flags(&self) -> u32 {
        let mut flags = MemberFlags::NONE;
        if self.hidden {
            flags = flags | MemberFlags::HIDDEN;
        }
        if self.is_static {
            flags = flags | MemberFlags::STATIC;
        }
        flags.bits()
    }
}

/// The member surface of an object.
///
/// Every call answers a typed failure instead of a fabricated value: a member
/// that is not there, a member the caller required, a call a property cannot
/// serve, and an invalidated object are told apart by [`ObjectError`].
pub trait Dispatch {
    /// Reads the member `name`.
    ///
    /// When the member is a property and `flags` does not contain
    /// [`MemberFlags::IGNORE_PROPERTY`], the getter answers the value.
    /// [`MemberFlags::ENSURE`] creates the member as an empty value first.
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError>;

    /// Writes `value` to the member `name`.
    ///
    /// [`MemberFlags::ENSURE`] creates the member, and
    /// [`MemberFlags::MUST_EXIST`] makes an absent member the failure
    /// [`ObjectError::MemberMustExist`] instead. When the member is a property
    /// and the flags do not ignore properties, the setter takes the write.
    /// [`MemberFlags::HIDDEN`] and [`MemberFlags::STATIC`] set the member's
    /// reported flags.
    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError>;

    /// Removes the member `name`.
    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError>;

    /// Walks the members, answering each name, its flags and its value.
    ///
    /// The walk stops when the callback answers `false`. With
    /// [`MemberFlags::ENUM_NO_VALUE`] no value is retrieved and the callback
    /// receives the empty variant; otherwise a property's getter answers,
    /// unless the flags ignore properties.
    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError>;

    /// Whether the object belongs to `class`.
    ///
    /// Every object belongs to `"Object"`; the other answers come from the
    /// class names the object carries.
    fn is_instance_of(
        &self,
        class: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError>;

    /// The class names the object carries.
    fn class_names(&self) -> Vec<TjsString>;

    /// Adds a class name to the object.
    ///
    /// The names an object carries are how a class question is answered, so an
    /// object that cannot hold one cannot answer it. Every object can: the name
    /// is appended to the list the object already keeps, and both the list and
    /// the class question read that one list.
    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError>;

    /// The engine's retained id for the object, when the object stands for
    /// one.
    ///
    /// An object the engine owns — the one a script's value names, reached
    /// through the engine's interface — answers the id the engine knows it
    /// by, so a value that holds it can cross into an engine call. Every
    /// object the Rust object model owns answers `None`: it has no engine id,
    /// and none is invented from its address, because an address is not a
    /// handle.
    fn engine_id(&self) -> Option<Tjs2ValueId> {
        None
    }

    /// The member this value keeps outside the engine, when it has one.
    ///
    /// An engine object cannot hold a value of the Rust object model. A handle
    /// that carries such a member answers it here, so a call that names it can
    /// reach the code the engine cannot store.
    fn rust_member(&self, _name: &TjsString) -> Option<Variant> {
        None
    }

    /// Whether the object is still usable.
    fn is_valid(&self) -> bool;

    /// Invalidates the object and drops its members.
    ///
    /// Answers whether the call changed anything: an object that was already
    /// invalid stays as it is.
    fn invalidate(&self) -> bool;

    /// Invokes the value as a function.
    ///
    /// `name` is the member the invocation names, or the empty name when the
    /// value itself is invoked; `argv` is the argument list. An object that is
    /// not a function cannot serve the call and answers
    /// [`ObjectError::CallNotServed`], which is the engine's own refusal for a
    /// value it cannot invoke.
    ///
    /// A value whose call runs code is not served here: the caller asks
    /// [`Self::context`] first and runs the code itself.
    fn func_call(
        &self,
        _name: &TjsString,
        _argv: Option<&[Variant]>,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        Err(ObjectError::CallNotServed)
    }

    /// The script context this object's calls run, when its calls run code.
    ///
    /// A context is code as well as a member surface: its call needs a frame, a
    /// receiver held as a value, and the machine that runs it, none of which this
    /// surface carries. A caller that has them asks this before it asks
    /// [`Self::func_call`], so the value whose call is code is found by the
    /// machine that can run it. A view that stands in front of a context answers
    /// the same call, because the name a call reaches is looked up through the
    /// view.
    fn context(&self) -> Option<&crate::vm::runtime::exec::ContextObject> {
        None
    }

    /// Constructs an object.
    ///
    /// `name` is the member the construction names, or the empty name when the
    /// object itself is the class; `argv` is the argument list. An object that
    /// is not a class cannot serve the call and answers
    /// [`ObjectError::CallNotServed`], which is the engine's own refusal.
    fn create_new(
        &self,
        _name: &TjsString,
        _argv: Option<&[Variant]>,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        Err(ObjectError::CallNotServed)
    }

    /// Applies a compound assignment to a member.
    ///
    /// `op` names the operator and `value` is the right operand. `Ok(false)`
    /// means the object does **not serve** the operator — not that the
    /// operator failed: the caller then reads the member, applies the operator
    /// itself and writes the result back, which is what the engine's base
    /// object does. `Ok(true)` means the object applied it, and an error means
    /// it was attempted and failed.
    fn operation(
        &self,
        _op: &TjsString,
        _value: &Variant,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        Ok(false)
    }

    /// Reads the member a number names.
    ///
    /// The default answers the number's decimal text, which is the engine's
    /// base rule; a container overrides it to answer an index without building
    /// the text.
    fn prop_get_by_num(
        &self,
        index: i64,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let name = TjsString::from(&Number::Integer(index));
        self.prop_get(&name, flags, objthis)
    }

    /// Writes the member a number names.
    ///
    /// The default writes the number's decimal text, which is the engine's
    /// base rule; a container overrides it to write an index without building
    /// the text.
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

/// The object a script dictionary and a plain script object both are.
///
/// The members are a [`MemberMap`], so enumeration follows the object's bucket
/// order; see that module for the order itself. The map is behind a
/// [`RefCell`] because the surface takes `&self` and a write changes the
/// members; no borrow is held while a property's call runs, so a property may
/// reach its own object again.
pub struct CustomObject {
    /// The members, in the object's enumeration order.
    members: RefCell<MemberMap<Member>>,
    /// The class names the object answers to, `"Object"` aside.
    class_names: RefCell<Vec<TjsString>>,
    /// Whether the object has been invalidated.
    invalid: Cell<bool>,
}

impl CustomObject {
    /// An object with no member that carries the given class names.
    pub fn new(class_names: impl IntoIterator<Item = TjsString>) -> Self {
        Self {
            members: RefCell::new(MemberMap::new()),
            class_names: RefCell::new(class_names.into_iter().collect()),
            invalid: Cell::new(false),
        }
    }

    /// Stores a property under `name`.
    ///
    /// `value` is what the member reads as when the property is not invoked,
    /// and the two calls stand in for the reads and the writes that do invoke
    /// it.
    pub fn define_property(
        &self,
        name: TjsString,
        value: Variant,
        getter: Option<Rc<dyn PropertyCall>>,
        setter: Option<Rc<dyn PropertyCall>>,
    ) -> Result<(), ObjectError> {
        self.check_valid()?;
        let member = Member {
            value,
            property: Some(Property::new(getter, setter)),
            hidden: false,
            is_static: false,
        };
        self.members.borrow_mut().insert(name, member);
        Ok(())
    }

    /// The number of members the object holds.
    pub fn len(&self) -> usize {
        self.members.borrow().len()
    }

    /// Whether the object holds no member.
    pub fn is_empty(&self) -> bool {
        self.members.borrow().is_empty()
    }

    /// The failure an invalidated object answers with.
    fn check_valid(&self) -> Result<(), ObjectError> {
        if self.invalid.get() {
            return Err(ObjectError::InvalidObject);
        }
        Ok(())
    }

    /// The member `name`: if it is there, and whether the flags create it.
    ///
    /// A read and a write both need this step: it answers the member's value
    /// and property (cloned out of the map, so no borrow is held afterwards),
    /// and it creates the member when the flags allow and require it.
    fn reach(
        &self,
        name: &TjsString,
        flags: MemberFlags,
    ) -> Result<(Variant, Option<Property>), ObjectError> {
        let present = self.members.borrow().get(name).is_some();
        if !present {
            if flags.contains(MemberFlags::MUST_EXIST) {
                return Err(ObjectError::MemberMustExist);
            }
            if !flags.contains(MemberFlags::ENSURE) {
                return Err(ObjectError::MemberNotFound);
            }
            self.members
                .borrow_mut()
                .insert(name.clone(), Member::value(Variant::Void));
        }
        let members = self.members.borrow();
        let member = members.get(name).ok_or(ObjectError::MemberNotFound)?;
        Ok((member.value.clone(), member.property.clone()))
    }

    /// The value a member reads as, applying its property unless the flags
    /// ignore properties.
    fn read(
        &self,
        value: &Variant,
        property: &Option<Property>,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        match property {
            Some(property) if !flags.contains(MemberFlags::IGNORE_PROPERTY) => {
                property.read(value, objthis)
            }
            _ => Ok(value.clone()),
        }
    }
}

impl Dispatch for CustomObject {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.check_valid()?;
        let (value, property) = self.reach(name, flags)?;
        self.read(&value, &property, flags, objthis)
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.check_valid()?;
        let (_, property) = self.reach(name, flags)?;
        {
            let mut members = self.members.borrow_mut();
            let member = members.get_mut(name).ok_or(ObjectError::MemberNotFound)?;
            member.hidden = flags.contains(MemberFlags::HIDDEN);
            member.is_static = flags.contains(MemberFlags::STATIC);
        }
        if let Some(property) = &property
            && !flags.contains(MemberFlags::IGNORE_PROPERTY)
            && property.write(value, objthis)?
        {
            return Ok(());
        }
        let mut members = self.members.borrow_mut();
        let member = members.get_mut(name).ok_or(ObjectError::MemberNotFound)?;
        member.value = value.clone();
        Ok(())
    }

    fn delete_member(
        &self,
        name: &TjsString,
        _flags: MemberFlags,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.check_valid()?;
        if self.members.borrow_mut().remove(name) {
            return Ok(());
        }
        Err(ObjectError::MemberNotFound)
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        self.check_valid()?;
        // The members are cloned out before a callback or a getter runs, so no
        // borrow is held across either: a property and a callback may reach
        // this object again, and a member added or dropped meanwhile does not
        // join or leave this walk.
        let mut members = Vec::new();
        for (name, member) in self.members.borrow().iter() {
            members.push((name.clone(), member.flags(), member.clone()));
        }
        for (name, member_flags, member) in &members {
            let value = if flags.contains(MemberFlags::ENUM_NO_VALUE) {
                Variant::Void
            } else {
                self.read(&member.value, &member.property, flags, None)?
            };
            if !callback(name, *member_flags, &value) {
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
        self.check_valid()?;
        let object_class = TjsString::from("Object");
        if *class == object_class {
            return Ok(true);
        }
        Ok(self.class_names.borrow().contains(class))
    }

    fn class_names(&self) -> Vec<TjsString> {
        self.class_names.borrow().clone()
    }

    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.check_valid()?;
        self.class_names.borrow_mut().push(name.clone());
        Ok(())
    }

    fn is_valid(&self) -> bool {
        !self.invalid.get()
    }

    fn invalidate(&self) -> bool {
        if self.invalid.get() {
            return false;
        }
        self.invalid.set(true);
        self.members.borrow_mut().clear();
        true
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::{CustomObject, Dispatch, MemberFlags, ObjectError, PropertyCall};
    use crate::vm::runtime::string::TjsString;
    use crate::vm::runtime::variant::Variant;

    // Citations name reference/krkr2/cpp/core/tjs2/tjsObject.cpp unless the
    // line says otherwise.

    /// A name.
    fn name(text: &str) -> TjsString {
        TjsString::from(text)
    }

    /// A getter that answers a fixed integer and counts its calls.
    struct Getter {
        /// What a read answers.
        answer: i64,
        /// How many times the getter ran.
        calls: Rc<Cell<usize>>,
    }

    impl PropertyCall for Getter {
        fn call(
            &self,
            value: Option<&Variant>,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<Variant, ObjectError> {
            assert!(value.is_none(), "a get is called without a value");
            self.calls.set(self.calls.get() + 1);
            Ok(Variant::from(self.answer))
        }
    }

    /// A setter that records the integer it was given.
    struct Setter {
        /// The last value a write handed over.
        seen: Rc<Cell<i64>>,
    }

    impl PropertyCall for Setter {
        fn call(
            &self,
            value: Option<&Variant>,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<Variant, ObjectError> {
            match value {
                Some(Variant::Integer(value)) => {
                    self.seen.set(*value);
                    Ok(Variant::Void)
                }
                _ => Err(ObjectError::CallNotServed),
            }
        }
    }

    /// A property that refuses every call.
    struct Refusing;

    impl PropertyCall for Refusing {
        fn call(
            &self,
            _value: Option<&Variant>,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<Variant, ObjectError> {
            Err(ObjectError::CallNotServed)
        }
    }

    /// The integer a variant holds, for comparing answers without a
    /// `PartialEq` on the variant.
    fn integer(value: &Variant) -> Option<i64> {
        match value {
            Variant::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// The error a call answered, panicking with what it answered instead.
    ///
    /// The variant has no equality of its own — the language's is `equals` —
    /// so a test that expects an error matches the error and reports the value
    /// it got instead.
    fn error_of<T: std::fmt::Debug>(result: Result<T, ObjectError>) -> ObjectError {
        match result {
            Ok(value) => panic!("the call answered {value:?} instead of an error"),
            Err(error) => error,
        }
    }

    /// The members an object holds, as the enumeration answers them.
    fn names(object: &CustomObject) -> Vec<String> {
        let mut names = Vec::new();
        object
            .enum_members(MemberFlags::ENUM_NO_VALUE, &mut |name, _, _| {
                names.push(name.to_string());
                true
            })
            .expect("the object enumerates");
        names.sort();
        names
    }

    /// A class name appended after the object was built is read by both readers
    /// of the store: the list itself and the class question
    /// (`reference/krkr2/cpp/core/tjs2/tjsObject.cpp:2236-2246`).
    #[test]
    fn an_appended_class_name_is_read_by_both_readers() {
        let object = CustomObject::new([TjsString::from("Base")]);
        object
            .add_class_name(&TjsString::from("Added"))
            .expect("the name is added");
        assert_eq!(
            object.class_names(),
            vec![TjsString::from("Base"), TjsString::from("Added")],
            "the list keeps the order the names arrived in"
        );
        assert_eq!(
            object.is_instance_of(&TjsString::from("Added"), None),
            Ok(true)
        );
        assert_eq!(
            object.is_instance_of(&TjsString::from("Base"), None),
            Ok(true)
        );
        assert_eq!(
            object.is_instance_of(&TjsString::from("Missing"), None),
            Ok(false)
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1276-1311
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1352-1414
    #[test]
    fn a_value_member_is_written_and_read() {
        let object = CustomObject::new(Vec::new());
        assert_eq!(
            object.prop_set(&name("a"), MemberFlags::ENSURE, &Variant::from(1i64), None),
            Ok(())
        );
        assert_eq!(
            object
                .prop_get(&name("a"), MemberFlags::NONE, None)
                .map(|value| integer(&value)),
            Ok(Some(1))
        );
        assert_eq!(
            object
                .prop_get(&name("a"), MemberFlags::IGNORE_PROPERTY, None)
                .map(|value| integer(&value)),
            Ok(Some(1))
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1296-1304
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1376-1385
    #[test]
    fn a_member_is_created_when_the_flags_allow_it() {
        let object = CustomObject::new(Vec::new());
        assert_eq!(
            error_of(object.prop_get(&name("a"), MemberFlags::NONE, None)),
            ObjectError::MemberNotFound
        );
        // an ensure write creates the member
        assert_eq!(
            object.prop_set(&name("b"), MemberFlags::ENSURE, &Variant::from(7i64), None),
            Ok(())
        );
        assert_eq!(
            object
                .prop_get(&name("b"), MemberFlags::NONE, None)
                .map(|value| integer(&value)),
            Ok(Some(7))
        );
        // an ensure read creates it as the empty value
        assert_eq!(
            object
                .prop_get(&name("c"), MemberFlags::ENSURE, None)
                .map(|value| matches!(value, Variant::Void)),
            Ok(true)
        );
        assert_eq!(
            object
                .prop_get(&name("c"), MemberFlags::NONE, None)
                .map(|value| matches!(value, Variant::Void)),
            Ok(true)
        );
        assert_eq!(names(&object), vec![String::from("b"), String::from("c")]);
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1292-1308
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2602-2638
    #[test]
    fn the_three_member_states_stay_apart() {
        let object = CustomObject::new(Vec::new());
        object
            .prop_set(&name("v"), MemberFlags::ENSURE, &Variant::Void, None)
            .expect("the member is stored");
        object
            .prop_set(&name("x"), MemberFlags::ENSURE, &Variant::from(1i64), None)
            .expect("the member is stored");
        // a present member holding the empty variant reads as that variant
        assert!(
            matches!(
                object.prop_get(&name("v"), MemberFlags::NONE, None),
                Ok(Variant::Void)
            ),
            "a void member is present and empty"
        );
        // a present member holding a value reads as its kind
        assert_eq!(
            object
                .prop_get(&name("x"), MemberFlags::NONE, None)
                .map(|value| value.type_name()),
            Ok("Integer")
        );
        // a member that is not there is a failure, not the empty variant
        assert_eq!(
            error_of(object.prop_get(&name("absent"), MemberFlags::NONE, None)),
            ObjectError::MemberNotFound,
            "an absent member is not a void member"
        );
        // the ensure flag still creates it, and then it reads as the empty
        // variant
        assert!(
            matches!(
                object.prop_get(&name("absent"), MemberFlags::ENSURE, None),
                Ok(Variant::Void)
            ),
            "the ensure flag creates the member"
        );
        assert!(
            matches!(
                object.prop_get(&name("absent"), MemberFlags::NONE, None),
                Ok(Variant::Void)
            ),
            "the created member is a void member"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:144-168
    #[test]
    fn the_by_number_default_uses_the_decimal_text() {
        let object = CustomObject::new(Vec::new());
        object
            .prop_set(&name("3"), MemberFlags::ENSURE, &Variant::from(30i64), None)
            .expect("the member is stored");
        // a numeric subscript is the member its decimal text names
        assert_eq!(
            object
                .prop_get_by_num(3, MemberFlags::NONE, None)
                .map(|value| integer(&value)),
            Ok(Some(30))
        );
        assert_eq!(
            object.prop_set_by_num(4, MemberFlags::ENSURE, &Variant::from(40i64), None),
            Ok(())
        );
        assert_eq!(
            object
                .prop_get(&name("4"), MemberFlags::NONE, None)
                .map(|value| integer(&value)),
            Ok(Some(40))
        );
        // a number the object holds no member for is the same miss a name is
        assert_eq!(
            error_of(object.prop_get_by_num(5, MemberFlags::NONE, None)),
            ObjectError::MemberNotFound
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1203-1208
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1674-1678
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:224-244
    #[test]
    fn an_object_that_is_not_a_function_refuses_the_call() {
        let object = CustomObject::new(Vec::new());
        // the base object is not a function and is not a class
        assert_eq!(
            error_of(object.func_call(&TjsString::new(), None, None)),
            ObjectError::CallNotServed
        );
        assert_eq!(
            error_of(object.create_new(&TjsString::new(), None, None)),
            ObjectError::CallNotServed
        );
        // an operator is not served, and that is not a failure
        assert_eq!(
            object.operation(&name("add"), &Variant::from(1i64), None),
            Ok(false)
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:739-743
    #[test]
    fn a_member_that_must_exist_is_not_created() {
        let object = CustomObject::new(Vec::new());
        assert_eq!(
            object.prop_set(
                &name("a"),
                MemberFlags::MUST_EXIST | MemberFlags::ENSURE,
                &Variant::from(1i64),
                None
            ),
            Err(ObjectError::MemberMustExist)
        );
        assert!(object.is_empty());
        // the same rule on a read
        assert_eq!(
            error_of(object.prop_get(&name("a"), MemberFlags::MUST_EXIST, None)),
            ObjectError::MemberMustExist
        );
        assert!(object.is_empty());
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1233-1275
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1315-1349
    #[test]
    fn a_property_takes_precedence_over_its_value() {
        let object = CustomObject::new(Vec::new());
        let calls = Rc::new(Cell::new(0));
        let seen = Rc::new(Cell::new(0));
        object
            .define_property(
                name("p"),
                Variant::from(1i64),
                Some(Rc::new(Getter {
                    answer: 41,
                    calls: Rc::clone(&calls),
                })),
                Some(Rc::new(Setter {
                    seen: Rc::clone(&seen),
                })),
            )
            .expect("the property is defined");
        // the getter answers, not the value
        assert_eq!(
            object
                .prop_get(&name("p"), MemberFlags::NONE, None)
                .map(|value| integer(&value)),
            Ok(Some(41))
        );
        assert_eq!(calls.get(), 1);
        // the ignore flag reads the value and never calls the getter
        assert_eq!(
            object
                .prop_get(&name("p"), MemberFlags::IGNORE_PROPERTY, None)
                .map(|value| integer(&value)),
            Ok(Some(1))
        );
        assert_eq!(calls.get(), 1);
        // a write goes to the setter and leaves the value alone
        assert_eq!(
            object.prop_set(&name("p"), MemberFlags::NONE, &Variant::from(9i64), None),
            Ok(())
        );
        assert_eq!(seen.get(), 9);
        assert_eq!(
            object
                .prop_get(&name("p"), MemberFlags::IGNORE_PROPERTY, None)
                .map(|value| integer(&value)),
            Ok(Some(1))
        );
        // a write that ignores the property stores the value
        assert_eq!(
            object.prop_set(
                &name("p"),
                MemberFlags::IGNORE_PROPERTY,
                &Variant::from(5i64),
                None
            ),
            Ok(())
        );
        assert_eq!(seen.get(), 9);
        assert_eq!(
            object
                .prop_get(&name("p"), MemberFlags::IGNORE_PROPERTY, None)
                .map(|value| integer(&value)),
            Ok(Some(5))
        );
        // the getter still answers
        assert_eq!(
            object
                .prop_get(&name("p"), MemberFlags::NONE, None)
                .map(|value| integer(&value)),
            Ok(Some(41))
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1065-1093
    #[test]
    fn a_property_that_declines_falls_back_to_the_value() {
        let object = CustomObject::new(Vec::new());
        object
            .define_property(
                name("p"),
                Variant::from(3i64),
                Some(Rc::new(Refusing)),
                Some(Rc::new(Refusing)),
            )
            .expect("the property is defined");
        assert_eq!(
            object
                .prop_get(&name("p"), MemberFlags::NONE, None)
                .map(|value| integer(&value)),
            Ok(Some(3))
        );
        assert_eq!(
            object.prop_set(&name("p"), MemberFlags::NONE, &Variant::from(8i64), None),
            Ok(())
        );
        assert_eq!(
            object
                .prop_get(&name("p"), MemberFlags::IGNORE_PROPERTY, None)
                .map(|value| integer(&value)),
            Ok(Some(8))
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1094-1127
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1074-1082
    #[test]
    fn the_enumeration_answers_names_flags_and_values() {
        let object = CustomObject::new(Vec::new());
        object
            .prop_set(&name("a"), MemberFlags::ENSURE, &Variant::from(1i64), None)
            .expect("the member is stored");
        object
            .prop_set(
                &name("h"),
                MemberFlags::ENSURE | MemberFlags::HIDDEN,
                &Variant::from(2i64),
                None,
            )
            .expect("the hidden member is stored");
        let calls = Rc::new(Cell::new(0));
        object
            .define_property(
                name("p"),
                Variant::from(0i64),
                Some(Rc::new(Getter {
                    answer: 5,
                    calls: Rc::clone(&calls),
                })),
                None,
            )
            .expect("the property is defined");
        // with values: the property's getter answers, and a hidden member
        // reports its flag
        let mut seen: Vec<(String, u32, Option<i64>)> = Vec::new();
        object
            .enum_members(MemberFlags::NONE, &mut |name, flags, value| {
                seen.push((name.to_string(), flags, integer(value)));
                true
            })
            .expect("the object enumerates");
        assert_eq!(seen.len(), 3);
        assert_eq!(calls.get(), 1);
        assert!(
            seen.iter()
                .any(|(name, flags, _)| { name == "h" && *flags == MemberFlags::HIDDEN.bits() })
        );
        assert!(
            seen.iter()
                .any(|(name, _, value)| name == "p" && *value == Some(5))
        );
        // without values: nothing is retrieved and the getter is not called
        let mut seen_names = Vec::new();
        object
            .enum_members(MemberFlags::ENUM_NO_VALUE, &mut |name, _, value| {
                assert!(matches!(value, Variant::Void));
                seen_names.push(name.to_string());
                true
            })
            .expect("the object enumerates");
        assert_eq!(seen_names.len(), 3);
        assert_eq!(calls.get(), 1);
        // the walk stops when the callback answers false
        let mut one = 0;
        object
            .enum_members(MemberFlags::ENUM_NO_VALUE, &mut |_, _, _| {
                one += 1;
                false
            })
            .expect("the object enumerates");
        assert_eq!(one, 1);
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1537-1552
    #[test]
    fn a_deleted_member_is_gone() {
        let object = CustomObject::new(Vec::new());
        object
            .prop_set(&name("a"), MemberFlags::ENSURE, &Variant::from(1i64), None)
            .expect("the member is stored");
        assert_eq!(
            object.delete_member(&name("a"), MemberFlags::NONE, None),
            Ok(())
        );
        assert_eq!(
            error_of(object.prop_get(&name("a"), MemberFlags::NONE, None)),
            ObjectError::MemberNotFound
        );
        assert_eq!(
            object.delete_member(&name("a"), MemberFlags::NONE, None),
            Err(ObjectError::MemberNotFound)
        );
        assert!(object.is_empty());
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1759-1785
    #[test]
    fn the_class_names_answer_is_instance_of() {
        let object = CustomObject::new([name("Widget"), name("Thing")]);
        assert_eq!(object.is_instance_of(&name("Object"), None), Ok(true));
        assert_eq!(object.is_instance_of(&name("Widget"), None), Ok(true));
        assert_eq!(object.is_instance_of(&name("Thing"), None), Ok(true));
        assert_eq!(object.is_instance_of(&name("Other"), None), Ok(false));
        assert_eq!(object.class_names(), vec![name("Widget"), name("Thing")]);
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1572-1585
    // reference/krkr2/cpp/core/tjs2/tjsObject.h:356
    #[test]
    fn an_invalidated_object_refuses_every_call() {
        let object = CustomObject::new(Vec::new());
        object
            .prop_set(&name("a"), MemberFlags::ENSURE, &Variant::from(1i64), None)
            .expect("the member is stored");
        assert!(object.is_valid());
        assert!(object.invalidate());
        assert!(!object.is_valid());
        // a second invalidation changes nothing
        assert!(!object.invalidate());
        assert_eq!(
            error_of(object.prop_get(&name("a"), MemberFlags::NONE, None)),
            ObjectError::InvalidObject
        );
        assert_eq!(
            object.prop_set(&name("a"), MemberFlags::NONE, &Variant::from(2i64), None),
            Err(ObjectError::InvalidObject)
        );
        assert_eq!(
            object.delete_member(&name("a"), MemberFlags::NONE, None),
            Err(ObjectError::InvalidObject)
        );
        assert_eq!(
            object.enum_members(MemberFlags::NONE, &mut |_, _, _| true),
            Err(ObjectError::InvalidObject)
        );
        assert_eq!(
            object.is_instance_of(&name("Object"), None),
            Err(ObjectError::InvalidObject)
        );
        assert!(object.is_empty());
    }
}
