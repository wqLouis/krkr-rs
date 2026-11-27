//! The dictionary object: a keyed object whose members a script reaches by
//! name, and the structured form both containers write.
//!
//! A dictionary is the object model's [`CustomObject`] and nothing else: its
//! members live in the object's own member map, so the enumeration order a
//! save writes is the object's order, and this module keeps no second table.
//! What makes it a dictionary is the policy the member surface applies and the
//! structured save it answers.
//!
//! The type is the dictionary a script holds; the class object and the seven
//! static members it publishes belong to the registry, and this module owns
//! the rule each of those members runs. The dictionary carries the members a
//! script writes into it, and a member walk reports only those.
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
//! # The structured form
//!
//! A structured save writes one of two forms of the same value, and both live
//! in this module because both walk both containers:
//!
//! * the **text** form, which is an expression the language reads back — so a
//!   save can be loaded as one expression rather than parsed by the engine;
//! * the **binary** form, a tagged byte stream behind an eight-byte signature.
//!
//! ## The text form
//!
//! [`Dictionary::save_struct_text`] answers the text a structured save writes,
//! and that text is observable: a game stores it in a save file and reads it
//! back as an expression. A container opens with `(const) [` or `(const) %[`
//! and a line break, one item or `"name" => value` per line at one more space
//! of indentation than the container, and closes with `]` at its own
//! indentation; `,` and a line break separate two items, and an empty
//! container closes with no line of its own.
//!
//! A name and a text are written inside double quotes with the language's
//! escapes, an integer is decimal, a real is hexadecimal with its decimal text
//! after it in a comment, an octet is `<% 00 AB %>`, and the empty value and
//! the null object are `void` and `null`. An object that is a container is
//! that container's own text; a container that is already being written is
//! `null /* object recursion detected */`; and any other object is
//! `null /* (object) "…" */` with the object's own text.
//!
//! A hidden member is skipped, and the save walks the members in the object's
//! enumeration order, which is the order every other member walk reports.
//!
//! ## The binary form
//!
//! The signature is `KBAD100` and a zero byte, then one value. A value is one
//! tag: the integers from zero through 127 and the short texts and octets in
//! the tag itself, and the larger or more specific forms behind a tag of their
//! own — a length in 8, 16 or 32 bits, an integer of the smallest width that
//! holds it, an IEEE double, an array or a map with its item count. A negative
//! integer has no tag of its own, so every negative value takes a width tag.
//! A map writes each name as a text before its value. The byte order is
//! little-endian on every machine — a value's width is fixed by the form, never
//! by the host — and a text is stored as its UTF-16 code units, so a save
//! neither encodes nor decodes a text.
//!
//! The reader accepts every tag the writer can produce and answers
//! [`StructError::Read`] for a tag it does not know, a length that does not fit
//! the bytes that follow it, or a body that ends in the middle of a value.
//!
//! ## The containers a walk meets
//!
//! An object value keeps its object and its receiver apart, and a walk follows
//! the receiver first and the object when there is no receiver. The handle the
//! walk follows names a container by the class names the object reports, so a
//! value that carries a dictionary or an array is walked as that container and
//! every other object is written as an object. A caller that holds the values
//! it created can answer the container question itself through
//! [`StructObjects`], and its answer is asked first.
//!
//! ## The copy
//!
//! An assignment of a structure is a **deep** copy: a member that holds a
//! container becomes a new container with its own values, while every other
//! value is copied as itself. A container already on the way down is copied as
//! the null object, which is the same refusal a save writes.
//!
//! # The streams
//!
//! [`Dictionary::save_struct`] and [`Dictionary::load_struct`] reach a file
//! through the runtime's one stream seam ([`super::stream`]): the host opens
//! the stream the name and the mode name, and this module never opens one.

use std::rc::Rc;

use super::array::ArrayObject;
use super::number::Number;
use super::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use super::octet::Octet;
use super::stream::{self, StreamError};
use super::string::TjsString;
use super::variant::{Closure, Variant, VariantError};

/// The line break every structured text writes.
const LINE_BREAK: &str = "\r\n";

/// The name the dictionary class is registered under.
pub const CLASS_NAME: &str = "Dictionary";

/// The signature the binary form starts with.
pub const BINARY_HEADER: [u8; 8] = *b"KBAD100\0";

/// A value's tag: the null object.
const TAG_NIL: u8 = 0xC0;
/// A value's tag: the empty value.
const TAG_VOID: u8 = 0xC1;
/// A value's tag: the integer one.
const TAG_TRUE: u8 = 0xC2;
/// A value's tag: the integer zero.
const TAG_FALSE: u8 = 0xC3;
/// A value's tag: a text whose length is one byte.
const TAG_STRING8: u8 = 0xC4;
/// A value's tag: a text whose length is two bytes.
const TAG_STRING16: u8 = 0xC5;
/// A value's tag: a text whose length is four bytes.
const TAG_STRING32: u8 = 0xC6;
/// A value's tag: a single-precision real.
const TAG_FLOAT: u8 = 0xCA;
/// A value's tag: a double-precision real.
const TAG_DOUBLE: u8 = 0xCB;
/// A value's tag: an integer of one unsigned byte.
const TAG_UINT8: u8 = 0xCC;
/// A value's tag: an integer of two unsigned bytes.
const TAG_UINT16: u8 = 0xCD;
/// A value's tag: an integer of four unsigned bytes.
const TAG_UINT32: u8 = 0xCE;
/// A value's tag: an integer of eight unsigned bytes.
const TAG_UINT64: u8 = 0xCF;
/// A value's tag: an integer of one signed byte.
const TAG_INT8: u8 = 0xD0;
/// A value's tag: an integer of two signed bytes.
const TAG_INT16: u8 = 0xD1;
/// A value's tag: an integer of four signed bytes.
const TAG_INT32: u8 = 0xD2;
/// A value's tag: an integer of eight signed bytes.
const TAG_INT64: u8 = 0xD3;
/// A byte string's tag for a length of zero, the first of six.
const TAG_FIXED_BYTES: u8 = 0xD4;
/// A byte string's tag: a length of two bytes.
const TAG_BYTES16: u8 = 0xDA;
/// A byte string's tag: a length of four bytes.
const TAG_BYTES32: u8 = 0xDB;
/// An array's tag: a count of two bytes.
const TAG_ARRAY16: u8 = 0xDC;
/// An array's tag: a count of four bytes.
const TAG_ARRAY32: u8 = 0xDD;
/// A map's tag: a count of two bytes.
const TAG_MAP16: u8 = 0xDE;
/// A map's tag: a count of four bytes.
const TAG_MAP32: u8 = 0xDF;
/// The first tag of the negative small integers: the reader answers them and
/// the writer never produces one.
const TAG_NEGATIVE_FIXED: u8 = 0xE0;
/// The last tag of the positive small integers.
const TAG_POSITIVE_FIXED: u8 = 0x7F;
/// A text's tag for a length of zero, the first of thirty-two.
const TAG_FIXED_TEXT: u8 = 0xA0;
/// The last tag of the short texts.
const TAG_FIXED_TEXT_LAST: u8 = 0xBF;
/// An array's tag for a count of zero, the first of sixteen.
const TAG_FIXED_ARRAY: u8 = 0x90;
/// The last tag of the short arrays.
const TAG_FIXED_ARRAY_LAST: u8 = 0x9F;
/// A map's tag for a count of zero, the first of sixteen.
const TAG_FIXED_MAP: u8 = 0x80;
/// The last tag of the short maps.
const TAG_FIXED_MAP_LAST: u8 = 0x8F;
/// The longest text a fixed tag carries.
const FIXED_TEXT_LIMIT: usize = 0x1F;
/// The longest byte string a fixed tag carries.
const FIXED_BYTES_LIMIT: usize = 5;

/// The failure of the structured form.
///
/// The messages are the ones a script reads, so each is the engine's own text
/// and not a summary of it.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum StructError {
    /// The stream the save or the load uses failed.
    #[error(transparent)]
    Stream(#[from] StreamError),

    /// The bytes are not a body this reader accepts.
    #[error("Read error")]
    Read,

    /// The stream does not hold the container kind the member loads.
    #[error("Invalid argument")]
    InvalidArgument,

    /// The value the call was given is not a container of the kind it needs.
    #[error("Specify a Dictionary object or an Array object")]
    NotAContainer,

    /// The value the call was given is not an object at all.
    #[error("Accessing to nullptr object")]
    NullAccess,

    /// An object the walk reached has been invalidated.
    #[error("The object is already invalidated")]
    InvalidObject,

    /// A member walk refused the save or the copy.
    #[error(transparent)]
    Members(#[from] ObjectError),

    /// A value cannot be held by the container it is written into.
    #[error("the length is out of range")]
    LengthOutOfRange,

    /// A value has no text form to write in a save.
    #[error(transparent)]
    Text(#[from] VariantError),
}

/// The dictionaries among the object values a structured save meets.
///
/// A save walks a dictionary's members, and a member whose value is another
/// dictionary is written as that dictionary's own text. The class names an
/// object reports already answer the question, so this is the caller's *extra*
/// answer for the values it created: it is asked first, and an object it does
/// not name is identified by its class names.
pub trait StructObjects {
    /// The dictionary the object value names, when it is one.
    fn dictionary(&self, value: &Closure) -> Option<&Dictionary>;
}

/// The resolver that names no object: every container is identified by the
/// class names it reports.
pub struct ClassNames;

impl StructObjects for ClassNames {
    fn dictionary(&self, _value: &Closure) -> Option<&Dictionary> {
        None
    }
}

/// The two containers a structured walk moves through.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Container {
    /// An array: a count and an element per index.
    Array,
    /// A dictionary: a walk over its non-hidden members.
    Dictionary,
}

/// The container an object is, from the class names it reports.
pub fn container_kind(object: &dyn Dispatch) -> Option<Container> {
    let names = object.class_names();
    let dictionary = TjsString::from("Dictionary");
    if names.iter().any(|name| name == &dictionary) {
        return Some(Container::Dictionary);
    }
    let array = TjsString::from("Array");
    if names.iter().any(|name| name == &array) {
        return Some(Container::Array);
    }
    None
}

/// The container a value names, with the caller's answer asked first.
fn container_object<'a>(
    closure: &'a Closure,
    objects: &'a dyn StructObjects,
) -> Option<(Container, &'a dyn Dispatch)> {
    if let Some(dictionary) = objects.dictionary(closure) {
        return Some((Container::Dictionary, dictionary));
    }
    let object = &**walked_handle(closure)?;
    container_kind(object).map(|kind| (kind, object))
}

/// The handle a walk follows: the receiver when the value carries one, and the
/// object otherwise.
fn walked_handle(closure: &Closure) -> Option<&Rc<dyn Dispatch>> {
    closure.objthis.as_ref().or(closure.object.as_ref())
}

/// The address a walk compares to refuse a second descent into one container.
///
/// The data address is the identity: two handles to the same container answer
/// the same address, and no value is invented from it.
pub fn identity(object: &dyn Dispatch) -> *const () {
    std::ptr::from_ref(object).cast::<()>()
}

/// The value that holds a container the walk made.
fn container_value(container: Rc<dyn Dispatch>) -> Variant {
    Variant::Object(Closure {
        object: Some(Rc::clone(&container)),
        objthis: Some(container),
    })
}

/// The whole text of a structured save of `object`.
///
/// # Errors
///
/// [`StructError::NotAContainer`] when `object` is not a container,
/// [`StructError::Members`] and [`StructError::InvalidObject`] when a
/// container's member walk fails, and [`StructError::Text`] when an object
/// value has no text.
pub fn structure_text(
    object: &dyn Dispatch,
    objects: &dyn StructObjects,
) -> Result<TjsString, StructError> {
    let kind = container_kind(object).ok_or(StructError::NotAContainer)?;
    let mut units = Vec::new();
    let mut stack = vec![identity(object)];
    write_container(&mut units, kind, object, 0, &mut stack, objects)?;
    Ok(TjsString::from(units))
}

/// The whole binary form of a structured save of `object`: the signature, then
/// the body.
///
/// # Errors
///
/// [`StructError::NotAContainer`] when `object` is not a container,
/// [`StructError::Members`] and [`StructError::InvalidObject`] when a
/// container's member walk fails, and [`StructError::Stream`] with a write
/// failure when a value does not fit the form.
pub fn structure_binary(object: &dyn Dispatch) -> Result<Vec<u8>, StructError> {
    let kind = container_kind(object).ok_or(StructError::NotAContainer)?;
    let mut bytes = Vec::from(BINARY_HEADER);
    let mut stack = vec![identity(object)];
    write_container_binary(&mut bytes, kind, object, &mut stack)?;
    Ok(bytes)
}

/// The value a body describes, read from its first byte.
///
/// Every byte of a container is consumed; the bytes after the value the body
/// ends with are not read, so a body with trailing bytes still loads.
///
/// # Errors
///
/// [`StructError::Read`] when the body is not a value this reader accepts.
pub fn read_structure(body: &[u8]) -> Result<Variant, StructError> {
    let mut index = 0usize;
    read_value(body, &mut index)
}

/// The value `name` holds, read through the binary stream the host opens.
///
/// # Errors
///
/// [`StructError::Stream`] when the host cannot open the name or the stream
/// fails, [`StructError::InvalidArgument`] when the stream does not start with
/// the binary signature, and [`StructError::Read`] when the body is not one
/// this reader accepts.
pub fn read_struct_stream(name: &TjsString, mode: &TjsString) -> Result<Variant, StructError> {
    let mut stream = stream::open_binary_read(name, mode)?;
    let size = stream.size()?;
    if size < BINARY_HEADER.len() as u64 {
        return Err(StructError::InvalidArgument);
    }
    let mut header = [0u8; BINARY_HEADER.len()];
    let read = stream.read(&mut header)?;
    if read != header.len() {
        return Err(StructError::Read);
    }
    if !is_binary(&header) {
        return Err(StructError::InvalidArgument);
    }
    let position = stream.position()?;
    let length = size.checked_sub(position).ok_or(StructError::Read)?;
    let length = usize::try_from(length).map_err(|_| StructError::Read)?;
    let mut body = vec![0u8; length];
    let read = stream.read(&mut body)?;
    if read != body.len() {
        return Err(StructError::Read);
    }
    read_structure(&body)
}

/// Whether `header` is the binary form's signature.
pub fn is_binary(header: &[u8]) -> bool {
    header == BINARY_HEADER
}

/// The deep copy of one value, which the containers' assignment walks.
///
/// `stack` holds the containers already being copied: a value that names one of
/// them is copied as the null object rather than looping.
///
/// # Errors
///
/// [`StructError::Members`] and [`StructError::InvalidObject`] when a
/// container's member walk fails, and [`StructError::LengthOutOfRange`] when a
/// new container cannot hold its values.
pub fn copy_value(value: &Variant, stack: &mut Vec<*const ()>) -> Result<Variant, StructError> {
    let Variant::Object(closure) = value else {
        return Ok(value.clone());
    };
    let Some(object) = closure.object.as_deref() else {
        return Ok(value.clone());
    };
    let Some(kind) = container_kind(object) else {
        return Ok(value.clone());
    };
    if stack.contains(&identity(object)) {
        return Ok(Variant::Object(Closure::null()));
    }
    copy_container(kind, object, stack)
}

/// The deep copy of a container.
fn copy_container(
    kind: Container,
    object: &dyn Dispatch,
    stack: &mut Vec<*const ()>,
) -> Result<Variant, StructError> {
    stack.push(identity(object));
    let copy = match kind {
        Container::Array => {
            let count = array_count(object)?;
            let fresh = ArrayObject::new();
            for index in 0..count {
                let value = array_element(object, index)?;
                let copied = copy_value(&value, stack)?;
                fresh
                    .append(copied)
                    .map_err(|_| StructError::LengthOutOfRange)?;
            }
            let fresh: Rc<dyn Dispatch> = Rc::new(fresh);
            container_value(fresh)
        }
        Container::Dictionary => {
            let fresh = Dictionary::new();
            let mut failure = None;
            object
                .enum_members(MemberFlags::IGNORE_PROPERTY, &mut |name, flags, value| {
                    if flags & MemberFlags::HIDDEN.bits() != 0 {
                        return true;
                    }
                    let copied = (|| -> Result<(), StructError> {
                        let copied = copy_value(value, stack)?;
                        fresh
                            .prop_set(
                                name,
                                MemberFlags::ENSURE | MemberFlags::IGNORE_PROPERTY,
                                &copied,
                                None,
                            )
                            .map_err(member_error)?;
                        Ok(())
                    })();
                    match copied {
                        Ok(()) => true,
                        Err(error) => {
                            failure = Some(error);
                            false
                        }
                    }
                })
                .map_err(member_error)?;
            if let Some(error) = failure {
                stack.pop();
                return Err(error);
            }
            let fresh: Rc<dyn Dispatch> = Rc::new(fresh);
            container_value(fresh)
        }
    };
    stack.pop();
    Ok(copy)
}

/// The number of elements an array reports.
fn array_count(object: &dyn Dispatch) -> Result<usize, StructError> {
    let count = object.prop_get(&TjsString::from("count"), MemberFlags::NONE, None)?;
    let count = count
        .to_integer()
        .map_err(|_| StructError::Members(ObjectError::CallNotServed))?;
    if count < 0 {
        return Err(StructError::Members(ObjectError::CallNotServed));
    }
    usize::try_from(count).map_err(|_| StructError::LengthOutOfRange)
}

/// One element of an array, by its index.
fn array_element(object: &dyn Dispatch, index: usize) -> Result<Variant, StructError> {
    let name = TjsString::from(index.to_string());
    object
        .prop_get(&name, MemberFlags::NONE, None)
        .map_err(|_refused| StructError::NotAContainer)
}

/// Maps a member walk's failure onto the structured form's.
pub fn member_error(error: ObjectError) -> StructError {
    match error {
        ObjectError::InvalidObject => StructError::InvalidObject,
        other => StructError::Members(other),
    }
}

/// The write failure a value that the form cannot hold answers.
fn write_error() -> StructError {
    StructError::Stream(StreamError::Write)
}

/// Writes a container's own text at `indent` spaces of indentation.
fn write_container(
    out: &mut Vec<u16>,
    kind: Container,
    object: &dyn Dispatch,
    indent: usize,
    stack: &mut Vec<*const ()>,
    objects: &dyn StructObjects,
) -> Result<(), StructError> {
    match kind {
        Container::Array => {
            let count = array_count(object)?;
            push_text(out, "(const) [");
            push_text(out, LINE_BREAK);
            let last = count.saturating_sub(1);
            for index in 0..count {
                let value = array_element(object, index)?;
                out.resize(out.len() + indent + 1, u16::from(b' '));
                write_value(out, &value, indent + 1, stack, objects)?;
                if index != last {
                    push_text(out, ",");
                }
                push_text(out, LINE_BREAK);
            }
            out.resize(out.len() + indent, u16::from(b' '));
            out.push(u16::from(b']'));
            Ok(())
        }
        Container::Dictionary => {
            push_text(out, "(const) %[");
            push_text(out, LINE_BREAK);
            let mut first = true;
            let mut failure = None;
            object
                .enum_members(MemberFlags::IGNORE_PROPERTY, &mut |name, flags, value| {
                    if flags & MemberFlags::HIDDEN.bits() != 0 {
                        return true;
                    }
                    if !first {
                        push_text(out, ",");
                        push_text(out, LINE_BREAK);
                    }
                    first = false;
                    out.resize(out.len() + indent + 1, u16::from(b' '));
                    push_text(out, "\"");
                    out.extend_from_slice(name.escape_c().as_utf16());
                    push_text(out, "\" => ");
                    match write_value(out, value, indent + 1, stack, objects) {
                        Ok(()) => true,
                        Err(error) => {
                            failure = Some(error);
                            false
                        }
                    }
                })
                .map_err(member_error)?;
            if let Some(error) = failure {
                return Err(error);
            }
            if !first {
                push_text(out, LINE_BREAK);
            }
            out.resize(out.len() + indent, u16::from(b' '));
            out.push(u16::from(b']'));
            Ok(())
        }
    }
}

/// Writes one value at `indent` spaces of indentation.
fn write_value(
    out: &mut Vec<u16>,
    value: &Variant,
    indent: usize,
    stack: &mut Vec<*const ()>,
    objects: &dyn StructObjects,
) -> Result<(), StructError> {
    match value {
        Variant::Void => {
            push_text(out, "void");
            Ok(())
        }
        Variant::Integer(number) => {
            push_text(out, &number.to_string());
            Ok(())
        }
        Variant::Real(number) => {
            push_text(out, &real_expression(*number));
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
        Variant::Object(closure) => write_object(out, closure, indent, stack, objects),
    }
}

/// Writes a value that is an object, walking the handle the value carries.
///
/// A container is its own structure; a container already on the way down, and
/// any other object, are written as a null with a comment that names what the
/// value is.
fn write_object(
    out: &mut Vec<u16>,
    closure: &Closure,
    indent: usize,
    stack: &mut Vec<*const ()>,
    objects: &dyn StructObjects,
) -> Result<(), StructError> {
    if let Some((kind, object)) = container_object(closure, objects) {
        if stack.contains(&identity(object)) {
            push_text(out, "null /* object recursion detected */");
            return Ok(());
        }
        stack.push(identity(object));
        let written = write_container(out, kind, object, indent, stack, objects);
        stack.pop();
        return written;
    }
    let Some(handle) = walked_handle(closure) else {
        push_text(out, "null");
        return Ok(());
    };
    // The comment names the walked handle as both its object and its
    // receiver, so the text is the object's own.
    let shown = Closure {
        object: Some(Rc::clone(handle)),
        objthis: Some(Rc::clone(handle)),
    };
    let text = Variant::Object(shown).to_string()?;
    push_text(out, "null /* (object) \"");
    out.extend_from_slice(text.escape_c().as_utf16());
    push_text(out, "\" */");
    Ok(())
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

/// Writes a container's binary form.
fn write_container_binary(
    out: &mut Vec<u8>,
    kind: Container,
    object: &dyn Dispatch,
    stack: &mut Vec<*const ()>,
) -> Result<(), StructError> {
    match kind {
        Container::Array => {
            let count = array_count(object)?;
            put_count(
                out,
                count,
                TAG_FIXED_ARRAY,
                TAG_FIXED_ARRAY_LAST,
                TAG_ARRAY16,
                TAG_ARRAY32,
            )?;
            for index in 0..count {
                let value = array_element(object, index)?;
                write_value_binary(out, &value, stack)?;
            }
            Ok(())
        }
        Container::Dictionary => {
            let mut count = 0usize;
            object
                .enum_members(MemberFlags::ENUM_NO_VALUE, &mut |_, flags, _| {
                    if flags & MemberFlags::HIDDEN.bits() == 0 {
                        count += 1;
                    }
                    true
                })
                .map_err(member_error)?;
            put_count(
                out,
                count,
                TAG_FIXED_MAP,
                TAG_FIXED_MAP_LAST,
                TAG_MAP16,
                TAG_MAP32,
            )?;
            let mut failure = None;
            object
                .enum_members(MemberFlags::IGNORE_PROPERTY, &mut |name, flags, value| {
                    if flags & MemberFlags::HIDDEN.bits() != 0 {
                        return true;
                    }
                    let written = (|| -> Result<(), StructError> {
                        put_text(out, name)?;
                        write_value_binary(out, value, stack)
                    })();
                    match written {
                        Ok(()) => true,
                        Err(error) => {
                            failure = Some(error);
                            false
                        }
                    }
                })
                .map_err(member_error)?;
            match failure {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }
}

/// Writes one value's binary form.
fn write_value_binary(
    out: &mut Vec<u8>,
    value: &Variant,
    stack: &mut Vec<*const ()>,
) -> Result<(), StructError> {
    match value {
        Variant::Void => {
            out.push(TAG_VOID);
            Ok(())
        }
        Variant::Integer(number) => {
            put_integer(out, *number);
            Ok(())
        }
        Variant::Real(number) => {
            out.push(TAG_DOUBLE);
            out.extend_from_slice(&number.to_le_bytes());
            Ok(())
        }
        Variant::String(text) => put_text(out, text),
        Variant::Octet(octet) => put_bytes(out, octet),
        Variant::Object(closure) => write_object_binary(out, closure, stack),
    }
}

/// Writes a value that is an object, walking the handle the value carries.
///
/// A container is its own binary form; a container already on the way down,
/// and any other object, are written as the null object.
fn write_object_binary(
    out: &mut Vec<u8>,
    closure: &Closure,
    stack: &mut Vec<*const ()>,
) -> Result<(), StructError> {
    let Some(handle) = walked_handle(closure) else {
        out.push(TAG_NIL);
        return Ok(());
    };
    let object = &**handle;
    let Some(kind) = container_kind(object) else {
        out.push(TAG_NIL);
        return Ok(());
    };
    if stack.contains(&identity(object)) {
        out.push(TAG_NIL);
        return Ok(());
    }
    stack.push(identity(object));
    let written = write_container_binary(out, kind, object, stack);
    stack.pop();
    written
}

/// Writes a text: the tag that carries its length, then its code units.
fn put_text(out: &mut Vec<u8>, text: &TjsString) -> Result<(), StructError> {
    let units = text.as_utf16();
    put_length(
        out,
        units.len(),
        TAG_FIXED_TEXT,
        FIXED_TEXT_LIMIT,
        Some(TAG_STRING8),
        TAG_STRING16,
        TAG_STRING32,
    )?;
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

/// Writes a byte string: the tag that carries its length, then the bytes.
fn put_bytes(out: &mut Vec<u8>, octet: &Octet) -> Result<(), StructError> {
    let bytes = octet.as_slice();
    put_length(
        out,
        bytes.len(),
        TAG_FIXED_BYTES,
        FIXED_BYTES_LIMIT,
        None,
        TAG_BYTES16,
        TAG_BYTES32,
    )?;
    out.extend_from_slice(bytes);
    Ok(())
}

/// Writes a length in the smallest of the four forms.
///
/// The first is a range of tags that carries the length itself, the second one
/// byte of length behind a tag when the kind has that form, and the last two
/// two and four bytes of length behind their tags.
fn put_length(
    out: &mut Vec<u8>,
    length: usize,
    fixed_first: u8,
    fixed_limit: usize,
    tag8: Option<u8>,
    tag16: u8,
    tag32: u8,
) -> Result<(), StructError> {
    if length <= fixed_limit {
        let fixed = u8::try_from(length).map_err(|_| write_error())?;
        out.push(fixed_first + fixed);
        return Ok(());
    }
    if let Some(tag8) = tag8
        && let Some(byte) = width::<u8>(length)
    {
        out.push(tag8);
        out.push(byte);
        return Ok(());
    }
    if let Some(value) = width::<u16>(length) {
        out.push(tag16);
        out.extend_from_slice(&value.to_le_bytes());
        return Ok(());
    }
    let value = u32::try_from(length).map_err(|_| write_error())?;
    out.push(tag32);
    out.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

/// The value `number` as a narrower integer, when it fits that width.
///
/// A width that cannot hold the value answers `None`, which is the range test
/// the caller's next-wider branch exists for — not a failure to report.
fn width<T>(number: impl TryInto<T>) -> Option<T> {
    number.try_into().ok()
}

/// Writes a container's item count in the smallest of the three forms.
fn put_count(
    out: &mut Vec<u8>,
    count: usize,
    fixed_first: u8,
    fixed_last: u8,
    tag16: u8,
    tag32: u8,
) -> Result<(), StructError> {
    let limit = usize::from(fixed_last - fixed_first);
    if count <= limit {
        let fixed = u8::try_from(count).map_err(|_| write_error())?;
        out.push(fixed_first + fixed);
        return Ok(());
    }
    if let Some(value) = width::<u16>(count) {
        out.push(tag16);
        out.extend_from_slice(&value.to_le_bytes());
        return Ok(());
    }
    let value = u32::try_from(count).map_err(|_| write_error())?;
    out.push(tag32);
    out.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Writes an integer: a value from zero through 127 in the tag itself, and
/// every other value behind the tag of the smallest width that holds it.
///
/// No tag stands for a negative value, so a negative value takes a width tag
/// and a byte order the form fixes: -1 is the one-byte tag and `0xFF`.
fn put_integer(out: &mut Vec<u8>, number: i64) {
    if number < 0 {
        if let Some(value) = width::<i8>(number) {
            out.push(TAG_INT8);
            out.push(value.to_le_bytes()[0]);
        } else if let Some(value) = width::<i16>(number) {
            out.push(TAG_INT16);
            out.extend_from_slice(&value.to_le_bytes());
        } else if let Some(value) = width::<i32>(number) {
            out.push(TAG_INT32);
            out.extend_from_slice(&value.to_le_bytes());
        } else {
            out.push(TAG_INT64);
            out.extend_from_slice(&number.to_le_bytes());
        }
        return;
    }
    if let Some(value) = width::<u8>(number) {
        if i64::from(value) <= i64::from(TAG_POSITIVE_FIXED) {
            out.push(value);
        } else {
            out.push(TAG_UINT8);
            out.push(value);
        }
        return;
    }
    if let Some(value) = width::<u16>(number) {
        out.push(TAG_UINT16);
        out.extend_from_slice(&value.to_le_bytes());
        return;
    }
    if let Some(value) = width::<u32>(number) {
        out.push(TAG_UINT32);
        out.extend_from_slice(&value.to_le_bytes());
        return;
    }
    out.push(TAG_UINT64);
    out.extend_from_slice(&number.to_le_bytes());
}

/// The bytes that follow, or the read error when the body ends first.
fn take<'a>(body: &'a [u8], index: &mut usize, count: usize) -> Result<&'a [u8], StructError> {
    let end = index.checked_add(count).ok_or(StructError::Read)?;
    let bytes = body.get(*index..end).ok_or(StructError::Read)?;
    *index = end;
    Ok(bytes)
}

/// One byte, or the read error when the body ends first.
fn take_byte(body: &[u8], index: &mut usize) -> Result<u8, StructError> {
    Ok(take(body, index, 1)?[0])
}

/// Two bytes, little-endian.
fn take_u16(body: &[u8], index: &mut usize) -> Result<u16, StructError> {
    let bytes = take(body, index, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Four bytes, little-endian.
fn take_u32(body: &[u8], index: &mut usize) -> Result<u32, StructError> {
    let bytes = take(body, index, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Eight bytes, little-endian.
fn take_u64(body: &[u8], index: &mut usize) -> Result<u64, StructError> {
    let bytes = take(body, index, 8)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

/// Reads the value the body holds at `index`.
fn read_value(body: &[u8], index: &mut usize) -> Result<Variant, StructError> {
    let tag = take_byte(body, index)?;
    match tag {
        TAG_NIL => Ok(Variant::Object(Closure::null())),
        TAG_VOID => Ok(Variant::Void),
        TAG_TRUE => Ok(Variant::from(1i64)),
        TAG_FALSE => Ok(Variant::from(0i64)),
        TAG_STRING8 => {
            let length = usize::from(take_byte(body, index)?);
            read_text(body, index, length)
        }
        TAG_STRING16 => {
            let length = usize::from(take_u16(body, index)?);
            read_text(body, index, length)
        }
        TAG_STRING32 => {
            let length = usize::try_from(take_u32(body, index)?).map_err(|_| StructError::Read)?;
            read_text(body, index, length)
        }
        TAG_FLOAT => {
            let bits = take_u32(body, index)?;
            Ok(Variant::Real(f64::from(f32::from_bits(bits))))
        }
        TAG_DOUBLE => {
            let bits = take_u64(body, index)?;
            Ok(Variant::Real(f64::from_bits(bits)))
        }
        TAG_UINT8 => Ok(Variant::from(i64::from(take_byte(body, index)?))),
        TAG_UINT16 => Ok(Variant::from(i64::from(take_u16(body, index)?))),
        TAG_UINT32 => Ok(Variant::from(i64::from(take_u32(body, index)?))),
        TAG_UINT64 => {
            let value = take_u64(body, index)?;
            // the same eight bytes as the integer they hold, so a value above
            // the signed maximum reads as the negative it is
            Ok(Variant::from(i64::from_le_bytes(value.to_le_bytes())))
        }
        TAG_INT8 => {
            let value = i8::from_le_bytes([take_byte(body, index)?]);
            Ok(Variant::from(i64::from(value)))
        }
        TAG_INT16 => {
            let bytes = take(body, index, 2)?;
            Ok(Variant::from(i64::from(i16::from_le_bytes([
                bytes[0], bytes[1],
            ]))))
        }
        TAG_INT32 => {
            let bytes = take(body, index, 4)?;
            Ok(Variant::from(i64::from(i32::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3],
            ]))))
        }
        TAG_INT64 => Ok(Variant::from(i64::from_le_bytes(
            take(body, index, 8)?
                .try_into()
                .map_err(|_| StructError::Read)?,
        ))),
        TAG_BYTES16 => {
            let length = usize::from(take_u16(body, index)?);
            read_octet(body, index, length)
        }
        TAG_BYTES32 => {
            let length = usize::try_from(take_u32(body, index)?).map_err(|_| StructError::Read)?;
            read_octet(body, index, length)
        }
        TAG_ARRAY16 => {
            let count = usize::from(take_u16(body, index)?);
            read_array(body, index, count)
        }
        TAG_ARRAY32 => {
            let count = usize::try_from(take_u32(body, index)?).map_err(|_| StructError::Read)?;
            read_array(body, index, count)
        }
        TAG_MAP16 => {
            let count = usize::from(take_u16(body, index)?);
            read_dictionary(body, index, count)
        }
        TAG_MAP32 => {
            let count = usize::try_from(take_u32(body, index)?).map_err(|_| StructError::Read)?;
            read_dictionary(body, index, count)
        }
        other if other <= TAG_POSITIVE_FIXED => Ok(Variant::from(i64::from(other))),
        // a negative small integer is the byte its tag holds, widened
        other if other >= TAG_NEGATIVE_FIXED => Ok(Variant::from(i64::from(other))),
        other if (TAG_FIXED_BYTES..TAG_FIXED_BYTES + 6).contains(&other) => {
            let length = usize::from(other - TAG_FIXED_BYTES);
            read_octet(body, index, length)
        }
        other if (TAG_FIXED_TEXT..=TAG_FIXED_TEXT_LAST).contains(&other) => {
            let length = usize::from(other - TAG_FIXED_TEXT);
            read_text(body, index, length)
        }
        other if (TAG_FIXED_ARRAY..=TAG_FIXED_ARRAY_LAST).contains(&other) => {
            let count = usize::from(other - TAG_FIXED_ARRAY);
            read_array(body, index, count)
        }
        other if (TAG_FIXED_MAP..=TAG_FIXED_MAP_LAST).contains(&other) => {
            let count = usize::from(other - TAG_FIXED_MAP);
            read_dictionary(body, index, count)
        }
        _ => Err(StructError::Read),
    }
}

/// Reads a text of `length` code units.
fn read_text(body: &[u8], index: &mut usize, length: usize) -> Result<Variant, StructError> {
    let bytes = take(body, index, length.checked_mul(2).ok_or(StructError::Read)?)?;
    let mut units = Vec::with_capacity(length);
    for [low, high] in bytes.as_chunks::<2>().0 {
        units.push(u16::from_le_bytes([*low, *high]));
    }
    Ok(Variant::String(TjsString::from(units)))
}

/// Reads a byte string of `length` bytes.
fn read_octet(body: &[u8], index: &mut usize, length: usize) -> Result<Variant, StructError> {
    let bytes = take(body, index, length)?;
    Ok(Variant::Octet(Octet::from_slice(bytes)))
}

/// Reads an array of `count` values.
fn read_array(body: &[u8], index: &mut usize, count: usize) -> Result<Variant, StructError> {
    let array = ArrayObject::new();
    for _ in 0..count {
        let value = read_value(body, index)?;
        array
            .append(value)
            .map_err(|_| StructError::LengthOutOfRange)?;
    }
    let array: Rc<dyn Dispatch> = Rc::new(array);
    Ok(container_value(array))
}

/// Reads a map of `count` name and value pairs.
fn read_dictionary(body: &[u8], index: &mut usize, count: usize) -> Result<Variant, StructError> {
    let dictionary = Dictionary::new();
    for _ in 0..count {
        let name = read_name(body, index)?;
        let value = read_value(body, index)?;
        dictionary
            .prop_set(
                &name,
                MemberFlags::ENSURE | MemberFlags::IGNORE_PROPERTY,
                &value,
                None,
            )
            .map_err(member_error)?;
    }
    let dictionary: Rc<dyn Dispatch> = Rc::new(dictionary);
    Ok(container_value(dictionary))
}

/// Reads the name a map writes before each value.
///
/// A name is a text and nothing else, so a tag that is not a text is the read
/// error rather than a name.
fn read_name(body: &[u8], index: &mut usize) -> Result<TjsString, StructError> {
    let tag = take_byte(body, index)?;
    let length = match tag {
        TAG_STRING8 => usize::from(take_byte(body, index)?),
        TAG_STRING16 => usize::from(take_u16(body, index)?),
        TAG_STRING32 => usize::try_from(take_u32(body, index)?).map_err(|_| StructError::Read)?,
        other if (TAG_FIXED_TEXT..=TAG_FIXED_TEXT_LAST).contains(&other) => {
            usize::from(other - TAG_FIXED_TEXT)
        }
        _ => return Err(StructError::Read),
    };
    match read_text(body, index, length)? {
        Variant::String(text) => Ok(text),
        _ => Err(StructError::Read),
    }
}

/// The receiver a dictionary class static runs on.
///
/// The object is the dictionary the call reached. A caller that holds the
/// value which names it also gives the handle, so a member whose answer is the
/// receiver can hand it back.
pub struct DictionaryReceiver<'a> {
    /// The dictionary the call reached.
    object: &'a dyn Dispatch,
    /// The value that names it, when the caller holds one.
    handle: Option<Variant>,
}

impl<'a> DictionaryReceiver<'a> {
    /// A receiver over `object`, named by `handle` when one is held.
    pub fn new(object: &'a dyn Dispatch, handle: Option<Variant>) -> Self {
        Self { object, handle }
    }
}

/// The static members the dictionary class publishes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DictionaryMember {
    /// Loads a text file; the member does nothing.
    Load,
    /// Loads a structured file into the receiver, or into a new dictionary.
    LoadStruct,
    /// Saves a text file; the member does nothing.
    Save,
    /// Saves the receiver's structure.
    SaveStruct,
    /// Copies another object's members without a deep copy.
    Assign,
    /// Copies another dictionary's structure, one deep copy per value.
    AssignStruct,
    /// Drops every member.
    Clear,
}

/// One row of the class's static member table.
struct DictionaryMemberSpec {
    /// The member the name denotes.
    member: DictionaryMember,
    /// The name the class publishes.
    name: &'static str,
    /// The fewest parameters a call accepts.
    min_params: usize,
}

/// The class's static member table, in the order the class registers them.
const CLASS_MEMBERS: &[DictionaryMemberSpec] = &[
    DictionaryMemberSpec {
        member: DictionaryMember::Load,
        name: "load",
        min_params: 0,
    },
    DictionaryMemberSpec {
        member: DictionaryMember::LoadStruct,
        name: "loadStruct",
        min_params: 1,
    },
    DictionaryMemberSpec {
        member: DictionaryMember::Save,
        name: "save",
        min_params: 0,
    },
    DictionaryMemberSpec {
        member: DictionaryMember::SaveStruct,
        name: "saveStruct",
        min_params: 1,
    },
    DictionaryMemberSpec {
        member: DictionaryMember::Assign,
        name: "assign",
        min_params: 1,
    },
    DictionaryMemberSpec {
        member: DictionaryMember::AssignStruct,
        name: "assignStruct",
        min_params: 1,
    },
    DictionaryMemberSpec {
        member: DictionaryMember::Clear,
        name: "clear",
        min_params: 0,
    },
];

impl DictionaryMember {
    /// The members the class publishes, in registration order.
    pub fn all() -> impl Iterator<Item = Self> {
        CLASS_MEMBERS.iter().map(|spec| spec.member)
    }

    /// The member `name` denotes, or `None` when it names no class member.
    pub fn lookup(name: &TjsString) -> Option<Self> {
        CLASS_MEMBERS
            .iter()
            .find(|spec| name.len() == spec.name.len() && name.eq_str(spec.name))
            .map(|spec| spec.member)
    }

    /// The name the class publishes for the member.
    pub fn name(self) -> &'static str {
        CLASS_MEMBERS
            .iter()
            .find(|spec| spec.member == self)
            .map(|spec| spec.name)
            .expect("every member has a table row")
    }

    /// The fewest parameters a call to the member accepts.
    fn min_params(self) -> usize {
        CLASS_MEMBERS
            .iter()
            .find(|spec| spec.member == self)
            .map(|spec| spec.min_params)
            .expect("every member has a table row")
    }

    /// Calls the member with `arguments`, on `receiver` when one was given.
    ///
    /// `loadStruct` takes no receiver and reads a throw-away dictionary; every
    /// other member needs one. A member whose failure is the structured form's
    /// answers the call surface's own refusal, which keeps the failure a raise
    /// a script can catch.
    ///
    /// # Errors
    ///
    /// [`ObjectError::ParameterCount`] when the call gives fewer arguments than
    /// the member needs, [`ObjectError::CallNotServed`] when the receiver is
    /// missing or the rule refused the call, and the receiver's own failure
    /// when it has been invalidated.
    pub fn call(
        self,
        arguments: &[Variant],
        receiver: Option<&DictionaryReceiver<'_>>,
    ) -> Result<Variant, ObjectError> {
        if arguments.len() < self.min_params() {
            return Err(ObjectError::ParameterCount);
        }
        match self {
            Self::Load | Self::Save => {
                let receiver = receiver.ok_or(ObjectError::CallNotServed)?;
                if !receiver.object.is_valid() {
                    return Err(ObjectError::InvalidObject);
                }
                Ok(Variant::Void)
            }
            Self::Clear => {
                let receiver = receiver.ok_or(ObjectError::CallNotServed)?;
                clear_object(receiver.object)?;
                Ok(Variant::Void)
            }
            Self::Assign => {
                let receiver = receiver.ok_or(ObjectError::CallNotServed)?;
                let clear = match arguments.get(1) {
                    None | Some(Variant::Void) => true,
                    Some(value) => value
                        .to_integer()
                        .map(|number| number != 0)
                        .map_err(|_| ObjectError::CallNotServed)?,
                };
                assign_into(receiver.object, &arguments[0], clear).map_err(surface_error)?;
                Ok(Variant::Void)
            }
            Self::AssignStruct => {
                let receiver = receiver.ok_or(ObjectError::CallNotServed)?;
                assign_struct_into(receiver.object, &arguments[0]).map_err(surface_error)?;
                Ok(Variant::Void)
            }
            Self::SaveStruct => {
                let receiver = receiver.ok_or(ObjectError::CallNotServed)?;
                if !receiver.object.is_valid() {
                    return Err(ObjectError::InvalidObject);
                }
                let name = argument_text(arguments, 0)?;
                let mode = optional_text(arguments.get(1))?;
                save_struct_object(receiver.object, &name, &mode).map_err(surface_error)?;
                Ok(receiver.handle.clone().unwrap_or(Variant::Void))
            }
            Self::LoadStruct => {
                let name = argument_text(arguments, 0)?;
                let mode = optional_text(arguments.get(1))?;
                load_struct_into(receiver, &name, &mode).map_err(surface_error)
            }
        }
    }
}

/// The text a required argument names.
fn argument_text(arguments: &[Variant], index: usize) -> Result<TjsString, ObjectError> {
    arguments
        .get(index)
        .ok_or(ObjectError::ParameterCount)?
        .to_string()
        .map_err(|_| ObjectError::CallNotServed)
}

/// The text an optional argument names; an absent or empty argument is empty.
fn optional_text(value: Option<&Variant>) -> Result<TjsString, ObjectError> {
    match value {
        None | Some(Variant::Void) => Ok(TjsString::new()),
        Some(value) => value.to_string().map_err(|_| ObjectError::CallNotServed),
    }
}

/// The call surface's answer for a structured form's failure.
///
/// The structured form's texts belong to the direct surface; a member call can
/// only answer the call surface's own refusal, which is the same raise every
/// other refused member gives.
fn surface_error(error: StructError) -> ObjectError {
    match error {
        StructError::Members(error) => error,
        StructError::InvalidObject => ObjectError::InvalidObject,
        _ => ObjectError::CallNotServed,
    }
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
            object: CustomObject::new([TjsString::from(CLASS_NAME)]),
        }
    }

    /// The text a structured save writes for this dictionary.
    ///
    /// The text is the dictionary's own, without a file or a stream around it;
    /// see the module documentation for the grammar. `objects` answers the
    /// dictionaries among the values it created; a value it does not name is
    /// identified by the class names its object reports.
    ///
    /// # Errors
    ///
    /// [`StructError::Members`] and [`StructError::InvalidObject`] when a
    /// container in the save cannot be walked, and [`StructError::Text`] when
    /// an object value has no text form.
    pub fn save_struct_text(&self, objects: &dyn StructObjects) -> Result<TjsString, StructError> {
        structure_text(self, objects)
    }

    /// The binary form of this dictionary, its signature included.
    ///
    /// # Errors
    ///
    /// [`StructError::Members`] and [`StructError::InvalidObject`] when a
    /// container in the save cannot be walked.
    pub fn save_struct_binary(&self) -> Result<Vec<u8>, StructError> {
        structure_binary(self)
    }

    /// Writes this dictionary to `name` in the form `mode` selects: the binary
    /// form when the mode holds a `b`, and the text form otherwise.
    ///
    /// # Errors
    ///
    /// [`StructError::Stream`] when the host cannot open the name or the stream
    /// fails, and the form's own failures as [`Self::save_struct_text`] reports
    /// them.
    pub fn save_struct(&self, name: &TjsString, mode: &TjsString) -> Result<(), StructError> {
        save_struct_object(self, name, mode)
    }

    /// Reads the value `name` holds in the binary form.
    ///
    /// The class member answers the value it read: the dictionary the body
    /// describes, a value the body holds at its root, and the refusal when the
    /// body's root is an array, which is a different container than the one the
    /// member loads.
    ///
    /// # Errors
    ///
    /// [`StructError::Stream`] when the host cannot open the name or the
    /// stream fails, [`StructError::InvalidArgument`] when the stream is not
    /// the binary form or its root is an array, and [`StructError::Read`] when
    /// the body is not one this reader accepts.
    pub fn load_struct(name: &TjsString, mode: &TjsString) -> Result<Variant, StructError> {
        load_struct_into(None, name, mode)
    }

    /// Copies `source`'s members into this dictionary, one deep copy per
    /// member, and drops the members it held.
    ///
    /// A member the source hides is not copied. The source must be a
    /// dictionary: an array, and any other object, is
    /// [`StructError::NotAContainer`], and a value that is not an object is
    /// [`StructError::NullAccess`].
    ///
    /// # Errors
    ///
    /// [`StructError::NotAContainer`] and [`StructError::NullAccess`] for a
    /// source this member does not take, [`StructError::Members`] and
    /// [`StructError::InvalidObject`] when a container on the way down cannot
    /// be walked, and [`StructError::LengthOutOfRange`] when a new container
    /// cannot hold its values.
    pub fn assign_struct(&self, source: &Variant) -> Result<(), StructError> {
        assign_struct_into(self, source)
    }
}

impl Default for Dictionary {
    fn default() -> Self {
        Self::new()
    }
}

/// Writes `object` to `name` in the form `mode` selects: the binary form when
/// the mode holds a `b`, and the text form otherwise.
///
/// # Errors
///
/// [`StructError::Stream`] when the host cannot open the name or the stream
/// fails, and the form's own failures as [`Dictionary::save_struct_text`]
/// reports them.
pub fn save_struct_object(
    object: &dyn Dispatch,
    name: &TjsString,
    mode: &TjsString,
) -> Result<(), StructError> {
    if mode.as_utf16().contains(&u16::from(b'b')) {
        let bytes = structure_binary(object)?;
        let mut stream = stream::open_binary_write(name, mode)?;
        stream.write(&bytes)?;
        return Ok(());
    }
    let text = structure_text(object, &ClassNames)?;
    let mut stream = stream::open_text_write(name, mode)?;
    stream.write(&text)?;
    Ok(())
}

/// Reads the value `name` holds and places it in `receiver` when the value is
/// a dictionary.
///
/// A receiver is cleared before the stream opens, so a load that fails leaves
/// it empty. A root dictionary becomes the receiver's own members and the
/// answer is the receiver; a root value of any other kind is the answer, and a
/// root array is the invalid argument the serializer raises for the wrong
/// container kind.
///
/// # Errors
///
/// [`StructError::Stream`] when the host cannot open the name or the stream
/// fails, [`StructError::InvalidArgument`] when the stream is not the binary
/// form or its root is an array, [`StructError::InvalidObject`] when the
/// receiver has been invalidated, and [`StructError::Read`] when the body is
/// not one this reader accepts.
pub fn load_struct_into(
    receiver: Option<&DictionaryReceiver<'_>>,
    name: &TjsString,
    mode: &TjsString,
) -> Result<Variant, StructError> {
    if let Some(receiver) = receiver {
        if !receiver.object.is_valid() {
            return Err(StructError::InvalidObject);
        }
        clear_object(receiver.object).map_err(member_error)?;
    }
    let value = read_struct_stream(name, mode)?;
    let root = match &value {
        Variant::Object(closure) => closure.object.as_deref().and_then(container_kind),
        Variant::Void
        | Variant::Integer(_)
        | Variant::Real(_)
        | Variant::String(_)
        | Variant::Octet(_) => None,
    };
    if root == Some(Container::Array) {
        return Err(StructError::InvalidArgument);
    }
    if let (Some(receiver), Some(Container::Dictionary)) = (receiver, root) {
        let Variant::Object(closure) = &value else {
            return Err(StructError::Read);
        };
        let Some(source) = closure.object.as_deref() else {
            return Err(StructError::Read);
        };
        assign_members(receiver.object, source)?;
        let answer = receiver.handle.clone();
        if let Some(answer) = answer {
            return Ok(answer);
        }
    }
    Ok(value)
}

/// Copies `source`'s members into `destination` without a deep copy.
///
/// A hidden member is skipped, and the flags the source reports travel with
/// the value.
fn assign_members(destination: &dyn Dispatch, source: &dyn Dispatch) -> Result<(), StructError> {
    let mut failure = None;
    source
        .enum_members(MemberFlags::IGNORE_PROPERTY, &mut |name, flags, value| {
            if flags & MemberFlags::HIDDEN.bits() != 0 {
                return true;
            }
            let written = destination.prop_set(
                name,
                MemberFlags::from_bits(flags) | MemberFlags::ENSURE | MemberFlags::IGNORE_PROPERTY,
                value,
                None,
            );
            match written {
                Ok(()) => true,
                Err(error) => {
                    failure = Some(member_error(error));
                    false
                }
            }
        })
        .map_err(member_error)?;
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Copies `source`'s members into `destination`, one shallow copy per value.
///
/// An array source is read as (name, value) pairs; any other object is walked
/// as a dictionary. The source must be an object, and the receiver is cleared
/// before any of its members are read.
///
/// # Errors
///
/// [`StructError::NullAccess`] when the source is not an object, and the
/// member walk's own failures.
pub fn assign_into(
    destination: &dyn Dispatch,
    source: &Variant,
    clear: bool,
) -> Result<(), StructError> {
    let Variant::Object(closure) = source else {
        return Err(StructError::NullAccess);
    };
    let Some(object) = closure.objthis.as_deref().or(closure.object.as_deref()) else {
        return Err(StructError::NullAccess);
    };
    if container_kind(object) == Some(Container::Array) {
        if clear {
            clear_object(destination).map_err(member_error)?;
        }
        let count = array_count(object)?;
        let mut index = 0usize;
        while index < count {
            let name = array_element(object, index)?;
            let name = name
                .to_string()
                .map_err(|_| StructError::Members(ObjectError::CallNotServed))?;
            index += 1;
            if index >= count {
                break;
            }
            let value = array_element(object, index)?;
            index += 1;
            destination
                .prop_set(
                    &name,
                    MemberFlags::ENSURE | MemberFlags::IGNORE_PROPERTY,
                    &value,
                    None,
                )
                .map_err(member_error)?;
        }
        return Ok(());
    }
    if clear {
        clear_object(destination).map_err(member_error)?;
    }
    assign_members(destination, object)
}

/// Copies `source`'s structure into `destination`, one deep copy per value.
///
/// # Errors
///
/// [`StructError::NullAccess`] when the source is not an object,
/// [`StructError::NotAContainer`] when it is not a dictionary, and the walk's
/// own failures.
pub fn assign_struct_into(destination: &dyn Dispatch, source: &Variant) -> Result<(), StructError> {
    let Variant::Object(closure) = source else {
        return Err(StructError::NullAccess);
    };
    let Some(object) = closure.objthis.as_deref().or(closure.object.as_deref()) else {
        return Err(StructError::NullAccess);
    };
    if container_kind(object) != Some(Container::Dictionary) {
        return Err(StructError::NotAContainer);
    }
    clear_object(destination).map_err(member_error)?;
    let mut stack = vec![identity(object)];
    let mut failure = None;
    object
        .enum_members(MemberFlags::IGNORE_PROPERTY, &mut |name, flags, value| {
            if flags & MemberFlags::HIDDEN.bits() != 0 {
                return true;
            }
            let copied = (|| -> Result<(), StructError> {
                let copied = copy_value(value, &mut stack)?;
                destination
                    .prop_set(
                        name,
                        MemberFlags::ENSURE | MemberFlags::IGNORE_PROPERTY,
                        &copied,
                        None,
                    )
                    .map_err(member_error)?;
                Ok(())
            })();
            match copied {
                Ok(()) => true,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        })
        .map_err(member_error)?;
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Drops every member `object` holds, hidden members included.
fn clear_object(object: &dyn Dispatch) -> Result<(), ObjectError> {
    let mut names = Vec::new();
    object.enum_members(MemberFlags::ENUM_NO_VALUE, &mut |name, _, _| {
        names.push(name.clone());
        true
    })?;
    for name in names {
        object.delete_member(&name, MemberFlags::NONE, None)?;
    }
    Ok(())
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

    fn func_member(
        &self,
        name: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Option<Result<Variant, ObjectError>> {
        self.object.func_member(name, objthis)
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.object.func_call(name, argv, objthis)
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{
        BINARY_HEADER, ClassNames, Container, Dictionary, StructError, StructObjects,
        container_kind, copy_value, read_structure, structure_binary, structure_text,
    };
    use crate::vm::runtime::array::ArrayObject;
    use crate::vm::runtime::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
    use crate::vm::runtime::octet::Octet;
    use crate::vm::runtime::string::TjsString;
    use crate::vm::runtime::variant::{Closure, Variant};

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
    fn saved(dictionary: &Dictionary) -> String {
        let text = dictionary
            .save_struct_text(&ClassNames)
            .expect("the dictionary saves");
        String::from_utf16(text.as_utf16()).expect("the save text is valid UTF-16")
    }

    /// The value that holds a dictionary the test created.
    fn dictionary_value(dictionary: &Rc<Dictionary>) -> Variant {
        let handle: Rc<dyn Dispatch> = dictionary.clone();
        Variant::Object(Closure {
            object: Some(handle),
            objthis: None,
        })
    }

    /// The value that holds an array the test created.
    fn array_value(array: &Rc<ArrayObject>) -> Variant {
        let handle: Rc<dyn Dispatch> = array.clone();
        Variant::Object(Closure {
            object: Some(handle),
            objthis: None,
        })
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
        let text = saved(&dictionary);
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
        let text = saved(&dictionary);
        assert!(text.contains("\"seen\" => 1"), "{text:?}");
        assert!(!text.contains("hidden"), "{text:?}");
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:434-456
    #[test]
    fn an_empty_dictionary_saves_an_empty_structure() {
        let dictionary = Dictionary::new();
        assert_eq!(saved(&dictionary), "(const) %[\r\n]");
    }

    // reference/krkr2/cpp/core/tjs2/tjsUtils.cpp:175-231
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:252-298
    #[test]
    fn a_real_is_written_in_hexadecimal_with_its_text() {
        let dictionary = Dictionary::new();
        write(&dictionary, "half", Variant::from(3.5f64));
        write(&dictionary, "zero", Variant::from(-0.0f64));
        write(&dictionary, "nan", Variant::from(f64::NAN));
        let text = saved(&dictionary);
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
        let text = saved(&dictionary);
        assert!(
            text.contains("\"quote \\\" backslash \\\\\" => \"\\tline\\n\\x01\\x61\""),
            "{text:?}"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsUtils.cpp:210-217
    #[test]
    fn an_octet_is_written_as_its_list_form() {
        let dictionary = Dictionary::new();
        write(
            &dictionary,
            "bytes",
            Variant::from(Octet::from_slice(&[0x00, 0xab, 0xff])),
        );
        write(&dictionary, "empty", Variant::from(Octet::new()));
        let text = saved(&dictionary);
        assert!(text.contains("\"bytes\" => <% 00 AB FF %>"), "{text:?}");
        assert!(text.contains("\"empty\" => <%  %>"), "{text:?}");
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:492-500
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1164-1197
    #[test]
    fn a_nested_dictionary_is_saved_at_its_depth() {
        let inner = Rc::new(Dictionary::new());
        write(&inner, "x", Variant::from(1i64));
        let outer = Dictionary::new();
        write(&outer, "inner", dictionary_value(&inner));
        assert_eq!(
            saved(&outer),
            "(const) %[\r\n \"inner\" => (const) %[\r\n  \"x\" => 1\r\n ]\r\n]"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:492-500
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1114-1156
    #[test]
    fn a_nested_array_is_saved_at_its_depth() {
        let inner = Rc::new(ArrayObject::new());
        inner
            .append(Variant::from(1i64))
            .expect("the value is appended");
        inner
            .append(Variant::from("two"))
            .expect("the value is appended");
        let outer = Dictionary::new();
        write(&outer, "list", array_value(&inner));
        assert_eq!(
            saved(&outer),
            "(const) %[\r\n \"list\" => (const) [\r\n  1,\r\n  \"two\"\r\n ]\r\n]"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1164-1174
    #[test]
    fn a_dictionary_that_repeats_is_written_as_its_comment() {
        let dictionary = Rc::new(Dictionary::new());
        write(&dictionary, "self", dictionary_value(&dictionary));
        assert_eq!(
            saved(&dictionary),
            "(const) %[\r\n \"self\" => null /* object recursion detected */\r\n]"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:492-500
    #[test]
    fn an_array_that_repeats_is_written_as_its_comment() {
        let array = Rc::new(ArrayObject::new());
        array
            .append(Variant::from(7i64))
            .expect("the value is appended");
        array
            .append(array_value(&array))
            .expect("the value is appended");
        let outer = Dictionary::new();
        write(&outer, "loop", array_value(&array));
        assert_eq!(
            saved(&outer),
            "(const) %[\r\n \"loop\" => (const) [\r\n  7,\r\n  null /* object recursion detected */\r\n ]\r\n]"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1197-1205
    #[test]
    fn an_object_that_is_not_a_container_is_written_with_its_text() {
        let dictionary = Dictionary::new();
        // a plain object is neither container, so the save writes it as one
        let handle: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        write(
            &dictionary,
            "other",
            Variant::from(Closure {
                object: Some(handle),
                objthis: None,
            }),
        );
        let text = saved(&dictionary);
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
            dictionary.save_struct_text(&ClassNames),
            Err(StructError::InvalidObject)
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:408-424
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:134-160
    #[test]
    fn the_binary_form_round_trips_the_value_kinds() {
        let inner = Rc::new(ArrayObject::new());
        inner
            .append(Variant::from(10i64))
            .expect("the value is appended");
        inner
            .append(Variant::from(20i64))
            .expect("the value is appended");
        let dictionary = Dictionary::new();
        write(&dictionary, "text", Variant::from("one"));
        write(&dictionary, "void", Variant::Void);
        write(
            &dictionary,
            "octet",
            Variant::from(Octet::from_slice(&[0, 0xab, 0xff])),
        );
        write(&dictionary, "real", Variant::from(2.5f64));
        write(&dictionary, "null", Variant::from(Closure::null()));
        write(&dictionary, "list", array_value(&inner));

        let bytes = dictionary
            .save_struct_binary()
            .expect("the dictionary saves");
        assert_eq!(&bytes[..BINARY_HEADER.len()], &BINARY_HEADER);
        // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:24-40
        // the map is six members, so the body starts with the short-map tag
        assert_eq!(bytes[BINARY_HEADER.len()], 0x86);

        let body = &bytes[BINARY_HEADER.len()..];
        let value = read_structure(body).expect("the body reads");
        let Variant::Object(closure) = value else {
            panic!("the body describes an object");
        };
        let object = closure.object.expect("the body describes an object");
        assert_eq!(container_kind(&*object), Some(Container::Dictionary));
        assert_eq!(
            object
                .prop_get(&name("text"), MemberFlags::NONE, None)
                .expect("the member is read")
                .to_string()
                .expect("the value has a text"),
            TjsString::from("one")
        );
        assert!(matches!(
            object
                .prop_get(&name("void"), MemberFlags::NONE, None)
                .expect("the member is read"),
            Variant::Void
        ));
        assert!(matches!(
            object
                .prop_get(&name("null"), MemberFlags::NONE, None)
                .expect("the member is read"),
            Variant::Object(_)
        ));
        match object
            .prop_get(&name("real"), MemberFlags::NONE, None)
            .expect("the member is read")
        {
            Variant::Real(value) => assert_eq!(value.to_bits(), 2.5f64.to_bits()),
            other => panic!("the real did not round trip: {other:?}"),
        }
        match object
            .prop_get(&name("octet"), MemberFlags::NONE, None)
            .expect("the member is read")
        {
            Variant::Octet(octet) => assert_eq!(octet.as_slice(), &[0x00, 0xab, 0xff]),
            other => panic!("the octet did not round trip: {other:?}"),
        }
        let list = object
            .prop_get(&name("list"), MemberFlags::NONE, None)
            .expect("the member is read");
        let Variant::Object(list) = list else {
            panic!("the list is an object");
        };
        let list = list.object.expect("the list is an object");
        assert_eq!(container_kind(&*list), Some(Container::Array));
        assert!(
            matches!(
                list.prop_get(&name("count"), MemberFlags::NONE, None)
                    .expect("the count is read"),
                Variant::Integer(2)
            ),
            "the list holds two values"
        );
    }

    /// The body of a one-item array that holds `value`.
    fn frame_body(value: Variant) -> Vec<u8> {
        let array = Rc::new(ArrayObject::new());
        array.append(value).expect("the value is appended");
        let bytes = structure_binary(&*array).expect("the array saves");
        bytes[BINARY_HEADER.len()..].to_vec()
    }

    /// The value the one-item frame `body` holds.
    fn frame_element(body: &[u8]) -> Variant {
        let value = read_structure(body).expect("the body reads");
        let Variant::Object(closure) = value else {
            panic!("the body describes an object");
        };
        let object = closure.object.expect("the body describes an object");
        object
            .prop_get(&name("0"), MemberFlags::NONE, None)
            .expect("the element is read")
    }

    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:118-128
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:225-231
    #[test]
    fn a_negative_integer_round_trips() {
        // the small-negative tag is compared against its own unsigned value,
        // so no negative value is written in a tag: each takes the smallest
        // signed width that holds it
        for (value, body) in [
            (-1i64, &[0x91, 0xD0, 0xFF][..]),
            (-5, &[0x91, 0xD0, 0xFB][..]),
            (-32, &[0x91, 0xD0, 0xE0][..]),
            (-33, &[0x91, 0xD0, 0xDF][..]),
            (-128, &[0x91, 0xD0, 0x80][..]),
            (-129, &[0x91, 0xD1, 0x7F, 0xFF][..]),
        ] {
            assert_eq!(
                frame_body(Variant::from(value)),
                body,
                "the bytes of {value}"
            );
            let element = frame_element(body);
            assert!(
                matches!(element, Variant::Integer(read) if read == value),
                "{value} reads back as itself: {element:?}"
            );
        }
    }

    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:290-298
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:33-34
    #[test]
    fn a_negative_byte_in_a_tag_reads_as_the_byte_it_holds() {
        // another writer may hold the tag: the tag's byte is the value, so
        // 0xFF is the integer 255 and never -1
        //
        // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:295-298 (a byte
        // in the negative-fixnum range reads as the tag's own unsigned value)
        let element = frame_element(&[0x91, 0xFF]);
        assert!(
            matches!(element, Variant::Integer(255)),
            "the element reads as the byte its tag holds: {element:?}"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsVariant.h:192-194
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1211-1220
    #[test]
    fn a_save_walks_the_receiver_of_a_value() {
        let inner = Rc::new(Dictionary::new());
        write(&inner, "x", Variant::from(1i64));
        let inner_handle: Rc<dyn Dispatch> = inner.clone();

        // the receiver is the container and the object is empty: the walk
        // follows the receiver
        let receiver_value = Variant::Object(Closure {
            object: None,
            objthis: Some(Rc::clone(&inner_handle)),
        });
        assert_eq!(
            frame_body(receiver_value.clone()),
            [0x91, 0x81, 0xA1, 0x78, 0x00, 0x01],
            "the receiver's structure is the value's bytes"
        );
        let receiver = Dictionary::new();
        write(&receiver, "v", receiver_value);
        assert_eq!(
            saved(&receiver),
            "(const) %[\r\n \"v\" => (const) %[\r\n  \"x\" => 1\r\n ]\r\n]"
        );

        // the receiver is no container and the object is one: the walk
        // follows the receiver, so the value is the null object
        let other: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let object_value = Variant::Object(Closure {
            object: Some(Rc::clone(&inner_handle)),
            objthis: Some(other),
        });
        assert_eq!(
            frame_body(object_value.clone()),
            [0x91, 0xC0],
            "the receiver is not a container"
        );
        let object = Dictionary::new();
        write(&object, "v", object_value);
        let text = saved(&object);
        assert!(
            text.starts_with("(const) %[\r\n \"v\" => null /* (object) \"(object 0x"),
            "{text:?}"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:415-424
    #[test]
    fn a_body_that_is_not_a_value_is_the_read_error() {
        for body in [
            &[][..],
            &[0xC7][..],
            // a text of three code units with one byte behind it
            &[0xA3, 0x41][..],
            // a map whose first name is not a text
            &[0x81, 0x01][..],
        ] {
            assert!(
                matches!(read_structure(body), Err(StructError::Read)),
                "the body {body:?} must be the read error"
            );
        }
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:560-600
    // reference/krkr2/cpp/core/tjs2/tjsError.h:271-272
    #[test]
    fn assign_struct_takes_a_dictionary_and_nothing_else() {
        let source = Rc::new(Dictionary::new());
        write(&source, "a", Variant::from(1i64));
        let target = Dictionary::new();
        target
            .assign_struct(&dictionary_value(&source))
            .expect("the dictionary is copied");
        assert!(
            matches!(
                target
                    .prop_get(&name("a"), MemberFlags::NONE, None)
                    .expect("the member is read"),
                Variant::Integer(1)
            ),
            "the member is copied"
        );

        // an array is the other container, not this one
        let array = Rc::new(ArrayObject::new());
        assert_eq!(
            target.assign_struct(&array_value(&array)),
            Err(StructError::NotAContainer)
        );
        // a value that is not an object at all, and the null object
        assert_eq!(
            target.assign_struct(&Variant::from(5i64)),
            Err(StructError::NullAccess)
        );
        assert_eq!(
            target.assign_struct(&Variant::from(Closure::null())),
            Err(StructError::NullAccess)
        );
        assert_eq!(
            StructError::NotAContainer.to_string(),
            "Specify a Dictionary object or an Array object"
        );
        assert_eq!(
            StructError::NullAccess.to_string(),
            "Accessing to nullptr object"
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:604-660
    #[test]
    fn assign_struct_copies_nested_containers_and_refuses_a_loop() {
        let inner = Rc::new(Dictionary::new());
        write(&inner, "x", Variant::from("y"));
        let inner_handle: Rc<dyn Dispatch> = inner.clone();
        let source = Rc::new(Dictionary::new());
        write(&source, "inner", dictionary_value(&inner));
        write(&source, "self", dictionary_value(&source));
        let target = Dictionary::new();
        target
            .assign_struct(&dictionary_value(&source))
            .expect("the dictionary is copied");
        let copied = target
            .prop_get(&name("inner"), MemberFlags::NONE, None)
            .expect("the member is read");
        let Variant::Object(copied) = copied else {
            panic!("the copy is an object");
        };
        let copied = copied.object.expect("the copy is an object");
        assert!(
            !Rc::ptr_eq(&copied, &inner_handle),
            "the member must be a new dictionary"
        );
        assert_eq!(
            copied
                .prop_get(&name("x"), MemberFlags::NONE, None)
                .expect("the member is read")
                .to_string()
                .expect("the value has a text"),
            TjsString::from("y")
        );
        assert!(matches!(
            target
                .prop_get(&name("self"), MemberFlags::NONE, None)
                .expect("the member is read"),
            Variant::Object(ref closure) if closure.is_null()
        ));
    }

    /// A resolver that names one dictionary: the value it created.
    struct Named {
        handle: Rc<dyn Dispatch>,
        dictionary: Rc<Dictionary>,
    }

    impl StructObjects for Named {
        fn dictionary(&self, value: &Closure) -> Option<&Dictionary> {
            let object = value.object.as_ref()?;
            if Rc::ptr_eq(object, &self.handle) {
                return Some(self.dictionary.as_ref());
            }
            None
        }
    }

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:417-447
    #[test]
    fn a_caller_that_names_its_dictionary_is_asked_first() {
        // a value whose object answers no class name is still a dictionary
        // when the caller names it
        let inner = Rc::new(Dictionary::new());
        write(&inner, "x", Variant::from(1i64));
        let plain: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let named = Named {
            handle: Rc::clone(&plain),
            dictionary: Rc::clone(&inner),
        };
        let outer = Dictionary::new();
        write(
            &outer,
            "inner",
            Variant::from(Closure {
                object: Some(plain),
                objthis: None,
            }),
        );

        // the class names identify nothing, so the caller's answer is the one
        // the walk follows
        let text = structure_text(&outer, &named).expect("the dictionary saves");
        let text = String::from_utf16(text.as_utf16()).expect("the text is valid UTF-16");
        assert!(
            text.contains("\"inner\" => (const) %[\r\n  \"x\" => 1\r\n ]"),
            "{text:?}"
        );

        // with no answer from the caller, the same value is written as an
        // object, because its object reports no container class name
        let text = structure_text(&outer, &ClassNames).expect("the dictionary saves");
        let text = String::from_utf16(text.as_utf16()).expect("the text is valid UTF-16");
        assert!(text.contains("\"inner\" => null /* (object)"), "{text:?}");
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1290-1360
    #[test]
    fn a_copy_of_a_loop_is_the_null_object() {
        let array = Rc::new(ArrayObject::new());
        array
            .append(Variant::from(1i64))
            .expect("the value is appended");
        array
            .append(array_value(&array))
            .expect("the value is appended");
        let mut stack = vec![super::identity(&*array)];
        let copied = copy_value(&array_value(&array), &mut stack).expect("the value is copied");
        assert!(matches!(
            copied,
            Variant::Object(ref closure) if closure.is_null()
        ));
    }
}
