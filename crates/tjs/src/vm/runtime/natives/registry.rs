//! The native classes the Rust side publishes, and the objects they make.
//!
//! A script reaches a native class through the global object. The global
//! handle answers the names registered here before it asks the engine, so a
//! name that would otherwise resolve to the engine's own class resolves to the
//! Rust one instead.
//!
//! Each class object is built from the member table of the module that owns the
//! class: the names, the arities and the rules stay in that table, and this
//! module only turns a table lookup into the callable value a script reads and
//! the call it makes. The members a class owns itself — its finalizer, its
//! constructor member, `Math.random`, `Math.RandomGenerator` and `RegExp.last`
//! — are added here because the module's table does not hold them.
//!
//! The class registry belongs to one global scope, so a read of a class name
//! answers the same class object every time and a class static such as
//! `RegExp.last` keeps its state for that scope. An instance the registry
//! makes is a Rust object with no engine id; a write of it to the global object
//! stays in the global handle's own store.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use super::date::{self, Date, DateMember};
use super::exception::{CLASS_NAME as EXCEPTION, Exception, ExceptionMember};
use super::math::MathMember;
use super::random::{RandomGenerator, RandomGeneratorMember, Seed, Xorshift};
use super::regexp::{CLASS_NAME as REGEXP, RegExpMember, RegExpObject};
use crate::vm::runtime::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant};

/// The name the arithmetic class is registered under.
const MATH: &str = "Math";

/// The name the generator class is registered under.
const RANDOM_GENERATOR: &str = "RandomGenerator";

/// The call a method value makes.
type MethodCall = dyn Fn(&[Variant]) -> Result<Variant, ObjectError>;

/// The classes the Rust side publishes, by the global name each is read from.
///
/// One registry belongs to one global scope. Its classes are made once, so a
/// name answers one object for the life of the scope.
pub struct NativeRegistry {
    /// The class objects, by the name the global object publishes them under.
    classes: BTreeMap<&'static str, Rc<dyn Dispatch>>,
}

impl NativeRegistry {
    /// The classes a global scope publishes.
    pub fn new() -> Self {
        let last = Rc::new(LastRegExp::default());
        let random_generator: Rc<dyn Dispatch> = class_object(RandomGeneratorClass);
        let math = class_object(MathClass {
            random: Rc::new(RefCell::new(Xorshift::seeded(clock_word()))),
            random_generator: Rc::clone(&random_generator),
        });
        let exception = class_object(ExceptionClass);
        let regexp = class_object(RegExpClass { last });
        let date = class_object(DateClass {
            zone: system_offset(),
        });

        let mut classes = BTreeMap::new();
        classes.insert(MATH, math);
        classes.insert(EXCEPTION, exception);
        classes.insert(REGEXP, regexp);
        classes.insert(date::CLASS_NAME, date);
        Self { classes }
    }

    /// The class object the global name denotes, when it denotes one.
    pub fn get(&self, name: &TjsString) -> Option<Variant> {
        let key = class_key(name)?;
        self.classes
            .get(key)
            .map(|object| object_value(Rc::clone(object)))
    }

    /// Whether the global name denotes a class this registry publishes.
    pub fn contains(&self, name: &TjsString) -> bool {
        class_key(name).is_some()
    }

    /// The names the registry publishes.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.classes.keys().copied()
    }
}

impl Default for NativeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The registry key a name denotes, when the name is one of the global classes.
fn class_key(name: &TjsString) -> Option<&'static str> {
    let units = name.as_utf16();
    [MATH, EXCEPTION, REGEXP, date::CLASS_NAME]
        .iter()
        .copied()
        .find(|candidate| units.iter().copied().eq(candidate.encode_utf16()))
}

/// A class object: the members a class publishes and the instances it makes.
struct ClassObject {
    /// The class's own rules.
    class: Box<dyn Class>,
    /// Members a script added to the class object, and the object's class
    /// names.
    members: CustomObject,
}

/// The rules one class contributes to its class object.
trait Class {
    /// The name the class object carries.
    fn name(&self) -> &'static str;

    /// The member names the class object publishes.
    fn members(&self) -> Vec<&'static str>;

    /// The value a member read answers, when the class publishes the member.
    fn get(&self, name: &TjsString) -> Option<Variant>;

    /// Calls the member `name`.
    fn call(&self, name: &TjsString, arguments: &[Variant]) -> Result<Variant, ObjectError>;

    /// Constructs an instance.
    fn construct(&self, _arguments: &[Variant]) -> Result<Variant, ObjectError> {
        Err(ObjectError::CallNotServed)
    }
}

/// A class object over one class's rules.
fn class_object<T: Class + 'static>(class: T) -> Rc<dyn Dispatch> {
    let name = TjsString::from(class.name());
    Rc::new(ClassObject {
        class: Box::new(class),
        members: CustomObject::new([name]),
    })
}

impl Dispatch for ClassObject {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        match self.members.prop_get(name, flags, objthis) {
            Ok(value) => return Ok(value),
            Err(ObjectError::MemberNotFound | ObjectError::MemberMustExist) => {}
            Err(error) => return Err(error),
        }
        if *name == TjsString::from(self.class.name()) {
            return Ok(refused_method());
        }
        self.class.get(name).ok_or(ObjectError::MemberNotFound)
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.members.prop_set(name, flags, value, objthis)
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.members.delete_member(name, flags, objthis)
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        let mut seen: Vec<TjsString> = Vec::new();
        let mut stopped = false;
        self.members
            .enum_members(flags, &mut |name, member_flags, value| {
                seen.push(name.clone());
                let keep = callback(name, member_flags, value);
                stopped = !keep;
                keep
            })?;
        if stopped {
            return Ok(());
        }
        for name in self.class.members() {
            let name = TjsString::from(name);
            if seen.contains(&name) {
                continue;
            }
            let value = if flags.contains(MemberFlags::ENUM_NO_VALUE) {
                Variant::Void
            } else {
                self.class.get(&name).unwrap_or(Variant::Void)
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
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        if self.members.is_instance_of(class, objthis)? {
            return Ok(true);
        }
        Ok(*class == TjsString::from("Class"))
    }

    fn class_names(&self) -> Vec<TjsString> {
        self.members.class_names()
    }

    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.members.add_class_name(name)
    }

    fn is_valid(&self) -> bool {
        self.members.is_valid()
    }

    fn invalidate(&self) -> bool {
        self.members.invalidate()
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        if name.is_empty() || *name == TjsString::from(self.class.name()) {
            return Err(ObjectError::CallNotServed);
        }
        self.class.call(name, argv.unwrap_or(&[]))
    }

    fn create_new(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        if !name.is_empty() {
            return Err(ObjectError::CallNotServed);
        }
        self.class.construct(argv.unwrap_or(&[]))
    }
}

/// The arithmetic class object.
struct MathClass {
    /// The generator behind `Math.random`.
    random: Rc<RefCell<Xorshift>>,
    /// The generator class `Math.RandomGenerator` reads.
    random_generator: Rc<dyn Dispatch>,
}

impl Class for MathClass {
    fn name(&self) -> &'static str {
        MATH
    }

    fn members(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> =
            MathMember::ALL.iter().map(|member| member.name()).collect();
        names.extend(["random", RANDOM_GENERATOR, "finalize"]);
        names
    }

    fn get(&self, name: &TjsString) -> Option<Variant> {
        if *name == TjsString::from("random") {
            let random = Rc::clone(&self.random);
            return Some(method(move |_arguments| {
                Ok(Variant::Real(random.borrow_mut().next_real()))
            }));
        }
        if *name == TjsString::from(RANDOM_GENERATOR) {
            return Some(object_value(Rc::clone(&self.random_generator)));
        }
        if *name == TjsString::from("finalize") {
            return Some(empty_method());
        }
        let member = MathMember::lookup(name)?;
        if let Some(value) = member.value() {
            return Some(value);
        }
        Some(method(move |arguments| {
            member
                .call(arguments)
                .map_err(|_| ObjectError::CallNotServed)
        }))
    }

    fn call(&self, name: &TjsString, arguments: &[Variant]) -> Result<Variant, ObjectError> {
        if *name == TjsString::from("random") {
            return Ok(Variant::Real(self.random.borrow_mut().next_real()));
        }
        if *name == TjsString::from("finalize") {
            return Ok(Variant::Void);
        }
        if *name == TjsString::from(RANDOM_GENERATOR) {
            return Err(ObjectError::CallNotServed);
        }
        let Some(member) = MathMember::lookup(name) else {
            return Err(ObjectError::CallNotServed);
        };
        if member.constant().is_some() {
            return Err(ObjectError::CallNotServed);
        }
        member
            .call(arguments)
            .map_err(|_| ObjectError::CallNotServed)
    }
}

/// The generator class object.
struct RandomGeneratorClass;

impl Class for RandomGeneratorClass {
    fn name(&self) -> &'static str {
        RANDOM_GENERATOR
    }

    fn members(&self) -> Vec<&'static str> {
        RandomGeneratorMember::ALL
            .iter()
            .map(|member| member.name())
            .collect()
    }

    fn get(&self, name: &TjsString) -> Option<Variant> {
        let member = RandomGeneratorMember::lookup(name)?;
        Some(if member == RandomGeneratorMember::Finalize {
            empty_method()
        } else {
            refused_method()
        })
    }

    fn call(&self, name: &TjsString, _arguments: &[Variant]) -> Result<Variant, ObjectError> {
        if RandomGeneratorMember::lookup(name) == Some(RandomGeneratorMember::Finalize) {
            return Ok(Variant::Void);
        }
        Err(ObjectError::CallNotServed)
    }

    fn construct(&self, arguments: &[Variant]) -> Result<Variant, ObjectError> {
        let generator = RandomGenerator::new(arguments, fresh_seed())
            .map_err(|_| ObjectError::CallNotServed)?;
        Ok(random_generator_value(generator))
    }
}

/// The exception class object.
struct ExceptionClass;

impl Class for ExceptionClass {
    fn name(&self) -> &'static str {
        EXCEPTION
    }

    fn members(&self) -> Vec<&'static str> {
        ExceptionMember::ALL
            .iter()
            .filter(|member| member.is_class_member())
            .map(|member| member.name())
            .collect()
    }

    fn get(&self, name: &TjsString) -> Option<Variant> {
        let member = ExceptionMember::lookup(name)?;
        if member.is_class_member() {
            return Some(empty_method());
        }
        None
    }

    fn call(&self, name: &TjsString, _arguments: &[Variant]) -> Result<Variant, ObjectError> {
        if ExceptionMember::lookup(name).is_some_and(|member| member.is_class_member()) {
            return Ok(Variant::Void);
        }
        Err(ObjectError::CallNotServed)
    }

    fn construct(&self, arguments: &[Variant]) -> Result<Variant, ObjectError> {
        Exception::new(arguments).object()
    }
}

/// The regular-expression class object.
struct RegExpClass {
    /// The instance the last `test` or `exec` ran on.
    last: Rc<LastRegExp>,
}

impl Class for RegExpClass {
    fn name(&self) -> &'static str {
        REGEXP
    }

    fn members(&self) -> Vec<&'static str> {
        RegExpMember::ALL
            .iter()
            .map(|member| member.name())
            .collect()
    }

    fn get(&self, name: &TjsString) -> Option<Variant> {
        let member = RegExpMember::lookup(name)?;
        if member == RegExpMember::Last {
            return Some(self.last.read());
        }
        if member == RegExpMember::Finalize {
            return Some(empty_method());
        }
        if member.arity().is_some() {
            return Some(refused_method());
        }
        None
    }

    fn call(&self, name: &TjsString, _arguments: &[Variant]) -> Result<Variant, ObjectError> {
        if RegExpMember::lookup(name) == Some(RegExpMember::Finalize) {
            return Ok(Variant::Void);
        }
        Err(ObjectError::CallNotServed)
    }

    fn construct(&self, arguments: &[Variant]) -> Result<Variant, ObjectError> {
        tracked_regexp(arguments, Rc::downgrade(&self.last))
    }
}

/// The date class object.
struct DateClass {
    /// The system zone's standard offset, or the failure that reading it gave.
    zone: Result<i64, ZoneError>,
}

impl DateClass {
    /// The offset a class operation places a date at.
    fn offset(&self) -> Result<i64, ObjectError> {
        self.zone.clone().map_err(|_| ObjectError::CallNotServed)
    }
}

impl Class for DateClass {
    fn name(&self) -> &'static str {
        date::CLASS_NAME
    }

    fn members(&self) -> Vec<&'static str> {
        DateMember::ALL.iter().map(|member| member.name()).collect()
    }

    fn get(&self, name: &TjsString) -> Option<Variant> {
        let member = DateMember::lookup(name)?;
        if member == DateMember::Finalize {
            return Some(empty_method());
        }
        if member != DateMember::GetTimezoneOffset {
            return Some(refused_method());
        }
        let zone = self.zone.clone();
        Some(method(move |arguments| {
            let offset = zone.clone().map_err(|_| ObjectError::CallNotServed)?;
            let mut date = Date { seconds: 0, offset };
            DateMember::GetTimezoneOffset
                .call(&mut date, arguments)
                .map_err(|_| ObjectError::CallNotServed)
        }))
    }

    fn call(&self, name: &TjsString, arguments: &[Variant]) -> Result<Variant, ObjectError> {
        let Some(member) = DateMember::lookup(name) else {
            return Err(ObjectError::CallNotServed);
        };
        match member {
            DateMember::Finalize => Ok(Variant::Void),
            DateMember::GetTimezoneOffset => {
                let mut date = Date {
                    seconds: 0,
                    offset: self.offset()?,
                };
                member
                    .call(&mut date, arguments)
                    .map_err(|_| ObjectError::CallNotServed)
            }
            _ => Err(ObjectError::CallNotServed),
        }
    }

    fn construct(&self, arguments: &[Variant]) -> Result<Variant, ObjectError> {
        let date = Date::new(arguments, self.offset()?).map_err(|_| ObjectError::CallNotServed)?;
        Ok(date_value(date))
    }
}

/// The value the last `test` or `exec` recorded for the class object.
#[derive(Default)]
struct LastRegExp {
    /// The instance, or the empty value before the first call.
    value: RefCell<Option<Variant>>,
}

impl LastRegExp {
    /// The value a read of `RegExp.last` answers.
    fn read(&self) -> Variant {
        self.value.borrow().clone().unwrap_or(Variant::Void)
    }

    /// Records the instance a call ran on.
    fn record(&self, value: Variant) {
        *self.value.borrow_mut() = Some(value);
    }
}

/// A `RegExp` instance, with the class static it feeds.
struct TrackedRegExp {
    /// The instance's own members.
    inner: Rc<RegExpObject>,
    /// The class static, which is held weakly so the instance does not keep
    /// its class alive.
    last: Weak<LastRegExp>,
    /// The object itself, for the value the class static holds.
    this: Weak<TrackedRegExp>,
}

impl TrackedRegExp {
    /// The instance a construction makes.
    fn new(arguments: &[Variant], last: Weak<LastRegExp>) -> Result<Rc<Self>, ObjectError> {
        let inner = RegExpObject::new(arguments).map_err(|_| ObjectError::CallNotServed)?;
        Ok(Rc::new_cyclic(|this| Self {
            inner,
            last,
            this: this.clone(),
        }))
    }

    /// Records this instance as the class static's value.
    fn record(&self) {
        record_last(&self.last, &self.this);
    }
}

impl Dispatch for TrackedRegExp {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let value = self.inner.prop_get(name, flags, objthis)?;
        let tracks = matches!(
            RegExpMember::lookup(name),
            Some(RegExpMember::Test | RegExpMember::Exec)
        );
        if !tracks {
            return Ok(value);
        }
        let last = self.last.clone();
        let this = self.this.clone();
        Ok(method(move |arguments| {
            let answer = call_value(&value, arguments)?;
            record_last(&last, &this);
            Ok(answer)
        }))
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.inner.prop_set(name, flags, value, objthis)
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.inner.delete_member(name, flags, objthis)
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        self.inner.enum_members(flags, callback)
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        self.inner.is_instance_of(class, objthis)
    }

    fn class_names(&self) -> Vec<TjsString> {
        self.inner.class_names()
    }

    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.inner.add_class_name(name)
    }

    fn is_valid(&self) -> bool {
        self.inner.is_valid()
    }

    fn invalidate(&self) -> bool {
        self.inner.invalidate()
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let tracks = matches!(
            RegExpMember::lookup(name),
            Some(RegExpMember::Test | RegExpMember::Exec)
        );
        let answer = self.inner.func_call(name, argv, objthis)?;
        if tracks {
            self.record();
        }
        Ok(answer)
    }

    fn engine_id(&self) -> Option<crate::Tjs2ValueId> {
        self.inner.engine_id()
    }

    fn rust_member(&self, name: &TjsString) -> Option<Variant> {
        self.inner.rust_member(name)
    }
}

/// A generator instance.
struct RandomGeneratorObject {
    /// The generator's state.
    state: Rc<RefCell<RandomGenerator>>,
    /// Members a script added to the instance, and the object's class names.
    members: CustomObject,
}

impl Dispatch for RandomGeneratorObject {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        match self.members.prop_get(name, flags, objthis) {
            Ok(value) => return Ok(value),
            Err(ObjectError::MemberNotFound | ObjectError::MemberMustExist) => {}
            Err(error) => return Err(error),
        }
        let Some(member) = RandomGeneratorMember::lookup(name) else {
            return Err(ObjectError::MemberNotFound);
        };
        let state = Rc::clone(&self.state);
        Ok(method(move |arguments| {
            let mut generator = state.borrow_mut();
            member
                .call(&mut generator, arguments, fresh_seed())
                .map_err(|_| ObjectError::CallNotServed)
        }))
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.members.prop_set(name, flags, value, objthis)
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.members.delete_member(name, flags, objthis)
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        let mut seen: Vec<TjsString> = Vec::new();
        let mut stopped = false;
        self.members
            .enum_members(flags, &mut |name, member_flags, value| {
                seen.push(name.clone());
                let keep = callback(name, member_flags, value);
                stopped = !keep;
                keep
            })?;
        if stopped {
            return Ok(());
        }
        for member in RandomGeneratorMember::ALL {
            let name = TjsString::from(member.name());
            if seen.contains(&name) {
                continue;
            }
            let value = if flags.contains(MemberFlags::ENUM_NO_VALUE) {
                Variant::Void
            } else {
                let state = Rc::clone(&self.state);
                method(move |arguments| {
                    let mut generator = state.borrow_mut();
                    member
                        .call(&mut generator, arguments, fresh_seed())
                        .map_err(|_| ObjectError::CallNotServed)
                })
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

    fn is_valid(&self) -> bool {
        self.members.is_valid()
    }

    fn invalidate(&self) -> bool {
        self.members.invalidate()
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let Some(member) = RandomGeneratorMember::lookup(name) else {
            return Err(ObjectError::CallNotServed);
        };
        let arguments = argv.unwrap_or(&[]);
        let mut generator = self.state.borrow_mut();
        member
            .call(&mut generator, arguments, fresh_seed())
            .map_err(|_| ObjectError::CallNotServed)
    }
}

/// A date instance.
struct DateObject {
    /// The instant and the zone its fields are placed in.
    state: Rc<RefCell<Date>>,
    /// Members a script added to the instance, and the object's class names.
    members: CustomObject,
}

impl DateObject {
    /// The instance a construction makes.
    fn new(date: Date) -> Self {
        Self {
            state: Rc::new(RefCell::new(date)),
            members: CustomObject::new([TjsString::from(date::CLASS_NAME)]),
        }
    }
}

impl Dispatch for DateObject {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        match self.members.prop_get(name, flags, objthis) {
            Ok(value) => return Ok(value),
            Err(ObjectError::MemberNotFound | ObjectError::MemberMustExist) => {}
            Err(error) => return Err(error),
        }
        let Some(member) = DateMember::lookup(name) else {
            return Err(ObjectError::MemberNotFound);
        };
        let state = Rc::clone(&self.state);
        Ok(method(move |arguments| {
            let mut date = state.borrow_mut();
            member
                .call(&mut date, arguments)
                .map_err(|_| ObjectError::CallNotServed)
        }))
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.members.prop_set(name, flags, value, objthis)
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.members.delete_member(name, flags, objthis)
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        let mut seen: Vec<TjsString> = Vec::new();
        let mut stopped = false;
        self.members
            .enum_members(flags, &mut |name, member_flags, value| {
                seen.push(name.clone());
                let keep = callback(name, member_flags, value);
                stopped = !keep;
                keep
            })?;
        if stopped {
            return Ok(());
        }
        for member in DateMember::ALL {
            let name = TjsString::from(member.name());
            if seen.contains(&name) {
                continue;
            }
            let value = if flags.contains(MemberFlags::ENUM_NO_VALUE) {
                Variant::Void
            } else {
                let state = Rc::clone(&self.state);
                method(move |arguments| {
                    let mut date = state.borrow_mut();
                    member
                        .call(&mut date, arguments)
                        .map_err(|_| ObjectError::CallNotServed)
                })
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

    fn is_valid(&self) -> bool {
        self.members.is_valid()
    }

    fn invalidate(&self) -> bool {
        self.members.invalidate()
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let Some(member) = DateMember::lookup(name) else {
            return Err(ObjectError::CallNotServed);
        };
        let arguments = argv.unwrap_or(&[]);
        let mut date = self.state.borrow_mut();
        member
            .call(&mut date, arguments)
            .map_err(|_| ObjectError::CallNotServed)
    }
}

/// A method value: an object a script can call and read as a function.
struct NativeMethod {
    /// The rule the call runs.
    call: Box<MethodCall>,
}

impl Dispatch for NativeMethod {
    fn prop_get(
        &self,
        _name: &TjsString,
        _flags: MemberFlags,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        Err(ObjectError::MemberNotFound)
    }

    fn prop_set(
        &self,
        _name: &TjsString,
        _flags: MemberFlags,
        _value: &Variant,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        Err(ObjectError::MemberNotFound)
    }

    fn delete_member(
        &self,
        _name: &TjsString,
        _flags: MemberFlags,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        Err(ObjectError::MemberNotFound)
    }

    fn enum_members(
        &self,
        _flags: MemberFlags,
        _callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        Ok(())
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        let object = TjsString::from("Object");
        let function = TjsString::from("Function");
        Ok(*class == object || *class == function)
    }

    fn class_names(&self) -> Vec<TjsString> {
        Vec::new()
    }

    fn add_class_name(&self, _name: &TjsString) -> Result<(), ObjectError> {
        Err(ObjectError::CallNotServed)
    }

    fn is_valid(&self) -> bool {
        true
    }

    fn invalidate(&self) -> bool {
        false
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        if !name.is_empty() {
            return Err(ObjectError::CallNotServed);
        }
        (self.call)(argv.unwrap_or(&[]))
    }
}

/// The method a member read answers.
fn method(call: impl Fn(&[Variant]) -> Result<Variant, ObjectError> + 'static) -> Variant {
    let object: Rc<dyn Dispatch> = Rc::new(NativeMethod {
        call: Box::new(call),
    });
    object_value(object)
}

/// The empty finalizer a class object publishes.
fn empty_method() -> Variant {
    method(|_arguments| Ok(Variant::Void))
}

/// A method a class object publishes but cannot run without an instance.
fn refused_method() -> Variant {
    method(|_arguments| Err(ObjectError::CallNotServed))
}

/// The object value a dispatch object becomes.
fn object_value(object: Rc<dyn Dispatch>) -> Variant {
    Variant::Object(Closure {
        object: Some(object.clone()),
        objthis: Some(object),
    })
}

/// The instance value a construction makes.
fn tracked_regexp(arguments: &[Variant], last: Weak<LastRegExp>) -> Result<Variant, ObjectError> {
    let object = TrackedRegExp::new(arguments, last)?;
    Ok(object_value(object))
}

/// The instance value a generator construction makes.
fn random_generator_value(generator: RandomGenerator) -> Variant {
    let object: Rc<dyn Dispatch> = Rc::new(RandomGeneratorObject {
        state: Rc::new(RefCell::new(generator)),
        members: CustomObject::new([TjsString::from(RANDOM_GENERATOR)]),
    });
    object_value(object)
}

/// The instance value a date construction makes.
fn date_value(date: Date) -> Variant {
    let object: Rc<dyn Dispatch> = Rc::new(DateObject::new(date));
    object_value(object)
}

/// Records the instance a `test` or `exec` call ran on.
fn record_last(last: &Weak<LastRegExp>, this: &Weak<TrackedRegExp>) {
    let (Some(last), Some(this)) = (last.upgrade(), this.upgrade()) else {
        return;
    };
    let object: Rc<dyn Dispatch> = this;
    last.record(Variant::Object(Closure {
        object: Some(object.clone()),
        objthis: Some(object),
    }));
}

/// Calls a method value with the receiver it carries.
fn call_value(value: &Variant, arguments: &[Variant]) -> Result<Variant, ObjectError> {
    let Variant::Object(closure) = value else {
        return Err(ObjectError::CallNotServed);
    };
    let object = closure
        .object
        .as_deref()
        .ok_or(ObjectError::CallNotServed)?;
    object.func_call(
        &TjsString::new(),
        Some(arguments),
        closure.objthis.as_deref(),
    )
}

/// The low word of the wall clock's seconds, the seed the generators use.
fn clock_word() -> u32 {
    let seconds = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i128::from(elapsed.as_secs()),
        Err(error) => -i128::from(error.duration().as_secs()),
    };
    let modulus: i128 = 1 << 32;
    let low = seconds.rem_euclid(modulus);
    u32::try_from(low).expect("a value below 2^32 fits in a u32")
}

/// The seed a generator uses when a call gives none.
fn fresh_seed() -> Seed {
    Seed::Time(clock_word())
}

/// The system zone's standard offset, or the failure that reading it gave.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
enum ZoneError {
    /// The zone file cannot be read.
    #[error("the system zone file cannot be read")]
    Read,
    /// The text is not a zone the reader knows.
    #[error("the system zone text is not a zone")]
    Text,
    /// The file is not a zone image the reader knows.
    #[error("the system zone image is not a zone file")]
    Format,
    /// The zone image names no standard offset.
    #[error("the system zone names no standard offset")]
    NoStandard,
}

/// The system zone's standard offset, in seconds east of UTC.
///
/// `TZ` selects the zone when it is set: a POSIX form carries its offset, and
/// a zone name or a path names a zone image. With no `TZ`, the system's own
/// `/etc/localtime` is the image. A zone that cannot be read is a typed
/// failure, not an assumed offset.
fn system_offset() -> Result<i64, ZoneError> {
    let Some(value) = std::env::var_os("TZ") else {
        return zone_file(Path::new("/etc/localtime"));
    };
    let text = value.to_string_lossy();
    // a zone may name its image with the leading colon the POSIX form allows
    let name = match text.strip_prefix(':') {
        Some(name) => name,
        None => text.as_ref(),
    };
    if name.is_empty() {
        return Ok(0);
    }
    if name.starts_with('/') {
        return zone_file(Path::new(name));
    }
    let path = Path::new("/usr/share/zoneinfo").join(name);
    if path.is_file() {
        return zone_file(&path);
    }
    posix_offset(name)
}

/// The standard offset a zone image carries.
fn zone_file(path: &Path) -> Result<i64, ZoneError> {
    let bytes = std::fs::read(path).map_err(|_| ZoneError::Read)?;
    zone_image(&bytes)
}

/// The standard offset a TZif image carries: the last type that is not a
/// daylight-saving one.
fn zone_image(bytes: &[u8]) -> Result<i64, ZoneError> {
    let (version, first_counts) = zone_header(bytes, 0)?;
    let (time_size, start, counts) = if version >= b'2' {
        let first = zone_block_length(first_counts, 4, 8)?;
        let second = 44usize.checked_add(first).ok_or(ZoneError::Format)?;
        let (_, counts) = zone_header(bytes, second)?;
        (
            8usize,
            second.checked_add(44).ok_or(ZoneError::Format)?,
            counts,
        )
    } else {
        (4usize, 44usize, first_counts)
    };
    let timecnt = usize::try_from(counts[3]).map_err(|_| ZoneError::Format)?;
    let typecnt = usize::try_from(counts[4]).map_err(|_| ZoneError::Format)?;
    let transitions = timecnt.checked_mul(time_size).ok_or(ZoneError::Format)?;
    let types_start = start
        .checked_add(transitions)
        .and_then(|value| value.checked_add(timecnt))
        .ok_or(ZoneError::Format)?;
    let types_length = typecnt.checked_mul(6).ok_or(ZoneError::Format)?;
    let types_end = types_start
        .checked_add(types_length)
        .ok_or(ZoneError::Format)?;
    let types = bytes.get(types_start..types_end).ok_or(ZoneError::Format)?;
    let mut standard = None;
    for entry in types.as_chunks::<6>().0 {
        let offset = i32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]);
        if entry[4] == 0 {
            standard = Some(i64::from(offset));
        }
    }
    standard.ok_or(ZoneError::NoStandard)
}

/// The version and the six counts of a TZif header at `offset`.
fn zone_header(bytes: &[u8], offset: usize) -> Result<(u8, [u32; 6]), ZoneError> {
    let magic_end = offset.checked_add(4).ok_or(ZoneError::Format)?;
    if bytes.get(offset..magic_end).ok_or(ZoneError::Format)? != b"TZif" {
        return Err(ZoneError::Format);
    }
    let version = *bytes.get(magic_end).ok_or(ZoneError::Format)?;
    let mut counts = [0u32; 6];
    for (index, count) in counts.iter_mut().enumerate() {
        let start = offset
            .checked_add(20)
            .and_then(|value| value.checked_add(index.checked_mul(4)?))
            .ok_or(ZoneError::Format)?;
        let end = start.checked_add(4).ok_or(ZoneError::Format)?;
        let field: [u8; 4] = bytes
            .get(start..end)
            .ok_or(ZoneError::Format)?
            .try_into()
            .map_err(|_| ZoneError::Format)?;
        *count = u32::from_be_bytes(field);
    }
    Ok((version, counts))
}

/// The length of one TZif data block, from its header counts.
fn zone_block_length(
    counts: [u32; 6],
    time_size: usize,
    leap_size: usize,
) -> Result<usize, ZoneError> {
    let timecnt = usize::try_from(counts[3]).map_err(|_| ZoneError::Format)?;
    let typecnt = usize::try_from(counts[4]).map_err(|_| ZoneError::Format)?;
    let charcnt = usize::try_from(counts[5]).map_err(|_| ZoneError::Format)?;
    let leapcnt = usize::try_from(counts[2]).map_err(|_| ZoneError::Format)?;
    let stdcnt = usize::try_from(counts[0]).map_err(|_| ZoneError::Format)?;
    let gmtcnt = usize::try_from(counts[1]).map_err(|_| ZoneError::Format)?;
    let parts = [
        timecnt.checked_mul(time_size),
        Some(timecnt),
        typecnt.checked_mul(6),
        Some(charcnt),
        leapcnt.checked_mul(leap_size),
        Some(stdcnt),
        Some(gmtcnt),
    ];
    let mut total = 0usize;
    for part in parts {
        total = total
            .checked_add(part.ok_or(ZoneError::Format)?)
            .ok_or(ZoneError::Format)?;
    }
    Ok(total)
}

/// The standard offset a POSIX zone text names, in seconds east of UTC.
fn posix_offset(text: &str) -> Result<i64, ZoneError> {
    let rest = if let Some(after) = text.strip_prefix('<') {
        let end = after.find('>').ok_or(ZoneError::Text)?;
        &after[end + 1..]
    } else {
        let end = text
            .find(|character: char| !character.is_ascii_alphabetic())
            .unwrap_or(text.len());
        if end < 3 {
            return Err(ZoneError::Text);
        }
        &text[end..]
    };
    let (west, _) = posix_duration(rest)?;
    west.checked_neg().ok_or(ZoneError::Text)
}

/// A POSIX offset, and the text after it.
fn posix_duration(text: &str) -> Result<(i64, &str), ZoneError> {
    let (sign, rest) = match text.as_bytes().first() {
        Some(b'-') => (-1i64, &text[1..]),
        Some(b'+') => (1i64, &text[1..]),
        _ => (1i64, text),
    };
    let end = rest
        .find(|character: char| !(character.is_ascii_digit() || character == ':'))
        .unwrap_or(rest.len());
    if end == 0 {
        return Err(ZoneError::Text);
    }
    let (field, tail) = rest.split_at(end);
    let mut parts = field.split(':');
    let hours = posix_part(parts.next())?;
    let minutes = match parts.next() {
        Some(part) => posix_part(Some(part))?,
        None => 0,
    };
    let seconds = match parts.next() {
        Some(part) => posix_part(Some(part))?,
        None => 0,
    };
    if parts.next().is_some() || hours > 24 || minutes > 59 || seconds > 59 {
        return Err(ZoneError::Text);
    }
    let value = hours
        .checked_mul(3_600)
        .and_then(|value| {
            minutes
                .checked_mul(60)
                .and_then(|minutes| value.checked_add(minutes))
        })
        .and_then(|value| value.checked_add(seconds))
        .ok_or(ZoneError::Text)?;
    Ok((sign.checked_mul(value).ok_or(ZoneError::Text)?, tail))
}

/// One field of a POSIX offset, one or two digits.
fn posix_part(text: Option<&str>) -> Result<i64, ZoneError> {
    let text = text.ok_or(ZoneError::Text)?;
    if text.is_empty() || text.len() > 2 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ZoneError::Text);
    }
    text.parse::<i64>().map_err(|_| ZoneError::Text)
}
