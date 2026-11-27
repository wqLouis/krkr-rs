//! The decoded instruction stream: one entry per instruction, operands resolved.
//!
//! The code stream is words: an opcode, then operands that are a byte offset
//! into a frame, an offset into a constant area, or a distance to a code
//! position. Every one of them is converted once, when the block is built, into
//! what the arm that runs the instruction needs:
//!
//! | operand | in the stream | here |
//! |---|---|---|
//! | a register | a byte offset from the frame base | a slot of the frame's block |
//! | a constant | a byte offset into the constant area | an index into it |
//! | a jump | a distance in code words | an index into the stream |
//! | an argument list | a count and a tail of registers | a range of one table |
//!
//! The run loop then walks a slice with a plain position: no address is scaled,
//! no alignment is checked and no operand is proved in range per instruction,
//! because the conversion either produced an in-range value or refused the
//! stream.
//!
//! # What the conversion refuses
//!
//! A word that is not an opcode, a stream that ends inside an instruction, a
//! register outside the frame's window, a constant outside the constant area, a
//! jump outside the stream, an argument flag that is not one — each is a defect
//! in the stream and is refused with the failure the encoding names for it. A
//! script cannot reach this: its text compiles into a stream that is in range by
//! construction, and a stream that is not is refused rather than run.

use super::Fault;
use super::decode::{self, Operator, PropertyForm};
use crate::vm::codegen::opcode::vm;
use crate::vm::runtime::object::MemberFlags;

/// A register, as a slot of the frame's block.
///
/// The block index is what the frame indexes with, so the register's byte
/// offset, its alignment and the frame base are spent here and not per
/// instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot(u32);

impl Slot {
    /// The slot an instruction keeps no result in.
    pub const DISCARD: Self = Self(u32::MAX);

    /// A slot of the given block index.
    pub(super) fn of(index: u32) -> Self {
        Self(index)
    }

    /// The block index.
    #[inline]
    pub(super) fn index(self) -> usize {
        self.0 as usize
    }
}

/// Where a member's name comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Name {
    /// The constant area.
    Constant(u32),
    /// A register.
    Register(Slot),
    /// Nowhere: no member is named, and the value itself is the property.
    None,
}

/// Which way a member instruction names its member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// The name is a constant-area slot.
    Direct,
    /// The name is the value of a register.
    Indirect,
}

/// One entry of an argument list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Argument {
    /// The value the register holds.
    Value(Slot),
    /// The elements of the array the register holds.
    Expand(Slot),
    /// The caller's arguments from the signature's spread base onward.
    ExpandCaller,
}

/// The arguments one call hands its callee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arguments {
    /// The caller's own list, unchanged.
    Caller,
    /// Entries of the list table, each the value of its register.
    Listed {
        /// The first entry.
        start: u32,
        /// How many entries there are.
        count: u32,
    },
    /// Entries of the list table, each spliced as its flag says.
    Expanded {
        /// The first entry.
        start: u32,
        /// How many entries there are.
        count: u32,
    },
}

/// What one instruction does, and what it does it to.
///
/// The fields are the ones the arm reads, in the order it reads them, already
/// resolved: a [`Slot`] indexes the frame's block, a `u32` constant is an index
/// into the constant area, a `u32` target is an index into the stream, and an
/// [`Arguments`] is a range of the argument table.
#[derive(Clone, Copy, Debug)]
pub enum Op {
    /// Nothing to do.
    Nop,
    /// A place a debugger breaks at.
    Debugger,
    /// `%d = #v`
    Const {
        /// The result register.
        dst: Slot,
        /// The constant-area index.
        value: u32,
    },
    /// `%d = %s`
    Copy {
        /// The result register.
        dst: Slot,
        /// The source register.
        source: Slot,
    },
    /// `%d = void`
    Clear {
        /// The register to clear.
        dst: Slot,
    },
    /// `%d = void` for `count` registers upward.
    ClearRun {
        /// The first register.
        start: Slot,
        /// How many registers.
        count: u32,
    },
    /// The condition flag takes the value's truth.
    Truth {
        /// The value's register.
        source: Slot,
    },
    /// The condition flag takes the value's falsity.
    Falsity {
        /// The value's register.
        source: Slot,
    },
    /// `%l == %r`
    Equal {
        /// The left operand.
        left: Slot,
        /// The right operand.
        right: Slot,
    },
    /// `%l === %r`
    DiscernEqual {
        /// The left operand.
        left: Slot,
        /// The right operand.
        right: Slot,
    },
    /// `%l < %r`
    Less {
        /// The left operand.
        left: Slot,
        /// The right operand.
        right: Slot,
    },
    /// `%l > %r`
    Greater {
        /// The left operand.
        left: Slot,
        /// The right operand.
        right: Slot,
    },
    /// `%d = flag`
    StoreFlag {
        /// The result register.
        dst: Slot,
    },
    /// `%d = !flag`
    StoreNotFlag {
        /// The result register.
        dst: Slot,
    },
    /// `!flag`
    NotFlag,
    /// `%t = !%t`
    LogicalNot {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = ~%t`
    Complement {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = -%t`
    Negate {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = int %t`
    ToInteger {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = real %t`
    ToReal {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = string %t`
    ToString {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = octet %t`
    ToOctet {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = +%t`
    ToNumber {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = the character code of %t`
    CharacterCode {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = the character a code names`
    Character {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = typeof %t`
    TypeOf {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = %t instanceof %c`
    InstanceOf {
        /// The register the value is in and the result goes to.
        target: Slot,
        /// The register the class name is in.
        class: Slot,
    },
    /// `%t = %t.valid = false`
    Invalidate {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t = %t.valid`
    IsValid {
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// Jump to an instruction of the stream.
    Jump {
        /// The instruction index.
        target: usize,
    },
    /// Jump when the condition flag is set.
    JumpIfFlag {
        /// The instruction index.
        target: usize,
    },
    /// Jump when the condition flag is clear.
    JumpIfNotFlag {
        /// The instruction index.
        target: usize,
    },
    /// The result takes the value.
    Result {
        /// The value's register.
        source: Slot,
    },
    /// Answer the result.
    Return,
    /// Leave a protected region, answering the result.
    ExitProtected,
    /// Compile and run the script text a register holds.
    Evaluate {
        /// The register the text is in, and the result goes to.
        text: Slot,
        /// Whether the value the text produced is kept.
        keep: bool,
        /// The instruction's code position, for a trace.
        at: u32,
    },
    /// `%t++` on a register.
    StepReg {
        /// Whether the step is up.
        up: bool,
        /// The register the value is in and the result goes to.
        target: Slot,
    },
    /// `%t++` on a member.
    StepMember {
        /// Whether the step is up.
        up: bool,
        /// The way the member is named.
        form: PropertyForm,
        /// The result register.
        target: Slot,
        /// The register the object is in.
        object: Slot,
        /// The member's name.
        name: Name,
    },
    /// A compound assignment on a register.
    OperateReg {
        /// The operator.
        operator: Operator,
        /// The register the result goes to, and the left operand is in.
        target: Slot,
        /// The register the right operand is in.
        source: Slot,
    },
    /// A compound assignment on a member.
    OperateMember {
        /// The operator.
        operator: Operator,
        /// The way the member is named.
        form: PropertyForm,
        /// The result register.
        target: Slot,
        /// The register the object is in.
        object: Slot,
        /// The member's name.
        name: Name,
        /// The register the right operand is in.
        parameter: Slot,
    },
    /// `%d = %o[@k]` and its indirect form.
    ReadMember {
        /// The way the member is named.
        form: Form,
        /// The flags the read carries.
        flags: MemberFlags,
        /// The result register.
        dst: Slot,
        /// The register the object is in.
        object: Slot,
        /// The member's name.
        name: Name,
    },
    /// `%o[@k] = %v` and its indirect form.
    WriteMember {
        /// The way the member is named.
        form: Form,
        /// The flags the write carries.
        flags: MemberFlags,
        /// The register the object is in.
        object: Slot,
        /// The member's name.
        name: Name,
        /// The register the value is in.
        value: Slot,
    },
    /// `%d = delete %o[@k]` and its indirect form.
    DeleteMember {
        /// The way the member is named.
        form: Form,
        /// The result register, or none.
        dst: Slot,
        /// The register the object is in.
        object: Slot,
        /// The member's name.
        name: Name,
    },
    /// `%d = typeof %o[@k]` and its indirect form.
    TypeOfMember {
        /// The way the member is named.
        form: Form,
        /// The flags the read carries.
        flags: MemberFlags,
        /// The result register.
        dst: Slot,
        /// The register the object is in.
        object: Slot,
        /// The member's name.
        name: Name,
    },
    /// `%d = *%o`: a read through a property object.
    ReadProperty {
        /// The result register.
        dst: Slot,
        /// The register the property object is in.
        object: Slot,
    },
    /// `*%o = %v`: a write through a property object.
    WriteProperty {
        /// The register the property object is in.
        object: Slot,
        /// The register the value is in.
        value: Slot,
    },
    /// A call or a construction.
    Call(Call),
    /// `%d = global`
    Global {
        /// The result register.
        dst: Slot,
    },
    /// `%t` with the receiver taken from another register.
    ChangeThis {
        /// The register the object is in and the result goes to.
        target: Slot,
        /// The register the new receiver is in.
        receiver: Slot,
    },
    /// Publish the context's members onto the frame's receiver.
    PublishMembers,
    /// `%t.addClassName(%n)`
    AddClassName {
        /// The register the object is in.
        target: Slot,
        /// The register the name is in.
        name: Slot,
    },
    /// Run a protected region, and the catch's body if it fails.
    Enter(Protected),
    /// Throw the value a register holds.
    Throw {
        /// The register the value is in.
        value: Slot,
        /// The instruction's code position, for a trace.
        at: u32,
    },
}

/// The operands of a call instruction.
#[derive(Clone, Copy, Debug)]
pub struct Call {
    /// Whether the call constructs.
    pub construct: bool,
    /// The member the call names, when it names one.
    pub named: Name,
    /// The result register, or none.
    pub result: Slot,
    /// The register the callee is in.
    pub callee: Slot,
    /// The arguments it hands over.
    pub arguments: Arguments,
    /// The instruction's code position, for a trace.
    pub at: u32,
}

/// The operands of a protected region.
#[derive(Clone, Copy, Debug)]
pub struct Protected {
    /// The instruction index the catch's body starts at.
    pub target: usize,
    /// The register the failure is handed to.
    pub register: Slot,
}

/// One instruction: what it does, and the code position it came from.
#[derive(Clone, Copy, Debug)]
pub struct Instruction {
    /// The word position the instruction starts at.
    pub at: u32,
    /// What it does.
    pub op: Op,
}

impl Instruction {
    /// The code position a trace names for this instruction.
    pub fn position(&self) -> usize {
        self.at as usize
    }
}

/// The register window a stream's register operands are resolved against.
#[derive(Clone, Copy, Debug)]
pub struct Window {
    /// The block index of the frame base.
    pub base: i32,
    /// The number of values in the block.
    pub len: i32,
}

/// The decoded stream of one code area.
///
/// An argument list lives in a table of its own rather than in the instruction,
/// because it has as many entries as the call has arguments: the instruction
/// carries the range and the table the entries.
#[derive(Clone, Debug, Default)]
pub struct Stream {
    /// The instructions, in the order they run.
    instructions: Vec<Instruction>,
    /// Every argument list of the stream, in one table.
    arguments: Vec<Argument>,
}

impl Stream {
    /// The instructions, in the order they run.
    #[inline]
    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    /// The entries of one argument list.
    pub fn entries(&self, start: u32, count: u32) -> Result<&[Argument], Fault> {
        let start = start as usize;
        let end = start
            .checked_add(count as usize)
            .ok_or(Fault::Broken("an argument list is out of range"))?;
        self.arguments
            .get(start..end)
            .ok_or(Fault::Broken("an argument list is out of range"))
    }

    /// Decodes a code stream into the instructions it runs.
    ///
    /// # Errors
    ///
    /// The failure the encoding names for the first defect in the stream: a
    /// word that is not an opcode, an instruction that ends past the stream, a
    /// register outside the window, a constant outside the constant area, a
    /// jump outside the stream, or an argument flag that is not one.
    pub fn decode(code: &[i32], window: Window, constants: usize) -> Result<Self, Fault> {
        let mut starts = Vec::new();
        let mut at = 0;
        while at < code.len() {
            starts.push(at);
            at += decode::length(code, at)?;
        }
        let mut index_of = vec![u32::MAX; code.len() + 1];
        for (index, start) in starts.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| {
                Fault::Broken("a code stream has more instructions than a jump holds")
            })?;
            index_of[*start] = index;
        }
        let mut stream = Self {
            instructions: Vec::with_capacity(starts.len()),
            arguments: Vec::new(),
        };
        for start in starts {
            let op = stream.operation(code, start, &index_of, window, constants)?;
            stream.instructions.push(Instruction {
                at: u32::try_from(start)
                    .map_err(|_| Fault::Broken("a code stream is longer than a jump holds"))?,
                op,
            });
        }
        Ok(stream)
    }

    /// The instruction at a word position, decoded.
    fn operation(
        &mut self,
        code: &[i32],
        at: usize,
        index_of: &[u32],
        window: Window,
        constants: usize,
    ) -> Result<Op, Fault> {
        let opcode = decode::word(code, at)?;
        let reg = |slot: usize| register(code, at, slot, window);
        let value = |slot: usize| constant(code, at, slot, constants);
        let member =
            |slot: usize, form: PropertyForm| name(code, at, slot, form, window, constants);
        let jump = |slot: usize| target(code, at, slot, index_of);
        // an instruction that keeps no result names the discard register with
        // an address of zero
        let kept = |slot: usize| match decode::word(code, at + slot)? {
            0 => Ok(Slot::DISCARD),
            _ => reg(slot),
        };
        // a member write's operands are the object, the name and the value
        let write = |form: Form, flags: MemberFlags| -> Result<Op, Fault> {
            Ok(Op::WriteMember {
                form,
                flags,
                object: reg(1)?,
                name: member(
                    2,
                    match form {
                        Form::Direct => PropertyForm::Direct,
                        Form::Indirect => PropertyForm::Indirect,
                    },
                )?,
                value: reg(3)?,
            })
        };
        // the two families are the step and the operator rows, four forms of
        // one behaviour each
        if let Some((up, form)) = decode::step_of(opcode) {
            // a member form keeps no result when the target is address zero,
            // and the plain form writes the register it names
            let target = match form {
                PropertyForm::Plain => reg(1)?,
                _ => kept(1)?,
            };
            return Ok(match form {
                PropertyForm::Plain => Op::StepReg { up, target },
                _ => Op::StepMember {
                    up,
                    form,
                    target,
                    object: reg(2)?,
                    name: member(3, form)?,
                },
            });
        }
        if let Some((operator, form)) = decode::operator_of(opcode) {
            let target = match form {
                PropertyForm::Plain => reg(1)?,
                _ => kept(1)?,
            };
            return Ok(match form {
                PropertyForm::Plain => Op::OperateReg {
                    operator,
                    target,
                    source: reg(2)?,
                },
                _ => Op::OperateMember {
                    operator,
                    form,
                    target,
                    object: reg(2)?,
                    name: member(3, form)?,
                    parameter: reg(if form == PropertyForm::Closure { 3 } else { 4 })?,
                },
            });
        }
        let op = match opcode {
            vm::NOP => Op::Nop,
            vm::DEBUGGER => Op::Debugger,
            vm::CONST => Op::Const {
                dst: reg(1)?,
                value: value(2)?,
            },
            vm::CP => Op::Copy {
                dst: reg(1)?,
                source: reg(2)?,
            },
            vm::CL => Op::Clear { dst: reg(1)? },
            vm::CCL => Op::ClearRun {
                start: reg(1)?,
                count: count(code, at, 2)?,
            },
            vm::TT => Op::Truth { source: reg(1)? },
            vm::TF => Op::Falsity { source: reg(1)? },
            vm::CEQ => Op::Equal {
                left: reg(1)?,
                right: reg(2)?,
            },
            vm::CDEQ => Op::DiscernEqual {
                left: reg(1)?,
                right: reg(2)?,
            },
            vm::CLT => Op::Less {
                left: reg(1)?,
                right: reg(2)?,
            },
            vm::CGT => Op::Greater {
                left: reg(1)?,
                right: reg(2)?,
            },
            vm::SETF => Op::StoreFlag { dst: reg(1)? },
            vm::SETNF => Op::StoreNotFlag { dst: reg(1)? },
            vm::LNOT => Op::LogicalNot { target: reg(1)? },
            vm::NF => Op::NotFlag,
            vm::JF => Op::JumpIfFlag { target: jump(1)? },
            vm::JNF => Op::JumpIfNotFlag { target: jump(1)? },
            vm::JMP => Op::Jump { target: jump(1)? },
            vm::SRV => Op::Result { source: reg(1)? },
            vm::RET => Op::Return,
            vm::BNOT => Op::Complement { target: reg(1)? },
            vm::TYPEOF => Op::TypeOf { target: reg(1)? },
            vm::TYPEOFD => Op::TypeOfMember {
                form: Form::Direct,
                flags: MemberFlags::MUST_EXIST,
                dst: reg(1)?,
                object: reg(2)?,
                name: member(3, PropertyForm::Direct)?,
            },
            vm::TYPEOFI => Op::TypeOfMember {
                form: Form::Indirect,
                flags: MemberFlags::MUST_EXIST,
                dst: reg(1)?,
                object: reg(2)?,
                name: member(3, PropertyForm::Indirect)?,
            },
            vm::EVAL => Op::Evaluate {
                text: reg(1)?,
                keep: true,
                at: position(at)?,
            },
            vm::EEXP => Op::Evaluate {
                text: reg(1)?,
                keep: false,
                at: position(at)?,
            },
            vm::CHKINS => Op::InstanceOf {
                target: reg(1)?,
                class: reg(2)?,
            },
            vm::ASC => Op::CharacterCode { target: reg(1)? },
            vm::CHR => Op::Character { target: reg(1)? },
            vm::NUM => Op::ToNumber { target: reg(1)? },
            vm::CHS => Op::Negate { target: reg(1)? },
            vm::INV => Op::Invalidate { target: reg(1)? },
            vm::CHKINV => Op::IsValid { target: reg(1)? },
            vm::INT => Op::ToInteger { target: reg(1)? },
            vm::REAL => Op::ToReal { target: reg(1)? },
            vm::STR => Op::ToString { target: reg(1)? },
            vm::OCTET => Op::ToOctet { target: reg(1)? },
            vm::GPD => Op::ReadMember {
                form: Form::Direct,
                flags: MemberFlags::NONE,
                dst: reg(1)?,
                object: reg(2)?,
                name: member(3, PropertyForm::Direct)?,
            },
            vm::GPDS => Op::ReadMember {
                form: Form::Direct,
                flags: MemberFlags::IGNORE_PROPERTY,
                dst: reg(1)?,
                object: reg(2)?,
                name: member(3, PropertyForm::Direct)?,
            },
            vm::GPI => Op::ReadMember {
                form: Form::Indirect,
                flags: MemberFlags::NONE,
                dst: reg(1)?,
                object: reg(2)?,
                name: member(3, PropertyForm::Indirect)?,
            },
            vm::GPIS => Op::ReadMember {
                form: Form::Indirect,
                flags: MemberFlags::IGNORE_PROPERTY,
                dst: reg(1)?,
                object: reg(2)?,
                name: member(3, PropertyForm::Indirect)?,
            },
            vm::SPD => write(Form::Direct, MemberFlags::NONE)?,
            vm::SPDE => write(Form::Direct, MemberFlags::ENSURE)?,
            vm::SPDEH => write(Form::Direct, MemberFlags::ENSURE | MemberFlags::HIDDEN)?,
            vm::SPDS => write(
                Form::Direct,
                MemberFlags::ENSURE | MemberFlags::IGNORE_PROPERTY,
            )?,
            vm::SPI => write(Form::Indirect, MemberFlags::NONE)?,
            vm::SPIE => write(Form::Indirect, MemberFlags::ENSURE)?,
            vm::SPIS => write(
                Form::Indirect,
                MemberFlags::ENSURE | MemberFlags::IGNORE_PROPERTY,
            )?,
            vm::SETP => Op::WriteProperty {
                object: reg(1)?,
                value: reg(2)?,
            },
            vm::GETP => Op::ReadProperty {
                dst: reg(1)?,
                object: reg(2)?,
            },
            vm::DELD => Op::DeleteMember {
                form: Form::Direct,
                dst: kept(1)?,
                object: reg(2)?,
                name: member(3, PropertyForm::Direct)?,
            },
            vm::DELI => Op::DeleteMember {
                form: Form::Indirect,
                dst: kept(1)?,
                object: reg(2)?,
                name: member(3, PropertyForm::Indirect)?,
            },
            vm::CALL => Op::Call(Call {
                construct: false,
                named: Name::None,
                result: kept(1)?,
                callee: reg(2)?,
                arguments: self.argument_list(code, at, 3, window)?,
                at: position(at)?,
            }),
            vm::NEW => Op::Call(Call {
                construct: true,
                named: Name::None,
                result: kept(1)?,
                callee: reg(2)?,
                arguments: self.argument_list(code, at, 3, window)?,
                at: position(at)?,
            }),
            vm::CALLD => Op::Call(Call {
                construct: false,
                named: Name::Constant(value(3)?),
                result: kept(1)?,
                callee: reg(2)?,
                arguments: self.argument_list(code, at, 4, window)?,
                at: position(at)?,
            }),
            vm::CALLI => Op::Call(Call {
                construct: false,
                named: Name::Register(reg(3)?),
                result: kept(1)?,
                callee: reg(2)?,
                arguments: self.argument_list(code, at, 4, window)?,
                at: position(at)?,
            }),
            vm::ENTRY => Op::Enter(Protected {
                target: jump(1)?,
                register: reg(2)?,
            }),
            vm::EXTRY => Op::ExitProtected,
            vm::THROW => Op::Throw {
                value: reg(1)?,
                at: position(at)?,
            },
            vm::CHGTHIS => Op::ChangeThis {
                target: reg(1)?,
                receiver: reg(2)?,
            },
            vm::GLOBAL => Op::Global { dst: reg(1)? },
            vm::ADDCI => Op::AddClassName {
                target: reg(1)?,
                name: reg(2)?,
            },
            vm::REGMEMBER => Op::PublishMembers,
            _ => return Err(Fault::InvalidOpcode(opcode)),
        };
        Ok(op)
    }

    /// The argument list a call instruction hands over.
    fn argument_list(
        &mut self,
        code: &[i32],
        at: usize,
        start: usize,
        window: Window,
    ) -> Result<Arguments, Fault> {
        let count = decode::word(code, at + start)?;
        if count == -1 {
            // the caller's own list, unchanged
            return Ok(Arguments::Caller);
        }
        if count != -2 {
            let count = count_of(count, "an argument count is neither a list nor a marker")?;
            let first = self.push_list(code, at, start + 1, count, window)?;
            return Ok(Arguments::Listed {
                start: first,
                count,
            });
        }
        let pairs = decode::word(code, at + start + 1)?;
        let pairs = count_of(
            pairs,
            "an expanded argument list names a negative pair count",
        )?;
        let first = u32::try_from(self.arguments.len())
            .map_err(|_| Fault::Broken("an argument table is longer than a range holds"))?;
        for pair in 0..pairs as usize {
            let flag = decode::word(code, at + start + 2 + pair * 2)?;
            let register = register(code, at, start + 3 + pair * 2, window)?;
            let argument = match flag {
                FLAG_NORMAL => Argument::Value(register),
                FLAG_EXPAND => Argument::Expand(register),
                FLAG_UNNAMED_EXPAND => Argument::ExpandCaller,
                _ => {
                    return Err(Fault::Broken(
                        "an argument flag is neither normal nor an expansion",
                    ));
                }
            };
            self.arguments.push(argument);
        }
        Ok(Arguments::Expanded {
            start: first,
            count: pairs,
        })
    }

    /// The registers of a listed argument list, appended to the table.
    fn push_list(
        &mut self,
        code: &[i32],
        at: usize,
        start: usize,
        count: u32,
        window: Window,
    ) -> Result<u32, Fault> {
        let first = u32::try_from(self.arguments.len())
            .map_err(|_| Fault::Broken("an argument table is longer than a range holds"))?;
        for index in 0..count as usize {
            let register = register(code, at, start + index, window)?;
            self.arguments.push(Argument::Value(register));
        }
        Ok(first)
    }
}

/// The flag a call instruction writes before an argument that passes the
/// register's value: `fatNormal`.
const FLAG_NORMAL: i32 = 0;

/// The flag that splices the elements of the array a register holds:
/// `fatExpand`.
const FLAG_EXPAND: i32 = 1;

/// The flag that splices the caller's surplus arguments: `fatUnnamedExpand`.
const FLAG_UNNAMED_EXPAND: i32 = 2;

/// The `slot`-th operand of the instruction at `at`.
fn operand(code: &[i32], at: usize, slot: usize) -> Result<i32, Fault> {
    let index = at
        .checked_add(slot)
        .ok_or(Fault::Broken("an instruction's operand is out of range"))?;
    decode::word(code, index)
}

/// The register slot the `slot`-th operand names.
fn register(code: &[i32], at: usize, slot: usize, window: Window) -> Result<Slot, Fault> {
    let address = operand(code, at, slot)?;
    let register = decode::register(address)?;
    let index = i64::from(window.base) + i64::from(register);
    if index < 0 || index >= i64::from(window.len) {
        return Err(Fault::RegisterOutOfWindow { address });
    }
    u32::try_from(index)
        .map(Slot::of)
        .map_err(|_| Fault::RegisterOutOfWindow { address })
}

/// The constant-area index the `slot`-th operand names.
fn constant(code: &[i32], at: usize, slot: usize, constants: usize) -> Result<u32, Fault> {
    let address = operand(code, at, slot)?;
    let constant = decode::register(address)?;
    let index = usize::try_from(constant)
        .map_err(|_| Fault::Broken("a constant slot is outside the constant area"))?;
    if index >= constants {
        return Err(Fault::Broken(
            "a constant slot is outside the constant area",
        ));
    }
    u32::try_from(index).map_err(|_| Fault::Broken("a constant area is longer than an index holds"))
}

/// The member's name, from the constant area or from a register.
fn name(
    code: &[i32],
    at: usize,
    slot: usize,
    form: PropertyForm,
    window: Window,
    constants: usize,
) -> Result<Name, Fault> {
    match form {
        PropertyForm::Direct => constant(code, at, slot, constants).map(Name::Constant),
        PropertyForm::Indirect => register(code, at, slot, window).map(Name::Register),
        _ => Ok(Name::None),
    }
}

/// The instruction index a jump's distance names.
fn target(code: &[i32], at: usize, slot: usize, index_of: &[u32]) -> Result<usize, Fault> {
    let distance = operand(code, at, slot)?;
    let offset = decode::code_position(distance)?;
    let position = i64::try_from(at)
        .map_err(|_| Fault::Broken("a code position does not fit an address"))?
        .checked_add(i64::from(offset))
        .ok_or(Fault::Broken("a jump target is out of range"))?;
    if position < 0 {
        return Err(Fault::Broken("a jump target is before the code stream"));
    }
    let position =
        usize::try_from(position).map_err(|_| Fault::Broken("a jump target is out of range"))?;
    match index_of.get(position) {
        Some(index) if *index != u32::MAX => Ok(*index as usize),
        _ => Err(Fault::Broken("a jump target is not an instruction")),
    }
}

/// The count a plain operand names.
fn count(code: &[i32], at: usize, slot: usize) -> Result<u32, Fault> {
    let count = operand(code, at, slot)?;
    count_of(count, "a count is negative")
}

/// A non-negative operand, as a count.
fn count_of(count: i32, negative: &'static str) -> Result<u32, Fault> {
    u32::try_from(count).map_err(|_| Fault::Broken(negative))
}

/// The word position of the instruction at `at`.
fn position(at: usize) -> Result<u32, Fault> {
    u32::try_from(at).map_err(|_| Fault::Broken("a code stream is longer than a trace holds"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::codegen::opcode::{VARIANT_SIZE, vm_code_addr, vm_reg_addr};

    /// The window a context with two reserved registers and four temporaries
    /// lays out: the base sits after the reserve, and the block is the reserve,
    /// the discard register and the temporaries.
    fn window() -> Window {
        Window { base: 2, len: 7 }
    }

    /// A register operand is a slot of the block, so the byte offset, its
    /// alignment and the frame base are spent once and not per instruction
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:35`, where a register
    /// address is the register number times the size of a variant).
    #[test]
    fn a_register_operand_becomes_a_slot_of_the_block() {
        let words = [vm::CONST, vm_reg_addr(1), vm_reg_addr(0)];
        let stream = Stream::decode(&words, window(), 1).expect("the stream decodes");
        let instructions = stream.instructions();
        assert_eq!(instructions.len(), 1);
        let Op::Const { dst, value } = instructions[0].op else {
            panic!("the instruction is not a constant load");
        };
        assert_eq!(dst, Slot::of(3), "register 1 is the base plus one");
        assert_eq!(value, 0);
        assert_eq!(instructions[0].position(), 0);
        assert_eq!(VARIANT_SIZE, 20, "the address the slot replaced");
    }

    /// A register the window does not hold refuses the block instead of reaching
    /// the run loop, and so does a constant the constant area does not hold
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1300`).
    #[test]
    fn an_operand_outside_the_window_or_the_constants_is_refused() {
        let outside = [vm::CL, vm_reg_addr(6)];
        assert!(matches!(
            Stream::decode(&outside, window(), 0),
            Err(Fault::RegisterOutOfWindow { .. })
        ));
        let below = [vm::CL, vm_reg_addr(-3)];
        assert!(matches!(
            Stream::decode(&below, window(), 0),
            Err(Fault::RegisterOutOfWindow { .. })
        ));
        let constant = [vm::CONST, vm_reg_addr(1), vm_reg_addr(2)];
        assert!(matches!(
            Stream::decode(&constant, window(), 2),
            Err(Fault::Broken(_))
        ));
    }

    /// A jump is a distance in code words, and it becomes the index of the
    /// instruction it lands on
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:34`, the code address,
    /// and `tjsInterCodeExec.cpp:952-970`, the jump from the instruction's own
    /// position).
    #[test]
    fn a_jump_becomes_an_instruction_index() {
        // JNF over one instruction: the jump is two words, so the second
        // instruction is the target
        let words = [vm::JNF, vm_code_addr(2), vm::CL, vm_reg_addr(1), vm::RET];
        let stream = Stream::decode(&words, window(), 0).expect("the stream decodes");
        let instructions = stream.instructions();
        assert_eq!(instructions.len(), 3);
        let Op::JumpIfNotFlag { target } = instructions[0].op else {
            panic!("the first instruction is not a jump");
        };
        assert_eq!(target, 1, "the jump lands on the second instruction");
        assert_eq!(instructions[1].position(), 2, "its word position");
        assert!(matches!(instructions[2].op, Op::Return));
        // a jump that lands between two instructions is not one
        let words = [vm::JNF, vm_code_addr(1), vm::RET];
        assert!(matches!(
            Stream::decode(&words, window(), 0),
            Err(Fault::Broken(_))
        ));
    }

    /// An argument list becomes a range of one table: the registers of a listed
    /// list, and the pairs of an expanded one with each pair's own flag
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1994-2098`).
    #[test]
    fn an_argument_list_becomes_a_range_of_the_table() {
        // call %2 %1 with two registers
        let listed = [
            vm::CALL,
            vm_reg_addr(2),
            vm_reg_addr(1),
            2,
            vm_reg_addr(3),
            vm_reg_addr(4),
            vm::RET,
        ];
        let stream = Stream::decode(&listed, window(), 0).expect("the stream decodes");
        let Op::Call(call) = stream.instructions()[0].op else {
            panic!("the instruction is not a call");
        };
        let Arguments::Listed { start, count } = call.arguments else {
            panic!("the list is not a listed one");
        };
        assert_eq!(count, 2);
        let entries = stream.entries(start, count).expect("the list is there");
        assert_eq!(
            entries,
            [Argument::Value(Slot::of(5)), Argument::Value(Slot::of(6))]
        );
        // the same call with the caller's own list
        let spread = [vm::CALL, vm_reg_addr(2), vm_reg_addr(1), -1, vm::RET];
        let stream = Stream::decode(&spread, window(), 0).expect("the stream decodes");
        let Op::Call(call) = stream.instructions()[0].op else {
            panic!("the instruction is not a call");
        };
        assert_eq!(call.arguments, Arguments::Caller);
        // and with one value, one expansion and the caller's surplus
        let expanded = [
            vm::CALL,
            vm_reg_addr(2),
            vm_reg_addr(1),
            -2,
            3,
            0,
            vm_reg_addr(3),
            1,
            vm_reg_addr(4),
            2,
            0,
            vm::RET,
        ];
        let stream = Stream::decode(&expanded, window(), 0).expect("the stream decodes");
        let Op::Call(call) = stream.instructions()[0].op else {
            panic!("the instruction is not a call");
        };
        let Arguments::Expanded { start, count } = call.arguments else {
            panic!("the list is not an expanded one");
        };
        assert_eq!(count, 3);
        let entries = stream.entries(start, count).expect("the list is there");
        assert_eq!(
            entries,
            [
                Argument::Value(Slot::of(5)),
                Argument::Expand(Slot::of(6)),
                Argument::ExpandCaller,
            ]
        );
    }

    /// A member instruction names its member in the constant area or in a
    /// register, and the stream records which and what
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:42`, the four forms of
    /// a family).
    #[test]
    fn a_member_instruction_names_its_member_by_constant_or_register() {
        let direct = [vm::GPD, vm_reg_addr(1), vm_reg_addr(2), vm_reg_addr(0)];
        let stream = Stream::decode(&direct, window(), 1).expect("the stream decodes");
        let Op::ReadMember {
            form,
            flags,
            dst,
            object,
            name,
        } = stream.instructions()[0].op
        else {
            panic!("the instruction is not a member read");
        };
        assert_eq!(form, Form::Direct);
        assert_eq!(flags, MemberFlags::NONE);
        assert_eq!(dst, Slot::of(3));
        assert_eq!(object, Slot::of(4));
        assert_eq!(name, Name::Constant(0));
        let indirect = [vm::GPI, vm_reg_addr(1), vm_reg_addr(2), vm_reg_addr(3)];
        let stream = Stream::decode(&indirect, window(), 0).expect("the stream decodes");
        let Op::ReadMember { form, name, .. } = stream.instructions()[0].op else {
            panic!("the instruction is not a member read");
        };
        assert_eq!(form, Form::Indirect);
        assert_eq!(name, Name::Register(Slot::of(5)));
    }
}
