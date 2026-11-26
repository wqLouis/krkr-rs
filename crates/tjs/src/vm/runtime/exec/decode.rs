//! The instruction encoding: the layout of every opcode, and the decoder.
//!
//! One table describes the VM code: an opcode names a fixed list of operand
//! slots and, for the call instructions, a tail of register operands. The
//! bytecode writer and the loader use the same description, so a stream the
//! loader accepted is a stream this decoder reads.
//!
//! # The operand kinds
//!
//! A slot is a value that is not an address (an argument count, a register
//! count), a register address, or a code address. A register address is a byte
//! offset from the frame base: the register number times the size of a
//! variant. A code address is a code position times the size of a word. Both
//! are scaled, so both are converted before they are used.
//!
//! # The tail
//!
//! The four call instructions end with a count and then that many register
//! operands. A count of `-1` means the caller passes its own arguments through
//! unchanged; `-2` means at least one argument is expanded, and a pair count
//! and that many `(flag, register)` pairs follow. The tail is measured here and
//! consumed by the call family, which this part does not execute.
//!
//! # What is reachable
//!
//! Every one of the 128 opcodes has a layout, and the check below the table
//! refuses the build when one does not. Two opcodes are not reachable from the
//! code generator: it emits nothing for a consecutive clear, and nothing for a
//! member write that creates a hidden member. Both are still instructions,
//! because a script block can arrive as bytecode from a writer that is not this
//! front end, so both decode here and both execute.

use super::Fault;
use crate::vm::codegen::opcode::{VARIANT_SIZE, vm, vm_code_from_addr, vm_reg_from_addr};

/// The kind of one operand slot of an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    /// A value that is not an address: the argument count, or a register
    /// count.
    Plain,
    /// A register address: a byte offset from the frame base.
    Reg,
    /// A code address: a byte offset from the start of the code stream.
    Code,
}

/// How the operand list of an instruction ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailKind {
    /// The fixed slots are the whole instruction.
    None,
    /// A count, then that many register operands.
    RegList,
}

/// The operand layout of one opcode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The fixed operand slots, in order, of which the first `fixed_count`
    /// are used.
    pub fixed: [ArgKind; 4],
    /// How many of `fixed` an instruction of this opcode carries.
    pub fixed_count: usize,
    /// How the operand list ends.
    pub tail: TailKind,
}

/// A layout of the given slots, with no tail.
const fn plain(kinds: [ArgKind; 4], fixed_count: usize) -> Layout {
    Layout {
        fixed: kinds,
        fixed_count,
        tail: TailKind::None,
    }
}

/// A layout of the given slots, followed by a register list.
const fn listed(kinds: [ArgKind; 4], fixed_count: usize) -> Layout {
    Layout {
        fixed: kinds,
        fixed_count,
        tail: TailKind::RegList,
    }
}

/// No operand at all.
const fn none() -> Layout {
    plain([ArgKind::Plain; 4], 0)
}

/// One register operand.
const fn one() -> Layout {
    plain(
        [ArgKind::Reg, ArgKind::Plain, ArgKind::Plain, ArgKind::Plain],
        1,
    )
}

/// Two register operands.
const fn two() -> Layout {
    plain(
        [ArgKind::Reg, ArgKind::Reg, ArgKind::Plain, ArgKind::Plain],
        2,
    )
}

/// Three register operands.
const fn three() -> Layout {
    plain(
        [ArgKind::Reg, ArgKind::Reg, ArgKind::Reg, ArgKind::Plain],
        3,
    )
}

/// Four register operands.
const fn four() -> Layout {
    plain([ArgKind::Reg; 4], 4)
}

/// The base of the two-operand operator families: fourteen families of four
/// consecutive opcodes, the plain form and then the three property forms.
const OP_FAMILY_BASE: i32 = 26;

/// The number of two-operand operator families.
const OP_FAMILY_COUNT: i32 = 14;

/// The highest opcode of the operator family, as a pattern bound.
const OP_FAMILY_LIMIT: i32 = OP_FAMILY_COUNT * 4 - 1;

/// The base of the step families: the increment and the decrement, four
/// consecutive opcodes each.
const STEP_FAMILY_BASE: i32 = 18;

/// The number of step families.
const STEP_FAMILY_COUNT: i32 = 2;

/// The highest opcode of the step family, as a pattern bound.
const STEP_FAMILY_LIMIT: i32 = STEP_FAMILY_COUNT * 4 - 1;

/// The operand layout of `opcode`, or `None` when the word is not an opcode.
///
/// The layout is the table: one row per opcode, and the rows of a family are
/// computed from the family's base so the four forms cannot drift apart.
pub const fn layout_of(opcode: i32) -> Option<Layout> {
    if opcode < 0 || opcode >= vm::LAST {
        return None;
    }
    // the instructions without an operand
    if matches!(
        opcode,
        vm::NOP | vm::RET | vm::EXTRY | vm::REGMEMBER | vm::DEBUGGER | vm::NF
    ) {
        return Some(none());
    }
    // the instructions with one register operand
    if matches!(
        opcode,
        vm::TT
            | vm::TF
            | vm::SETF
            | vm::SETNF
            | vm::LNOT
            | vm::BNOT
            | vm::ASC
            | vm::CHR
            | vm::NUM
            | vm::CHS
            | vm::CL
            | vm::INV
            | vm::CHKINV
            | vm::TYPEOF
            | vm::EVAL
            | vm::EEXP
            | vm::INT
            | vm::REAL
            | vm::STR
            | vm::OCTET
            | vm::SRV
            | vm::THROW
            | vm::GLOBAL
    ) {
        return Some(one());
    }
    // the instructions with one code address
    if matches!(opcode, vm::JF | vm::JNF | vm::JMP) {
        return Some(plain(
            [
                ArgKind::Code,
                ArgKind::Plain,
                ArgKind::Plain,
                ArgKind::Plain,
            ],
            1,
        ));
    }
    // the instructions with two register operands
    if matches!(
        opcode,
        vm::CP
            | vm::CEQ
            | vm::CDEQ
            | vm::CLT
            | vm::CGT
            | vm::CHKINS
            | vm::CONST
            | vm::SETP
            | vm::GETP
            | vm::ADDCI
            | vm::CHGTHIS
    ) {
        return Some(two());
    }
    // the instructions with three register operands
    if matches!(
        opcode,
        vm::GPD
            | vm::SPD
            | vm::SPDE
            | vm::SPDEH
            | vm::SPDS
            | vm::GPI
            | vm::SPI
            | vm::SPIE
            | vm::GPDS
            | vm::GPIS
            | vm::SPIS
            | vm::DELD
            | vm::DELI
            | vm::TYPEOFD
            | vm::TYPEOFI
    ) {
        return Some(three());
    }
    // the class instance clear: a register and a plain count
    if opcode == vm::CCL {
        return Some(plain(
            [ArgKind::Reg, ArgKind::Plain, ArgKind::Plain, ArgKind::Plain],
            2,
        ));
    }
    // the protected block entry: a code address and a register
    if opcode == vm::ENTRY {
        return Some(plain(
            [ArgKind::Code, ArgKind::Reg, ArgKind::Plain, ArgKind::Plain],
            2,
        ));
    }
    // the step families: one register, three, three and two
    let step = opcode - STEP_FAMILY_BASE;
    if matches!(step, 0..=STEP_FAMILY_LIMIT) {
        return Some(match step % 4 {
            0 => one(),
            1 => three(),
            2 => three(),
            _ => two(),
        });
    }
    // the operator families: two registers, four, four and three
    let operand = opcode - OP_FAMILY_BASE;
    if matches!(operand, 0..=OP_FAMILY_LIMIT) {
        return Some(match operand % 4 {
            0 => two(),
            1 => four(),
            2 => four(),
            _ => three(),
        });
    }
    // the calls: the result and the object, plus the member for the two member
    // forms, and every one of them ends with the argument list
    if opcode == vm::CALL || opcode == vm::NEW {
        return Some(listed(
            [ArgKind::Reg, ArgKind::Reg, ArgKind::Plain, ArgKind::Plain],
            2,
        ));
    }
    if opcode == vm::CALLD || opcode == vm::CALLI {
        return Some(listed(
            [ArgKind::Reg, ArgKind::Reg, ArgKind::Reg, ArgKind::Plain],
            3,
        ));
    }
    None
}

/// The build fails when an opcode has no layout: an opcode the writer could not
/// serialise and the loader could not read back is a defect, not a default.
/// The build fails when an opcode has no layout: an opcode the writer could not
/// serialise and the loader could not read back is a defect, not a default.
const _: () = {
    let mut opcode = 0;
    while opcode < vm::LAST {
        assert!(
            layout_of(opcode).is_some(),
            "every VM opcode needs an operand layout"
        );
        opcode += 1;
    }
};

/// The mnemonic of an opcode, for a message.
///
/// The four forms of a family share the family's name and add the form's
/// suffix, so the table of names is the table of families and not a second copy
/// of the opcode list.
pub fn mnemonic(opcode: i32) -> String {
    let operand = opcode - OP_FAMILY_BASE;
    if matches!(operand, 0..=OP_FAMILY_LIMIT) {
        let family = match operand / 4 {
            0 => "LOR",
            1 => "LAND",
            2 => "BOR",
            3 => "BXOR",
            4 => "BAND",
            5 => "SAR",
            6 => "SAL",
            7 => "SR",
            8 => "ADD",
            9 => "SUB",
            10 => "MOD",
            11 => "DIV",
            12 => "IDIV",
            _ => "MUL",
        };
        return format!("{family}{}", suffix(operand % 4));
    }
    let step = opcode - STEP_FAMILY_BASE;
    if matches!(step, 0..=STEP_FAMILY_LIMIT) {
        let family = if step / 4 == 0 { "INC" } else { "DEC" };
        return format!("{family}{}", suffix(step % 4));
    }
    let fixed = match opcode {
        vm::NOP => "NOP",
        vm::CONST => "CONST",
        vm::CP => "CP",
        vm::CL => "CL",
        vm::CCL => "CCL",
        vm::TT => "TT",
        vm::TF => "TF",
        vm::CEQ => "CEQ",
        vm::CDEQ => "CDEQ",
        vm::CLT => "CLT",
        vm::CGT => "CGT",
        vm::SETF => "SETF",
        vm::SETNF => "SETNF",
        vm::LNOT => "LNOT",
        vm::NF => "NF",
        vm::JF => "JF",
        vm::JNF => "JNF",
        vm::JMP => "JMP",
        vm::BNOT => "BNOT",
        vm::TYPEOF => "TYPEOF",
        vm::TYPEOFD => "TYPEOFD",
        vm::TYPEOFI => "TYPEOFI",
        vm::EVAL => "EVAL",
        vm::EEXP => "EEXP",
        vm::CHKINS => "CHKINS",
        vm::ASC => "ASC",
        vm::CHR => "CHR",
        vm::NUM => "NUM",
        vm::CHS => "CHS",
        vm::INV => "INV",
        vm::CHKINV => "CHKINV",
        vm::INT => "INT",
        vm::REAL => "REAL",
        vm::STR => "STR",
        vm::OCTET => "OCTET",
        vm::CALL => "CALL",
        vm::CALLD => "CALLD",
        vm::CALLI => "CALLI",
        vm::NEW => "NEW",
        vm::GPD => "GPD",
        vm::SPD => "SPD",
        vm::SPDE => "SPDE",
        vm::SPDEH => "SPDEH",
        vm::GPI => "GPI",
        vm::SPI => "SPI",
        vm::SPIE => "SPIE",
        vm::GPDS => "GPDS",
        vm::SPDS => "SPDS",
        vm::GPIS => "GPIS",
        vm::SPIS => "SPIS",
        vm::SETP => "SETP",
        vm::GETP => "GETP",
        vm::DELD => "DELD",
        vm::DELI => "DELI",
        vm::SRV => "SRV",
        vm::RET => "RET",
        vm::ENTRY => "ENTRY",
        vm::EXTRY => "EXTRY",
        vm::THROW => "THROW",
        vm::CHGTHIS => "CHGTHIS",
        vm::GLOBAL => "GLOBAL",
        vm::ADDCI => "ADDCI",
        vm::REGMEMBER => "REGMEMBER",
        vm::DEBUGGER => "DEBUGGER",
        _ => "?",
    };
    fixed.to_string()
}

/// The suffix the property form of a family adds to its name.
fn suffix(form: i32) -> &'static str {
    match form {
        1 => "PD",
        2 => "PI",
        3 => "P",
        _ => "",
    }
}

/// The property form of an operator or a step instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyForm {
    /// The plain form: the left operand is a register.
    Plain,
    /// The direct form: the member's name is a constant.
    Direct,
    /// The indirect form: the member's name is in a register.
    Indirect,
    /// The closure form: the value itself is the property.
    Closure,
}

/// One of the fourteen two-operand operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    /// `||=`
    LOr,
    /// `&&=`
    LAnd,
    /// `|=`
    BOr,
    /// `^=`
    BXor,
    /// `&=`
    BAnd,
    /// `>>=`
    Sar,
    /// `<<=`
    Sal,
    /// `>>>=`
    Sr,
    /// `+=`
    Add,
    /// `-=`
    Sub,
    /// `%=`
    Mod,
    /// `/=`
    Div,
    /// `\=`
    IDiv,
    /// `*=`
    Mul,
}

impl Operator {
    /// The name the operator is handed to an object under.
    ///
    /// The engine passes the operator as a number; this runtime names it, and
    /// the object model's surface takes the name.
    pub fn name(self) -> &'static str {
        match self {
            Self::LOr => "lor",
            Self::LAnd => "land",
            Self::BOr => "bor",
            Self::BXor => "bxor",
            Self::BAnd => "band",
            Self::Sar => "sar",
            Self::Sal => "sal",
            Self::Sr => "sr",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mod => "mod",
            Self::Div => "div",
            Self::IDiv => "idiv",
            Self::Mul => "mul",
        }
    }

    /// Whether the operator converts its right operand before its left one.
    ///
    /// Two operators do, and the order decides which operand a conversion
    /// failure names: the remainder reads the divisor first and the integer
    /// division does the same. Every other operator converts the left operand
    /// first.
    pub fn right_first(self) -> bool {
        matches!(self, Self::Mod | Self::IDiv)
    }
}

/// The operator an opcode names, when it names one.
pub fn operator_of(opcode: i32) -> Option<(Operator, PropertyForm)> {
    let offset = opcode - OP_FAMILY_BASE;
    if !matches!(offset, 0..=OP_FAMILY_LIMIT) {
        return None;
    }
    let operator = match offset / 4 {
        0 => Operator::LOr,
        1 => Operator::LAnd,
        2 => Operator::BOr,
        3 => Operator::BXor,
        4 => Operator::BAnd,
        5 => Operator::Sar,
        6 => Operator::Sal,
        7 => Operator::Sr,
        8 => Operator::Add,
        9 => Operator::Sub,
        10 => Operator::Mod,
        11 => Operator::Div,
        12 => Operator::IDiv,
        _ => Operator::Mul,
    };
    Some((operator, form_of(offset % 4)))
}

/// The step family an opcode belongs to: the increment or the decrement.
pub fn step_of(opcode: i32) -> Option<(bool, PropertyForm)> {
    let offset = opcode - STEP_FAMILY_BASE;
    if !matches!(offset, 0..=STEP_FAMILY_LIMIT) {
        return None;
    }
    Some((offset / 4 == 0, form_of(offset % 4)))
}

/// The property form the offset inside a family names.
fn form_of(form: i32) -> PropertyForm {
    match form {
        1 => PropertyForm::Direct,
        2 => PropertyForm::Indirect,
        3 => PropertyForm::Closure,
        _ => PropertyForm::Plain,
    }
}

/// One word of the code stream, or the failure that says the stream ended
/// inside an instruction.
pub fn word(code: &[i32], index: usize) -> Result<i32, Fault> {
    code.get(index).copied().ok_or(Fault::Truncated)
}

/// The number of words the instruction at `ip` occupies.
///
/// The measurement is the layout's: the opcode, its fixed slots, and the
/// argument list when the instruction has one. It never guesses, and it never
/// reads past the end.
pub fn length(code: &[i32], ip: usize) -> Result<usize, Fault> {
    let opcode = word(code, ip)?;
    let layout = layout_of(opcode).ok_or(Fault::InvalidOpcode(opcode))?;
    let mut slot = ip + 1;
    for _ in 0..layout.fixed_count {
        let _ = word(code, slot)?;
        slot += 1;
    }
    if layout.tail == TailKind::RegList {
        let count = word(code, slot)?;
        slot += 1;
        if count == -1 {
            // the caller's own arguments, unchanged
        } else if count == -2 {
            let pairs = word(code, slot)?;
            slot += 1;
            if pairs < 0 {
                return Err(Fault::Broken(
                    "an expanded argument list names a negative pair count",
                ));
            }
            let words = words_of(pairs, 2)?;
            slot = add_words(slot, words)?;
            if slot > code.len() {
                return Err(Fault::Truncated);
            }
        } else if count < 0 {
            return Err(Fault::Broken(
                "an argument count is neither a list nor a marker",
            ));
        } else {
            let words = words_of(count, 1)?;
            slot = add_words(slot, words)?;
            if slot > code.len() {
                return Err(Fault::Truncated);
            }
        }
    }
    Ok(slot - ip)
}

/// `count` items of `width` words each, as a number of words.
fn words_of(count: i32, width: usize) -> Result<usize, Fault> {
    let count =
        usize::try_from(count).map_err(|_| Fault::Broken("an argument list is too long"))?;
    count
        .checked_mul(width)
        .ok_or(Fault::Broken("an argument list is too long"))
}

/// `slot` advanced by `words`, or the failure that says the stream is too
/// short.
fn add_words(slot: usize, words: usize) -> Result<usize, Fault> {
    slot.checked_add(words)
        .ok_or(Fault::Broken("an argument list is too long"))
}

/// The register a register address names, relative to the frame base.
pub fn register(address: i32) -> Result<i32, Fault> {
    if address % VARIANT_SIZE != 0 {
        return Err(Fault::MisalignedRegister(address));
    }
    Ok(vm_reg_from_addr(address))
}

/// The code position a code address names, relative to the start of the code
/// stream.
pub fn code_position(address: i32) -> Result<i32, Fault> {
    if address % 4 != 0 {
        return Err(Fault::MisalignedCode(address));
    }
    Ok(vm_code_from_addr(address))
}

/// Whether the opcode carries no operand at all.
pub fn is_operand_less(opcode: i32) -> bool {
    matches!(
        opcode,
        vm::NOP | vm::RET | vm::EXTRY | vm::REGMEMBER | vm::DEBUGGER | vm::NF
    )
}

/// The part of the runtime an opcode belongs to, when this part does not
/// execute it.
///
/// The name is the part's, so a refusal says who owns the behaviour instead of
/// saying the opcode is unknown.
pub fn owner(opcode: i32) -> Option<&'static str> {
    match opcode {
        vm::GPD
        | vm::SPD
        | vm::SPDE
        | vm::SPDEH
        | vm::SPDS
        | vm::GPI
        | vm::SPI
        | vm::SPIE
        | vm::GPDS
        | vm::GPIS
        | vm::SPIS
        | vm::SETP
        | vm::GETP
        | vm::DELD
        | vm::DELI
        | vm::TYPEOFD
        | vm::TYPEOFI => Some("the member access part"),
        vm::CALL | vm::CALLD | vm::CALLI | vm::NEW => Some("the call part"),
        vm::ENTRY | vm::EXTRY | vm::THROW => Some("the exception part"),
        vm::CHGTHIS | vm::GLOBAL | vm::ADDCI | vm::REGMEMBER => Some("the context part"),
        vm::EVAL | vm::EEXP => Some("the compile entry"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table covers every opcode and nothing that is not one
    /// (`crates/tjs/cpp/tjs2/tjsVMCodes.h:383`, where the same table is a
    /// compile-time check).
    #[test]
    fn every_opcode_has_a_layout_and_nothing_else_does() {
        for opcode in 0..vm::LAST {
            assert!(layout_of(opcode).is_some(), "opcode {opcode} has no layout");
        }
        assert_eq!(layout_of(-1), None);
        assert_eq!(layout_of(vm::LAST), None);
        assert_eq!(layout_of(vm::LAST + 1), None);
    }

    /// A register address is the register number times the size of a variant
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:35`).
    #[test]
    fn a_register_address_is_scaled_by_the_size_of_a_variant() {
        assert_eq!(register(40), Ok(2));
        assert_eq!(register(-40), Ok(-2));
        assert_eq!(register(0), Ok(0));
        assert_eq!(register(3), Err(Fault::MisalignedRegister(3)));
    }

    /// A code address is the position times the size of a word
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:34`).
    #[test]
    fn a_code_address_is_scaled_by_the_size_of_a_word() {
        assert_eq!(code_position(12), Ok(3));
        assert_eq!(code_position(-4), Ok(-1));
        assert_eq!(code_position(2), Err(Fault::MisalignedCode(2)));
    }

    /// The four forms of a family are four consecutive opcodes, the plain one
    /// first (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:42`).
    #[test]
    fn a_family_is_the_plain_form_then_the_three_property_forms() {
        assert_eq!(
            operator_of(vm::ADD),
            Some((Operator::Add, PropertyForm::Plain))
        );
        assert_eq!(
            operator_of(vm::ADD_PD),
            Some((Operator::Add, PropertyForm::Direct))
        );
        assert_eq!(
            operator_of(vm::ADD_PI),
            Some((Operator::Add, PropertyForm::Indirect))
        );
        assert_eq!(
            operator_of(vm::ADD_P),
            Some((Operator::Add, PropertyForm::Closure))
        );
        assert_eq!(
            operator_of(vm::MUL),
            Some((Operator::Mul, PropertyForm::Plain))
        );
        assert_eq!(operator_of(vm::NOP), None);
        assert_eq!(step_of(vm::INC), Some((true, PropertyForm::Plain)));
        assert_eq!(step_of(vm::DEC_P), Some((false, PropertyForm::Closure)));
        assert_eq!(step_of(vm::ADD), None);
    }

    /// Two opcodes are not reachable from the generator and are still
    /// instructions: the consecutive clear and the member write that creates a
    /// hidden member (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2793-2811`,
    /// `:2814-2837`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:893`,
    /// `:1186`, where both execute).
    #[test]
    fn the_two_opcodes_the_generator_cannot_emit_still_decode() {
        let clear = layout_of(vm::CCL).expect("the consecutive clear has a layout");
        assert_eq!(clear.fixed_count, 2);
        assert_eq!(clear.fixed[0], ArgKind::Reg);
        assert_eq!(clear.fixed[1], ArgKind::Plain);
        let hidden = layout_of(vm::SPDEH).expect("the hidden write has a layout");
        assert_eq!(hidden.fixed_count, 3);
        assert_eq!(hidden.fixed[0], ArgKind::Reg);
        assert_eq!(hidden.fixed[1], ArgKind::Reg);
        assert_eq!(hidden.fixed[2], ArgKind::Reg);
        assert_eq!(mnemonic(vm::CCL), "CCL");
        assert_eq!(mnemonic(vm::SPDEH), "SPDEH");
    }

    /// The mnemonic of a family form is the family's name and the form's
    /// suffix, so the names cannot drift from the numbering.
    #[test]
    fn a_mnemonic_names_the_family_and_the_form() {
        assert_eq!(mnemonic(vm::ADD), "ADD");
        assert_eq!(mnemonic(vm::ADD_PD), "ADDPD");
        assert_eq!(mnemonic(vm::ADD_PI), "ADDPI");
        assert_eq!(mnemonic(vm::ADD_P), "ADDP");
        assert_eq!(mnemonic(vm::INC), "INC");
        assert_eq!(mnemonic(vm::DEC_PI), "DECPI");
        assert_eq!(mnemonic(vm::CALLD), "CALLD");
        assert_eq!(mnemonic(vm::ENTRY), "ENTRY");
    }

    /// The argument list is measured, not guessed: the count is followed by
    /// that many registers, `-1` appends nothing, and `-2` appends a pair count
    /// and two words per pair
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1994-2098`).
    #[test]
    fn a_call_measures_its_argument_list() {
        // calld %0, %1, %2 with two arguments
        let code = [vm::CALLD, 0, 20, 40, 2, 60, 80];
        assert_eq!(length(&code, 0), Ok(7));
        // the same with the caller's arguments, unchanged
        let code = [vm::CALLD, 0, 20, 40, -1];
        assert_eq!(length(&code, 0), Ok(5));
        // the same with one listed and one expanded argument pair
        let code = [vm::CALLD, 0, 20, 40, -2, 2, 0, 60, 1, 80];
        assert_eq!(length(&code, 0), Ok(10));
    }

    /// A stream that ends inside an instruction is refused, never read past
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1300`, the
    /// invalid-opcode refusal).
    #[test]
    fn a_truncated_instruction_is_refused() {
        let code = [vm::CALLD, 0, 20, 40, 3, 60];
        assert_eq!(length(&code, 0), Err(Fault::Truncated));
        let code = [vm::CONST];
        assert_eq!(length(&code, 0), Err(Fault::Truncated));
        let code = [200];
        assert_eq!(length(&code, 0), Err(Fault::InvalidOpcode(200)));
    }

    /// The instructions with no operand are exactly the ones the layout says
    /// have none.
    #[test]
    fn the_operand_less_instructions_agree_with_their_layout() {
        for opcode in 0..vm::LAST {
            let layout = layout_of(opcode).expect("every opcode has a layout");
            assert_eq!(
                is_operand_less(opcode),
                layout.fixed_count == 0 && layout.tail == TailKind::None,
                "opcode {opcode}"
            );
        }
    }

    /// Every opcode this part does not execute names its owner, and every
    /// opcode it does execute names none
    /// (`docs/code/tjs/interpreter.md` §6, the packet plan).
    #[test]
    fn the_opcodes_another_part_owns_are_named() {
        let executed = [
            vm::NOP,
            vm::CONST,
            vm::CP,
            vm::CL,
            vm::CCL,
            vm::TT,
            vm::TF,
            vm::CEQ,
            vm::CDEQ,
            vm::CLT,
            vm::CGT,
            vm::SETF,
            vm::SETNF,
            vm::LNOT,
            vm::NF,
            vm::JF,
            vm::JNF,
            vm::JMP,
            vm::SRV,
            vm::RET,
            vm::DEBUGGER,
        ];
        for opcode in executed {
            assert_eq!(owner(opcode), None, "{} is executed", mnemonic(opcode));
        }
        assert_eq!(owner(vm::GPD), Some("the member access part"));
        assert_eq!(owner(vm::SPDEH), Some("the member access part"));
        assert_eq!(owner(vm::CALL), Some("the call part"));
        assert_eq!(owner(vm::THROW), Some("the exception part"));
        assert_eq!(owner(vm::GLOBAL), Some("the context part"));
        assert_eq!(owner(vm::EVAL), Some("the compile entry"));
    }
}
