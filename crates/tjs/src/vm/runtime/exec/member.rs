//! Member access: the fifteen instructions that read, write, delete and type a
//! member.
//!
//! A member instruction names a value to reach into and a name to reach for.
//! The value is a register; the name is a slot of the constant area for the
//! direct form and a register for the indirect one. What the instruction then
//! does depends on the kind of the value, and that is the whole point of this
//! module: a text and a byte string are **their own member surface**, not
//! objects, and the interpreter reaches them before it ever looks for an
//! object.
//!
//! | value | a read | a write |
//! |---|---|---|
//! | a text | its length, or one of its units | refused: a text cannot grow |
//! | a byte string | its length, or one of its bytes | refused |
//! | anything else | the object's member surface | the object's member surface |
//!
//! The diversions are not an optimisation of the object path and must not be
//! merged with it: a text has no member map, its `length` is not a member, and
//! its index is a code unit rather than a position in a container. A port that
//! sent a text to the object path would answer a different value rather than
//! fail, which is worse than a refusal.
//!
//! # The two index rules differ
//!
//! A text's index `n` answers the empty text when `n` is exactly its length,
//! and raises only outside `0 …= length`; a byte string's index raises at
//! `length` as well, because a byte string has no "the end is empty" rule. Both
//! are the engine's, and the asymmetry is real.
//!
//! # The type of a subscripted text
//!
//! The direct form of `typeof` reads the member into the destination and then
//! applies the type, so `typeof s.length` answers `Integer` — and so does the
//! indirect form's byte-string branch. The indirect form's **text** branch is
//! the exception: the engine applies the type to `ra[code[1]]`, which indexes
//! the variant array by a *byte* address rather than by a register number, so
//! it writes somewhere outside the frame and the destination keeps the
//! character it read. `typeof s[1]` is therefore the character, not `String`.
//! That is recorded as `string_subscript_typeof` in the object model's fixture,
//! and computing the type from the value would be more correct than the engine
//! and therefore wrong.

use crate::vm::runtime::object::{MemberFlags, ObjectError};
use crate::vm::runtime::octet::Octet;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Target, Variant};

use super::frame::{Code, Frame};
use super::instruction::{Form, Name, Slot};
use super::op;
use super::throw::Unwind;
use super::{Fault, Raise};

/// The text a value with no text answers when a member instruction names it
/// with one: nothing, so the lookup then answers "no such member" for the empty
/// name.
const NO_NAME: &str = "";

/// The member a text and a byte string answer their count with.
const LENGTH: &str = "length";

/// A read: `%d = %o[@k]` or `%d = %o[%n]`.
pub fn read(
    code: &Code,
    frame: &mut Frame,
    form: Form,
    flags: MemberFlags,
    destination: Slot,
    holder: Slot,
    name: &Name,
) -> Result<(), Unwind> {
    let name = value_of(code, frame, name);
    let value = frame.slot(holder);
    let answer = match value {
        Variant::String(text) => string_get(text, &name)?,
        Variant::Octet(octet) => octet_get(octet, &name)?,
        _ => object_get(value, form, &name, flags)?,
    };
    frame.store(destination, answer);
    Ok(())
}

/// A write: `%o[@k] = %v` or `%o[%n] = %v`, and the four spellings that decide
/// whether the member is created, hidden or written through its property.
pub fn write(
    code: &Code,
    frame: &mut Frame,
    form: Form,
    flags: MemberFlags,
    holder: Slot,
    name: &Name,
    written: Slot,
) -> Result<(), Unwind> {
    let name = value_of(code, frame, name);
    let value = frame.slot(written);
    let target = frame.slot(holder);
    match target {
        // a text and a byte string cannot grow, and the refusal is an access
        // denial rather than a missing member
        Variant::String(_) | Variant::Octet(_) => Err(write_refused(&name)),
        _ => object_set(target, form, &name, value, flags),
    }
}

/// A read through a property object: `%d = *%s`.
///
/// The member name is the null one the engine passes, which this surface
/// spells as the empty text: an object that does not answer it refuses, and a
/// script sees a raise either way.
pub fn read_property(frame: &mut Frame, destination: Slot, holder: Slot) -> Result<(), Unwind> {
    let target = frame.slot(holder);
    let name = Variant::String(TjsString::new());
    let answer = object_get(target, Form::Direct, &name, MemberFlags::NONE)?;
    frame.store(destination, answer);
    Ok(())
}

/// A write through a property object: `*%o = %v`.
pub fn write_property(frame: &mut Frame, holder: Slot, written: Slot) -> Result<(), Unwind> {
    let target = frame.slot(holder);
    let value = frame.slot(written);
    let name = Variant::String(TjsString::new());
    object_set(target, Form::Direct, &name, value, MemberFlags::NONE)
}

/// A delete: `%d = delete %o[@k]` and its indirect form.
///
/// A failure answers a false rather than raising, and the destination may be
/// the discard register, which asks for no answer.
pub fn delete(
    code: &Code,
    frame: &mut Frame,
    form: Form,
    destination: Slot,
    holder: Slot,
    name: &Name,
) -> Result<(), Unwind> {
    let name = value_of(code, frame, name);
    let value = frame.slot(holder);
    let closure = as_closure(value)?;
    let object = closure.object.as_deref().ok_or_else(null_object)?;
    let text = text_of(&name)?;
    // the direct form hands the delete the frame's own receiver and the
    // indirect one the closure's
    let receiver = match form {
        Form::Direct => frame.receiver(),
        Form::Indirect => closure.objthis.clone().or_else(|| frame.receiver()),
    };
    let answer = match object.delete_member(&text, MemberFlags::NONE, receiver.as_deref()) {
        Ok(()) => Variant::Integer(1),
        // a refusal is the answer, not a failure the instruction reports
        Err(_refused) => Variant::Integer(0),
    };
    frame.store(destination, answer);
    Ok(())
}

/// The type of a member: `%d = typeof %o[@k]` and its indirect form.
pub fn type_of(
    code: &Code,
    frame: &mut Frame,
    form: Form,
    flags: MemberFlags,
    destination: Slot,
    holder: Slot,
    name: &Name,
) -> Result<(), Unwind> {
    let name = value_of(code, frame, name);
    let value = frame.slot(holder);
    let answer = match value {
        Variant::String(text) => {
            let character = string_get(text, &name)?;
            // the indirect form's text branch never reaches its destination, so
            // the character it read is the answer
            match form {
                Form::Direct => type_of_value(&character),
                Form::Indirect => character,
            }
        }
        // the byte string's branch applies the type in both forms
        Variant::Octet(octet) => type_of_value(&octet_get(octet, &name)?),
        _ => match object_get(value, form, &name, flags) {
            Ok(read) => type_of_value(&read),
            // an absent member is the text `undefined`, and no other failure is
            Err(unwind) if unwind.fault() == Fault::Raised(Raise::MemberNotFound) => {
                Variant::String(TjsString::from("undefined"))
            }
            Err(other) => return Err(other),
        },
    };
    frame.store(destination, answer);
    Ok(())
}

/// The type of a value: its kind, in the language's spelling.
fn type_of_value(value: &Variant) -> Variant {
    Variant::String(TjsString::from(value.type_name()))
}

/// The member surface of an object, read.
fn object_get(
    value: &Variant,
    form: Form,
    name: &Variant,
    flags: MemberFlags,
) -> Result<Variant, Unwind> {
    let closure = as_closure(value)?;
    let object = closure.object.as_deref().ok_or_else(null_object)?;
    let objthis = closure.objthis.as_deref();
    match (form, name) {
        // the indirect form takes an integer subscript to the by-number entry
        (Form::Indirect, Variant::Integer(index)) => object
            .prop_get_by_num(*index, flags, objthis)
            .map_err(|error| member_fault(error, &TjsString::from(index.to_string()))),
        _ => {
            let text = text_of(name)?;
            match object.prop_get(&text, flags, objthis) {
                Ok(value) => Ok(value),
                Err(error) => Err(member_fault(error, &text)),
            }
        }
    }
}

/// The member surface of an object, written.
fn object_set(
    value: &Variant,
    form: Form,
    name: &Variant,
    written: &Variant,
    flags: MemberFlags,
) -> Result<(), Unwind> {
    let closure = as_closure(value)?;
    let object = closure.object.as_deref().ok_or_else(null_object)?;
    let objthis = closure.objthis.as_deref();
    match (form, name) {
        (Form::Indirect, Variant::Integer(index)) => object
            .prop_set_by_num(*index, flags, written, objthis)
            .map_err(|error| member_fault(error, &TjsString::from(index.to_string()))),
        _ => {
            let text = text_of(name)?;
            match object.prop_set(&text, flags, written, objthis) {
                Ok(()) => Ok(()),
                Err(error) => Err(member_fault(error, &text)),
            }
        }
    }
}

/// A text's member read.
///
/// A name that is a number is an index; the name `length` answers the number of
/// units; a name that starts with a digit is a decimal index; anything else is
/// not there.
fn string_get(text: &TjsString, name: &Variant) -> Result<Variant, Unwind> {
    if let Some(index) = index_of(name) {
        return string_index(text, index);
    }
    let name = diversion_name(name)?;
    if name.eq_str(LENGTH) {
        return Ok(Variant::Integer(length_of(text)?));
    }
    if starts_with_digit(&name) {
        return string_index(text, decimal_prefix(&name));
    }
    Err(member_not_found(&name))
}

/// A text's member write: every name is refused, and the two that name a
/// position or the count are refused *as an access* rather than as a missing
/// member.
fn write_refused(name: &Variant) -> Unwind {
    if index_of(name).is_some() {
        return Raise::AccessDenied.into();
    }
    let name = match diversion_name(name) {
        Ok(name) => name,
        Err(fault) => return fault,
    };
    if name.eq_str(LENGTH) || starts_with_digit(&name) {
        return Raise::AccessDenied.into();
    }
    member_not_found(&name)
}

/// One unit of a text.
///
/// An index exactly at the length answers the empty text, and anything outside
/// `0 …= length` is a range error.
fn string_index(text: &TjsString, index: i64) -> Result<Variant, Unwind> {
    let length = i64::try_from(text.len()).map_err(|_| Fault::Broken("a text is too long"))?;
    if index == length {
        return Ok(Variant::String(TjsString::new()));
    }
    if index < 0 || index > length {
        return Err(Raise::RangeError.into());
    }
    let position = usize::try_from(index).map_err(|_| Raise::RangeError)?;
    match text.as_utf16().get(position) {
        Some(unit) => Ok(Variant::String(TjsString::from(&[*unit][..]))),
        None => Err(Raise::RangeError.into()),
    }
}

/// A byte string's member read.
fn octet_get(octet: &Octet, name: &Variant) -> Result<Variant, Unwind> {
    if let Some(index) = index_of(name) {
        return octet_index(octet, index);
    }
    let name = diversion_name(name)?;
    if name.eq_str(LENGTH) {
        return Ok(Variant::Integer(length_of_usize(octet.len())?));
    }
    if starts_with_digit(&name) {
        return octet_index(octet, decimal_prefix(&name));
    }
    Err(member_not_found(&name))
}

/// One byte of a byte string.
///
/// An index at the length is a range error: a byte string has no rule that the
/// end is empty.
fn octet_index(octet: &Octet, index: i64) -> Result<Variant, Unwind> {
    let length =
        i64::try_from(octet.len()).map_err(|_| Fault::Broken("a byte string is too long"))?;
    if index < 0 || index >= length {
        return Err(Raise::RangeError.into());
    }
    let position = usize::try_from(index).map_err(|_| Raise::RangeError)?;
    match octet.as_slice().get(position) {
        Some(byte) => Ok(Variant::Integer(i64::from(*byte))),
        None => Err(Raise::RangeError.into()),
    }
}

/// The index a name spells, when the name is a number.
///
/// A real converts with a truncating cast; a text is not a number here even
/// when it reads as one, because the diversion tests the variant's kind.
fn index_of(name: &Variant) -> Option<i64> {
    match name {
        Variant::Integer(index) => Some(*index),
        // the engine casts a real with `(tjs_int)`; a value outside the range a
        // 64-bit integer holds saturates rather than stopping the process
        Variant::Real(real) => Some(*real as i64),
        _ => None,
    }
}

/// The name a diversion reads, or the "no such member" failure a value with no
/// text answers.
fn diversion_name(name: &Variant) -> Result<TjsString, Unwind> {
    match name.to_string() {
        Ok(text) => Ok(text),
        Err(_) => Err(member_not_found(&TjsString::from(NO_NAME))),
    }
}

/// The name a member instruction passes to an object, or the conversion failure
/// a value with no text raises.
fn text_of(name: &Variant) -> Result<TjsString, Unwind> {
    name.to_string()
        .map_err(|error| op::refused_conversion(error, name))
}

/// Whether the name begins with a decimal digit, which is the test for a
/// subscript.
fn starts_with_digit(name: &TjsString) -> bool {
    matches!(name.as_utf16().first(), Some(unit) if (u16::from(b'0')..=u16::from(b'9')).contains(unit))
}

/// The integer a name's leading digits read as.
///
/// The engine's `TJS_atoi` stops at the first unit that is not a digit; a run
/// too long for the range saturates, which then fails the index check rather
/// than wrapping to a position that exists.
fn decimal_prefix(name: &TjsString) -> i64 {
    let mut value: i64 = 0;
    for unit in name.as_utf16() {
        let digit = match u32::from(*unit).checked_sub(u32::from(b'0')) {
            Some(digit) if digit <= 9 => i64::from(digit),
            _ => break,
        };
        // a run that does not fit saturates, which then fails the index check
        // rather than wrapping to a position that exists
        value = match value
            .checked_mul(10)
            .and_then(|value| value.checked_add(digit))
        {
            Some(next) => next,
            None => return i64::MAX,
        };
    }
    value
}

/// A text's length as a value.
fn length_of(text: &TjsString) -> Result<i64, Fault> {
    i64::try_from(text.len()).map_err(|_| Fault::Broken("a text is too long"))
}

/// A length as a value.
fn length_of_usize(length: usize) -> Result<i64, Fault> {
    i64::try_from(length).map_err(|_| Fault::Broken("a byte string is too long"))
}

/// The object a value is reached through.
fn as_closure(value: &Variant) -> Result<&Closure, Unwind> {
    match value {
        Variant::Object(closure) => Ok(closure),
        _ => Err(Unwind::raised(Raise::Convert {
            to: Target::Object.name(),
        })
        .with_value(value)),
    }
}

/// The failure that says the value names no object to reach into.
///
/// The engine reaches every member through the value's closure, and a null
/// closure refuses before it reads anything: the failure is the null access,
/// whatever member the instruction named.
fn null_object() -> Unwind {
    Raise::NullAccess.into()
}

/// The raise a failed member call becomes, keeping the name the engine's own
/// message carries.
fn member_fault(error: ObjectError, name: &TjsString) -> Unwind {
    match error {
        ObjectError::MemberNotFound | ObjectError::MemberMustExist => {
            Unwind::raised(Raise::MemberNotFound).with_text(name.clone())
        }
        other => Unwind::from(other),
    }
}

/// The raise for a name that is not there.
pub(super) fn member_not_found(name: &TjsString) -> Unwind {
    Unwind::raised(Raise::MemberNotFound).with_text(name.clone())
}

/// The value the instruction names a member with: a constant-area slot for the
/// direct form and a register for the indirect one.
fn value_of(code: &Code, frame: &Frame, name: &Name) -> Variant {
    match name {
        Name::Constant(index) => code.value(*index).clone(),
        Name::Register(slot) => frame.slot(*slot).clone(),
        Name::None => Variant::Void,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::runtime::object::{CustomObject, Dispatch};
    use std::rc::Rc;

    /// Equality over every kind, which a variant does not carry.
    fn same(left: &Variant, right: &Variant) -> bool {
        super::op::discern_equals(left, right)
    }

    /// Whether the answer is the raise named.
    fn refused<T>(answer: Result<T, Unwind>, raise: Raise) -> bool {
        matches!(answer, Err(unwind) if unwind.fault() == Fault::Raised(raise))
    }

    fn text(value: &str) -> Variant {
        Variant::String(TjsString::from(value))
    }

    fn integer(value: i64) -> Variant {
        Variant::Integer(value)
    }

    /// A text's length is its number of units
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:57-63`).
    #[test]
    fn a_texts_length_is_its_units() {
        assert!(same(
            &string_get(&TjsString::from("abc"), &text("length")).expect("a length"),
            &integer(3)
        ));
        assert!(same(
            &string_get(&TjsString::from(""), &text("length")).expect("a length"),
            &integer(0)
        ));
    }

    /// A text's index answers one unit, the end is empty, and outside the range
    /// is a range error
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:64-84`).
    #[test]
    fn a_texts_index_answers_a_unit_and_the_end_is_empty() {
        let source = TjsString::from("abc");
        assert!(same(
            &string_get(&source, &integer(1)).expect("the second unit"),
            &text("b")
        ));
        assert!(same(
            &string_get(&source, &text("1")).expect("a decimal name"),
            &text("b")
        ));
        assert!(same(
            &string_get(&source, &integer(3)).expect("the end"),
            &text("")
        ));
        assert!(refused(string_get(&source, &integer(4)), Raise::RangeError));
        assert!(refused(
            string_get(&source, &integer(-1)),
            Raise::RangeError
        ));
    }

    /// A text has no member that is not its length or a unit
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:86-87`).
    #[test]
    fn a_text_answers_no_other_member() {
        assert!(refused(
            string_get(&TjsString::from("abc"), &text("nope")),
            Raise::MemberNotFound
        ));
    }

    /// A text cannot be written, and the refusal is an access rather than a
    /// missing member (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:110-122`).
    #[test]
    fn a_text_write_is_a_refused_access() {
        assert_eq!(
            write_refused(&text("length")).fault(),
            Fault::Raised(Raise::AccessDenied)
        );
        assert_eq!(
            write_refused(&text("1")).fault(),
            Fault::Raised(Raise::AccessDenied)
        );
        assert_eq!(
            write_refused(&integer(1)).fault(),
            Fault::Raised(Raise::AccessDenied)
        );
        assert_eq!(
            write_refused(&text("nope")).fault(),
            Fault::Raised(Raise::MemberNotFound)
        );
    }

    /// A byte string's length and index answer numbers, and its end is a range
    /// error (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:138-155`).
    #[test]
    fn a_byte_strings_index_answers_a_byte() {
        let source = Octet::from_slice(b"ABC");
        assert!(same(
            &octet_get(&source, &text("length")).expect("a length"),
            &integer(3)
        ));
        assert!(same(
            &octet_get(&source, &integer(1)).expect("the second byte"),
            &integer(66)
        ));
        assert!(same(
            &octet_get(&source, &text("2")).expect("a decimal name"),
            &integer(67)
        ));
        assert!(
            refused(octet_get(&source, &integer(3)), Raise::RangeError),
            "a byte string has no empty end"
        );
        assert!(refused(octet_get(&source, &integer(-1)), Raise::RangeError));
    }

    /// A byte string cannot be written either
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:169-186`).
    #[test]
    fn a_byte_string_write_is_a_refused_access() {
        assert_eq!(
            write_refused(&text("length")).fault(),
            Fault::Raised(Raise::AccessDenied)
        );
        assert_eq!(
            write_refused(&integer(0)).fault(),
            Fault::Raised(Raise::AccessDenied)
        );
    }

    /// The type of a value is the language's spelling, so the direct form of a
    /// text's `length` answers `Integer`
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1876-1882`).
    #[test]
    fn the_direct_type_of_a_texts_length_is_integer() {
        let length = string_get(&TjsString::from("abc"), &text("length")).expect("a length");
        assert!(same(&type_of_value(&length), &text("Integer")));
    }

    /// The indirect form of a text's subscript answers the character, because
    /// the engine's type step indexes the register array by a byte address and
    /// never reaches the destination
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1927`, and the
    /// object model's recorded `string_subscript_typeof`).
    #[test]
    fn the_indirect_type_of_a_texts_subscript_is_the_character() {
        let character = string_get(&TjsString::from("abc"), &integer(1)).expect("a unit");
        assert!(same(&character, &text("b")));
        assert!(
            !same(&type_of_value(&character), &text("b")),
            "the type step would answer String, which is not the recorded answer"
        );
    }

    /// The byte string's indirect type does apply the type, which is the
    /// asymmetry the two branches carry
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1931-1937`).
    #[test]
    fn the_indirect_type_of_a_byte_strings_subscript_is_integer() {
        let byte = octet_get(&Octet::from_slice(b"ABC"), &integer(1)).expect("a byte");
        assert!(same(&type_of_value(&byte), &text("Integer")));
    }

    /// A leading digit is a decimal subscript, and the run stops at the first
    /// unit that is not one (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:66`,
    /// `TJS_atoi`).
    #[test]
    fn a_decimal_name_is_its_leading_digits() {
        assert_eq!(decimal_prefix(&TjsString::from("12abc")), 12);
        assert_eq!(decimal_prefix(&TjsString::from("0")), 0);
        assert!(starts_with_digit(&TjsString::from("9x")));
        assert!(!starts_with_digit(&TjsString::from("x9")));
        assert!(!starts_with_digit(&TjsString::from("")));
        assert_eq!(
            decimal_prefix(&TjsString::from("99999999999999999999")),
            i64::MAX
        );
    }

    /// A property object answers its default member, which the engine names with
    /// a null member name and this surface spells as the empty text
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1468-1491`).
    #[test]
    fn a_property_object_is_read_and_written_by_its_default_member() {
        let object: std::rc::Rc<dyn Dispatch> =
            std::rc::Rc::new(CustomObject::new(vec![TjsString::from("Object")]));
        let target = Variant::Object(Closure {
            object: Some(object),
            objthis: None,
        });
        let name = text("");
        // nothing answers the default member yet, so the read refuses
        assert!(refused(
            object_get(&target, Form::Direct, &name, MemberFlags::NONE),
            Raise::MemberNotFound
        ));
        object_set(
            &target,
            Form::Direct,
            &name,
            &integer(7),
            MemberFlags::ENSURE,
        )
        .expect("a default-member write");
        let answer = object_get(&target, Form::Direct, &name, MemberFlags::NONE).expect("a read");
        assert!(same(&answer, &integer(7)));
    }

    /// A value that is not an object has no member surface of its own
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1416-1420`).
    #[test]
    fn a_value_that_is_not_an_object_cannot_be_reached_into() {
        assert!(refused(
            as_closure(&integer(3)).map(|closure| Variant::Object(closure.clone())),
            Raise::Convert { to: "Object" }
        ));
        let object: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let closure = Closure {
            object: Some(object),
            objthis: None,
        };
        assert!(as_closure(&Variant::Object(closure)).is_ok());
    }
}
