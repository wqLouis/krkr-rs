//! The engine as an object: the global scope, the held values and the scripts.
//!
//! The script language reaches everything through one object. A declared `var`
//! and a declared `function` are its members, the class names a script reads are
//! its members too, and a script that runs at the top level runs in its scope.
//! This module is that owner: it holds the global object, the values it is asked
//! to keep ([`RetainedTable`]) and the blocks it has compiled, and it answers the
//! operations a host asks of a script engine.
//!
//! # Ownership, not member semantics
//!
//! Every operation here is one call into the object model. A member read is
//! [`Dispatch::prop_get`], a call is [`Dispatch::func_call`] or a context run by
//! the [`Machine`], a construction is [`Dispatch::create_new`]. The engine adds
//! two things and nothing else: the **id** a value is named by after the call
//! that made it, and the **owner** of the state those calls run against. A rule
//! written here instead of in the object model would be a second member rule,
//! and there is one.
//!
//! # The global object
//!
//! [`GlobalObject`] is a [`CustomObject`] with the class registry in front of it.
//! A name the object holds answers first, so a script that writes a class name
//! shadows it; a name it does not hold is answered by the registry; a name the
//! script deleted stays deleted. That is the whole of the global's rule, and it
//! is why the class names are read through one object rather than through a
//! table the engine also has to consult.
//!
//! # The scripts
//!
//! A script is compiled once and kept, by name and by what it was compiled as
//! (a script or an expression). A block is reusable because it owns its contexts
//! and its code: running it again lays out a fresh frame over the same contexts,
//! which is what a script block is. The source travels with the block, so a
//! second compile under the same name with different text replaces the entry
//! instead of answering the old block.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use super::exec::build::{Block, BuildError};
use super::exec::{Fault, Machine, Operator};
use super::natives::registry::NativeRegistry;
use super::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use super::retained::{RetainError, RetainedId, RetainedTable};
use super::string::TjsString;
use super::variant::Variant;
use crate::vm::codegen::context::ContextType;
use crate::vm::{ParseError, compile, compile_expression_result};

/// The object an id names, with the receiver its members are called on.
type Object = (Rc<dyn Dispatch>, Option<Rc<dyn Dispatch>>);

/// The reason an engine operation did not answer.
///
/// Every branch carries the failure of the part that raised it: the front end's,
/// the block builder's, the code's, an object entry's, or the table's.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// The text does not compile.
    #[error(transparent)]
    Compile(#[from] ParseError),

    /// The compiled image does not describe a block.
    #[error(transparent)]
    Build(#[from] BuildError),

    /// The code raised, or the value it named cannot be run.
    #[error(transparent)]
    Run(#[from] Fault),

    /// The object surface refused the operation.
    #[error(transparent)]
    Object(#[from] ObjectError),

    /// The table refused the id.
    #[error(transparent)]
    Retained(#[from] RetainError),

    /// A construction answered a value that is not an object.
    #[error("the class produced no object")]
    NoObject,
}

/// The operations an engine applies to a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Operation {
    /// A compound assignment: `member op= right`.
    Compound(Operator),

    /// `member++`.
    Increment,

    /// `member--`.
    Decrement,
}

impl Operation {
    /// The name the object model's operation entry knows the operation by.
    fn name(self) -> &'static str {
        match self {
            Self::Compound(operator) => operator.name(),
            Self::Increment => "inc",
            Self::Decrement => "dec",
        }
    }
}

/// The engine's global object.
///
/// The members a script and the engine's own code declare live in a
/// [`CustomObject`]; the class names live in the registry in front of it. A
/// script that writes a class name shadows it, and one that deletes it leaves the
/// name absent — the registry does not come back through a deletion.
pub struct GlobalObject {
    /// The declared members.
    members: CustomObject,
    /// The class objects the global publishes.
    natives: NativeRegistry,
    /// The class names a script deleted from the global.
    removed: RefCell<Vec<TjsString>>,
}

impl GlobalObject {
    /// A global with the registry's classes and no declared member.
    pub fn new() -> Self {
        Self {
            members: CustomObject::new(Vec::new()),
            natives: NativeRegistry::new(),
            removed: RefCell::new(Vec::new()),
        }
    }

    /// The class value `name` denotes, when the name has not been deleted.
    fn published(&self, name: &TjsString) -> Option<Variant> {
        if !self.is_published(name) {
            return None;
        }
        self.natives.get(name)
    }

    /// Whether the registry publishes the name and the script has not deleted
    /// it.
    fn is_published(&self, name: &TjsString) -> bool {
        !self.removed.borrow().contains(name) && self.natives.contains(name)
    }

    /// The class names the global publishes, with their class objects.
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
        let removed = self.removed.borrow();
        let mut entries = Vec::new();
        for published in self.natives.names() {
            let name = TjsString::from(published);
            if removed.contains(&name) {
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
        match self.members.prop_get(name, flags, objthis) {
            Ok(value) => Ok(value),
            // a name the object does not hold is the registry's, unless the
            // script deleted it
            Err(ObjectError::MemberNotFound | ObjectError::MemberMustExist) => {
                if let Some(value) = self.published(name) {
                    return Ok(value);
                }
                if flags.contains(MemberFlags::MUST_EXIST) {
                    return Err(ObjectError::MemberMustExist);
                }
                Err(ObjectError::MemberNotFound)
            }
            Err(error) => Err(error),
        }
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        // a published class name exists at the global, so a write that requires
        // the member is served: the write creates the member that shadows it
        if flags.contains(MemberFlags::MUST_EXIST) && self.is_published(name) {
            let flags = MemberFlags::from_bits(
                flags.bits() & !MemberFlags::MUST_EXIST.bits() | MemberFlags::ENSURE.bits(),
            );
            return self.members.prop_set(name, flags, value, objthis);
        }
        self.members.prop_set(name, flags, value, objthis)
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        if self.is_published(name) {
            self.removed.borrow_mut().push(name.clone());
            return match self.members.delete_member(name, flags, objthis) {
                // the class name was the member, and it is gone: nothing the
                // members hold has to be removed as well
                Ok(()) | Err(ObjectError::MemberNotFound) => Ok(()),
                Err(error) => Err(error),
            };
        }
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
        for (name, value) in self.published_entries(flags)? {
            if seen.contains(&name) {
                continue;
            }
            if !callback(&name, 0, &value) {
                return Ok(());
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
}

impl fmt::Debug for GlobalObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GlobalObject")
            .field("members", &self.members.len())
            .field("natives", &self.natives.names().count())
            .finish()
    }
}

/// How a compiled script was compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ScriptKind {
    /// The text is a script: its statements run, and its answer is what its top
    /// code stored.
    Script,

    /// The text is an expression: its value is the answer.
    Expression,
}

/// The cache key a script is filed under.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ScriptKey {
    /// The name the caller labelled the block with.
    name: String,
    /// What the text was compiled as.
    kind: ScriptKind,
}

/// One compiled script, with the text it was compiled from.
struct Compiled {
    /// The text, so a compile under the same name with other text is not
    /// answered from here.
    source: String,
    /// The block the text became.
    block: Rc<Block>,
}

/// The script engine: one global scope, one set of held values, one cache of
/// compiled scripts.
pub struct EngineCore {
    /// The global object every top-level call runs against.
    global: Rc<GlobalObject>,
    /// The values the engine holds for its callers.
    retained: RefCell<RetainedTable>,
    /// The blocks this engine has compiled.
    scripts: RefCell<BTreeMap<ScriptKey, Compiled>>,
}

impl EngineCore {
    /// An engine with an empty global scope.
    pub fn new() -> Self {
        Self {
            global: Rc::new(GlobalObject::new()),
            retained: RefCell::new(RetainedTable::new()),
            scripts: RefCell::new(BTreeMap::new()),
        }
    }

    /// The machine a call runs in, with this engine's global as its scope.
    fn machine(&self) -> Machine {
        let global: Rc<dyn Dispatch> = self.global.clone();
        Machine::with_global(global)
    }

    /// Compiles `source` and runs its top-level context.
    ///
    /// The answer is what the top-level code stored: a script's `return`, or
    /// nothing when its top code falls off the end.
    ///
    /// # Errors
    ///
    /// The compile's failure, the builder's, or the code's; no step substitutes
    /// a value for a failure.
    pub fn run(&self, source: &str, name: &str) -> Result<Variant, CoreError> {
        self.execute(source, name, ScriptKind::Script)
    }

    /// Compiles `expression` and answers the value it produced.
    ///
    /// # Errors
    ///
    /// The compile's failure, the builder's, or the code's; no step substitutes
    /// a value for a failure.
    pub fn eval(&self, expression: &str, name: &str) -> Result<Variant, CoreError> {
        self.execute(expression, name, ScriptKind::Expression)
    }

    /// Compiles `source` as `kind` and runs its top-level context.
    fn execute(&self, source: &str, name: &str, kind: ScriptKind) -> Result<Variant, CoreError> {
        let block = self.block(source, name, kind)?;
        let top_level = block
            .top_level()
            .ok_or(BuildError::TopLevel { index: -1 })?
            .object()
            .clone();
        let mut machine = self.machine();
        machine
            .run_context_traced(&top_level, None, &[])
            .map_err(|unwind| CoreError::Run(unwind.fault()))
    }

    /// The block `source` compiles to, from the cache when it is there.
    fn block(&self, source: &str, name: &str, kind: ScriptKind) -> Result<Rc<Block>, CoreError> {
        let key = ScriptKey {
            name: name.to_string(),
            kind,
        };
        let cached = self
            .scripts
            .borrow()
            .get(&key)
            .filter(|cached| cached.source == source)
            .map(|cached| Rc::clone(&cached.block));
        if let Some(block) = cached {
            return Ok(block);
        }
        let image = match kind {
            ScriptKind::Script => compile(source, name)?,
            ScriptKind::Expression => compile_expression_result(source, name, true)?,
        };
        let block = Rc::new(Block::build(&image)?);
        self.scripts.borrow_mut().insert(
            key,
            Compiled {
                source: source.to_string(),
                block: Rc::clone(&block),
            },
        );
        Ok(block)
    }

    /// Keeps `value` and answers the id that names it.
    pub fn retain(&self, value: Variant) -> RetainedId {
        self.retained.borrow_mut().retain(value)
    }

    /// Keeps `object` as an object value and answers the id that names it.
    pub fn retain_object(&self, object: &Rc<dyn Dispatch>) -> RetainedId {
        self.retained.borrow_mut().retain_object(object)
    }

    /// Makes a second id for a value that is already held.
    ///
    /// # Errors
    ///
    /// [`RetainError::Stale`] when the id names no held value.
    pub fn duplicate(&self, id: RetainedId) -> Result<RetainedId, RetainError> {
        self.retained.borrow_mut().duplicate(id)
    }

    /// Releases the holding `id` names.
    ///
    /// # Errors
    ///
    /// [`RetainError::Stale`] when the id names no held value.
    pub fn release(&self, id: RetainedId) -> Result<(), RetainError> {
        self.retained.borrow_mut().release(id).map(|_| ())
    }

    /// How many values the engine holds right now.
    pub fn retained_count(&self) -> usize {
        self.retained.borrow().count()
    }

    /// A fresh retention of the global object — the object a script's `global`
    /// names.
    pub fn global_object(&self) -> RetainedId {
        let global: Rc<dyn Dispatch> = self.global.clone();
        self.retain_object(&global)
    }

    /// The value `id` names.
    ///
    /// # Errors
    ///
    /// [`RetainError::Stale`] when the id names no held value.
    pub fn value(&self, id: RetainedId) -> Result<Variant, CoreError> {
        let value = self.retained.borrow().value(id)?.clone();
        Ok(value)
    }

    /// The object `id` names, with the receiver its members are called on.
    ///
    /// # Errors
    ///
    /// [`RetainError::Stale`] when the id names no held value,
    /// [`RetainError::NotAnObject`] when it names a non-object value and
    /// [`RetainError::NullObject`] when it names the null object.
    fn object(&self, id: RetainedId) -> Result<Object, CoreError> {
        match self.value(id)? {
            Variant::Object(closure) => match closure.object {
                Some(object) => Ok((object, closure.objthis)),
                None => Err(CoreError::Retained(RetainError::NullObject(id))),
            },
            _ => Err(CoreError::Retained(RetainError::NotAnObject(id))),
        }
    }

    /// Reads the member `name` of the object `id` names.
    ///
    /// A member that is not there is [`ObjectError::MemberNotFound`], and a
    /// member that holds nothing is the empty variant: the two are different
    /// answers and the caller sees which it got.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no object, and
    /// [`CoreError::Object`] when the read is refused.
    pub fn member_read(&self, id: RetainedId, name: &TjsString) -> Result<Variant, CoreError> {
        let (object, objthis) = self.object(id)?;
        Ok(object.prop_get(name, MemberFlags::NONE, objthis.as_deref())?)
    }

    /// Writes `value` to the member `name` of the object `id` names, creating
    /// the member when it is not there.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no object, and
    /// [`CoreError::Object`] when the write is refused.
    pub fn member_write(
        &self,
        id: RetainedId,
        name: &TjsString,
        value: &Variant,
    ) -> Result<(), CoreError> {
        let (object, objthis) = self.object(id)?;
        Ok(object.prop_set(name, MemberFlags::ENSURE, value, objthis.as_deref())?)
    }

    /// Removes the member `name` from the object `id` names.
    ///
    /// An absent member is the read's failure and not this one's: a delete names
    /// a member it expects to find.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no object, and
    /// [`CoreError::Object`] when the member is not there or the delete is
    /// refused.
    pub fn delete_member(&self, id: RetainedId, name: &TjsString) -> Result<(), CoreError> {
        let (object, objthis) = self.object(id)?;
        Ok(object.delete_member(name, MemberFlags::NONE, objthis.as_deref())?)
    }

    /// The names of the object's members, sorted and without repeats, leaving
    /// the hidden ones out.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no object, and
    /// [`CoreError::Object`] when the walk is refused.
    pub fn enum_members(&self, id: RetainedId) -> Result<Vec<TjsString>, CoreError> {
        let (object, _) = self.object(id)?;
        let mut names = Vec::new();
        object.enum_members(
            MemberFlags::IGNORE_PROPERTY | MemberFlags::ENUM_NO_VALUE,
            &mut |name, flags, _| {
                if flags & MemberFlags::HIDDEN.bits() == 0 {
                    names.push(name.clone());
                }
                true
            },
        )?;
        names.sort_by(|left, right| left.as_utf16().cmp(right.as_utf16()));
        names.dedup();
        Ok(names)
    }

    /// The class names the object `id` names carries.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no object.
    pub fn class_names(&self, id: RetainedId) -> Result<Vec<TjsString>, CoreError> {
        let (object, _) = self.object(id)?;
        Ok(object.class_names())
    }

    /// Whether the object `id` names belongs to `class`.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no object, and
    /// [`CoreError::Object`] when the question is refused.
    pub fn is_instance_of(
        &self,
        id: RetainedId,
        class: &TjsString,
    ) -> Result<bool, CoreError> {
        let (object, objthis) = self.object(id)?;
        Ok(object.is_instance_of(class, objthis.as_deref())?)
    }

    /// Whether the object `id` names is still usable.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no object. A failure is never
    /// reported as either answer.
    pub fn is_valid(&self, id: RetainedId) -> Result<bool, CoreError> {
        let (object, _) = self.object(id)?;
        Ok(object.is_valid())
    }

    /// Invokes the value `id` names with `args`.
    ///
    /// A value whose call is code runs it in this engine's scope, with the
    /// receiver the value carries; any other value is invoked through the object
    /// model. A value that is not an object, and the null object, are not
    /// callable.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no held value, and
    /// [`CoreError::Run`] when the call raises or the value cannot be invoked.
    pub fn call_value(
        &self,
        id: RetainedId,
        args: &[Variant],
    ) -> Result<Variant, CoreError> {
        let closure = match self.value(id)? {
            Variant::Object(closure) => closure,
            _ => return Err(not_callable()),
        };
        let Some(object) = closure.object else {
            return Err(not_callable());
        };
        let objthis = closure.objthis;
        if let Some(context) = object.context() {
            // a property's context is not callable: its call is the member the
            // property stands for, and the object model serves that
            if context.kind() == ContextType::Property {
                return Err(not_callable());
            }
            return self
                .machine()
                .run_context(context, objthis, args)
                .map_err(CoreError::Run);
        }
        Ok(object.func_call(&TjsString::new(), Some(args), objthis.as_deref())?)
    }

    /// Constructs a new object through the value `id` names, and keeps it for
    /// the caller.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no object,
    /// [`CoreError::Run`] when a context is named instead of a class, and
    /// [`CoreError::Object`], [`CoreError::NoObject`] when the construction is
    /// refused or answers no object.
    pub fn create_new(
        &self,
        id: RetainedId,
        args: &[Variant],
    ) -> Result<RetainedId, CoreError> {
        let (object, objthis) = self.object(id)?;
        if object.context().is_some() {
            // a context is not a class a script constructs: construction would
            // run its code against an object it made, which is not a call
            return Err(not_callable());
        }
        let answer = object.create_new(&TjsString::new(), Some(args), objthis.as_deref())?;
        let is_object = matches!(&answer, Variant::Object(closure) if closure.object.is_some());
        if !is_object {
            return Err(CoreError::NoObject);
        }
        Ok(self.retain(answer))
    }

    /// Applies `operation` to the member `name` of the object `id` names, and
    /// answers the member's value afterwards.
    ///
    /// The object gets the operation first, because an object that serves it
    /// observes the read and the write as the one operation they are. An object
    /// that does not serve it is read, applied and written back, which is the
    /// base object's own rule.
    ///
    /// `name` is `None` for the value's default member.
    ///
    /// # Errors
    ///
    /// [`CoreError::Retained`] when the id names no object,
    /// [`CoreError::Object`] when the member is refused, and
    /// [`CoreError::Run`] when the operator raises.
    pub fn operation(
        &self,
        id: RetainedId,
        name: Option<&TjsString>,
        operation: Operation,
        right: &Variant,
    ) -> Result<Variant, CoreError> {
        let (object, objthis) = self.object(id)?;
        let objthis = objthis.as_deref();
        let name = name.cloned().unwrap_or_default();
        let served = object.operation(&TjsString::from(operation.name()), right, objthis)?;
        if served {
            return Ok(object.prop_get(&name, MemberFlags::IGNORE_PROPERTY, objthis)?);
        }
        let current = object.prop_get(&name, MemberFlags::NONE, objthis)?;
        let answer = match operation {
            Operation::Compound(operator) => super::exec::op::apply(operator, &current, right)?,
            Operation::Increment => super::exec::op::step_up(&current)?,
            Operation::Decrement => super::exec::op::step_down(&current)?,
        };
        object.prop_set(&name, MemberFlags::NONE, &answer, objthis)?;
        Ok(answer)
    }
}

impl Default for EngineCore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for EngineCore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EngineCore")
            .field("global", &self.global)
            .field("retained", &self.retained.borrow().count())
            .field("scripts", &self.scripts.borrow().len())
            .finish()
    }
}

/// The failure a value that is not callable raises.
///
/// The language reports it as the call's own failure rather than as a refusal of
/// the object surface, so it is the failure a script sees.
fn not_callable() -> CoreError {
    CoreError::Run(Fault::Raised(super::exec::Raise::NotCallable {
        name: TjsString::new(),
    }))
}
