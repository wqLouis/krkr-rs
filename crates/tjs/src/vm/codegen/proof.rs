//! The proof instrument: the node-building C interface.
//!
//! These calls let the native grammar drive the Rust generator, so that
//! the two code streams can be compared word for word. They are **not**
//! the shipped interface — the shipped path hands the Rust parser's tree
//! to the Rust generator directly, and the native side consumes the
//! script-block image (`tjs2_codegen_block_image`).
//!
//! A node is a number that names a node in the generator's own table;
//! no subtree is copied across the boundary.
//!
//! Every entry point catches a panic and reports it as an error value.
//! No entry point unwinds into the native caller.

#![allow(unsafe_code)]

use std::ffi::c_int;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use crate::vm::codegen::CodegenError;
use crate::vm::codegen::Generator;
use crate::vm::codegen::context::ContextType;
use crate::vm::codegen::emit::SubParam;
use crate::vm::codegen::node::Node;
use crate::vm::codegen::value::Value;

use crate::vm::codegen::ffi::{
    TJS2_CG_ERROR_PANIC, TJS2_CG_ERROR_RANGE, TJS2_CG_ERROR_STATE, TJS2_CG_OK, c_string,
    tjs2_codegen, tjs2_codegen_value,
};

/// Runs the given work, and turns a panic into an error value.
fn guard<T>(fallback: T, work: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(value) => value,
        Err(payload) => {
            let message = if let Some(text) = payload.downcast_ref::<&str>() {
                (*text).to_string()
            } else if let Some(text) = payload.downcast_ref::<String>() {
                text.clone()
            } else {
                "the generator panicked".to_string()
            };
            crate::vm::codegen::ffi::PANIC_MESSAGE.with(|slot| *slot.borrow_mut() = Some(message));
            fallback
        }
    }
}

/// Records the failure and returns the code the caller sees.
///
/// The error's text goes into the handle's message slot, so the caller can
/// read it after the fact; it is not dropped.
fn failure(generator: *mut tjs2_codegen, error: CodegenError) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    generator.error = TJS2_CG_ERROR_STATE;
    generator.message = Some(c_string(&error.to_string()));
    TJS2_CG_ERROR_STATE
}
/// The kind of a context, from the native number.
fn context_type(kind: c_int) -> Option<ContextType> {
    Some(match kind {
        crate::vm::codegen::ffi::TJS2_CG_CONTEXT_TOP_LEVEL => ContextType::TopLevel,
        crate::vm::codegen::ffi::TJS2_CG_CONTEXT_FUNCTION => ContextType::Function,
        crate::vm::codegen::ffi::TJS2_CG_CONTEXT_EXPR_FUNCTION => ContextType::ExprFunction,
        crate::vm::codegen::ffi::TJS2_CG_CONTEXT_PROPERTY => ContextType::Property,
        crate::vm::codegen::ffi::TJS2_CG_CONTEXT_PROPERTY_SETTER => ContextType::PropertySetter,
        crate::vm::codegen::ffi::TJS2_CG_CONTEXT_PROPERTY_GETTER => ContextType::PropertyGetter,
        crate::vm::codegen::ffi::TJS2_CG_CONTEXT_CLASS => ContextType::Class,
        crate::vm::codegen::ffi::TJS2_CG_CONTEXT_SUPER_CLASS_GETTER => {
            ContextType::SuperClassGetter
        }
        _ => return None,
    })
}

/// The kind of a value, from the native number.
fn value_kind(kind: c_int) -> Option<Value> {
    Some(match kind {
        crate::vm::codegen::ffi::TJS2_CG_VALUE_VOID => Value::Void,
        crate::vm::codegen::ffi::TJS2_CG_VALUE_INTEGER => Value::Integer(0),
        crate::vm::codegen::ffi::TJS2_CG_VALUE_REAL => Value::Real(0.0),
        crate::vm::codegen::ffi::TJS2_CG_VALUE_STRING => Value::String(String::new()),
        crate::vm::codegen::ffi::TJS2_CG_VALUE_OCTET => Value::Octet(Vec::new()),
        crate::vm::codegen::ffi::TJS2_CG_VALUE_OBJECT => {
            Value::Object(crate::vm::codegen::value::ObjectId(0))
        }
        crate::vm::codegen::ffi::TJS2_CG_VALUE_NULL => {
            Value::Object(crate::vm::codegen::value::ObjectId::NULL)
        }
        crate::vm::codegen::ffi::TJS2_CG_VALUE_CONTEXT => Value::Context(0),
        crate::vm::codegen::ffi::TJS2_CG_VALUE_ARRAY => Value::ConstArray {
            identity: crate::vm::codegen::value::ObjectId(0),
            elements: Vec::new(),
        },
        crate::vm::codegen::ffi::TJS2_CG_VALUE_DICTIONARY => Value::ConstDictionary {
            identity: crate::vm::codegen::value::ObjectId(0),
            entries: Vec::new(),
        },
        _ => return None,
    })
}

/// The node of the given number, for the commands that take one.
fn node_argument(generator: &tjs2_codegen, node: c_int) -> Option<Rc<Node>> {
    node_of(generator, node)
}

/// The node of the given number.
fn node_of(generator: &tjs2_codegen, node: c_int) -> Option<Rc<Node>> {
    generator.nodes.get(node as usize).cloned()
}

/// Adds a node to the generator's table and returns its number.
fn add_node(generator: &mut tjs2_codegen, node: Rc<Node>) -> c_int {
    generator.nodes.push(node);
    generator.nodes.len() as c_int - 1
}

/// Makes a node with an opcode.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_make_np0(
    generator: *mut tjs2_codegen,
    opcode: c_int,
    position: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        add_node(generator, Node::new(opcode, position))
    })
}

/// Makes a node with an opcode and one child.
///
/// The constant expressions are folded where the node is made, so the
/// folding happens here too.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_make_np1(
    generator: *mut tjs2_codegen,
    opcode: c_int,
    position: c_int,
    child: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let child = node_of(generator, child);
        let node = crate::vm::codegen::folding::make_np1(opcode, position, child.as_ref());
        add_node(generator, node)
    })
}

/// Makes a node with an opcode and two children.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_make_np2(
    generator: *mut tjs2_codegen,
    opcode: c_int,
    position: c_int,
    child1: c_int,
    child2: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let child1 = node_of(generator, child1);
        let child2 = node_of(generator, child2);
        let node = crate::vm::codegen::folding::make_np2(
            opcode,
            position,
            child1.as_ref(),
            child2.as_ref(),
        );
        add_node(generator, node)
    })
}

/// Makes a node with an opcode and three children.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_make_np3(
    generator: *mut tjs2_codegen,
    opcode: c_int,
    position: c_int,
    child1: c_int,
    child2: c_int,
    child3: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let mut node = Node::new(opcode, position);
        for child in [child1, child2, child3] {
            if let Some(child) = node_of(generator, child) {
                crate::vm::codegen::node::add_child(&mut node, child);
            }
        }
        add_node(generator, node)
    })
}

/// Adds a child to a node.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_node_add_child(
    generator: *mut tjs2_codegen,
    node: c_int,
    child: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let Some(mut target) = node_of(generator, node) else {
            return TJS2_CG_ERROR_RANGE;
        };
        let Some(child) = node_of(generator, child) else {
            return TJS2_CG_ERROR_RANGE;
        };
        crate::vm::codegen::node::add_child(&mut target, child);
        // the node is shared, so the table's copy is replaced
        match generator.nodes.get_mut(node as usize) {
            Some(slot) => {
                *slot = target;
                TJS2_CG_OK
            }
            None => TJS2_CG_ERROR_RANGE,
        }
    })
}

/// The opcode of a node.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_node_kind(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    match node_of(generator, node) {
        Some(node) => node.opcode,
        None => {
            generator.error = TJS2_CG_ERROR_RANGE;
            TJS2_CG_ERROR_RANGE
        }
    }
}

/// The place of a node in the script.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_node_position(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    match node_of(generator, node) {
        Some(node) => node.position,
        None => {
            generator.error = TJS2_CG_ERROR_RANGE;
            TJS2_CG_ERROR_RANGE
        }
    }
}

/// Sets the opcode of a node.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_node_set_opcode(
    generator: *mut tjs2_codegen,
    node: c_int,
    opcode: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let Some(mut target) = node_of(generator, node) else {
            return TJS2_CG_ERROR_RANGE;
        };
        crate::vm::codegen::node::set_opcode(&mut target, opcode);
        match generator.nodes.get_mut(node as usize) {
            Some(slot) => {
                *slot = target;
                TJS2_CG_OK
            }
            None => TJS2_CG_ERROR_RANGE,
        }
    })
}

/// The value the interface carries.
///
/// The kind says which of the payload fields holds the value; the value
/// is the one the generator folds with.
fn value_from_image(value: &tjs2_codegen_value) -> Option<Value> {
    let kind = value_kind(value.kind)?;
    Some(match kind {
        Value::Integer(_) => Value::Integer(value.integer),
        Value::Real(_) => Value::Real(value.real),
        Value::String(_) => {
            let text = if value.string.is_null() {
                String::new()
            } else {
                // SAFETY: the caller promises the string is readable.
                let units = unsafe {
                    std::slice::from_raw_parts(value.string, value.string_length as usize)
                };
                String::from_utf16_lossy(units)
            };
            Value::String(text)
        }
        Value::Octet(_) => {
            let bytes = if value.octet.is_null() {
                Vec::new()
            } else {
                // SAFETY: the caller promises the bytes are readable.
                unsafe { std::slice::from_raw_parts(value.octet, value.octet_length as usize) }
                    .to_vec()
            };
            Value::Octet(bytes)
        }
        Value::Object(_) => Value::Object(crate::vm::codegen::value::ObjectId(value.object)),
        Value::Context(_) => Value::Context(value.object as usize),
        Value::Void => Value::Void,
        Value::ConstArray { .. } => {
            // the elements of a constant object live in the block's
            // object table, and the seam's value names the object
            // without carrying the table; the seam carries the kind
            Value::ConstArray {
                identity: crate::vm::codegen::value::ObjectId(value.object),
                elements: Vec::new(),
            }
        }
        Value::ConstDictionary { .. } => Value::ConstDictionary {
            identity: crate::vm::codegen::value::ObjectId(value.object),
            entries: Vec::new(),
        },
    })
}

/// Sets the value of a node.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_node_set_value(
    generator: *mut tjs2_codegen,
    node: c_int,
    value: *const tjs2_codegen_value,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        if value.is_null() {
            return TJS2_CG_ERROR_RANGE;
        }
        // SAFETY: the caller promises the value is readable.
        let value = unsafe { &*value };
        let Some(mut target) = node_of(generator, node) else {
            return TJS2_CG_ERROR_RANGE;
        };
        let Some(payload) = value_from_image(value) else {
            return TJS2_CG_ERROR_RANGE;
        };
        crate::vm::codegen::node::set_value(&mut target, payload);
        match generator.nodes.get_mut(node as usize) {
            Some(slot) => {
                *slot = target;
                TJS2_CG_OK
            }
            None => TJS2_CG_ERROR_RANGE,
        }
    })
}

/// Makes a node with a constant value.
///
/// `MakeConstValNode` makes the node of a constant value; the value is
/// the one the interface carries.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_make_const(
    generator: *mut tjs2_codegen,
    position: c_int,
    value: *const tjs2_codegen_value,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        if value.is_null() {
            return TJS2_CG_ERROR_RANGE;
        }
        // SAFETY: the caller promises the value is readable.
        let value = unsafe { &*value };
        let Some(payload) = value_from_image(value) else {
            return TJS2_CG_ERROR_RANGE;
        };
        add_node(
            generator,
            Node::with_value(crate::vm::codegen::token::T_CONSTVAL, position, payload),
        )
    })
}

/// The number of contexts the generator holds.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_context_count(generator: *const tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_ref() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    generator.generator.contexts().len() as c_int
}

/// The number of words in the code of a context.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_code_size(
    generator: *mut tjs2_codegen,
    context: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    match generator.generator.contexts().get(context as usize) {
        Some(context) => context.code().len() as c_int,
        None => {
            generator.error = TJS2_CG_ERROR_RANGE;
            TJS2_CG_ERROR_RANGE
        }
    }
}

/// One word of the code of a context.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_code_word(
    generator: *mut tjs2_codegen,
    context: c_int,
    index: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    let Some(context) = generator.generator.contexts().get(context as usize) else {
        return TJS2_CG_ERROR_RANGE;
    };
    match context.code().get(index as usize) {
        Some(word) => *word,
        None => {
            generator.error = TJS2_CG_ERROR_RANGE;
            TJS2_CG_ERROR_RANGE
        }
    }
}

/// Compares the code of a context with the code the caller holds.
///
/// # Safety
///
/// `generator` must be a live handle, and `code` must hold `length`
/// readable words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_compare_code(
    generator: *const tjs2_codegen,
    context: c_int,
    code: *const i32,
    length: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_ref() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    let Some(context) = generator.generator.contexts().get(context as usize) else {
        return TJS2_CG_ERROR_RANGE;
    };
    let mine = context.code();
    if mine.len() != length as usize {
        return -(mine.len() as c_int) - 1;
    }
    if length == 0 {
        return 0;
    }
    if code.is_null() {
        return TJS2_CG_ERROR_RANGE;
    }
    // SAFETY: the caller promises `length` readable words.
    let theirs = unsafe { std::slice::from_raw_parts(code, length as usize) };
    for (index, (mine, theirs)) in mine.iter().zip(theirs.iter()).enumerate() {
        if mine != theirs {
            return index as c_int + 1;
        }
    }
    0
}

/// Compares the data of a context with the data the caller holds.
///
/// # Safety
///
/// `generator` must be a live handle, and `values` must hold `count`
/// readable values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_compare_data(
    generator: *const tjs2_codegen,
    context: c_int,
    values: *const tjs2_codegen_value,
    count: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_ref() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    let Some(context) = generator.generator.contexts().get(context as usize) else {
        return TJS2_CG_ERROR_RANGE;
    };
    let mine = context.data();
    if mine.len() != count as usize {
        return -(mine.len() as c_int) - 1;
    }
    if count == 0 {
        return 0;
    }
    if values.is_null() {
        return TJS2_CG_ERROR_RANGE;
    }
    // SAFETY: the caller promises `count` readable values.
    let theirs = unsafe { std::slice::from_raw_parts(values, count as usize) };
    let mine_values: Vec<&Value> = mine.iter().collect();
    let their_values: Vec<&tjs2_codegen_value> = theirs.iter().collect();
    for (index, (mine, theirs)) in mine_values.iter().zip(their_values.iter()).enumerate() {
        if !same_value(mine, theirs) {
            if std::env::var("TJS2_CG_DEBUG_DATA").is_ok() {
                eprintln!("--- the data ---");
                for (place, value) in mine_values.iter().enumerate() {
                    eprintln!("  rust[{}] = {:?}", place, value);
                }
                for (place, value) in their_values.iter().enumerate() {
                    eprintln!(
                        "  cpp[{}] = kind {} int {} real {} str_len {}",
                        place, value.kind, value.integer, value.real, value.string_length
                    );
                }
            }
            return index as c_int + 1;
        }
    }
    0
}

/// Whether two values are the same.
fn same_value(mine: &Value, theirs: &tjs2_codegen_value) -> bool {
    match mine {
        Value::Void => theirs.kind == crate::vm::codegen::ffi::TJS2_CG_VALUE_VOID,
        Value::Integer(value) => {
            theirs.kind == crate::vm::codegen::ffi::TJS2_CG_VALUE_INTEGER
                && theirs.integer == *value
        }
        Value::Real(value) => {
            theirs.kind == crate::vm::codegen::ffi::TJS2_CG_VALUE_REAL && theirs.real == *value
        }
        Value::String(value) => {
            if theirs.kind != crate::vm::codegen::ffi::TJS2_CG_VALUE_STRING {
                return false;
            }
            if value.is_empty() && theirs.string_length == 0 {
                return true;
            }
            if theirs.string.is_null() {
                return false;
            }
            let units = unsafe {
                // SAFETY: the caller promises the string is readable.
                std::slice::from_raw_parts(theirs.string, theirs.string_length as usize)
            };
            value.encode_utf16().eq(units.iter().copied())
        }
        Value::Octet(bytes) => {
            if theirs.kind != crate::vm::codegen::ffi::TJS2_CG_VALUE_OCTET {
                return false;
            }
            if theirs.octet.is_null() {
                return bytes.is_empty();
            }
            let theirs = unsafe {
                // SAFETY: the caller promises the bytes are readable.
                std::slice::from_raw_parts(theirs.octet, theirs.octet_length as usize)
            };
            bytes.as_slice() == theirs
        }
        Value::Object(id) => {
            theirs.kind == crate::vm::codegen::ffi::TJS2_CG_VALUE_OBJECT && theirs.object == id.0
        }
        Value::Context(index) => {
            theirs.kind == crate::vm::codegen::ffi::TJS2_CG_VALUE_CONTEXT
                && theirs.object == *index as u64
        }
        Value::ConstArray { .. } => {
            // the elements of a constant object live in the block's
            // object table, and the seam's value carries only the kind
            theirs.kind == crate::vm::codegen::ffi::TJS2_CG_VALUE_ARRAY
        }
        Value::ConstDictionary { .. } => {
            theirs.kind == crate::vm::codegen::ffi::TJS2_CG_VALUE_DICTIONARY
        }
    }
}

/// The place in the script the analyser is at.
///
/// The generator writes this place into the instructions that have no
/// place of their own. The native grammar tells the generator where the
/// analyser is every time the analyser reads.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_at_lex_position(
    generator: *mut tjs2_codegen,
    position: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        generator.generator.lex_position = position;
        TJS2_CG_OK
    })
}

/// Opens a code context.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_push_context(
    generator: *mut tjs2_codegen,
    name: *const u16,
    name_length: u32,
    kind: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    let Some(kind) = context_type(kind) else {
        generator.error = TJS2_CG_ERROR_RANGE;
        return TJS2_CG_ERROR_RANGE;
    };
    let units: Vec<u16> = if name.is_null() || name_length == 0 {
        Vec::new()
    } else {
        // SAFETY: the caller promises `name_length` readable code units.
        unsafe { std::slice::from_raw_parts(name, name_length as usize) }.to_vec()
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        generator.generator.push_context(Some(&units), kind) as c_int
    })
}

/// Closes the code context the code is generated for.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_pop_context(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        match generator.generator.pop_context() {
            Ok(()) => TJS2_CG_OK,
            Err(error) => {
                // the detail is kept, so the caller can read it with
                // tjs2_codegen_last_error_message
                generator.message = Some(c_string(&error.to_string()));
                TJS2_CG_ERROR_STATE
            }
        }
    })
}

/// The code of a node.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_create_expr_code(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let Some(node) = node_of(generator, node) else {
            return TJS2_CG_ERROR_RANGE;
        };
        match generator.generator.create_expr_code(&node) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => {
                // the detail is kept, so the caller can read it with
                // tjs2_codegen_last_error_message
                generator.message = Some(c_string(&error.to_string()));
                TJS2_CG_ERROR_STATE
            }
        }
    })
}

/// Adds a local variable to the context.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_add_local_variable(
    generator: *mut tjs2_codegen,
    name: *const u16,
    name_length: u32,
    init: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let text = read_string(name, name_length);
        generator.generator.add_local_variable(&text, init);
        TJS2_CG_OK
    })
}

/// Reads a string of code units.
fn read_string(name: *const u16, name_length: u32) -> String {
    if name.is_null() || name_length == 0 {
        return String::new();
    }
    // SAFETY: the caller promises `name_length` readable code units.
    let units = unsafe { std::slice::from_raw_parts(name, name_length as usize) };
    String::from_utf16_lossy(units)
}

/// The generator, for the calls the grammar drives.
///
/// The generator is the Rust implementation, so the proof instrument
/// drives the very same code the shipping path drives.
pub type SharedGenerator = Generator;

/// The sub-parameter of an emission.
pub fn sub_param(sub_type: i32, sub_address: i32) -> SubParam {
    SubParam {
        sub_type,
        sub_address,
    }
}

/// The `enter_if_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_enter_if_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        generator.generator.enter_if_code();
        TJS2_CG_OK
    })
}

/// The `create_if_expr_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_create_if_expr_code(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        match generator.generator.create_if_expr_code(node.as_ref()) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `exit_if_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_exit_if_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        match generator.generator.exit_if_code() {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `enter_else_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_enter_else_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        generator.generator.enter_else_code();
        TJS2_CG_OK
    })
}

/// The `exit_else_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_exit_else_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        match generator.generator.exit_else_code() {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `enter_while_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_enter_while_code(
    generator: *mut tjs2_codegen,
    do_while: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let do_while = do_while != 0;
        generator.generator.enter_while_code(do_while);
        TJS2_CG_OK
    })
}

/// The `create_while_expr_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_create_while_expr_code(
    generator: *mut tjs2_codegen,
    node: c_int,
    do_while: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        let do_while = do_while != 0;
        match generator
            .generator
            .create_while_expr_code(node.as_ref(), do_while)
        {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `exit_while_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_exit_while_code(
    generator: *mut tjs2_codegen,
    do_while: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let do_while = do_while != 0;
        match generator.generator.exit_while_code(do_while) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `enter_for_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_enter_for_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        generator.generator.enter_for_code();
        TJS2_CG_OK
    })
}

/// The `create_for_expr_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_create_for_expr_code(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        match generator.generator.create_for_expr_code(node.as_ref()) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `set_for_third_expr_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_set_for_third_expr_code(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        match generator.generator.set_for_third_expr_code(node.as_ref()) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `exit_for_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_exit_for_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        match generator.generator.exit_for_code() {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `enter_switch_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_enter_switch_code(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        match generator.generator.enter_switch_code(node.as_ref()) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `exit_switch_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_exit_switch_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        match generator.generator.exit_switch_code() {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `process_case_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_process_case_code(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        match generator.generator.process_case_code(node.as_ref()) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `enter_with_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_enter_with_code(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        match generator.generator.enter_with_code(node.as_ref()) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `exit_with_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_exit_with_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        match generator.generator.exit_with_code() {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `enter_try_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_enter_try_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        generator.generator.enter_try_code();
        TJS2_CG_OK
    })
}

/// The `enter_catch_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_enter_catch_code(
    generator: *mut tjs2_codegen,
    name: *const u16,
    name_length: u32,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let text = read_string(name, name_length);
        let name = if name.is_null() || name_length == 0 {
            None
        } else {
            Some(text.as_str())
        };
        match generator.generator.enter_catch_code(name) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `exit_try_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_exit_try_code(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        match generator.generator.exit_try_code() {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `process_throw_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_process_throw_code(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        match generator.generator.process_throw_code(node.as_ref()) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `do_break` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_do_break(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        match generator.generator.do_break() {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `do_continue` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_do_continue(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        match generator.generator.do_continue() {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `do_debugger` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_do_debugger(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        generator.generator.do_debugger();
        TJS2_CG_OK
    })
}

/// The `return_from_func` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_return_from_func(
    generator: *mut tjs2_codegen,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        match generator.generator.return_from_func(node.as_ref()) {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `create_extends_expr_code` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_create_extends_expr_code(
    generator: *mut tjs2_codegen,
    node: c_int,
    hold: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let node = node_argument(generator, node);
        let hold = hold != 0;
        match generator
            .generator
            .create_extends_expr_code(node.as_ref(), hold)
        {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `init_local_variable` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_init_local_variable(
    generator: *mut tjs2_codegen,
    name: *const u16,
    name_length: u32,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let text = read_string(name, name_length);
        let node = node_argument(generator, node);
        let node = node.expect("the node is there");
        match generator
            .generator
            .init_local_variable(text.as_str(), &node)
        {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `add_function_decl_arg` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_add_function_decl_arg(
    generator: *mut tjs2_codegen,
    name: *const u16,
    name_length: u32,
    node: c_int,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let text = read_string(name, name_length);
        let node = node_argument(generator, node);
        match generator
            .generator
            .add_function_decl_arg(text.as_str(), node.as_ref())
        {
            Ok(()) => TJS2_CG_OK,
            Err(error) => failure(generator, error),
        }
    })
}

/// The `add_function_decl_arg_collapse` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_add_function_decl_arg_collapse(
    generator: *mut tjs2_codegen,
    name: *const u16,
    name_length: u32,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let text = read_string(name, name_length);
        let name = if name.is_null() || name_length == 0 {
            None
        } else {
            Some(text.as_str())
        };
        generator.generator.add_function_decl_arg_collapse(name);
        TJS2_CG_OK
    })
}

/// The `set_property_decl_arg` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_set_property_decl_arg(
    generator: *mut tjs2_codegen,
    name: *const u16,
    name_length: u32,
) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        let text = read_string(name, name_length);
        generator.generator.set_property_decl_arg(text.as_str());
        TJS2_CG_OK
    })
}

/// The `enter_block` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_enter_block(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        generator.generator.enter_block();
        TJS2_CG_OK
    })
}

/// The `exit_block` command of the grammar.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_exit_block(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    guard(TJS2_CG_ERROR_PANIC, || {
        generator.generator.exit_block();
        TJS2_CG_OK
    })
}
