//! The disassembler: an object's code as readable instructions.
//!
//! The output is a listing, not a decompilation: every instruction is shown
//! with its opcode, its registers and the constants it names. A register is
//! written `%N` and a constant slot `*N`; the value a slot holds follows the
//! `=` when the instruction loads or stores one.

use crate::tjsb::format::{BytecodeError, Constants, Object};
use crate::tjsb::opcode;

/// One decoded instruction.
pub struct Instruction {
    /// The instruction's address in the object's code.
    pub at: usize,
    /// The opcode's name.
    pub name: &'static str,
    /// The instruction's operands.
    pub operands: String,
}

/// Decode an object's code.
///
/// The walk stops at the end of the code array; an instruction whose operands
/// run past the end is a malformed file.
pub fn disassemble(
    object: &Object,
    constants: &Constants,
) -> Result<Vec<Instruction>, BytecodeError> {
    let code = &object.code;
    let mut instructions = Vec::new();
    let mut at = 0usize;
    while at < code.len() {
        let value = match u16::try_from(code[at]) {
            Ok(value) => value,
            Err(_) => return Err(BytecodeError::Invalid("an opcode is negative")),
        };
        let name = opcode::name_of(value).ok_or(BytecodeError::Invalid(
            "the opcode table has no such opcode",
        ))?;
        let (size, operands) = if let Some((group, variant)) = opcode::group(value) {
            describe_group(group, variant, code, object, constants, at)?
        } else {
            describe(value, code, object, constants, at)?
        };
        if size == 0 {
            return Err(BytecodeError::Invalid("an instruction has no length"));
        }
        instructions.push(Instruction { at, name, operands });
        at += size;
    }
    Ok(instructions)
}

/// Decode the instruction at `at`.
fn describe(
    value: u16,
    code: &[i16],
    object: &Object,
    constants: &Constants,
    at: usize,
) -> Result<(usize, String), BytecodeError> {
    match value {
        opcode::NOP
        | opcode::NF
        | opcode::RET
        | opcode::EXTRY
        | opcode::REGMEMBER
        | opcode::DEBUGGER => Ok((1, String::new())),
        opcode::CONST => {
            let destination = register(code, at + 1)?;
            let slot = register(code, at + 2)?;
            let value = slot_value(object, constants, slot);
            Ok((3, format!("%{destination}, *{slot} = {value}")))
        }
        opcode::CP | opcode::CEQ | opcode::CDEQ | opcode::CLT | opcode::CGT | opcode::CHKINS => {
            let first = register(code, at + 1)?;
            let second = register(code, at + 2)?;
            Ok((3, format!("%{first}, %{second}")))
        }
        opcode::CL => {
            let first = register(code, at + 1)?;
            Ok((2, format!("%{first}")))
        }
        opcode::CCL => {
            let first = register(code, at + 1)?;
            let count = register(code, at + 2)?;
            let last = first.wrapping_add(count).wrapping_sub(1);
            Ok((3, format!("%{first}-%{last}")))
        }
        opcode::TT
        | opcode::TF
        | opcode::SETF
        | opcode::SETNF
        | opcode::LNOT
        | opcode::BNOT
        | opcode::TYPEOF
        | opcode::EVAL
        | opcode::EEXP
        | opcode::ASC
        | opcode::CHR
        | opcode::NUM
        | opcode::CHS
        | opcode::INV
        | opcode::CHKINV
        | opcode::INT
        | opcode::REAL
        | opcode::STR
        | opcode::OCTET => {
            let first = register(code, at + 1)?;
            Ok((2, format!("%{first}")))
        }
        opcode::JF | opcode::JNF | opcode::JMP => {
            let offset = register(code, at + 1)?;
            let here = i32::try_from(at)
                .map_err(|_| BytecodeError::Invalid("the code is too large to address"))?;
            let target = offset.wrapping_add(here);
            Ok((2, format!("{target}")))
        }
        opcode::CALL | opcode::CALLD | opcode::CALLI | opcode::NEW => {
            describe_call(value, code, at)
        }
        opcode::GPD | opcode::GPDS => {
            let first = register(code, at + 1)?;
            let second = register(code, at + 2)?;
            let slot = register(code, at + 3)?;
            let value = slot_value(object, constants, slot);
            Ok((4, format!("%{first}, %{second}.*{slot} = {value}")))
        }
        opcode::SPD | opcode::SPDE | opcode::SPDEH | opcode::SPDS => {
            let first = register(code, at + 1)?;
            let slot = register(code, at + 2)?;
            let third = register(code, at + 3)?;
            let value = slot_value(object, constants, slot);
            Ok((4, format!("%{first}.*{slot} = {value}, %{third}")))
        }
        opcode::GPI | opcode::GPIS => {
            let first = register(code, at + 1)?;
            let second = register(code, at + 2)?;
            let third = register(code, at + 3)?;
            Ok((4, format!("%{first}, %{second}.%{third}")))
        }
        opcode::SPI | opcode::SPIE | opcode::SPIS => {
            let first = register(code, at + 1)?;
            let second = register(code, at + 2)?;
            let third = register(code, at + 3)?;
            Ok((4, format!("%{first}.%{second}, %{third}")))
        }
        opcode::SETP | opcode::GETP => {
            let first = register(code, at + 1)?;
            let second = register(code, at + 2)?;
            Ok((3, format!("%{first}, %{second}")))
        }
        opcode::DELD | opcode::TYPEOFD => {
            let first = register(code, at + 1)?;
            let second = register(code, at + 2)?;
            let third = register(code, at + 3)?;
            Ok((4, format!("%{first}, %{second}.*{third}")))
        }
        opcode::DELI | opcode::TYPEOFI => {
            let first = register(code, at + 1)?;
            let second = register(code, at + 2)?;
            let third = register(code, at + 3)?;
            Ok((4, format!("%{first}, %{second}.%{third}")))
        }
        opcode::SRV => {
            let first = register(code, at + 1)?;
            Ok((2, format!("%{first}")))
        }
        opcode::ENTRY => {
            let offset = register(code, at + 1)?;
            let first = register(code, at + 2)?;
            let here = i32::try_from(at)
                .map_err(|_| BytecodeError::Invalid("the code is too large to address"))?;
            Ok((3, format!("{}, %{first}", offset.wrapping_add(here))))
        }
        opcode::THROW => {
            let first = register(code, at + 1)?;
            Ok((2, format!("%{first}")))
        }
        opcode::CHGTHIS => {
            let first = register(code, at + 1)?;
            let second = register(code, at + 2)?;
            Ok((3, format!("%{first}, %{second}")))
        }
        opcode::GLOBAL => {
            let first = register(code, at + 1)?;
            Ok((2, format!("%{first}")))
        }
        opcode::ADDCI => {
            let first = register(code, at + 1)?;
            let second = register(code, at + 2)?;
            Ok((3, format!("%{first}, %{second}")))
        }
        _ => Err(BytecodeError::Invalid(
            "the opcode table has no such opcode",
        )),
    }
}

/// Decode the instruction of a four-way group.
fn describe_group(
    group: &str,
    variant: u16,
    code: &[i16],
    object: &Object,
    constants: &Constants,
    at: usize,
) -> Result<(usize, String), BytecodeError> {
    let unary = group == "inc" || group == "dec";
    let first = register(code, at + 1)?;
    match variant {
        0 => {
            if unary {
                Ok((2, format!("%{first}")))
            } else {
                let second = register(code, at + 2)?;
                Ok((3, format!("%{first}, %{second}")))
            }
        }
        1 => {
            let second = register(code, at + 2)?;
            let third = register(code, at + 3)?;
            if unary {
                let value = slot_value(object, constants, third);
                Ok((4, format!("%{first}, %{second}.*{third} = {value}")))
            } else {
                let fourth = register(code, at + 4)?;
                let value = slot_value(object, constants, third);
                Ok((
                    5,
                    format!("%{first}, %{second}.*{third} = {value}, %{fourth}"),
                ))
            }
        }
        2 => {
            let second = register(code, at + 2)?;
            let third = register(code, at + 3)?;
            if unary {
                Ok((4, format!("%{first}, %{second}.%{third}")))
            } else {
                let fourth = register(code, at + 4)?;
                Ok((5, format!("%{first}, %{second}.%{third}, %{fourth}")))
            }
        }
        3 => {
            let second = register(code, at + 2)?;
            if unary {
                Ok((3, format!("%{first}, %{second}")))
            } else {
                let third = register(code, at + 3)?;
                Ok((4, format!("%{first}, %{second}, %{third}")))
            }
        }
        _ => Err(BytecodeError::Invalid(
            "an opcode group has no such variant",
        )),
    }
}

/// Decode a call, a delegated call, an invoked call or a construction.
fn describe_call(value: u16, code: &[i16], at: usize) -> Result<(usize, String), BytecodeError> {
    let first = register(code, at + 1)?;
    let second = register(code, at + 2)?;
    let delegated = value == opcode::CALLD || value == opcode::CALLI;
    let mut head = format!("%{first}, %{second}");
    if delegated {
        let third = register(code, at + 3)?;
        if value == opcode::CALLD {
            head.push_str(&format!(".*{third}"));
        } else {
            head.push_str(&format!(".%{third}"));
        }
    }
    let start = if delegated { 5 } else { 4 };
    let count = register(code, at + start - 1)?;
    let mut arguments: Vec<String> = Vec::new();
    let size;
    if count == -1 {
        size = start;
        arguments.push("...".to_string());
    } else if count == -2 {
        let start = start + 1;
        let count = register(code, at + start - 1)?;
        let count = length_of(count)?;
        size = start + count * 2;
        for index in 0..count {
            let flag = register(code, at + start + index * 2)?;
            let argument = register(code, at + start + index * 2 + 1)?;
            arguments.push(match flag {
                0 => format!("%{argument}"),
                1 => format!("%{argument}*"),
                2 => "*".to_string(),
                _ => format!("%{argument}?"),
            });
        }
    } else {
        let count = length_of(count)?;
        size = start + count;
        for index in 0..count {
            let argument = register(code, at + start + index)?;
            arguments.push(format!("%{argument}"));
        }
    }
    Ok((size, format!("{head}({})", arguments.join(", "))))
}

/// The register an operand names.
fn register(code: &[i16], index: usize) -> Result<i32, BytecodeError> {
    let value = code.get(index).copied().ok_or(BytecodeError::Truncated {
        what: "an instruction's operand",
    })?;
    Ok(i32::from(value))
}

/// A count as a length.
fn length_of(count: i32) -> Result<usize, BytecodeError> {
    usize::try_from(count).map_err(|_| BytecodeError::Invalid("a count is negative"))
}

/// The value a constant slot holds.
fn slot_value(object: &Object, constants: &Constants, slot: i32) -> String {
    let Some(index) = usize::try_from(slot).ok() else {
        return "<invalid slot>".to_string();
    };
    let Some((kind, value)) = object.data.get(index).copied() else {
        return "<missing slot>".to_string();
    };
    value_text(constants, kind, value)
}

/// The text of a constant value.
fn value_text(constants: &Constants, kind: i16, index: i16) -> String {
    match kind {
        0 => "void".to_string(),
        1 => "null".to_string(),
        2 => format!("<inter-object {index}>"),
        3 => match string_at(constants, index) {
            Some(text) => format!("\"{}\"", escape(text)),
            None => format!("<missing string {index}>"),
        },
        4 => match octet_at(constants, index) {
            Some(bytes) => format!("<octet: {} bytes>", bytes.len()),
            None => format!("<missing octet {index}>"),
        },
        5 => match real_at(constants, index) {
            Some(value) => format!("{value:?}"),
            None => format!("<missing real {index}>"),
        },
        6 => match byte_at(constants, index) {
            Some(value) => format!("{value}"),
            None => format!("<missing byte {index}>"),
        },
        7 => match short_at(constants, index) {
            Some(value) => format!("{value}"),
            None => format!("<missing short {index}>"),
        },
        8 => match long_at(constants, index) {
            Some(value) => format!("{value}"),
            None => format!("<missing integer {index}>"),
        },
        9 => match longlong_at(constants, index) {
            Some(value) => format!("{value}"),
            None => format!("<missing long integer {index}>"),
        },
        10 => format!("<inter-generator {index}>"),
        _ => format!("<kind {kind} {index}>"),
    }
}

/// The string at an index of the string table.
fn string_at(constants: &Constants, index: i16) -> Option<&str> {
    let index = usize::try_from(index).ok()?;
    constants.strings.get(index).map(String::as_str)
}

/// The octet at an index of the octet table.
fn octet_at(constants: &Constants, index: i16) -> Option<&[u8]> {
    let index = usize::try_from(index).ok()?;
    constants.octets.get(index).map(Vec::as_slice)
}

/// The real at an index of the real table.
fn real_at(constants: &Constants, index: i16) -> Option<f64> {
    let index = usize::try_from(index).ok()?;
    constants.doubles.get(index).copied()
}

/// The byte at an index of the byte table.
fn byte_at(constants: &Constants, index: i16) -> Option<i8> {
    let index = usize::try_from(index).ok()?;
    constants.bytes.get(index).copied()
}

/// The short at an index of the short table.
fn short_at(constants: &Constants, index: i16) -> Option<i16> {
    let index = usize::try_from(index).ok()?;
    constants.shorts.get(index).copied()
}

/// The integer at an index of the integer table.
fn long_at(constants: &Constants, index: i16) -> Option<i32> {
    let index = usize::try_from(index).ok()?;
    constants.longs.get(index).copied()
}

/// The long integer at an index of the long-integer table.
fn longlong_at(constants: &Constants, index: i16) -> Option<i64> {
    let index = usize::try_from(index).ok()?;
    constants.longlongs.get(index).copied()
}

/// A string as readable text.
pub(super) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control.is_control() => {
                out.push_str(&format!("\\u{{{:04x}}}", u32::from(control)));
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tjsb::format;

    /// The fixture's object loads the first string into a register and
    /// returns, so the listing holds two instructions.
    #[test]
    fn the_fixture_disassembles() {
        let bytes = format::fixture();
        let file = format::parse(&bytes).expect("the fixture parses");
        let instructions =
            disassemble(&file.objects[0], &file.constants).expect("the object disassembles");
        assert_eq!(instructions.len(), 3);
        assert_eq!(instructions[0].at, 0);
        assert_eq!(instructions[0].name, "const");
        assert_eq!(instructions[0].operands, "%1, *0 = \"hello\"");
        assert_eq!(instructions[1].at, 3);
        assert_eq!(instructions[1].name, "ret");
        assert_eq!(instructions[1].operands, "");
        assert_eq!(instructions[2].at, 4);
        assert_eq!(instructions[2].name, "nop");
        assert_eq!(instructions[2].operands, "");
    }

    /// A string is shown with its quotes and its escapes.
    #[test]
    fn a_string_is_shown_with_its_quotes_and_escapes() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("with \"quotes\""), "with \\\"quotes\\\"");
        assert_eq!(escape("line\nbreak"), "line\\nbreak");
    }
}
