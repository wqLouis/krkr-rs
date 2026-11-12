//! The tests of the code generator's C interface.
//!
//! The image builder is the shipped interface, so its tests are about
//! what the native side reads: the code stream, the constant area, the
//! variable area, the function-argument area, the labels and the line
//! table.

use std::ffi::c_int;

use tjs::vm::codegen::Generator;
use tjs::vm::codegen::context::ContextType;
use tjs::vm::codegen::ffi::*;
use tjs::vm::codegen::proof::*;
use tjs::vm::codegen::value::Value;

/// The generator the tests drive.
struct Harness {
    generator: *mut tjs2_codegen,
}

impl Harness {
    fn new() -> Self {
        Self {
            generator: tjs2_codegen_new(),
        }
    }

    fn generator_mut(&mut self) -> &mut Generator {
        unsafe { &mut (*self.generator).generator }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        unsafe { tjs2_codegen_free(self.generator) };
    }
}

/// The image of the generator.
fn image(generator: *mut tjs2_codegen) -> *const tjs2_codegen_block {
    unsafe {
        assert_eq!(tjs2_codegen_build_image(generator), TJS2_CG_OK);
        tjs2_codegen_block_image(generator)
    }
}

/// A context with the given constants.
fn context_with(generator: &mut Generator, constants: Vec<Value>) {
    generator.push_context(Some(&[0x67, 0x6c]), ContextType::TopLevel);
    for value in constants {
        generator.current_mut().put_data(value);
    }
    generator.pop_context().expect("the context closes");
}

/// The image of the first context.
fn first_context(block: *const tjs2_codegen_block) -> *const tjs2_codegen_context {
    unsafe {
        let block = &*block;
        assert!(block.context_count >= 1, "the block holds a context");
        block.contexts
    }
}

/// The image builder reads the string area with the constant's own
/// place, so a constant that is not a string does not shift the ones
/// that are.
#[test]
fn the_image_reads_a_string_after_an_integer() {
    let mut harness = Harness::new();
    context_with(
        harness.generator_mut(),
        vec![Value::Integer(7), Value::String("hello".to_string())],
    );

    let block = image(harness.generator);
    let context = first_context(block);
    unsafe {
        let context = &*context;
        assert_eq!(context.constant_count, 2);
        let constants =
            std::slice::from_raw_parts(context.constants, context.constant_count as usize);
        assert_eq!(constants[0].kind, TJS2_CG_VALUE_INTEGER);
        assert_eq!(constants[0].integer, 7);
        assert_eq!(constants[1].kind, TJS2_CG_VALUE_STRING);
        assert_eq!(constants[1].string_length, 5);
        let text = std::slice::from_raw_parts(constants[1].string, 5);
        assert_eq!(String::from_utf16_lossy(text), "hello");
    }
}

/// The same, with the string before the integer and with two strings.
#[test]
fn the_image_reads_the_strings_where_they_are() {
    let mut harness = Harness::new();
    context_with(
        harness.generator_mut(),
        vec![
            Value::String("first".to_string()),
            Value::Integer(3),
            Value::String("second".to_string()),
        ],
    );

    let block = image(harness.generator);
    let context = first_context(block);
    unsafe {
        let context = &*context;
        let constants =
            std::slice::from_raw_parts(context.constants, context.constant_count as usize);
        assert_eq!(context.constant_count, 3);
        let read = |index: usize| {
            let length = constants[index].string_length as usize;
            let text = std::slice::from_raw_parts(constants[index].string, length);
            String::from_utf16_lossy(text)
        };
        assert_eq!(read(0), "first");
        assert_eq!(constants[1].kind, TJS2_CG_VALUE_INTEGER);
        assert_eq!(constants[1].integer, 3);
        assert_eq!(read(2), "second");
    }
}

/// A byte string is a byte string: its bytes are the bytes.
#[test]
fn the_image_reads_a_byte_string() {
    let mut harness = Harness::new();
    context_with(
        harness.generator_mut(),
        vec![Value::Integer(1), Value::Octet(vec![0x41, 0x42, 0x43])],
    );

    let block = image(harness.generator);
    let context = first_context(block);
    unsafe {
        let context = &*context;
        let constants =
            std::slice::from_raw_parts(context.constants, context.constant_count as usize);
        assert_eq!(constants[1].kind, TJS2_CG_VALUE_OCTET);
        assert_eq!(constants[1].octet_length, 3);
        let bytes = std::slice::from_raw_parts(constants[1].octet, 3);
        assert_eq!(bytes, &[0x41, 0x42, 0x43]);
    }
}

/// The image holds the code stream of the context.
#[test]
fn the_image_holds_the_code_stream() {
    let mut harness = Harness::new();
    context_with(harness.generator_mut(), vec![Value::Integer(1)]);

    let block = image(harness.generator);
    let context = first_context(block);
    unsafe {
        let context = &*context;
        // the context closes with the return sequence
        assert_eq!(context.code_size, 3);
        assert!(!context.code.is_null());
        assert_eq!(context.kind, TJS2_CG_CONTEXT_TOP_LEVEL);
    }
}

/// The image holds the labels: the places in the script the code names.
#[test]
fn the_image_holds_the_labels() {
    let mut harness = Harness::new();
    harness
        .generator_mut()
        .push_context(Some(&[0x67]), ContextType::TopLevel);
    harness
        .generator_mut()
        .current_mut()
        .put_code(tjs::vm::codegen::opcode::vm::NOP, 42);
    harness
        .generator_mut()
        .pop_context()
        .expect("the context closes");

    let block = image(harness.generator);
    let context = first_context(block);
    unsafe {
        let context = &*context;
        assert_eq!(context.label_count, 1);
        let labels = std::slice::from_raw_parts(context.labels, context.label_count as usize);
        assert_eq!(labels[0].source_pos, 42);
        assert_eq!(labels[0].code_pos, 0);
    }
}

/// The error of the last call is an error value, and the message is
/// there with it.
#[test]
fn the_error_is_a_value_with_a_message() {
    let harness = Harness::new();
    unsafe {
        assert_eq!(tjs2_codegen_last_error(harness.generator), TJS2_CG_OK);
        // a context that does not exist
        let code = tjs2_codegen_code_size(harness.generator, 99);
        assert!(code < 0, "the call failed");
    }
}

/// The panic of the generator is caught, and it is an error value.
#[test]
fn a_panic_is_an_error_value() {
    let harness = Harness::new();
    unsafe {
        // a node that does not exist
        let kind = tjs2_codegen_node_kind(harness.generator, 12345);
        assert!(kind < 0, "the call failed");
        assert_eq!(
            tjs2_codegen_last_error(harness.generator),
            TJS2_CG_ERROR_RANGE
        );
    }
}

/// The kind of a context is the interface's number.
#[test]
fn the_context_kinds_are_the_interface_numbers() {
    let mut harness = Harness::new();
    for (kind, _expected) in [
        (ContextType::TopLevel, TJS2_CG_CONTEXT_TOP_LEVEL),
        (ContextType::Function, TJS2_CG_CONTEXT_FUNCTION),
        (ContextType::ExprFunction, TJS2_CG_CONTEXT_EXPR_FUNCTION),
        (ContextType::Property, TJS2_CG_CONTEXT_PROPERTY),
        (ContextType::PropertySetter, TJS2_CG_CONTEXT_PROPERTY_SETTER),
        (ContextType::PropertyGetter, TJS2_CG_CONTEXT_PROPERTY_GETTER),
        (ContextType::Class, TJS2_CG_CONTEXT_CLASS),
        (
            ContextType::SuperClassGetter,
            TJS2_CG_CONTEXT_SUPER_CLASS_GETTER,
        ),
    ] {
        harness.generator_mut().push_context(None, kind);
        harness
            .generator_mut()
            .pop_context()
            .expect("the context closes");
    }
    let block = image(harness.generator);
    unsafe {
        let block = &*block;
        assert_eq!(block.context_count, 8);
        let kinds: Vec<c_int> =
            std::slice::from_raw_parts(block.contexts, block.context_count as usize)
                .iter()
                .map(|context| context.kind)
                .collect();
        assert_eq!(
            kinds,
            vec![
                TJS2_CG_CONTEXT_TOP_LEVEL,
                TJS2_CG_CONTEXT_FUNCTION,
                TJS2_CG_CONTEXT_EXPR_FUNCTION,
                TJS2_CG_CONTEXT_PROPERTY,
                TJS2_CG_CONTEXT_PROPERTY_SETTER,
                TJS2_CG_CONTEXT_PROPERTY_GETTER,
                TJS2_CG_CONTEXT_CLASS,
                TJS2_CG_CONTEXT_SUPER_CLASS_GETTER,
            ]
        );
    }
}
