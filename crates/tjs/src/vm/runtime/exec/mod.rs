//! The interpreter: the frame a call runs in, and the instructions it runs.
//!
//! The front end compiles script text into code streams; this module executes
//! one. A call takes a block from the page stack, lays the frame out in it, and
//! the run loop reads the code one instruction at a time.
//!
//! # What this part executes
//!
//! | group | opcodes | what it does |
//! |---|---|---|
//! | the control and comparison instructions | `0 … 17` | the condition flag, the compare, the jump |
//! | the step instructions | `18 … 25` | `++` and `--`, on a register and on a member |
//! | the two-operand operators | `26 … 81` | the fourteen compound assignments, on a register and on a member |
//! | the single-operand operators | `82 … 98` | `~`, `typeof`, `#`, `$`, `+`, `-`, `int`, `real`, `string`, `octet`, `invalidate`, `isvalid`, `instanceof` |
//! | the frame instructions | `118`, `119`, `127` | the result, the return, the debugger break |
//!
//! Everything else is owned by another part of the interpreter and is named as
//! such: [`Fault::Refused`] carries the opcode and the part that owns it, so a
//! refusal says who is missing rather than saying the opcode is unknown. The
//! opcodes outside this part are the fifteen member accesses and the two `typeof`
//! member forms, the four calls, the three exception instructions, the four
//! context instructions and the two compile-and-run instructions.
//!
//! # The two opcodes the generator cannot emit
//!
//! The consecutive clear and the member write that creates a hidden member are
//! not reachable from the code generator, and a script block that arrives as
//! bytecode can still carry them. The consecutive clear executes here; the hidden
//! write reaches the member seam with its flags resolved, so the one thing it
//! adds — the hidden flag — is decided where every other member write's flags
//! are.
//!
//! # The member seam
//!
//! A member read or write is the member access part's, not this one's, so the
//! fifteen access opcodes and the two `typeof` member forms stop at a named
//! seam. The compound assignment on a member is not that seam: it reads, applies
//! and writes through the object surface, which is the engine's own base-object
//! behaviour, and the closure form asks the object's `operation` entry instead.

use std::cmp::Ordering;
use std::rc::Rc;

use crate::vm::codegen::opcode::vm;
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::Variant;

pub mod call;
pub mod decode;
pub mod frame;
pub mod member;
pub mod op;

pub use decode::{Operator, PropertyForm};
pub use frame::{BlockStack, Code, Frame, ObjThisContext};

/// The failure a raising rule reports.
///
/// The value that failed travels with it, so the message a script reads can be
/// the engine's own text rather than a summary.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Raise {
    /// A value has no conversion of the kind the operation asked for.
    Convert {
        /// The value the conversion was asked of.
        value: Box<Variant>,
        /// The kind the conversion asked for, in the variant's own label.
        to: &'static str,
    },
    /// A divisor of zero.
    DivideByZero,
    /// An integer result does not fit a 64-bit integer.
    IntegerOutOfRange,
    /// A member the instruction named is not there.
    ///
    /// The name travels because the engine's own message carries it.
    MemberNotFound {
        /// The name the instruction named.
        name: TjsString,
    },
    /// A member the instruction named cannot be written: a text's or a byte
    /// string's surface is read-only, and a write to it is an access denial
    /// rather than a missing member.
    AccessDenied,
    /// A subscript is outside the value it indexes.
    RangeError,
    /// A value a call instruction named cannot be called.
    ///
    /// The engine answers this as the language's own failure rather than as a
    /// refusal of the object surface, so a script sees it; the member the call
    /// named travels with it because a script's message names it.
    NotCallable {
        /// The member the call named, or nothing when it named none.
        name: TjsString,
    },
}

/// Two raises are the same failure when they are the same kind, name the same
/// kind and hold equal values.
///
/// A variant has no equality of its own, so the value is compared with the strict
/// comparison, which is equality over every kind.
impl PartialEq for Raise {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Convert {
                    value: left,
                    to: left_to,
                },
                Self::Convert {
                    value: right,
                    to: right_to,
                },
            ) => left_to == right_to && op::discern_equals(left, right),
            (Self::DivideByZero, Self::DivideByZero) => true,
            (Self::IntegerOutOfRange, Self::IntegerOutOfRange) => true,
            (Self::MemberNotFound { name: left }, Self::MemberNotFound { name: right }) => {
                left == right
            }
            (Self::AccessDenied, Self::AccessDenied) => true,
            (Self::RangeError, Self::RangeError) => true,
            (Self::NotCallable { name: left }, Self::NotCallable { name: right }) => left == right,
            _ => false,
        }
    }
}

/// The reason an instruction did not answer.
///
/// Every branch is a distinct outcome, because a caller has to tell them apart:
/// a part is missing, the code raised, the code stream is defective, or a
/// register is outside the window.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Fault {
    /// The instruction belongs to a part of the interpreter this build does not
    /// have.
    #[error("the instruction {opcode} belongs to {part}")]
    Refused {
        /// The instruction's mnemonic.
        opcode: String,
        /// The part that owns the behaviour.
        part: &'static str,
    },
    /// The code raised: a conversion, a divisor, or an integer that did not fit.
    #[error("the code raised")]
    Raised(Raise),
    /// The code stream does not hold an opcode at this position.
    #[error("the word {0} is not an opcode")]
    InvalidOpcode(i32),
    /// The code stream ends in the middle of an instruction.
    #[error("the code stream ends inside an instruction")]
    Truncated,
    /// The code stream does something the encoding does not allow.
    #[error("the code stream is defective: {0}")]
    Broken(&'static str),
    /// A register address is not a multiple of the size of a variant.
    #[error("the register address {0} is not a variant boundary")]
    MisalignedRegister(i32),
    /// A code address is not a multiple of the size of a word.
    #[error("the code address {0} is not a word boundary")]
    MisalignedCode(i32),
    /// A register address is outside the frame's window.
    #[error("the register address {address} is outside the frame")]
    RegisterOutOfWindow {
        /// The address the instruction named.
        address: i32,
    },
    /// The object surface refused a call the instruction needed.
    #[error("the object refused the call")]
    Object(ObjectError),
}

impl From<ObjectError> for Fault {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}

/// The interpreter: the pages a call's block comes from, and the global object
/// its scope falls back to.
#[derive(Default)]
pub struct Machine {
    /// The pages blocks are taken from.
    blocks: BlockStack,
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
            blocks: BlockStack::new(),
            global: Some(global),
        }
    }

    /// The pages the machine has handed out and taken back.
    pub fn blocks(&self) -> &BlockStack {
        &self.blocks
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
    ) -> Result<Variant, Fault> {
        let block = self.blocks.allocate(code.block_len()?);
        let mut frame = Frame::new(block, code, receiver, self.global.clone(), argv)?;
        let answer = self.execute(code, &mut frame, argv);
        self.blocks.release(frame.into_block());
        answer
    }

    /// Executes the code stream from its first instruction to its return.
    fn execute(&self, code: &Code, frame: &mut Frame, argv: &[Variant]) -> Result<Variant, Fault> {
        let mut ip: usize = 0;
        let mut flag = false;
        let mut result = Variant::Void;
        loop {
            let opcode = decode::word(&code.code, ip)?;
            let next = ip
                .checked_add(decode::length(&code.code, ip)?)
                .ok_or(Fault::Broken("a code position does not fit a length"))?;
            match opcode {
                // the instructions with nothing to do
                vm::NOP | vm::DEBUGGER => {}
                // the constant area
                vm::CONST => {
                    let value = code.constant(operand(&code.code, ip, 2)?)?.clone();
                    *frame.reg_mut(operand(&code.code, ip, 1)?)? = value;
                }
                vm::CP => {
                    let source = operand(&code.code, ip, 2)?;
                    let value = frame.reg(source)?.clone();
                    *frame.reg_mut(operand(&code.code, ip, 1)?)? = value;
                }
                vm::CL => {
                    *frame.reg_mut(operand(&code.code, ip, 1)?)? = Variant::Void;
                }
                vm::CCL => {
                    let address = operand(&code.code, ip, 1)?;
                    let count = operand(&code.code, ip, 2)?;
                    frame.clear_run(address, count)?;
                }
                // the condition flag
                vm::TT => {
                    flag = op::truth(frame.reg(operand(&code.code, ip, 1)?)?)?;
                }
                vm::TF => {
                    flag = !op::truth(frame.reg(operand(&code.code, ip, 1)?)?)?;
                }
                vm::CEQ => {
                    let left = operand(&code.code, ip, 1)?;
                    let right = operand(&code.code, ip, 2)?;
                    flag = frame.reg(left)?.equals(frame.reg(right)?);
                }
                vm::CDEQ => {
                    let left = operand(&code.code, ip, 1)?;
                    let right = operand(&code.code, ip, 2)?;
                    flag = op::discern_equals(frame.reg(left)?, frame.reg(right)?);
                }
                vm::CLT => {
                    let left = operand(&code.code, ip, 1)?;
                    let right = operand(&code.code, ip, 2)?;
                    flag = less(frame.reg(left)?, frame.reg(right)?)?;
                }
                vm::CGT => {
                    let left = operand(&code.code, ip, 1)?;
                    let right = operand(&code.code, ip, 2)?;
                    flag = greater(frame.reg(left)?, frame.reg(right)?)?;
                }
                vm::SETF => {
                    *frame.reg_mut(operand(&code.code, ip, 1)?)? =
                        Variant::Integer(i64::from(flag));
                }
                vm::SETNF => {
                    *frame.reg_mut(operand(&code.code, ip, 1)?)? =
                        Variant::Integer(i64::from(!flag));
                }
                vm::LNOT => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    *frame.reg_mut(address)? = op::logical_not(&value)?;
                }
                vm::NF => {
                    flag = !flag;
                }
                // the jumps: the distance is from the instruction's own position
                vm::JF => {
                    if flag {
                        ip = jump(&code.code, ip, operand(&code.code, ip, 1)?)?;
                        continue;
                    }
                }
                vm::JNF => {
                    if !flag {
                        ip = jump(&code.code, ip, operand(&code.code, ip, 1)?)?;
                        continue;
                    }
                }
                vm::JMP => {
                    ip = jump(&code.code, ip, operand(&code.code, ip, 1)?)?;
                    continue;
                }
                // the result and the return
                vm::SRV => {
                    result = frame.reg(operand(&code.code, ip, 1)?)?.clone();
                }
                vm::RET => return Ok(result),
                // the single-operand operators
                vm::BNOT => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    *frame.reg_mut(address)? = op::complement(&value)?;
                }
                vm::CHS => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    *frame.reg_mut(address)? = op::negate(&value)?;
                }
                vm::NUM => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    *frame.reg_mut(address)? = op::to_number(&value)?;
                }
                vm::ASC => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    *frame.reg_mut(address)? = op::character_code(&value)?;
                }
                vm::CHR => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    *frame.reg_mut(address)? = op::character_from(&value)?;
                }
                vm::INT => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    let integer = value
                        .to_integer()
                        .map_err(|error| op::refused_conversion(error, &value))?;
                    *frame.reg_mut(address)? = Variant::Integer(integer);
                }
                vm::REAL => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    let real = value
                        .to_real()
                        .map_err(|error| op::refused_conversion(error, &value))?;
                    *frame.reg_mut(address)? = Variant::Real(real);
                }
                vm::STR => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    let text = value
                        .to_string()
                        .map_err(|error| op::refused_conversion(error, &value))?;
                    *frame.reg_mut(address)? = Variant::String(text);
                }
                vm::OCTET => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    let octet = value
                        .to_octet()
                        .map_err(|error| op::refused_conversion(error, &value))?;
                    *frame.reg_mut(address)? = Variant::Octet(octet);
                }
                vm::TYPEOF => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    *frame.reg_mut(address)? = Variant::String(TjsString::from(value.type_name()));
                }
                vm::INV => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    let invalidated = match &value {
                        Variant::Object(closure) => match &closure.object {
                            Some(object) => object.invalidate(),
                            None => false,
                        },
                        _ => false,
                    };
                    *frame.reg_mut(address)? = Variant::Integer(i64::from(invalidated));
                }
                vm::CHKINV => {
                    let address = operand(&code.code, ip, 1)?;
                    let value = frame.reg(address)?.clone();
                    let valid = match &value {
                        Variant::Object(closure) => match &closure.object {
                            Some(object) => object.is_valid(),
                            None => true,
                        },
                        _ => true,
                    };
                    *frame.reg_mut(address)? = Variant::Integer(i64::from(valid));
                }
                vm::CHKINS => {
                    let target = operand(&code.code, ip, 1)?;
                    let name = operand(&code.code, ip, 2)?;
                    let value = frame.reg(name)?.clone();
                    let class = op::class_name(&value)?;
                    let instance = frame.reg(target)?;
                    let belongs = match instance {
                        Variant::Object(closure) => match &closure.object {
                            Some(object) => {
                                object.is_instance_of(&class, closure.objthis.as_deref())?
                            }
                            None => false,
                        },
                        _ => false,
                    };
                    *frame.reg_mut(target)? = Variant::Integer(i64::from(belongs));
                }
                // the member instructions, then the two families, and then the
                // parts another part owns
                _ if member::handles(opcode) => member::execute(code, frame, ip, opcode)?,
                _ if call::handles(opcode) => call::execute(code, frame, argv, ip, opcode)?,
                _ => match (decode::step_of(opcode), decode::operator_of(opcode)) {
                    (Some((up, form)), _) => self.step(frame, code, ip, up, form)?,
                    (None, Some((operator, form))) => {
                        self.operate(frame, code, ip, operator, form)?;
                    }
                    (None, None) => return Err(refused(opcode)),
                },
            }
            ip = next;
        }
    }

    /// Applies a step instruction.
    ///
    /// The plain form converts and stores the register; the three property forms
    /// are the same instruction on a member.
    fn step(
        &self,
        frame: &mut Frame,
        code: &Code,
        ip: usize,
        up: bool,
        form: PropertyForm,
    ) -> Result<(), Fault> {
        let target = operand(&code.code, ip, 1)?;
        if form == PropertyForm::Plain {
            let value = frame.reg(target)?.clone();
            let answer = if up {
                op::step_up(&value)?
            } else {
                op::step_down(&value)?
            };
            *frame.reg_mut(target)? = answer;
            return Ok(());
        }
        let call = MemberCall {
            what: MemberOperation::Step(up),
            form,
            target,
            object: operand(&code.code, ip, 2)?,
            name: match form {
                PropertyForm::Direct => Name::Constant(operand(&code.code, ip, 3)?),
                PropertyForm::Indirect => Name::Register(operand(&code.code, ip, 3)?),
                _ => Name::None,
            },
            parameter: None,
        };
        self.member_operation(frame, code, call)
    }

    /// Applies a two-operand operator.
    fn operate(
        &self,
        frame: &mut Frame,
        code: &Code,
        ip: usize,
        operator: Operator,
        form: PropertyForm,
    ) -> Result<(), Fault> {
        let target = operand(&code.code, ip, 1)?;
        if form == PropertyForm::Plain {
            let source = operand(&code.code, ip, 2)?;
            let left = frame.reg(target)?.clone();
            let right = frame.reg(source)?.clone();
            let answer = op::apply(operator, &left, &right)?;
            *frame.reg_mut(target)? = answer;
            return Ok(());
        }
        let call = MemberCall {
            what: MemberOperation::Operator(operator),
            form,
            target,
            object: operand(&code.code, ip, 2)?,
            name: match form {
                PropertyForm::Direct => Name::Constant(operand(&code.code, ip, 3)?),
                PropertyForm::Indirect => Name::Register(operand(&code.code, ip, 3)?),
                _ => Name::None,
            },
            parameter: match form {
                PropertyForm::Direct | PropertyForm::Indirect => Some(operand(&code.code, ip, 4)?),
                _ => Some(operand(&code.code, ip, 3)?),
            },
        };
        self.member_operation(frame, code, call)
    }

    /// Applies an operator to a member.
    ///
    /// A member named by a constant or by a register is read, operated on and
    /// written back, which is what the engine's base object does for a compound
    /// assignment. The closure form — the object itself as the
    /// property — is the one the object's own `operation` entry is asked, because
    /// it is the one form the surface can express without a name.
    ///
    /// # Why the property forms do not consult the object's own entry
    ///
    /// The engine hands the object the member's *name* and the operation, and a
    /// name-kind operation is not on this runtime's object surface, which takes
    /// the operator and the right operand only. So a named member takes the base
    /// object's path. Every object that does not override the entry behaves
    /// identically; an object that does override it is reached by the closure
    /// form, and by the named forms only through the seam this names.
    fn member_operation(
        &self,
        frame: &mut Frame,
        code: &Code,
        call: MemberCall,
    ) -> Result<(), Fault> {
        let MemberCall {
            what,
            form,
            target,
            object,
            name,
            parameter,
        } = call;
        let receiver = frame.reg(object)?.clone();
        let closure = match &receiver {
            Variant::Object(closure) => closure.clone(),
            _ => {
                return Err(Fault::Broken(
                    "a property operation names a value that is not an object",
                ));
            }
        };
        let holder = match &closure.object {
            Some(holder) => holder.clone(),
            None => {
                return Err(Fault::Broken("a property operation names a null object"));
            }
        };
        let objthis = closure.objthis.as_deref();
        let parameter = match parameter {
            Some(address) => frame.reg(address)?.clone(),
            None => Variant::Void,
        };
        let text = match &name {
            Name::Constant(slot) => text_of(code.constant(*slot)?)?,
            Name::Register(address) => {
                let value = frame.reg(*address)?.clone();
                text_of(&value)?
            }
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
                    opcode: decode::mnemonic(match what {
                        MemberOperation::Operator(operator) => operator_opcode(operator),
                        MemberOperation::Step(up) => {
                            if up {
                                vm::INC_P
                            } else {
                                vm::DEC_P
                            }
                        }
                    }),
                    part: "the closure form of a property operation",
                });
            }
            let answer = holder.prop_get(&text, MemberFlags::IGNORE_PROPERTY, objthis)?;
            if target != 0 {
                *frame.reg_mut(target)? = answer;
            }
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
        if target != 0 {
            *frame.reg_mut(target)? = answer;
        }
        Ok(())
    }
}

/// One property operation, as the instruction spells it.
struct MemberCall {
    /// The operation the instruction applies.
    what: MemberOperation,
    /// The form the instruction takes.
    form: PropertyForm,
    /// The register the result goes to, or `0` for none.
    target: i32,
    /// The register that holds the object.
    object: i32,
    /// Where the member's name comes from.
    name: Name,
    /// The register that holds the right operand, when there is one.
    parameter: Option<i32>,
}

/// The text a value names a member with.
fn text_of(value: &Variant) -> Result<TjsString, Fault> {
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

/// Where a member operation's name comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Name {
    /// The constant area.
    Constant(i32),
    /// A register.
    Register(i32),
    /// Nowhere: the value itself is the property.
    None,
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

/// The fault for an instruction another part owns.
fn refused(opcode: i32) -> Fault {
    match decode::owner(opcode) {
        Some(part) => Fault::Refused {
            opcode: decode::mnemonic(opcode),
            part,
        },
        None => Fault::Broken("a refused instruction names no owner"),
    }
}

/// The `slot`-th operand of the instruction at `ip`.
///
/// The slot counts from the opcode, so slot 1 is the first operand.
pub(super) fn operand(code: &[i32], ip: usize, slot: usize) -> Result<i32, Fault> {
    let index = ip
        .checked_add(slot)
        .ok_or(Fault::Broken("an instruction's operand is out of range"))?;
    decode::word(code, index)
}

/// The instruction position a jump distance names.
///
/// The distance is relative to the instruction the jump is, not to the
/// instruction after it.
fn jump(code: &[i32], ip: usize, distance: i32) -> Result<usize, Fault> {
    let offset = decode::code_position(distance)?;
    let target = i64::try_from(ip)
        .map_err(|_| Fault::Broken("a code position does not fit an address"))?
        .checked_add(i64::from(offset))
        .ok_or(Fault::Broken("a jump target is out of range"))?;
    if target < 0 {
        return Err(Fault::Broken("a jump target is before the code stream"));
    }
    let target =
        usize::try_from(target).map_err(|_| Fault::Broken("a jump target is out of range"))?;
    if target > code.len() {
        return Err(Fault::Broken("a jump target is past the code stream"));
    }
    Ok(target)
}

/// The comparison the less-than instruction asks for.
///
/// The comparison answers whether the *other* operand is greater, which is the
/// same question as whether this one is less.
pub fn less(left: &Variant, right: &Variant) -> Result<bool, Fault> {
    ordering(left, right, Ordering::Less)
}

/// The comparison the greater-than instruction asks for.
pub fn greater(left: &Variant, right: &Variant) -> Result<bool, Fault> {
    ordering(left, right, Ordering::Greater)
}

/// Whether the comparison of the two values is `wanted`.
///
/// An unordered pair — one operand is a not-a-number — is false for both
/// operators, and a value with no ordering at all is refused.
fn ordering(left: &Variant, right: &Variant, wanted: Ordering) -> Result<bool, Fault> {
    match left.compare(right) {
        Ok(Some(order)) => Ok(order == wanted),
        Ok(None) => Ok(false),
        Err(error) => Err(op::refused_conversion(error, left)),
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
                1,
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
                1,
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
                1,
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
                1,
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
                    1,
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
        assert!(matches!(refused, Err(Fault::Raised(Raise::Convert { .. }))));
    }

    /// A member access is the member access part's, and the refusal names the
    /// instruction and the part
    /// (`docs/code/tjs/interpreter.md` §6, the packet plan).
    #[test]
    fn the_property_object_access_is_refused_by_name() {
        // `*o` forces the property surface, which no part implements yet
        let code = code_of(
            &[vm::SETP, register(1), register(0), vm::RET],
            &[Variant::String(TjsString::from("m"))],
        );
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]);
        match answer {
            Err(Fault::Refused { opcode, part }) => {
                assert_eq!(opcode, "SETP");
                assert_eq!(part, "the property-object access part");
            }
            other => panic!("expected a refusal, got {other:?}"),
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
                0,
                vm::CONST,
                register(3),
                1,
                vm::SPDEH,
                register(2),
                2,
                register(3),
                vm::GPDS,
                register(4),
                register(2),
                2,
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
        };
        let mut machine = Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(op::discern_equals(&answer, &Variant::Integer(5)));
    }

    /// A code stream that ends inside an instruction is refused, not read past
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1300`).
    #[test]
    fn a_truncated_stream_is_refused() {
        let code = code_of(&[vm::CONST, register(1)], &[]);
        let mut machine = Machine::new();
        assert!(matches!(
            machine.run(&code, None, &[]),
            Err(Fault::Truncated)
        ));
    }

    /// A jump before the first instruction or past the last is refused.
    #[test]
    fn a_jump_outside_the_stream_is_refused() {
        let code = code_of(
            &[vm::JMP, crate::vm::codegen::opcode::vm_code_addr(-4)],
            &[],
        );
        let mut machine = Machine::new();
        assert!(matches!(
            machine.run(&code, None, &[]),
            Err(Fault::Broken(_))
        ));
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
        };
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
}
