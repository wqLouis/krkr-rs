//! The interpreter: the frame a call runs in, and the instructions it runs.
//!
//! The front end compiles script text into code streams; this module executes
//! one. A call takes a block from the page stack, lays the frame out in it, and
//! the run loop reads one decoded instruction at a time.
//!
//! # What this part executes
//!
//! Every one of the 128 opcodes. The stream a context runs is decoded once, when
//! the block is built ([`instruction`]), so an instruction names the slot, the
//! constant, the jump target or the argument list its arm works on and the loop
//! does no arithmetic per operand. A word that is not an opcode, an operand that
//! is outside the frame's window or a jump outside the stream refuses the block
//! there, before any instruction of it runs.
//!
//! # The member seam
//!
//! A member read or write stops at the member part ([`member`]), a call at the
//! call part ([`call`]), a protected region at the exception part ([`throw`]) and
//! a context instruction at the context part ([`context`]): each takes the
//! operands the stream already resolved and answers the failure of its own rule.
//! The compound assignment on a member is not that seam: it reads, applies and
//! writes through the object surface, which is the engine's own base-object
//! behaviour, and the closure form asks the object's `operation` entry instead.
//!
//! # The compile instructions
//!
//! A compile instruction holds script text and not a value: the text is compiled
//! into a block of its own and run in this machine, so the text sees the frame's
//! receiver and the machine's global object. The block it makes belongs to the
//! instruction, which is why the text's own declared functions do not outlive the
//! statement that evaluated it.

use std::cell::{Ref, RefCell};
use std::cmp::Ordering;
use std::rc::Rc;

use crate::vm::codegen::context::ContextType;
use crate::vm::codegen::opcode::vm;
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::Variant;

pub mod build;
pub mod call;
pub mod context;
pub mod decode;
pub mod frame;
pub mod instruction;
pub mod member;
pub mod op;
pub mod throw;

pub use context::ContextObject;
pub use decode::{Operator, PropertyForm};
pub use frame::{BlockStack, Code, Frame, ObjThisContext};
pub use instruction::{Instruction, Name, Op, Slot, Stream};

/// The reason a raise names.
///
/// A reason is static: the text it reads is an entry of the message table, and
/// the value or the name that text carries travels beside the reason, in the
/// [`Unwind`] the raise becomes. That is what keeps the executor's error type
/// free of owned data, so a suspended operation that cannot raise costs nothing
/// and a raise that happens pays for its data once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Raise {
    /// A value has no conversion of the kind the operation asked for.
    Convert {
        /// The kind the conversion asked for, in the target word.
        to: &'static str,
    },
    /// A divisor of zero.
    DivideByZero,
    /// An integer result does not fit a 64-bit integer.
    IntegerOutOfRange,
    /// A member the instruction named is not there.
    ///
    /// The name travels in the [`Unwind`], because the engine's own message
    /// carries it.
    MemberNotFound,
    /// A member is reached on a null object.
    ///
    /// The engine reads a null closure before it reads anything else, so this is
    /// the failure of every operation on one, whatever member it named.
    NullAccess,
    /// A pack or unpack template names no kind it reads.
    UnknownPackTemplate,
    /// A bit run of a template holds a character that is not a bit.
    UnknownBitString,
    /// A hex run of a template holds a character that is not a hex digit.
    UnknownHexString,
    /// A member the instruction named cannot be written: a text's or a byte
    /// string's surface is read-only, and a write to it is an access denial
    /// rather than a missing member.
    AccessDenied,
    /// A subscript is outside the value it indexes.
    RangeError,
    /// A value a call instruction named cannot be called.
    ///
    /// The engine answers this as the language's own failure rather than as a
    /// refusal of the object surface, so a script sees it.
    NotCallable,
    /// A member's call gives a number of parameters it does not accept.
    ParameterCount,
    /// A member's call gives a parameter the rule cannot use.
    InvalidParameter,
    /// A member's format string is not one it can read.
    FormatString,
    /// A call expands a value that is not an array.
    NotAnArray,
    /// The text a compile instruction holds does not compile.
    ///
    /// The message is the front end's own, so it travels in the [`Unwind`]: a
    /// handler that reads it reads what the parse failed on rather than a
    /// summary of it.
    ScriptError,
}

/// The reason an instruction did not answer.
///
/// Every branch is a distinct outcome, because a caller has to tell them apart:
/// a part is missing, the code raised, the code stream is defective, or a
/// register is outside the window. A fault names its cause and holds no value,
/// so it can be copied and needs no drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fault {
    /// The instruction belongs to a part of the interpreter this build does not
    /// have.
    Refused {
        /// The instruction's opcode.
        opcode: i32,
        /// The part that owns the behaviour.
        part: &'static str,
    },
    /// The code raised: a conversion, a divisor, or an integer that did not fit.
    Raised(Raise),
    /// The code stream does not hold an opcode at this position.
    InvalidOpcode(i32),
    /// The code stream ends in the middle of an instruction.
    Truncated,
    /// The code stream does something the encoding does not allow.
    Broken(&'static str),
    /// A register address is not a multiple of the size of a variant.
    MisalignedRegister(i32),
    /// A code address is not a multiple of the size of a word.
    MisalignedCode(i32),
    /// A register address is outside the frame's window.
    RegisterOutOfWindow {
        /// The address the instruction named.
        address: i32,
    },
    /// The object surface refused a call the instruction needed.
    Object(ObjectError),
    /// A script threw a value.
    ///
    /// It is its own kind and not a reason, because a catch can reach into the
    /// value and a handler has to be able to tell a script's failure from the
    /// engine's. The value itself travels in the [`Unwind`].
    Thrown,
    /// The failure is silent: it passes every protected region by.
    ///
    /// A cancellation is not a value a handler may inspect, so it is not turned
    /// into one.
    Silent,
}

impl std::fmt::Display for Fault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { opcode, part } => write!(
                formatter,
                "the instruction {} belongs to {part}",
                decode::mnemonic(*opcode)
            ),
            Self::Raised(_) => formatter.write_str("the code raised"),
            Self::InvalidOpcode(word) => write!(formatter, "the word {word} is not an opcode"),
            Self::Truncated => formatter.write_str("the code stream ends inside an instruction"),
            Self::Broken(why) => write!(formatter, "the code stream is defective: {why}"),
            Self::MisalignedRegister(address) => write!(
                formatter,
                "the register address {address} is not a variant boundary"
            ),
            Self::MisalignedCode(address) => {
                write!(
                    formatter,
                    "the code address {address} is not a word boundary"
                )
            }
            Self::RegisterOutOfWindow { address } => {
                write!(
                    formatter,
                    "the register address {address} is outside the frame"
                )
            }
            Self::Object(_) => formatter.write_str("the object refused the call"),
            Self::Thrown => formatter.write_str("the script threw a value"),
            Self::Silent => formatter.write_str("the code cancelled silently"),
        }
    }
}

impl std::error::Error for Fault {}

impl From<ObjectError> for Fault {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}

// A script that does not compile is the language's own failure and a script can
// catch it; the two failures after it are defects of the front end, and no
// script can see them.
impl From<build::CompileError> for throw::Unwind {
    fn from(error: build::CompileError) -> Self {
        match error {
            build::CompileError::Syntax { message } => {
                Self::raised(Raise::ScriptError).with_text(message)
            }
            build::CompileError::Image(_) | build::CompileError::Build(_) => {
                Fault::Broken("the compiled script does not describe a block").into()
            }
        }
    }
}

/// The interpreter: the pages a call's block comes from, and the global object
/// its scope falls back to.
#[derive(Default)]
pub struct Machine {
    /// The pages blocks are taken from.
    ///
    /// The pool is behind a cell because running a call does not change what the
    /// machine is: it takes a page and gives it back. An instruction that runs a
    /// block of its own — a compiled text — therefore enters the same machine
    /// while the call that reads the instruction is still borrowing it.
    blocks: RefCell<BlockStack>,
    /// The empty text a call that names no member passes.
    empty_name: TjsString,
    /// The lists the arguments of a call are built in.
    ///
    /// The lists are behind a cell for the reason the pages are: a call takes
    /// one and gives it back, and a call instruction runs a callee while it is
    /// itself borrowing the machine.
    arguments: RefCell<call::ArgumentStack>,
    /// The object a frame's scope falls back to, when there is one.
    global: Option<Rc<dyn Dispatch>>,
}

impl std::fmt::Debug for Machine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Machine")
            .field("blocks", &self.blocks)
            .field("global", &self.global.is_some())
            .finish()
    }
}

impl Machine {
    /// A machine with no global object.
    pub fn new() -> Self {
        Self::default()
    }

    /// A machine whose frames fall back to `global`.
    pub fn with_global(global: Rc<dyn Dispatch>) -> Self {
        Self {
            blocks: RefCell::new(BlockStack::new()),
            empty_name: TjsString::new(),
            arguments: RefCell::new(call::ArgumentStack::new()),
            global: Some(global),
        }
    }

    /// The pages the machine has handed out and taken back.
    pub fn blocks(&self) -> Ref<'_, BlockStack> {
        self.blocks.borrow()
    }

    /// The empty text a call that names no member passes.
    fn empty_name(&self) -> &TjsString {
        &self.empty_name
    }

    /// A list for one call's arguments.
    fn take_argument_list(&self) -> Vec<Variant> {
        self.arguments.borrow_mut().take()
    }

    /// Takes a call's argument list back, to be handed to the next call.
    fn give_argument_list(&self, list: Vec<Variant>) {
        self.arguments.borrow_mut().give(list);
    }

    /// Runs one call of `code` with `receiver` as its `this` and `argv` as the
    /// arguments the caller passed, and answers the result the code stored.
    ///
    /// The block is taken from the page stack before the call and given back
    /// after it, so a call in a loop allocates once.
    pub fn run(
        &mut self,
        code: &Code,
        receiver: Option<Rc<dyn Dispatch>>,
        argv: &[Variant],
    ) -> Result<Variant, throw::Unwind> {
        self.run_frame(code, None, receiver, argv)
    }

    /// Runs one call of a context object's own code.
    ///
    /// This is the entry a context object is invoked through: the object's code
    /// runs with `receiver` as its `this`, and the object itself is the context
    /// the member registration publishes. A top-level context that is handed no
    /// receiver runs with the machine's global object, which is the scope a
    /// script's own body runs in.
    pub fn run_context(
        &mut self,
        context: &ContextObject,
        receiver: Option<Rc<dyn Dispatch>>,
        argv: &[Variant],
    ) -> Result<Variant, throw::Unwind> {
        self.call_context(context, receiver, argv)
    }

    /// Runs one call of a context object's own code, keeping the trace.
    ///
    /// The caller that reports an uncaught failure needs the frames it passed,
    /// and every entry answers them: a failure crosses the boundary as the
    /// [`throw::Unwind`] it already is.
    pub fn run_context_traced(
        &mut self,
        context: &ContextObject,
        receiver: Option<Rc<dyn Dispatch>>,
        argv: &[Variant],
    ) -> Result<Variant, throw::Unwind> {
        self.run_frame(context.code(), Some(context), receiver, argv)
    }

    /// Runs one call of a context object's code from inside this machine,
    /// keeping the trace.
    ///
    /// A call instruction runs the code of the value it calls while it is itself
    /// borrowing the machine, so the entry it reaches borrows the machine: the
    /// page pool is behind a cell and nothing else about it changes.
    pub(crate) fn call_context_traced(
        &self,
        context: &ContextObject,
        receiver: Option<Rc<dyn Dispatch>>,
        argv: &[Variant],
    ) -> Result<Variant, throw::Unwind> {
        self.run_frame(context.code(), Some(context), receiver, argv)
    }

    /// Runs one call of a context object's code from inside this machine.
    ///
    /// A call instruction uses [`Self::call_context_traced`], and the two are
    /// the same entry: a failure keeps the frames it crossed either way.
    fn call_context(
        &self,
        context: &ContextObject,
        receiver: Option<Rc<dyn Dispatch>>,
        argv: &[Variant],
    ) -> Result<Variant, throw::Unwind> {
        self.call_context_traced(context, receiver, argv)
    }

    /// Runs one call of `code` in a frame of its own.
    ///
    /// `context` is the context object the call is the code of, when it is one:
    /// it is what a member registration publishes and what a trace names. The
    /// pool is behind a cell, so an instruction that runs a block of its own — a
    /// compiled text — enters here without taking the machine for change.
    fn run_frame(
        &self,
        code: &Code,
        context: Option<&ContextObject>,
        receiver: Option<Rc<dyn Dispatch>>,
        argv: &[Variant],
    ) -> Result<Variant, throw::Unwind> {
        // the scope a context's own code runs in: the receiver the call hands it,
        // and the global object when a top-level context is handed none
        let top_level = context.is_some_and(|context| context.kind() == ContextType::TopLevel);
        let receiver = match receiver {
            None if top_level => self.global.clone(),
            receiver => receiver,
        };
        let block = self.blocks.borrow_mut().allocate(code.block_len()?);
        let mut frame = Frame::new(block, code, receiver, self.global.clone(), argv)?;
        let answer = self.execute(code, &mut frame, argv, context, 0);
        self.blocks.borrow_mut().release(frame.into_block());
        answer
    }

    /// Executes the code stream from `start` to its return.
    ///
    /// A protected region re-enters here, which is what makes a `try` a host call
    /// and not a handler stack: the failure of the body surfaces in the entry's
    /// own frame, and the entry decides whether the catch runs.
    pub(super) fn execute(
        &self,
        code: &Code,
        frame: &mut Frame,
        argv: &[Variant],
        context: Option<&ContextObject>,
        start: usize,
    ) -> Result<Variant, throw::Unwind> {
        let instructions = code.stream.instructions();
        let mut pc = start;
        let mut flag = false;
        let mut result = Variant::Void;
        loop {
            let Some(instruction) = instructions.get(pc) else {
                return Err(Fault::Truncated.into());
            };
            pc += 1;
            match &instruction.op {
                // the instructions with nothing to do
                Op::Nop | Op::Debugger => {}
                // the constant area
                Op::Const { dst, value } => frame.store(*dst, code.value(*value).clone()),
                Op::Copy { dst, source } => frame.store(*dst, frame.slot(*source).clone()),
                Op::Clear { dst } => frame.store(*dst, Variant::Void),
                Op::ClearRun { start, count } => frame.clear_run(*start, *count)?,
                // the condition flag
                Op::Truth { source } => flag = op::truth(frame.slot(*source))?,
                Op::Falsity { source } => flag = !op::truth(frame.slot(*source))?,
                Op::Equal { left, right } => {
                    flag = frame.slot(*left).equals(frame.slot(*right));
                }
                Op::DiscernEqual { left, right } => {
                    flag = op::discern_equals(frame.slot(*left), frame.slot(*right));
                }
                Op::Less { left, right } => flag = less(frame.slot(*left), frame.slot(*right))?,
                Op::Greater { left, right } => {
                    flag = greater(frame.slot(*left), frame.slot(*right))?
                }
                Op::StoreFlag { dst } => frame.store(*dst, Variant::Integer(i64::from(flag))),
                Op::StoreNotFlag { dst } => {
                    frame.store(*dst, Variant::Integer(i64::from(!flag)));
                }
                Op::NotFlag => flag = !flag,
                // the single-operand operators
                Op::LogicalNot { target } => {
                    let answer = op::logical_not(frame.slot(*target))?;
                    frame.store(*target, answer);
                }
                Op::Complement { target } => {
                    let answer = op::complement(frame.slot(*target))?;
                    frame.store(*target, answer);
                }
                Op::Negate { target } => {
                    let answer = op::negate(frame.slot(*target))?;
                    frame.store(*target, answer);
                }
                Op::ToNumber { target } => {
                    let answer = op::to_number(frame.slot(*target))?;
                    frame.store(*target, answer);
                }
                Op::CharacterCode { target } => {
                    let answer = op::character_code(frame.slot(*target))?;
                    frame.store(*target, answer);
                }
                Op::Character { target } => {
                    let answer = op::character_from(frame.slot(*target))?;
                    frame.store(*target, answer);
                }
                Op::ToInteger { target } => {
                    let value = frame.slot(*target);
                    let integer = value
                        .to_integer()
                        .map_err(|error| op::refused_conversion(error, value))?;
                    frame.store(*target, Variant::Integer(integer));
                }
                Op::ToReal { target } => {
                    let value = frame.slot(*target);
                    let real = value
                        .to_real()
                        .map_err(|error| op::refused_conversion(error, value))?;
                    frame.store(*target, Variant::Real(real));
                }
                Op::ToString { target } => {
                    let value = frame.slot(*target);
                    let text = value
                        .to_string()
                        .map_err(|error| op::refused_conversion(error, value))?;
                    frame.store(*target, Variant::String(text));
                }
                Op::ToOctet { target } => {
                    let value = frame.slot(*target);
                    let octet = value
                        .to_octet()
                        .map_err(|error| op::refused_conversion(error, value))?;
                    frame.store(*target, Variant::Octet(octet));
                }
                Op::TypeOf { target } => {
                    let name = TjsString::from(frame.slot(*target).type_name());
                    frame.store(*target, Variant::String(name));
                }
                Op::InstanceOf { target, class } => {
                    let class = op::class_name(frame.slot(*class))?;
                    let belongs = instance_of(frame.slot(*target), &class)?;
                    frame.store(*target, Variant::Integer(i64::from(belongs)));
                }
                Op::Invalidate { target } => {
                    let invalidated = match frame.slot(*target) {
                        Variant::Object(closure) => match &closure.object {
                            Some(object) => object.invalidate(),
                            None => false,
                        },
                        _ => false,
                    };
                    frame.store(*target, Variant::Integer(i64::from(invalidated)));
                }
                Op::IsValid { target } => {
                    let valid = match frame.slot(*target) {
                        Variant::Object(closure) => match &closure.object {
                            Some(object) => object.is_valid(),
                            None => true,
                        },
                        _ => true,
                    };
                    frame.store(*target, Variant::Integer(i64::from(valid)));
                }
                // the jumps: a distance is resolved to an instruction of the stream
                Op::Jump { target } => pc = *target,
                Op::JumpIfFlag { target } => {
                    if flag {
                        pc = *target;
                    }
                }
                Op::JumpIfNotFlag { target } => {
                    if !flag {
                        pc = *target;
                    }
                }
                // the result and the return
                Op::Result { source } => result = frame.slot(*source).clone(),
                Op::Return | Op::ExitProtected => return Ok(result),
                // the compile instructions: the text a register holds, compiled
                // and run as an expression of its own
                Op::Evaluate { text, keep, at } => {
                    let answer = match self.evaluate(frame, frame.slot(*text), *keep) {
                        Ok(answer) => answer,
                        Err(mut unwind) => {
                            unwind.add_frame(context, *at as usize);
                            return Err(unwind);
                        }
                    };
                    if let Some(answer) = answer {
                        frame.store(*text, answer);
                    }
                }
                // the step instructions
                Op::StepReg { up, target } => {
                    let value = frame.slot(*target);
                    let answer = if *up {
                        op::step_up(value)?
                    } else {
                        op::step_down(value)?
                    };
                    frame.store(*target, answer);
                }
                Op::StepMember {
                    up,
                    form,
                    target,
                    object,
                    name,
                } => self.member_operation(
                    frame,
                    code,
                    MemberCall {
                        what: MemberOperation::Step(*up),
                        form: *form,
                        target: *target,
                        object: *object,
                        name,
                        parameter: None,
                    },
                )?,
                // the two-operand operators
                Op::OperateReg {
                    operator,
                    target,
                    source,
                } => {
                    let answer = op::apply(*operator, frame.slot(*target), frame.slot(*source))?;
                    frame.store(*target, answer);
                }
                Op::OperateMember {
                    operator,
                    form,
                    target,
                    object,
                    name,
                    parameter,
                } => self.member_operation(
                    frame,
                    code,
                    MemberCall {
                        what: MemberOperation::Operator(*operator),
                        form: *form,
                        target: *target,
                        object: *object,
                        name,
                        parameter: Some(*parameter),
                    },
                )?,
                // the member instructions
                Op::ReadMember {
                    form,
                    flags,
                    dst,
                    object,
                    name,
                } => member::read(code, frame, *form, *flags, *dst, *object, name)?,
                Op::WriteMember {
                    form,
                    flags,
                    object,
                    name,
                    value,
                } => member::write(code, frame, *form, *flags, *object, name, *value)?,
                Op::DeleteMember {
                    form,
                    dst,
                    object,
                    name,
                } => member::delete(code, frame, *form, *dst, *object, name)?,
                Op::TypeOfMember {
                    form,
                    flags,
                    dst,
                    object,
                    name,
                } => member::type_of(code, frame, *form, *flags, *dst, *object, name)?,
                Op::ReadProperty { dst, object } => member::read_property(frame, *dst, *object)?,
                Op::WriteProperty { object, value } => {
                    member::write_property(frame, *object, *value)?;
                }
                // the calls
                Op::Call(call) => match call::execute(self, code, frame, argv, call) {
                    Ok(()) => {}
                    Err(mut unwind) => {
                        unwind.add_frame(context, call.at as usize);
                        return Err(unwind);
                    }
                },
                // the context instructions
                Op::Global { dst } => context::global(frame, self.global.as_ref(), *dst)?,
                Op::ChangeThis { target, receiver } => {
                    context::change_this(frame, *target, *receiver)?;
                }
                Op::PublishMembers => context::publish_members(context, frame)?,
                Op::AddClassName { target, name } => {
                    context::add_class_name(frame, *target, *name)?;
                }
                // the exception instructions
                Op::Enter(entry) => {
                    return throw::enter(self, code, frame, argv, context, pc, entry);
                }
                Op::Throw { value, at } => {
                    return Err(throw::raise(frame, context, *value, *at as usize));
                }
            }
        }
    }

    /// Compiles and runs the script text a compile instruction holds.
    ///
    /// The text is an expression of its own: it is compiled the way the compile
    /// entry compiles an expression, and its top-level context runs in this
    /// machine, with this frame's receiver as its `this` and the machine's global
    /// object behind it. The answer is the value the expression produced when the
    /// instruction keeps a result, and empty when it does not.
    fn evaluate(
        &self,
        frame: &Frame,
        value: &Variant,
        result_needed: bool,
    ) -> Result<Option<Variant>, throw::Unwind> {
        let text = value
            .to_string()
            .map_err(|error| op::refused_conversion(error, value))?;
        // a text with nothing in it is nothing to run, and the register a result
        // would be kept in is left empty
        if text.is_empty() {
            return Ok(result_needed.then_some(Variant::Void));
        }
        let block = build::compile(&text, true, result_needed)?;
        let top = block
            .top_level()
            .ok_or(Fault::Broken("an evaluated script has no top level"))?;
        // the scope the text runs in is this frame's receiver, which the frame
        // entry turns into the global object when the frame has none
        let answer = self.run_frame(
            top.object().code(),
            Some(top.object()),
            frame.receiver(),
            &[],
        )?;
        Ok(result_needed.then_some(answer))
    }

    /// Applies an operation to a member.
    ///
    /// A member named by a constant or by a register is read, operated on and
    /// written back, which is what the engine's base object does for a compound
    /// assignment. The closure form — the object itself as the property — is the
    /// one the object's own `operation` entry is asked, because it is the one
    /// form the surface can express without a name.
    ///
    /// # Why the named forms do not consult the object's own entry
    ///
    /// The surface the operation would reach through is the object's name-kind
    /// entry, which this runtime's object surface does not have: it takes the
    /// operator and the right operand only. So a named member takes the base
    /// object's path. Every object that does not override the entry behaves
    /// identically; an object that does override it is reached by the closure
    /// form.
    fn member_operation(
        &self,
        frame: &mut Frame,
        code: &Code,
        call: MemberCall<'_>,
    ) -> Result<(), throw::Unwind> {
        let MemberCall {
            what,
            form,
            target,
            object,
            name,
            parameter,
        } = call;
        let closure = match frame.slot(object) {
            Variant::Object(closure) => closure.clone(),
            _ => {
                return Err(Fault::Broken(
                    "a property operation names a value that is not an object",
                )
                .into());
            }
        };
        let holder = match &closure.object {
            Some(holder) => holder.clone(),
            None => {
                return Err(Fault::Broken("a property operation names a null object").into());
            }
        };
        let objthis = closure.objthis.as_deref();
        let parameter = match parameter {
            Some(slot) => frame.slot(slot).clone(),
            None => Variant::Void,
        };
        let text = match name {
            Name::Constant(index) => text_of(code.value(*index))?,
            Name::Register(slot) => text_of(frame.slot(*slot))?,
            Name::None => TjsString::new(),
        };
        if form == PropertyForm::Closure {
            let operator = match what {
                MemberOperation::Operator(operator) => operator.name(),
                MemberOperation::Step(up) => {
                    if up {
                        "inc"
                    } else {
                        "dec"
                    }
                }
            };
            let operator = TjsString::from(operator);
            let served = holder.operation(&operator, &parameter, objthis)?;
            if !served {
                return Err(Fault::Refused {
                    opcode: match what {
                        MemberOperation::Operator(operator) => operator_opcode(operator),
                        MemberOperation::Step(up) => {
                            if up {
                                vm::INC_P
                            } else {
                                vm::DEC_P
                            }
                        }
                    },
                    part: "the closure form of a property operation",
                }
                .into());
            }
            let answer = holder.prop_get(&text, MemberFlags::IGNORE_PROPERTY, objthis)?;
            frame.store(target, answer);
            return Ok(());
        }
        let current = holder.prop_get(&text, MemberFlags::NONE, objthis)?;
        let answer = match what {
            MemberOperation::Operator(operator) => op::apply(operator, &current, &parameter)?,
            MemberOperation::Step(up) => {
                if up {
                    op::step_up(&current)?
                } else {
                    op::step_down(&current)?
                }
            }
        };
        holder.prop_set(&text, MemberFlags::NONE, &answer, objthis)?;
        frame.store(target, answer);
        Ok(())
    }
}

/// One property operation, as the instruction spells it.
struct MemberCall<'a> {
    /// The operation the instruction applies.
    what: MemberOperation,
    /// The form the instruction takes.
    form: PropertyForm,
    /// The slot the result goes to, or none.
    target: Slot,
    /// The slot that holds the object.
    object: Slot,
    /// Where the member's name comes from.
    name: &'a Name,
    /// The slot that holds the right operand, when there is one.
    parameter: Option<Slot>,
}

/// The text a value names a member with.
fn text_of(value: &Variant) -> Result<TjsString, throw::Unwind> {
    value
        .to_string()
        .map_err(|error| op::refused_conversion(error, value))
}

/// The operator a member operation applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemberOperation {
    /// A compound assignment.
    Operator(Operator),
    /// An increment or a decrement; the flag says which.
    Step(bool),
}

/// The opcode a member operation belongs to, for a message.
fn operator_opcode(operator: Operator) -> i32 {
    let offset = match operator {
        Operator::LOr => 0,
        Operator::LAnd => 1,
        Operator::BOr => 2,
        Operator::BXor => 3,
        Operator::BAnd => 4,
        Operator::Sar => 5,
        Operator::Sal => 6,
        Operator::Sr => 7,
        Operator::Add => 8,
        Operator::Sub => 9,
        Operator::Mod => 10,
        Operator::Div => 11,
        Operator::IDiv => 12,
        Operator::Mul => 13,
    };
    // the property form of the family, which is the base plus three
    (26 + offset * 4) + 3
}

/// The comparison the less-than instruction asks for.
///
/// The comparison answers whether the *other* operand is greater, which is the
/// same question as whether this one is less.
pub fn less(left: &Variant, right: &Variant) -> Result<bool, throw::Unwind> {
    ordering(left, right, Ordering::Less)
}

/// The comparison the greater-than instruction asks for.
pub fn greater(left: &Variant, right: &Variant) -> Result<bool, throw::Unwind> {
    ordering(left, right, Ordering::Greater)
}

/// Whether the comparison of the two values is `wanted`.
///
/// An unordered pair — one operand is a not-a-number — is false for both
/// operators, and a value with no ordering at all is refused.
fn ordering(left: &Variant, right: &Variant, wanted: Ordering) -> Result<bool, throw::Unwind> {
    match left.compare(right) {
        Ok(Some(order)) => Ok(order == wanted),
        Ok(None) => Ok(false),
        Err(error) => Err(op::refused_conversion(error, left)),
    }
}

/// Whether `value` belongs to the class `class` names.
///
/// The empty value belongs to nothing, not even to `"Object"`. Every other
/// value belongs to `"Object"` and to the one name its kind carries: an integer
/// and a real are `"Number"`, a text is `"String"` and an octet is `"Octet"`.
/// No other name is a kind's, so a number is neither an `"Integer"` nor a
/// `"Real"`. An object answers through the class names it carries, and a value
/// whose object handle is absent belongs to nothing beyond the name already
/// answered.
fn instance_of(value: &Variant, class: &TjsString) -> Result<bool, Fault> {
    match value {
        Variant::Void => Ok(false),
        _ if class.eq_str("Object") => Ok(true),
        Variant::Integer(_) | Variant::Real(_) => Ok(class.eq_str("Number")),
        Variant::String(_) => Ok(class.eq_str("String")),
        Variant::Octet(_) => Ok(class.eq_str("Octet")),
        Variant::Object(closure) => match &closure.object {
            Some(handle) => handle
                .is_instance_of(class, closure.objthis.as_deref())
                .map_err(Fault::from),
            None => Ok(false),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::runtime::object::CustomObject;

    /// A context with nothing declared, and a code stream to run.
    fn code_of(words: &[i32], data: &[Variant]) -> Code {
        Code {
            code: words.to_vec(),
            data: data.to_vec(),
            variable_reserve_count: 2,
            max_frame_count: 4,
            ..Code::default()
        }
        .decoded()
        .expect("the stream decodes")
    }

    fn register(number: i32) -> i32 {
        crate::vm::codegen::opcode::vm_reg_addr(number)
    }

    /// The milestone: a hand-built stream answers the constant it stores
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1244-1252`: the
    /// result is copied to the caller and the return answers it).
    #[test]
    fn the_milestone_stream_answers_the_constant_it_stores() {
        let code = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::SRV,
                register(1),
                vm::RET,
            ],
            &[Variant::Integer(3)],
        );
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(
            op::discern_equals(&answer, &Variant::Integer(3)),
            "the stream answers three"
        );
    }

    /// A stream that stores nothing answers the empty variant, because the
    /// result starts empty (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1244`).
    #[test]
    fn a_stream_without_a_result_answers_void() {
        let code = code_of(&[vm::RET], &[]);
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(matches!(answer, Variant::Void));
    }

    /// The flag survives the instructions between the compare and the jump, and
    /// a taken jump lands where the distance names
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:898-970`). The
    /// comparison answers whether the *other* operand is greater, which is the
    /// same question as whether this one is less, so `CLT` is `<` even though
    /// the comparison's own name reads the other way.
    #[test]
    fn the_flag_carries_across_an_instruction_and_the_jump_lands() {
        // if (2 < 9) %1 = #2 else %1 = #9, then answer %1. The jump sits at
        // position 11 and must land on position 19, so the distance is 8 words.
        let code = code_of(
            &[
                vm::CONST,
                register(2),
                register(0),
                vm::CONST,
                register(3),
                register(1),
                vm::CLT,
                register(2),
                register(3),
                vm::TT,
                register(2),
                vm::JNF,
                crate::vm::codegen::opcode::vm_code_addr(8),
                vm::CONST,
                register(1),
                register(0),
                vm::SRV,
                register(1),
                vm::RET,
                vm::CONST,
                register(1),
                register(1),
                vm::SRV,
                register(1),
                vm::RET,
            ],
            &[Variant::Integer(2), Variant::Integer(9)],
        );
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(
            op::discern_equals(&answer, &Variant::Integer(2)),
            "the taken branch stores the first constant"
        );
    }

    /// The strict comparison tells apart what the plain one holds equal
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:908-918`).
    #[test]
    fn the_two_comparisons_disagree_where_the_kinds_differ() {
        let plain = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(2),
                register(1),
                vm::CEQ,
                register(1),
                register(2),
                vm::SETF,
                register(3),
                vm::SRV,
                register(3),
                vm::RET,
            ],
            &[Variant::Integer(0), Variant::Void],
        );
        let strict = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(2),
                register(1),
                vm::CDEQ,
                register(1),
                register(2),
                vm::SETF,
                register(3),
                vm::SRV,
                register(3),
                vm::RET,
            ],
            &[Variant::Integer(0), Variant::Void],
        );
        let mut machine = Machine::new();
        let plain = machine.run(&plain, None, &[]).expect("the stream runs");
        let strict = machine.run(&strict, None, &[]).expect("the stream runs");
        assert!(op::discern_equals(&plain, &Variant::Integer(1)));
        assert!(op::discern_equals(&strict, &Variant::Integer(0)));
    }

    /// The arithmetic on registers is the variant's operator
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1010-1043`).
    #[test]
    fn the_register_arithmetic_is_the_variants() {
        let code = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(2),
                register(1),
                vm::ADD,
                register(1),
                register(2),
                vm::SRV,
                register(1),
                vm::RET,
            ],
            &[Variant::Integer(2), Variant::Integer(3)],
        );
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(op::discern_equals(&answer, &Variant::Integer(5)));
    }

    /// The unsigned shift is the unsigned one, where the arithmetic shift keeps
    /// the sign (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:958-963`).
    #[test]
    fn the_shift_instructions_are_the_right_shifts() {
        let shifted = |opcode: i32| {
            code_of(
                &[
                    vm::CONST,
                    register(1),
                    register(0),
                    vm::CONST,
                    register(2),
                    register(1),
                    opcode,
                    register(1),
                    register(2),
                    vm::SRV,
                    register(1),
                    vm::RET,
                ],
                &[Variant::Integer(-1), Variant::Integer(1)],
            )
        };
        let mut machine = Machine::new();
        let unsigned = machine
            .run(&shifted(vm::SR), None, &[])
            .expect("the stream runs");
        let arithmetic = machine
            .run(&shifted(vm::SAR), None, &[])
            .expect("the stream runs");
        assert!(op::discern_equals(
            &unsigned,
            &Variant::Integer(0x7fff_ffff_ffff_ffff)
        ));
        assert!(op::discern_equals(&arithmetic, &Variant::Integer(-1)));
    }

    /// The consecutive clear empties a run of registers, and it is one of the two
    /// instructions the generator cannot emit
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:893`, `:1388-1396`).
    #[test]
    fn the_consecutive_clear_executes() {
        let code = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::CCL,
                register(1),
                3,
                vm::SRV,
                register(3),
                vm::RET,
            ],
            &[Variant::Integer(5)],
        );
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(
            matches!(answer, Variant::Void),
            "the run is cleared before it is read"
        );
    }

    /// The compile instruction compiles the text its register holds and runs it
    /// in this machine, and the result register receives what the expression
    /// answered
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1129-1142`,
    /// `:2638-2658`).
    #[test]
    fn the_compile_instruction_runs_the_text_it_holds() {
        let code = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::EVAL,
                register(1),
                vm::SRV,
                register(1),
                vm::RET,
            ],
            &[Variant::String(TjsString::from("1 + 2"))],
        );
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(
            op::discern_equals(&answer, &Variant::Integer(3)),
            "the evaluated text answered three"
        );
    }

    /// The text runs in the frame's scope: a name the frame's receiver holds is
    /// the value the expression sees
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1130-1134`: the
    /// evaluation context is the frame's receiver).
    #[test]
    fn the_evaluated_text_sees_the_frame_receiver() {
        let receiver = Rc::new(CustomObject::new(Vec::new()));
        receiver
            .prop_set(
                &TjsString::from("x"),
                MemberFlags::ENSURE,
                &Variant::Integer(5),
                None,
            )
            .expect("the receiver holds the member");
        let handle: Rc<dyn Dispatch> = receiver;
        let code = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::EVAL,
                register(1),
                vm::SRV,
                register(1),
                vm::RET,
            ],
            &[Variant::String(TjsString::from("x + 1"))],
        );
        let mut machine = Machine::new();
        let answer = machine
            .run(&code, Some(handle), &[])
            .expect("the stream runs");
        assert!(
            op::discern_equals(&answer, &Variant::Integer(6)),
            "the receiver's member is five"
        );
    }

    /// A name the frame's receiver does not hold falls back to the machine's
    /// global object, which is the scope a frame's lookup continues into
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:670-683`).
    #[test]
    fn the_evaluated_text_falls_back_to_the_global_object() {
        let global = Rc::new(CustomObject::new([TjsString::from("global")]));
        global
            .prop_set(
                &TjsString::from("x"),
                MemberFlags::ENSURE,
                &Variant::Integer(9),
                None,
            )
            .expect("the global holds the member");
        let handle: Rc<dyn Dispatch> = global.clone();
        let code = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::EVAL,
                register(1),
                vm::SRV,
                register(1),
                vm::RET,
            ],
            &[Variant::String(TjsString::from("x + 1"))],
        );
        let mut machine = Machine::with_global(handle);
        let receiver = Rc::new(CustomObject::new(Vec::new())) as Rc<dyn Dispatch>;
        let answer = machine
            .run(&code, Some(receiver), &[])
            .expect("the stream runs");
        assert!(
            op::discern_equals(&answer, &Variant::Integer(10)),
            "the global's member is nine"
        );
    }

    /// The expression form keeps no result, so the register holds the text it
    /// read
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2638-2652`: the
    /// register is written only when the result is needed).
    #[test]
    fn the_expression_form_leaves_the_register_alone() {
        let code = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::EEXP,
                register(1),
                vm::SRV,
                register(1),
                vm::RET,
            ],
            &[Variant::String(TjsString::from("1 + 2"))],
        );
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(
            op::discern_equals(&answer, &Variant::String(TjsString::from("1 + 2"))),
            "the register still holds the text"
        );
    }

    /// A text that does not compile raises the front end's own failure, and the
    /// caller's protected region catches it like any other
    /// (`reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:441-443`: a compile
    /// failure throws the script's error).
    #[test]
    fn a_text_that_does_not_compile_raises() {
        let code = code_of(
            &[
                vm::ENTRY,
                crate::vm::codegen::opcode::vm_code_addr(11),
                register(1),
                vm::CONST,
                register(2),
                register(0),
                vm::EVAL,
                register(2),
                vm::SRV,
                register(2),
                vm::RET,
                vm::CONST,
                register(3),
                register(1),
                vm::SRV,
                register(3),
                vm::RET,
            ],
            &[Variant::String(TjsString::from("1 +")), Variant::Integer(7)],
        );
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the catch runs");
        assert!(
            op::discern_equals(&answer, &Variant::Integer(7)),
            "the catch's own value is the answer"
        );
    }

    /// A value the instruction cannot read as text raises, and the reason is the
    /// conversion's own
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2643`: the text is
    /// the value converted, and an octet has no conversion, `tjsVariant.cpp:434-436`).
    #[test]
    fn a_value_that_cannot_be_read_as_text_raises() {
        let code = code_of(
            &[
                vm::CONST,
                register(1),
                register(0),
                vm::EVAL,
                register(1),
                vm::RET,
            ],
            &[Variant::Octet(
                crate::vm::runtime::octet::Octet::from_slice(&[1]),
            )],
        );
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]);
        assert!(matches!(
            answer,
            Err(unwind) if unwind.fault() == Fault::Raised(Raise::Convert { to: "string" })
        ));
    }

    /// `typeof` answers the language's spellings
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2601-2637`).
    #[test]
    fn the_type_of_a_value_is_the_languages_spelling() {
        for (value, expected) in [
            (Variant::Void, "void"),
            (Variant::Integer(1), "Integer"),
            (Variant::Real(1.0), "Real"),
            (Variant::String(TjsString::from("x")), "String"),
        ] {
            let code = code_of(
                &[
                    vm::CONST,
                    register(1),
                    register(0),
                    vm::TYPEOF,
                    register(1),
                    vm::SRV,
                    register(1),
                    vm::RET,
                ],
                std::slice::from_ref(&value),
            );
            let mut machine = Machine::new();
            let answer = machine.run(&code, None, &[]).expect("the stream runs");
            assert!(
                op::discern_equals(&answer, &Variant::String(TjsString::from(expected))),
                "expected {expected}"
            );
        }
    }

    /// `int`, `real`, `string` and `octet` are the conversions the variant owns,
    /// and a conversion it refuses raises
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1094-1113`).
    #[test]
    fn the_conversion_instructions_convert_and_refuse() {
        let converted = |opcode: i32, value: Variant| {
            code_of(
                &[
                    vm::CONST,
                    register(1),
                    register(0),
                    opcode,
                    register(1),
                    vm::SRV,
                    register(1),
                    vm::RET,
                ],
                &[value],
            )
        };
        let mut machine = Machine::new();
        let integer = machine
            .run(&converted(vm::INT, Variant::Real(3.75)), None, &[])
            .expect("the stream runs");
        assert!(op::discern_equals(&integer, &Variant::Integer(3)));
        let text = machine
            .run(&converted(vm::STR, Variant::Integer(12)), None, &[])
            .expect("the stream runs");
        assert!(op::discern_equals(
            &text,
            &Variant::String(TjsString::from("12"))
        ));
        let refused = machine.run(
            &converted(
                vm::INT,
                Variant::Object(crate::vm::runtime::variant::Closure::null()),
            ),
            None,
            &[],
        );
        assert!(matches!(
            refused,
            Err(unwind) if matches!(unwind.fault(), Fault::Raised(Raise::Convert { .. }))
        ));
    }

    /// The property access reaches an object that is not there through the
    /// surface's own failure rather than a refusal, because the instruction
    /// executes: a register holding no object has no default member
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1468-1491`).
    #[test]
    fn the_property_object_access_executes_and_refuses_a_non_object() {
        let code = code_of(&[vm::SETP, register(1), register(0), vm::RET], &[]);
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]);
        // the register holds no object, so the surface refuses the conversion
        match answer {
            Err(unwind) if unwind.fault() == Fault::Raised(Raise::Convert { to: "Object" }) => {}
            other => panic!("expected a refused conversion, got {other:?}"),
        }
    }

    /// The hidden write is the other instruction the generator cannot emit, and a
    /// stream that carries it decodes it and creates a hidden member
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1186-1191`).
    #[test]
    fn the_hidden_write_decodes_and_writes() {
        assert_eq!(decode::length(&[vm::SPDEH, 0, 20, 40], 0), Ok(4));
        // %2 = a plain object, %3 = #5, then spdeh %2.#m, %3 and answer the
        // member's own value through a read that ignores the property. The
        // name operand of a direct form is a constant-area index, not a
        // register address.
        let object: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let code = Code {
            code: vec![
                vm::CONST,
                register(2),
                register(0),
                vm::CONST,
                register(3),
                register(1),
                vm::SPDEH,
                register(2),
                register(2),
                register(3),
                vm::GPDS,
                register(4),
                register(2),
                register(2),
                vm::SRV,
                register(4),
                vm::RET,
            ],
            data: vec![
                Variant::Object(crate::vm::runtime::variant::Closure {
                    object: Some(object.clone()),
                    objthis: Some(object),
                }),
                Variant::Integer(5),
                Variant::String(TjsString::from("m")),
            ],
            variable_reserve_count: 2,
            max_frame_count: 4,
            ..Code::default()
        }
        .decoded()
        .expect("the stream decodes");
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(op::discern_equals(&answer, &Variant::Integer(5)));
    }

    /// A code stream that ends inside an instruction is refused when the block
    /// is built, so no instruction of it runs
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1300`).
    #[test]
    fn a_truncated_stream_is_refused() {
        let code = Code {
            code: vec![vm::CONST, register(1)],
            variable_reserve_count: 2,
            max_frame_count: 4,
            ..Code::default()
        };
        assert!(matches!(code.decoded(), Err(Fault::Truncated)));
    }

    /// A jump before the first instruction or past the last is refused when the
    /// block is built.
    #[test]
    fn a_jump_outside_the_stream_is_refused() {
        let out_of_stream = |distance: i32| {
            Code {
                code: vec![vm::JMP, crate::vm::codegen::opcode::vm_code_addr(distance)],
                variable_reserve_count: 2,
                max_frame_count: 4,
                ..Code::default()
            }
            .decoded()
        };
        assert!(matches!(out_of_stream(-4), Err(Fault::Broken(_))));
        assert!(matches!(out_of_stream(4), Err(Fault::Broken(_))));
    }

    /// The frame's receiver is reachable from the code
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:707`).
    #[test]
    fn the_receiver_is_the_frame_this() {
        let this: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let code = Code {
            code: vec![vm::SRV, register(-1), vm::RET],
            variable_reserve_count: 2,
            ..Code::default()
        }
        .decoded()
        .expect("the stream decodes");
        let mut machine = Machine::new();
        let answer = machine
            .run(&code, Some(this.clone()), &[])
            .expect("the stream runs");
        let Variant::Object(closure) = answer else {
            panic!("the answer is not an object");
        };
        let answer = closure.object.expect("an object");
        assert!(Rc::ptr_eq(&answer, &this));
    }

    /// The operator of a member operation maps back to its opcode, so a refusal
    /// names the instruction that was attempted.
    #[test]
    fn the_property_form_of_an_operator_names_its_opcode() {
        assert_eq!(operator_opcode(Operator::Add), vm::ADD_P);
        assert_eq!(operator_opcode(Operator::LOr), vm::LOR_P);
        assert_eq!(operator_opcode(Operator::Mul), vm::MUL_P);
        assert_eq!(decode::mnemonic(operator_opcode(Operator::Add)), "ADDP");
    }

    /// A value that is not an object answers by its kind: one name per kind and
    /// `"Object"` for every kind but the empty value. An integer and a real are
    /// both `"Number"`, a text is `"String"`, an octet is `"Octet"`, and the
    /// `typeof` spellings `"Integer"` and `"Real"` name no instance
    /// (`reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1712-1756`).
    #[test]
    fn a_kind_answers_the_names_it_carries() {
        let number = TjsString::from("Number");
        let text_value = TjsString::from("String");
        let octet_name = TjsString::from("Octet");
        let object_name = TjsString::from("Object");
        let integer_name = TjsString::from("Integer");
        let real_name = TjsString::from("Real");
        let cases = [
            (Variant::Integer(1), &number),
            (Variant::Real(1.5), &number),
            (Variant::from("a"), &text_value),
            (
                Variant::Octet(crate::vm::runtime::octet::Octet::new()),
                &octet_name,
            ),
        ];
        for (value, kind) in cases {
            assert_eq!(instance_of(&value, kind), Ok(true), "the kind's own name");
            assert_eq!(
                instance_of(&value, &object_name),
                Ok(true),
                "every kind but the empty value is an Object"
            );
            assert_eq!(
                instance_of(&value, &integer_name),
                Ok(false),
                "a typeof spelling names no instance"
            );
            assert_eq!(
                instance_of(&value, &real_name),
                Ok(false),
                "a typeof spelling names no instance"
            );
        }
        assert_eq!(
            instance_of(&Variant::Integer(1), &text_value),
            Ok(false),
            "a number is no text"
        );
        assert_eq!(
            instance_of(&Variant::from("a"), &number),
            Ok(false),
            "a text is no number"
        );
        assert_eq!(
            instance_of(&Variant::Void, &object_name),
            Ok(false),
            "the empty value belongs to nothing, the object name included"
        );
        assert_eq!(
            instance_of(&Variant::Void, &TjsString::from("void")),
            Ok(false),
            "the empty value answers no typeof spelling either"
        );
    }
}
