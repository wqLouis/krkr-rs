//! The dictionary object: a keyed object whose members a script reaches by
//! name.
//!
//! A dictionary is the object model's [`CustomObject`] and nothing else: its
//! members live in the object's own member map, so the enumeration order a
//! save writes is the object's order, and this module keeps no second table.
//! What makes it a dictionary is the policy the member surface applies and the
//! structured save it answers.
//!
//! The type is the dictionary a script holds, not the class object: the
//! class's own members live on the class, which this type does not model, so
//! the dictionary carries the members a script writes into it and a member
//! walk reports only those.
//!
//! # The member policy
//!
//! The plain member surface of an object fails a read of a member it does not
//! hold; a dictionary answers the empty value instead, and only a read that
//! *requires* the member fails. The table is the whole difference in the read
//! path:
//!
//! | call | absent member |
//! |---|---|
//! | a plain read | the empty value |
//! | a read that requires the member | [`ObjectError::MemberMustExist`] |
//! | a plain write | [`ObjectError::MemberNotFound`] |
//! | a write that creates the member | the member is created, empty |
//!
//! A read that creates the member (the ensure flag) still creates it before
//! answering, and a member that is present reads as its value under every
//! flag. An object that is not a dictionary reports the absent member, which
//! is what lets the language tell the two receivers apart.
//!
//! # The structured save
//!
//! [`Dictionary::save_struct_text`] answers the text a structured save writes,
//! and that text is observable: a game stores it in a save file and reads it
//! back as an expression. The text is
//!
//! * the opening `(const) %[` and a line break;
//! * one `"name" => value` per member, indented one space deeper than the
//!   dictionary, with `,` and a line break between the members;
//! * a line break and `]` at the dictionary's own indentation.
//!
//! A name and a text are written inside double quotes with the language's
//! escapes, an integer is decimal, a real is hexadecimal with its decimal text
//! after it in a comment, an octet is `<% 00 AB %>`, and the empty value and
//! the null object are `void` and `null`. A member whose value is a dictionary
//! is that dictionary's own text; a dictionary that is already being written
//! is `null /* object recursion detected */`; and an object that is not a
//! dictionary is `null /* (object) "…" */` with the object's own text.
//!
//! A hidden member is skipped, and the save walks the members in the object's
//! enumeration order, which is the order every other member walk reports.
//!
//! # The dictionary inside a value
//!
//! An object value carries a handle the value layer does not let this module
//! read, so the save cannot tell by itself whether a member's object is
//! another dictionary. The caller that creates the values answers that
//! through [`StructObjects`]; a caller that says nothing about an object gets
//! the object's own text in the save, which is what an object that is not a
//! dictionary writes anyway.

use std::rc::Rc;

use super::number::Number;
use super::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use super::string::TjsString;
use super::variant::{Closure, Variant, VariantError};

/// The line break every structured save writes.
const LINE_BREAK: &str = "\r\n";

/// The failure of a dictionary operation.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum DictionaryError {
    /// The dictionary could not walk or write its members.
    #[error(transparent)]
    Members(#[from] ObjectError),

    /// An object value has no text form to write in a save.
    #[error(transparent)]
    Text(#[from] VariantError),
}

/// The dictionaries among the object values a structured save meets.
///
/// A save walks a dictionary's members, and a member whose value is another
/// dictionary is written as that dictionary's own text. An object value's
/// handle does not answer which dictionary it is, so the caller that creates
/// the values answers here; an object the caller does not name is written as
/// an object, not as a structure.
pub trait StructObjects {
    /// The dictionary the object value names, when it is one.
    fn dictionary(&self, value: &Closure) -> Option<&Dictionary>;
}

/// The object a script dictionary is.
///
/// The storage is a [`CustomObject`]'s, reached through the same member
/// surface; see the module documentation for the one rule the dictionary
/// changes. The object answers to the class name `Dictionary`.
pub struct Dictionary {
    /// The members and the member surface the policy wraps.
    object: CustomObject,
}

impl Dictionary {
    /// A dictionary that holds no member.
    pub fn new() -> Self {
        Self {
            object: CustomObject::new([TjsString::from("Dictionary")]),
        }
    }

    /// The text a structured save writes for this dictionary.
    ///
    /// The text is the dictionary's own, without a file or a stream around
    /// it; see the module documentation for the grammar. `objects` names the
    /// dictionaries among the member values, so a member that holds another
    /// dictionary is written as that dictionary's structure.
    ///
    /// # Errors
    ///
    /// [`DictionaryError::Members`] when a dictionary in the save has been
    /// invalidated, and [`DictionaryError::Text`] when an object value has no
    /// text form.
    pub fn save_struct_text(
        &self,
        objects: &dyn StructObjects,
    ) -> Result<TjsString, DictionaryError> {
        let mut units = Vec::new();
        let mut stack = Vec::new();
        save_into(self, &mut units, 0, &mut stack, objects)?;
        Ok(TjsString::from(units))
    }
}

impl Default for Dictionary {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatch for Dictionary {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        match self.object.prop_get(name, flags, objthis) {
            Err(ObjectError::MemberNotFound) if !flags.contains(MemberFlags::MUST_EXIST) => {
                Ok(Variant::Void)
            }
            answer => answer,
        }
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.object.prop_set(name, flags, value, objthis)
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.object.delete_member(name, flags, objthis)
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        self.object.enum_members(flags, callback)
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        self.object.is_instance_of(class, objthis)
    }

    fn class_names(&self) -> Vec<TjsString> {
        self.object.class_names()
    }

    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.object.add_class_name(name)
    }

    fn is_valid(&self) -> bool {
        self.object.is_valid()
    }

    fn invalidate(&self) -> bool {
        self.object.invalidate()
    }
}

/// Writes the dictionary's own structure at the given indentation depth.
///
/// The stack holds the dictionaries on the way down, so a member that names
/// one of them is written as the recursion comment instead of looping.
fn save_into(
    dictionary: &Dictionary,
    out: &mut Vec<u16>,
    depth: usize,
    stack: &mut Vec<*const Dictionary>,
    objects: &dyn StructObjects,
) -> Result<(), DictionaryError> {
    push_text(out, "(const) %[");
    push_text(out, LINE_BREAK);
    stack.push(dictionary as *const Dictionary);
    let mut first = true;
    let mut failure = None;
    let walked =
        dictionary.enum_members(MemberFlags::IGNORE_PROPERTY, &mut |name, flags, value| {
            if flags & MemberFlags::HIDDEN.bits() != 0 {
                return true;
            }
            if !first {
                push_text(out, ",");
                push_text(out, LINE_BREAK);
            }
            first = false;
            out.resize(out.len() + depth + 1, u16::from(b' '));
            push_text(out, "\"");
            out.extend_from_slice(name.escape_c().as_utf16());
            push_text(out, "\" => ");
            match write_value(out, value, depth + 1, stack, objects) {
                Ok(()) => true,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        });
    stack.pop();
    walked?;
    if let Some(error) = failure {
        return Err(error);
    }
    if !first {
        push_text(out, LINE_BREAK);
    }
    out.resize(out.len() + depth, u16::from(b' '));
    out.push(u16::from(b']'));
    Ok(())
}

/// Writes one member value at the given indentation depth.
fn write_value(
    out: &mut Vec<u16>,
    value: &Variant,
    depth: usize,
    stack: &mut Vec<*const Dictionary>,
    objects: &dyn StructObjects,
) -> Result<(), DictionaryError> {
    match value {
        Variant::Void => {
            push_text(out, "void");
            Ok(())
        }
        Variant::Integer(number) => {
            push_text(out, &number.to_string());
            Ok(())
        }
        Variant::Real(value) => {
            push_text(out, &real_expression(*value));
            Ok(())
        }
        Variant::String(text) => {
            push_text(out, "\"");
            out.extend_from_slice(text.escape_c().as_utf16());
            push_text(out, "\"");
            Ok(())
        }
        Variant::Octet(octet) => {
            push_text(out, "<% ");
            push_text(out, &octet.to_string());
            push_text(out, " %>");
            Ok(())
        }
        Variant::Object(closure) => write_object(out, closure, depth, stack, objects),
    }
}

/// Writes a member value that is an object.
///
/// A dictionary is its own structure; a dictionary already on the stack, and
/// any other object, are written as a null with a comment that names what the
/// value is.
fn write_object(
    out: &mut Vec<u16>,
    closure: &Closure,
    depth: usize,
    stack: &mut Vec<*const Dictionary>,
    objects: &dyn StructObjects,
) -> Result<(), DictionaryError> {
    if let Some(dictionary) = objects.dictionary(closure) {
        if stack
            .iter()
            .any(|entry| std::ptr::eq(*entry, dictionary as *const Dictionary))
        {
            push_text(out, "null /* object recursion detected */");
            return Ok(());
        }
        return save_into(dictionary, out, depth, stack, objects);
    }
    match &closure.object {
        None => {
            push_text(out, "null");
            Ok(())
        }
        Some(object) => {
            // The comment names the object twice, as its object and as its
            // receiver, whatever receiver the value itself carries.
            let shown = Closure {
                object: Some(Rc::clone(object)),
                objthis: Some(Rc::clone(object)),
            };
            let text = Variant::Object(shown).to_string()?;
            push_text(out, "null /* (object) \"");
            out.extend_from_slice(text.escape_c().as_utf16());
            push_text(out, "\" */");
            Ok(())
        }
    }
}

/// The expression a real takes in a structured save: its hexadecimal form,
/// then its decimal text in a comment.
fn real_expression(value: f64) -> String {
    format!("{} /* {} */", real_hexadecimal(value), Number::Real(value))
}

/// The hexadecimal form of a real.
///
/// A value that is not finite, or is a signed zero, takes the text form the
/// language gives it. The other values are written as the leading `0x1.`, the
/// thirteen hexadecimal digits of the significand, `p`, and the unbiased
/// binary exponent.
fn real_hexadecimal(value: f64) -> String {
    if value.is_nan() {
        return String::from("NaN");
    }
    if value.is_infinite() {
        return String::from(if value < 0.0 {
            "-Infinity"
        } else {
            "+Infinity"
        });
    }
    if value == 0.0 {
        return String::from(if value.is_sign_negative() {
            "-0.0"
        } else {
            "+0.0"
        });
    }
    let bits = value.to_bits();
    let mut text = String::with_capacity(24);
    text.push_str(if bits >> 63 == 1 { "-0x1." } else { "0x1." });
    for shift in (0..=48u64).rev().step_by(4) {
        let nibble = u8::try_from((bits >> shift) & 0xf).expect("a four-bit value fits in a byte");
        let digit = usize::from(nibble);
        text.push(char::from(b"0123456789ABCDEF"[digit]));
    }
    let exponent =
        i64::try_from((bits >> 52) & 0x7ff).expect("an eleven-bit value fits in an i64") - 1023;
    text.push('p');
    text.push_str(&exponent.to_string());
    text
}

/// Appends a text to the save.
fn push_text(out: &mut Vec<u16>, text: &str) {
    out.extend(text.encode_utf16());
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{Dictionary, DictionaryError, StructObjects};
    use crate::vm::runtime::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
    use crate::vm::runtime::string::TjsString;
    use crate::vm::runtime::variant::{Closure, Variant};

    /// A resolver that names no object.
    struct NoObjects;

    impl StructObjects for NoObjects {
        fn dictionary(&self, _value: &Closure) -> Option<&Dictionary> {
            None
        }
    }

    /// A name.
    fn name(text: &str) -> TjsString {
        TjsString::from(text)
    }

    /// Writes a member the way an assignment does.
    fn write(dictionary: &Dictionary, key: &str, value: Variant) {
        dictionary
            .prop_set(&name(key), MemberFlags::ENSURE, &value, None)
            .expect("the write is served");
    }

    /// The text of a save, as Rust text.
    fn saved(dictionary: &Dictionary, objects: &dyn StructObjects) -> String {
        let text = dictionary
            .save_struct_text(objects)
            .expect("the dictionary saves");
        String::from_utf16(text.as_utf16()).expect("the save text is valid UTF-16")
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:745-758
    #[test]
    fn an_absent_read_answers_the_empty_value() {
        let dictionary = Dictionary::new();
        assert!(matches!(
            dictionary.prop_get(&name("absent"), MemberFlags::NONE, None),
            Ok(Variant::Void)
        ));
        // a read that requires the member reports the absence
        assert!(matches!(
            dictionary.prop_get(&name("absent"), MemberFlags::MUST_EXIST, None),
            Err(ObjectError::MemberMustExist)
        ));
        // a read that creates the member creates it, and it reads as empty
        assert!(matches!(
            dictionary.prop_get(&name("absent"), MemberFlags::ENSURE, None),
            Ok(Variant::Void)
        ));
        // the member is present: deleting it is served, and a second delete
        // reports the absence
        assert_eq!(
            dictionary.delete_member(&name("absent"), MemberFlags::NONE, None),
            Ok(())
        );
        assert_eq!(
            dictionary.delete_member(&name("absent"), MemberFlags::NONE, None),
            Err(ObjectError::MemberNotFound)
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1585-1616
    #[test]
    fn a_plain_write_of_an_absent_member_is_a_failure() {
        let dictionary = Dictionary::new();
        assert_eq!(
            dictionary.prop_set(&name("a"), MemberFlags::NONE, &Variant::from(1i64), None),
            Err(ObjectError::MemberNotFound)
        );
        assert_eq!(
            dictionary.delete_member(&name("a"), MemberFlags::NONE, None),
            Err(ObjectError::MemberNotFound)
        );
        // the same write with the ensure flag creates the member
        write(&dictionary, "a", Variant::from(1i64));
        assert_eq!(
            dictionary
                .prop_get(&name("a"), MemberFlags::NONE, None)
                .map(|value| match value {
                    Variant::Integer(number) => Some(number),
                    _ => None,
                }),
            Ok(Some(1))
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:722-729
    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:429-457
    #[test]
    fn the_save_writes_the_members_in_object_order() {
        let dictionary = Dictionary::new();
        write(&dictionary, "a", Variant::from(1i64));
        write(&dictionary, "b", Variant::from(2i64));
        let mut order = Vec::new();
        dictionary
            .enum_members(MemberFlags::ENUM_NO_VALUE, &mut |name, _, _| {
                order.push(name.to_string());
                true
            })
            .expect("the dictionary enumerates");
        let text = saved(&dictionary, &NoObjects);
        let mut positions = Vec::new();
        for member in &order {
            let needle = format!("\"{member}\" => ");
            let found = match text.find(&needle) {
                Some(position) => position,
                None => panic!("the save holds no {needle:?}: {text:?}"),
            };
            positions.push(found);
        }
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "the save order is not the enumeration order: {text:?}"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:468-474
    #[test]
    fn a_hidden_member_is_not_saved() {
        let dictionary = Dictionary::new();
        write(&dictionary, "seen", Variant::from(1i64));
        dictionary
            .prop_set(
                &name("hidden"),
                MemberFlags::ENSURE | MemberFlags::HIDDEN,
                &Variant::from(2i64),
                None,
            )
            .expect("the hidden member is written");
        let text = saved(&dictionary, &NoObjects);
        assert!(text.contains("\"seen\" => 1"), "{text:?}");
        assert!(!text.contains("hidden"), "{text:?}");
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:434-456
    #[test]
    fn an_empty_dictionary_saves_an_empty_structure() {
        let dictionary = Dictionary::new();
        assert_eq!(saved(&dictionary, &NoObjects), "(const) %[\r\n]");
    }

    // reference/krkr2/cpp/core/tjs2/tjsUtils.cpp:175-231
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:252-298
    #[test]
    fn a_real_is_written_in_hexadecimal_with_its_text() {
        let dictionary = Dictionary::new();
        write(&dictionary, "half", Variant::from(3.5f64));
        write(&dictionary, "zero", Variant::from(-0.0f64));
        write(&dictionary, "nan", Variant::from(f64::NAN));
        let text = saved(&dictionary, &NoObjects);
        assert!(
            text.contains("\"half\" => 0x1.C000000000000p1 /* 3.5 */"),
            "{text:?}"
        );
        assert!(text.contains("\"zero\" => -0.0 /* -0.0 */"), "{text:?}");
        assert!(text.contains("\"nan\" => NaN /* NaN */"), "{text:?}");
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:486-490
    // reference/krkr2/cpp/core/tjs2/tjsString.cpp:164-230
    // reference/krkr2/cpp/core/tjs2/tjsUtils.cpp:204-209
    #[test]
    fn a_name_and_a_text_are_written_escaped() {
        let dictionary = Dictionary::new();
        write(
            &dictionary,
            "quote \" backslash \\",
            Variant::from("\tline\n\u{01}a"),
        );
        let text = saved(&dictionary, &NoObjects);
        assert!(
            text.contains("\"quote \\\" backslash \\\\\" => \"\\tline\\n\\x01\\x61\""),
            "{text:?}"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsUtils.cpp:210-217
    #[test]
    fn an_octet_is_written_as_its_list_form() {
        use crate::vm::runtime::octet::Octet;

        let dictionary = Dictionary::new();
        write(
            &dictionary,
            "bytes",
            Variant::from(Octet::from_slice(&[0x00, 0xab, 0xff])),
        );
        write(&dictionary, "empty", Variant::from(Octet::new()));
        let text = saved(&dictionary, &NoObjects);
        assert!(text.contains("\"bytes\" => <% 00 AB FF %>"), "{text:?}");
        assert!(text.contains("\"empty\" => <%  %>"), "{text:?}");
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:492-500
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1164-1197
    #[test]
    fn a_nested_dictionary_is_saved_at_its_depth() {
        struct Nested {
            inner: Rc<Dictionary>,
            handle: Rc<dyn Dispatch>,
        }

        impl StructObjects for Nested {
            fn dictionary(&self, value: &Closure) -> Option<&Dictionary> {
                let object = value.object.as_ref()?;
                if Rc::ptr_eq(object, &self.handle) {
                    return Some(self.inner.as_ref());
                }
                None
            }
        }

        let inner = Rc::new(Dictionary::new());
        write(&inner, "x", Variant::from(1i64));
        let handle: Rc<dyn Dispatch> = inner.clone();
        let nested = Nested {
            inner: Rc::clone(&inner),
            handle: Rc::clone(&handle),
        };
        let outer = Dictionary::new();
        write(
            &outer,
            "inner",
            Variant::from(Closure {
                object: Some(handle),
                objthis: None,
            }),
        );
        assert_eq!(
            saved(&outer, &nested),
            "(const) %[\r\n \"inner\" => (const) %[\r\n  \"x\" => 1\r\n ]\r\n]"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1164-1174
    #[test]
    fn a_dictionary_that_repeats_is_written_as_its_comment() {
        struct Repeat {
            dictionary: Rc<Dictionary>,
            handle: Rc<dyn Dispatch>,
        }

        impl StructObjects for Repeat {
            fn dictionary(&self, value: &Closure) -> Option<&Dictionary> {
                let object = value.object.as_ref()?;
                if Rc::ptr_eq(object, &self.handle) {
                    return Some(self.dictionary.as_ref());
                }
                None
            }
        }

        let dictionary = Rc::new(Dictionary::new());
        let handle: Rc<dyn Dispatch> = dictionary.clone();
        let repeat = Repeat {
            dictionary: Rc::clone(&dictionary),
            handle: Rc::clone(&handle),
        };
        write(
            &dictionary,
            "self",
            Variant::from(Closure {
                object: Some(handle),
                objthis: None,
            }),
        );
        assert_eq!(
            saved(&dictionary, &repeat),
            "(const) %[\r\n \"self\" => null /* object recursion detected */\r\n]"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1197-1205
    #[test]
    fn an_object_that_is_not_a_dictionary_is_written_with_its_text() {
        let dictionary = Dictionary::new();
        // a plain object is not a dictionary, so the save writes it as one
        let handle: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        write(
            &dictionary,
            "other",
            Variant::from(Closure {
                object: Some(handle),
                objthis: None,
            }),
        );
        let text = saved(&dictionary, &NoObjects);
        assert!(
            text.starts_with("(const) %[\r\n \"other\" => null /* (object) \"(object 0x"),
            "{text:?}"
        );
        assert!(text.ends_with(")\" */\r\n]"), "{text:?}");
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:745-758
    #[test]
    fn an_invalidated_dictionary_refuses_the_save() {
        let dictionary = Dictionary::new();
        assert!(dictionary.invalidate());
        assert_eq!(
            dictionary.save_struct_text(&NoObjects),
            Err(DictionaryError::Members(ObjectError::InvalidObject))
        );
    }
}
