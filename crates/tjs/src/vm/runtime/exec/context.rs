//! The context objects: a code context as the object a receiver is taken from.
//!
//! A context is not only a code stream. The functions a block declares become
//! members of an object, a declared class is an object whose members are its
//! methods, and a call reaches one through the object surface like any other
//! member. This module is that object: [`ContextObject`] is a dispatch object
//! over a [`Code`] and a member store, and the four instructions here are what
//! build and rebind it.
//!
//! # The chain
//!
//! A class answers the members its own store holds and then its parent's, so
//! `super` reaches the parent's member through the same read every other member
//! takes. A function context does **not** chain: a nested function sees its own
//! registers, its receiver and the global object, and nothing of the context
//! that declared it. Chaining a function's store into its parent's would be a
//! lexical capture, which the language does not have.
//!
//! The link to the parent is set after the object is made, because a context is
//! built before the one that holds it, and it is a weak link: a parent's members
//! hold its children, so a strong link would close a cycle and keep the whole
//! block alive as long as one of its contexts is.
//!
//! # What the four instructions do
//!
//! | instruction | what it does |
//! |---|---|
//! | `GLOBAL %d` | names the machine's global object in a register |
//! | `CHGTHIS %c %o` | rebinds the closure in `%c` so its receiver is `%o` |
//! | `REGMEMBER` | publishes the context's own members onto the frame's receiver, rebinding each one's receiver to it |
//! | `ADDCI %o %v` | adds `%v` to the object `%o`'s class names |
//!
//! `CHGTHIS` is what makes a declared function a method: a receiver is a
//! property of the closure, so rebinding it is enough to change what the
//! function sees as `this`, including inside a block it calls.
//!
//! # The call
//!
//! A context is code, so a call of one is not a member surface's to answer: the
//! code needs a frame, the receiver as a value, and the machine that runs it.
//! [`Dispatch::context`] is how the caller finds the context it asks about, and
//! the machine runs the body — an unnamed call with the receiver the call hands
//! over, and a named call as the member the name reaches. This module therefore
//! answers no call at all, and its surface is the member store and the class
//! names.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use crate::vm::codegen::context::ContextType;
use crate::vm::codegen::opcode::vm;
use crate::vm::runtime::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant};

use super::Fault;
use super::frame::{Code, Frame};

/// The member flags a published member carries: it is created when absent, and
/// it is written through rather than through its property.
///
/// A static member is not published at all, so it never reaches this: the
/// `STATIC` flag is tested before the write.
fn publish_flags() -> MemberFlags {
    MemberFlags::ENSURE | MemberFlags::IGNORE_PROPERTY
}

/// Whether an opcode is one of the four context instructions.
pub fn handles(opcode: i32) -> bool {
    matches!(opcode, vm::CHGTHIS | vm::GLOBAL | vm::ADDCI | vm::REGMEMBER)
}

/// Executes a context instruction.
///
/// `context` is the object of the context being executed, which is what
/// `REGMEMBER` publishes and what a class's own name is read from.
pub fn execute(
    code: &Code,
    frame: &mut Frame,
    context: Option<&ContextObject>,
    global: Option<&Rc<dyn Dispatch>>,
    ip: usize,
    opcode: i32,
) -> Result<(), Fault> {
    match opcode {
        vm::GLOBAL => {
            let address = operand(code, ip, 1)?;
            let object = match global {
                Some(global) => global.clone(),
                None => return Err(Fault::Broken("the machine has no global object to name")),
            };
            let value = Variant::Object(Closure {
                object: Some(object.clone()),
                objthis: Some(object),
            });
            frame.set(address, value)
        }
        vm::CHGTHIS => {
            let target = operand(code, ip, 1)?;
            let receiver = operand(code, ip, 2)?;
            let new = match frame.reg(receiver)? {
                Variant::Object(closure) => closure.object.clone(),
                _ => None,
            };
            let value = frame.reg(target)?.clone();
            match &value {
                Variant::Object(closure) => {
                    let rebound = Variant::Object(Closure {
                        object: closure.object.clone(),
                        objthis: new,
                    });
                    frame.set(target, rebound)
                }
                _ => Err(Fault::Raised(super::Raise::Convert {
                    value: Box::new(value.clone()),
                    to: "Object",
                })),
            }
        }
        vm::REGMEMBER => {
            let Some(context) = context else {
                return Err(Fault::Broken(
                    "a member registration runs with no context object",
                ));
            };
            publish(context, frame)
        }
        vm::ADDCI => {
            let target = operand(code, ip, 1)?;
            let value = frame.reg(operand(code, ip, 2)?)?.clone();
            let named = frame.reg(target)?.clone();
            let object = match &named {
                Variant::Object(closure) => closure.object.clone(),
                _ => None,
            };
            let object = match object {
                Some(object) => object,
                None => {
                    return Err(Fault::Raised(super::Raise::Convert {
                        value: Box::new(named),
                        to: "Object",
                    }));
                }
            };
            // a value with no text names the empty class, which is what the
            // engine's null string becomes
            // a value with no text names the empty class, which is what the
            // engine's null string becomes
            let name = value
                .to_string()
                .unwrap_or_else(|_no_text| TjsString::new());
            object.add_class_name(&name).map_err(Fault::from)
        }
        _ => Err(Fault::Broken("a context instruction that is not one")),
    }
}

/// Publishes a context's members onto the frame's receiver.
///
/// Every member that is not static is written onto the receiver, and a member
/// whose value is an object has its receiver rebound to the destination, so a
/// class's functions become the instance's methods. The walk ignores properties:
/// it publishes the values, not what a property would answer.
fn publish(context: &ContextObject, frame: &mut Frame) -> Result<(), Fault> {
    let destination = match frame.receiver() {
        Some(destination) => destination,
        None => {
            return Err(Fault::Broken(
                "a member registration names no object to publish onto",
            ));
        }
    };
    let mut failure = None;
    context
        .enum_members(MemberFlags::IGNORE_PROPERTY, &mut |name, flags, value| {
            if flags & MemberFlags::STATIC.bits() != 0 {
                return true;
            }
            let mut copied = value.clone();
            if let Variant::Object(closure) = &mut copied {
                closure.objthis = Some(destination.clone());
            }
            match destination.prop_set(name, publish_flags(), &copied, Some(destination.as_ref())) {
                Ok(()) => true,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        })
        .map_err(Fault::from)?;
    match failure {
        Some(error) => Err(Fault::from(error)),
        None => Ok(()),
    }
}

/// The object one code context is.
///
/// It answers the members its store holds and, for a class, its parent's, and it
/// answers its own name and its parents' for a question about classes. It is also
/// code: [`Dispatch::context`] answers the context itself, and the machine runs
/// the code, because a call of it needs a frame this surface cannot be handed.
pub struct ContextObject {
    /// The code the context runs.
    code: Code,
    /// What kind of context it is.
    kind: ContextType,
    /// The name of the context: a class's name is the name an instance answers
    /// to.
    name: TjsString,
    /// The context that holds it.
    ///
    /// The link is weak, because a parent's members hold its children: a strong
    /// link would close a cycle, and every context of a block would then live as
    /// long as one of them. The block owns every context it built, so the link
    /// answers for as long as the block does.
    parent: RefCell<Option<Weak<ContextObject>>>,
    /// The members the context publishes: the functions it declares and, for a
    /// class, the members its body installs.
    members: CustomObject,
    /// The class names added to the context after it was built.
    ///
    /// The chain a class answers to is this, then the context's own name, then
    /// its parent's; a name added here is a name an instance of the class
    /// answers to.
    added: RefCell<Vec<TjsString>>,
}

impl ContextObject {
    /// A context object of the given kind, holding the given code.
    pub fn new(code: Code, kind: ContextType, name: impl Into<TjsString>) -> Self {
        Self {
            code,
            kind,
            name: name.into(),
            parent: RefCell::new(None),
            members: CustomObject::new(Vec::new()),
            added: RefCell::new(Vec::new()),
        }
    }

    /// Makes `parent` the context that holds this one.
    ///
    /// A context is built before the context that holds it, so the link is made
    /// after both objects exist.
    pub fn set_parent(&self, parent: &Rc<ContextObject>) {
        *self.parent.borrow_mut() = Some(Rc::downgrade(parent));
    }

    /// The context that holds this one, while it is alive.
    pub fn parent(&self) -> Option<Rc<ContextObject>> {
        self.parent.borrow().as_ref().and_then(Weak::upgrade)
    }

    /// The code the context runs.
    pub fn code(&self) -> &Code {
        &self.code
    }

    /// The kind of the context.
    pub fn kind(&self) -> ContextType {
        self.kind
    }

    /// The name of the context.
    pub fn name(&self) -> &TjsString {
        &self.name
    }

    /// Stores a member the context publishes.
    pub fn define(&self, name: &TjsString, value: Variant) -> Result<(), ObjectError> {
        self.members
            .prop_set(name, MemberFlags::ENSURE, &value, None)
    }

    /// The members the context publishes, for the block builder to fill.
    pub fn members(&self) -> &CustomObject {
        &self.members
    }

    /// The context a lookup of this one's members continues into.
    ///
    /// Only a class and the proxy that holds a class's super class continue into
    /// their parent. A function does not, because a function that could read its
    /// declarer's members would be a capture, and the language has none.
    fn continuation(&self) -> Option<Rc<ContextObject>> {
        match self.kind {
            ContextType::Class | ContextType::SuperClassGetter => self.parent(),
            _ => None,
        }
    }

    /// The class names the context answers to, its own first and then its
    /// parents'.
    fn chain(&self) -> Vec<TjsString> {
        let mut names = Vec::new();
        if self.kind == ContextType::Class && !self.name.is_empty() {
            names.push(self.name.clone());
        }
        names.extend(self.added.borrow().iter().cloned());
        if let Some(parent) = self.continuation() {
            names.extend(parent.chain());
        }
        names
    }
}

impl Dispatch for ContextObject {
    /// Reads a member: the context's own store, then — for a class — its
    /// parent's.
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        match self.members.prop_get(name, flags, objthis) {
            Err(ObjectError::MemberNotFound) => match self.continuation() {
                Some(parent) => parent.prop_get(name, flags, objthis),
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
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
        self.members.enum_members(flags, callback)
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        if class == &TjsString::from("Object") {
            return Ok(true);
        }
        Ok(self.chain().iter().any(|name| name == class))
    }

    fn class_names(&self) -> Vec<TjsString> {
        self.chain()
    }

    /// A name added to the context joins its chain, which is the list both
    /// readers of class names walk.
    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.added.borrow_mut().push(name.clone());
        Ok(())
    }

    fn is_valid(&self) -> bool {
        self.members.is_valid()
    }

    fn invalidate(&self) -> bool {
        self.members.invalidate()
    }

    /// A context is code: the caller that has a machine runs its body, and this
    /// surface has neither the frame nor the raise for it.
    fn context(&self) -> Option<&ContextObject> {
        Some(self)
    }
}

impl std::fmt::Debug for ContextObject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ContextObject")
            .field("kind", &self.kind.name())
            .field("name", &self.name)
            .finish()
    }
}

/// The `slot`-th operand of the instruction at `ip`.
fn operand(code: &Code, ip: usize, slot: usize) -> Result<i32, Fault> {
    super::operand(&code.code, ip, slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> TjsString {
        TjsString::from(value)
    }

    fn same(left: &Variant, right: &Variant) -> bool {
        crate::vm::runtime::exec::op::discern_equals(left, right)
    }

    fn context(kind: ContextType, name: &str) -> Rc<ContextObject> {
        Rc::new(ContextObject::new(Code::default(), kind, name))
    }

    fn register(number: i32) -> i32 {
        crate::vm::codegen::opcode::vm_reg_addr(number)
    }

    fn code_of(words: Vec<i32>, data: Vec<Variant>) -> Code {
        Code {
            code: words,
            data,
            variable_reserve_count: 2,
            max_frame_count: 4,
            ..Code::default()
        }
    }

    /// A context answers the members it publishes
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:886-949`,
    /// `RegisterFunction`).
    #[test]
    fn a_context_answers_its_own_members() {
        let object = context(ContextType::Class, "C");
        object
            .define(&text("v"), Variant::Integer(7))
            .expect("the member is stored");
        let read = object
            .prop_get(&text("v"), MemberFlags::NONE, None)
            .expect("the member is there");
        assert!(same(&read, &Variant::Integer(7)));
        assert!(matches!(
            object.prop_get(&text("nope"), MemberFlags::NONE, None),
            Err(ObjectError::MemberNotFound)
        ));
    }

    /// A class reaches its parent's member, and a function does not reach its
    /// declarer's — the language has no capture
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2812-2820`).
    #[test]
    fn a_class_reaches_its_parent_and_a_function_does_not() {
        let parent = context(ContextType::Class, "Base");
        parent
            .define(&text("v"), Variant::Integer(1))
            .expect("the member is stored");
        let child = Rc::new(ContextObject::new(
            Code::default(),
            ContextType::Class,
            "Child",
        ));
        child.set_parent(&parent);
        let read = child
            .prop_get(&text("v"), MemberFlags::NONE, None)
            .expect("the parent answers");
        assert!(same(&read, &Variant::Integer(1)));

        let declarer = context(ContextType::Function, "outer");
        declarer
            .define(&text("v"), Variant::Integer(1))
            .expect("the member is stored");
        let nested = Rc::new(ContextObject::new(
            Code::default(),
            ContextType::Function,
            "inner",
        ));
        nested.set_parent(&declarer);
        assert!(
            matches!(
                nested.prop_get(&text("v"), MemberFlags::NONE, None),
                Err(ObjectError::MemberNotFound)
            ),
            "a nested function does not read its declarer's members"
        );
    }

    /// The parent link does not keep the parent alive. A parent's members hold
    /// its children, so a strong link back would close a cycle and neither
    /// object would ever be freed; the reference keeps a plain pointer and takes a
    /// reference on it only in its debug build
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:176`, `:210-212`).
    #[test]
    fn the_parent_link_does_not_close_a_cycle() {
        let parent = context(ContextType::Class, "Base");
        let child = Rc::new(ContextObject::new(
            Code::default(),
            ContextType::Class,
            "Child",
        ));
        parent
            .define(
                &text("m"),
                Variant::Object(Closure {
                    object: Some(child.clone()),
                    objthis: None,
                }),
            )
            .expect("the member is stored");
        child.set_parent(&parent);
        let parent_link = Rc::downgrade(&parent);
        let child_link = Rc::downgrade(&child);
        drop(parent);
        drop(child);
        assert!(
            parent_link.upgrade().is_none(),
            "the parent is freed with its last handle"
        );
        assert!(
            child_link.upgrade().is_none(),
            "the child is freed with its last handle"
        );
    }

    /// The parent link answers only while the parent is alive, because it holds
    /// no reference to it
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:176`, `:210-212`).
    #[test]
    fn the_parent_link_answers_only_while_the_parent_is_alive() {
        let parent = context(ContextType::Class, "Base");
        let child = Rc::new(ContextObject::new(
            Code::default(),
            ContextType::Class,
            "Child",
        ));
        child.set_parent(&parent);
        assert_eq!(
            child.parent().map(|parent| parent.name().clone()),
            Some(text("Base"))
        );
        drop(parent);
        assert!(child.parent().is_none(), "the link holds no reference");
    }

    /// The class names of a chain answer the class question, the child's name
    /// first (`reference/krkr2/cpp/core/tjs2/tjsObject.cpp:2233-2247`).
    #[test]
    fn a_chain_answers_the_class_names() {
        let parent = context(ContextType::Class, "Base");
        let child = Rc::new(ContextObject::new(
            Code::default(),
            ContextType::Class,
            "Child",
        ));
        child.set_parent(&parent);
        let names: Vec<String> = child.class_names().iter().map(|n| n.to_string()).collect();
        assert_eq!(names, vec!["Child".to_string(), "Base".to_string()]);
        assert_eq!(child.is_instance_of(&text("Child"), None), Ok(true));
        assert_eq!(child.is_instance_of(&text("Base"), None), Ok(true));
        assert_eq!(child.is_instance_of(&text("Other"), None), Ok(false));
        assert_eq!(child.is_instance_of(&text("Object"), None), Ok(true));
    }

    /// A function context carries no class name, so it answers only the class
    /// every object belongs to.
    #[test]
    fn a_function_context_is_no_class() {
        let object = context(ContextType::Function, "f");
        assert!(object.class_names().is_empty());
        assert_eq!(object.is_instance_of(&text("f"), None), Ok(false));
        assert_eq!(object.is_instance_of(&text("Object"), None), Ok(true));
    }

    /// `GLOBAL` names the machine's global object, with itself as its receiver
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1283-1287`).
    #[test]
    fn global_names_the_machines_global_object() {
        let global: Rc<dyn Dispatch> = Rc::new(CustomObject::new([text("global")]));
        let code = code_of(
            vec![vm::GLOBAL, register(1), vm::SRV, register(1), vm::RET],
            vec![],
        );
        let mut machine = super::super::Machine::with_global(global.clone());
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        let Variant::Object(closure) = answer else {
            panic!("the answer is not an object");
        };
        let object = closure.object.expect("an object");
        assert!(Rc::ptr_eq(&object, &global));
        let receiver = closure.objthis.expect("a receiver");
        assert!(Rc::ptr_eq(&receiver, &global));
    }

    /// A machine with no global object refuses `GLOBAL` rather than naming
    /// nothing.
    #[test]
    fn global_without_a_global_object_is_refused() {
        let code = code_of(vec![vm::GLOBAL, register(1), vm::RET], vec![]);
        let mut machine = super::super::Machine::new();
        assert!(matches!(
            machine.run(&code, None, &[]),
            Err(Fault::Broken(_))
        ));
    }

    /// `CHGTHIS` rebinds the closure's receiver, and a receiver of the null
    /// object clears it
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1276-1281`).
    #[test]
    fn chgthis_rebinds_the_receiver() {
        let callee: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let receiver: Rc<dyn Dispatch> = Rc::new(CustomObject::new([text("receiver")]));
        let code = code_of(
            vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(2),
                register(1),
                vm::CHGTHIS,
                register(1),
                register(2),
                vm::SRV,
                register(1),
                vm::RET,
            ],
            vec![
                Variant::Object(Closure {
                    object: Some(callee),
                    objthis: None,
                }),
                Variant::Object(Closure {
                    object: Some(receiver.clone()),
                    objthis: Some(receiver.clone()),
                }),
            ],
        );
        let mut machine = super::super::Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        let Variant::Object(closure) = answer else {
            panic!("the answer is not an object");
        };
        let rebound = closure.objthis.expect("a receiver");
        assert!(Rc::ptr_eq(&rebound, &receiver));
    }

    /// `REGMEMBER` publishes the context's members onto the frame's receiver and
    /// rebinds each one's receiver to it
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2708-2740`).
    #[test]
    fn regmember_publishes_the_contexts_members() {
        let function: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let context = Rc::new(ContextObject::new(
            Code {
                code: vec![vm::REGMEMBER, vm::RET],
                variable_reserve_count: 2,
                ..Code::default()
            },
            ContextType::Class,
            "C",
        ));
        context
            .define(
                &text("m"),
                Variant::Object(Closure {
                    object: Some(function),
                    objthis: None,
                }),
            )
            .expect("the function is stored");
        let instance: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let mut machine = super::super::Machine::new();
        machine
            .run_context(&context, Some(instance.clone()), &[])
            .expect("the context runs");
        let member = instance
            .prop_get(&text("m"), MemberFlags::NONE, None)
            .expect("the member was published");
        let Variant::Object(closure) = member else {
            panic!("the published member is not an object");
        };
        let rebound = closure.objthis.expect("a receiver");
        assert!(
            Rc::ptr_eq(&rebound, &instance),
            "the published function's receiver is the instance"
        );
    }

    /// A static member is not published
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2717-2719`).
    #[test]
    fn regmember_skips_a_static_member() {
        let context = Rc::new(ContextObject::new(
            Code {
                code: vec![vm::REGMEMBER, vm::RET],
                variable_reserve_count: 2,
                ..Code::default()
            },
            ContextType::Class,
            "C",
        ));
        context
            .members()
            .prop_set(
                &text("s"),
                MemberFlags::ENSURE | MemberFlags::STATIC,
                &Variant::Integer(1),
                None,
            )
            .expect("the static member is stored");
        let instance: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let mut machine = super::super::Machine::new();
        machine
            .run_context(&context, Some(instance.clone()), &[])
            .expect("the context runs");
        assert!(matches!(
            instance.prop_get(&text("s"), MemberFlags::NONE, None),
            Err(ObjectError::MemberNotFound)
        ));
    }

    /// `ADDCI` appends the name the instruction names to the object in its
    /// register (`reference/krkr2/cpp/core/tjs2/tjsObject.cpp:2236-2246`,
    /// `ClassInstanceInfo(TJS_CII_ADD, ...)`).
    #[test]
    fn addci_appends_the_class_name() {
        let instance = Rc::new(CustomObject::new(Vec::new()));
        let handle: Rc<dyn Dispatch> = instance.clone();
        let code = code_of(
            vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(2),
                register(1),
                vm::ADDCI,
                register(1),
                register(2),
                vm::SRV,
                register(1),
                vm::RET,
            ],
            vec![
                Variant::Object(Closure {
                    object: Some(handle),
                    objthis: None,
                }),
                Variant::String(text("C")),
            ],
        );
        let mut machine = super::super::Machine::new();
        let _ = machine.run(&code, None, &[]).expect("the stream runs");
        assert_eq!(instance.class_names(), vec![text("C")]);
        assert_eq!(instance.is_instance_of(&text("C"), None), Ok(true));
        assert_eq!(instance.is_instance_of(&text("Other"), None), Ok(false));
    }

    /// A name added to a class joins the chain both of its readers walk, so an
    /// instance of it answers the question the name asks
    /// (`reference/krkr2/cpp/core/tjs2/tjsObject.cpp:2236-2246`).
    #[test]
    fn a_name_added_to_a_class_joins_its_chain() {
        let base = context(ContextType::Class, "Base");
        let child = Rc::new(ContextObject::new(
            Code::default(),
            ContextType::Class,
            "Child",
        ));
        child.set_parent(&base);
        child
            .add_class_name(&text("Alias"))
            .expect("the name is added");
        let names: Vec<String> = child.class_names().iter().map(|n| n.to_string()).collect();
        assert_eq!(
            names,
            vec!["Child".to_string(), "Alias".to_string(), "Base".to_string()]
        );
        assert_eq!(child.is_instance_of(&text("Alias"), None), Ok(true));
    }

    /// The four opcodes are the ones handled, and no others.
    #[test]
    fn the_four_instructions_are_the_ones_handled() {
        for opcode in [vm::CHGTHIS, vm::GLOBAL, vm::ADDCI, vm::REGMEMBER] {
            assert!(handles(opcode));
        }
        assert!(!handles(vm::CALL));
        assert!(!handles(vm::NOP));
    }
}
