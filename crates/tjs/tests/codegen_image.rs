//! The image of a script block: the areas the native side reads.
//!
//! The image is the shipped interface: the compile entry makes it, and
//! the C++ side reads it. The tests below read it back the way the C++
//! side reads it — the code stream, the constant area, the variable
//! area, the function-argument area, the labels and the line table.
//!
//! The reference's own script block is the oracle for every value read
//! here, and every assertion below cites the line that defines it.

use std::ffi::{CStr, c_int};

use tjs::vm::codegen::ffi::*;
use tjs::vm::*;

/// A compiled script: the handle owns the image.
struct Compiled {
    /// The handle the entry returned.
    handle: *mut tjs2_script,
}

impl Compiled {
    /// Compiles the text; the analyser takes it as UTF-16 code units.
    fn compile(text: &str) -> Self {
        let units: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: the units are readable for the length the call takes,
        // and the null values pointer means "no conditional-compilation
        // values".
        let handle = unsafe {
            tjs2_script_compile(units.as_ptr(), units.len() as u32, 0, 0, std::ptr::null())
        };
        assert!(!handle.is_null(), "the entry returned a handle");
        Self { handle }
    }

    /// The status of the compile.
    fn status(&self) -> c_int {
        // SAFETY: the handle is live.
        unsafe { tjs2_script_status(self.handle) }
    }

    /// The message of the first failure.
    fn message(&self) -> Option<String> {
        // SAFETY: the handle is live.
        let message = unsafe { tjs2_script_error_message(self.handle) };
        if message.is_null() {
            return None;
        }
        // SAFETY: the message is the handle's own C string.
        Some(
            unsafe { CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// The number of failures the front end counted.
    fn count(&self) -> c_int {
        // SAFETY: the handle is live.
        unsafe { tjs2_script_error_count(self.handle) }
    }
}

impl Drop for Compiled {
    fn drop(&mut self) {
        // SAFETY: the handle is live and not freed.
        unsafe { tjs2_script_free(self.handle) };
    }
}

/// The image of the script block.
///
/// The image belongs to the handle, so it lives as long as the handle
/// does.
fn block(compiled: &Compiled) -> &tjs2_codegen_block {
    // SAFETY: the handle is live.
    let block = unsafe { tjs2_script_block_image(compiled.handle) };
    assert!(!block.is_null(), "the entry built the image");
    // SAFETY: the block is the handle's own image, and it is freed with
    // the handle.
    unsafe { &*block }
}

/// The image of one context of the block.
fn context(compiled: &Compiled, index: usize) -> &tjs2_codegen_context {
    let block = block(compiled);
    assert!(
        (index as i32) < block.context_count,
        "the block holds the context"
    );
    // SAFETY: the context is one of the block's own, and it is freed
    // with the handle.
    unsafe { &*block.contexts.add(index) }
}

/// The constant area of a context.
fn constants(context: &tjs2_codegen_context) -> &[tjs2_codegen_value] {
    if context.constant_count == 0 {
        return &[];
    }
    // SAFETY: the constants are the context's own, and they are freed
    // with the handle.
    unsafe { std::slice::from_raw_parts(context.constants, context.constant_count as usize) }
}

/// The variable area of a context: the register every slot holds.
fn registers(context: &tjs2_codegen_context) -> Vec<c_int> {
    if context.variable_count == 0 {
        return Vec::new();
    }
    // SAFETY: the variables are the context's own, and they are freed
    // with the handle.
    unsafe {
        std::slice::from_raw_parts(context.variables, context.variable_count as usize)
            .iter()
            .map(|variable| variable.register_)
            .collect()
    }
}

/// The function-argument area of a context: the register every argument
/// holds.
fn argument_registers(context: &tjs2_codegen_context) -> Vec<c_int> {
    if context.func_arg_count == 0 {
        return Vec::new();
    }
    // SAFETY: the arguments are the context's own, and they are freed
    // with the handle.
    unsafe {
        std::slice::from_raw_parts(context.func_args, context.func_arg_count as usize)
            .iter()
            .map(|argument| argument.register_)
            .collect()
    }
}

/// The labels of a context: the place in the code and the place in the
/// script.
fn labels(context: &tjs2_codegen_context) -> Vec<(i32, i32)> {
    if context.label_count == 0 {
        return Vec::new();
    }
    // SAFETY: the labels are the context's own, and they are freed with
    // the handle.
    unsafe {
        std::slice::from_raw_parts(context.labels, context.label_count as usize)
            .iter()
            .map(|label| (label.code_pos, label.source_pos))
            .collect()
    }
}

/// The text of a string constant.
fn text(constant: &tjs2_codegen_value) -> String {
    // SAFETY: the string is the constant's own, and it is freed with the
    // handle.
    let units =
        unsafe { std::slice::from_raw_parts(constant.string, constant.string_length as usize) };
    String::from_utf16_lossy(units)
}

/// The bytes of a byte-string constant.
fn bytes(constant: &tjs2_codegen_value) -> Vec<u8> {
    // SAFETY: the bytes are the constant's own, and they are freed with
    // the handle.
    unsafe { std::slice::from_raw_parts(constant.octet, constant.octet_length as usize) }.to_vec()
}

/// The image's string area is read with the constant's own place, so a
/// constant that is not a string does not shift the ones that are.
///
/// The reference's constant area holds the values the code names, in
/// creation order, and a value the area already holds is re-used
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:638-681`).
#[test]
fn the_image_reads_a_string_after_an_integer() {
    let compiled = Compiled::compile("var a = 1;\nvar b = \"hello\";\n");
    let constants = constants(context(&compiled, 0));
    assert_eq!(constants.len(), 4, "the area holds four values");
    // the value the first statement assigns
    assert_eq!(constants[0].kind, TJS2_CG_VALUE_INTEGER);
    assert_eq!(constants[0].integer, 1);
    // the name of the first variable
    assert_eq!(constants[1].kind, TJS2_CG_VALUE_STRING);
    assert_eq!(text(&constants[1]), "a");
    // the value the second statement assigns: the string is where the
    // integer did not shift it
    assert_eq!(constants[2].kind, TJS2_CG_VALUE_STRING);
    assert_eq!(text(&constants[2]), "hello");
    // the name of the second variable
    assert_eq!(constants[3].kind, TJS2_CG_VALUE_STRING);
    assert_eq!(text(&constants[3]), "b");
}

/// The same, with two strings and an integer between them.
///
/// The reference's constant area is one area for the whole context, and
/// every value has its own place in it
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:638-681`).
#[test]
fn the_image_reads_the_strings_where_they_are() {
    let compiled = Compiled::compile("var a = \"first\"; var b = 3; var c = \"second\";");
    let constants = constants(context(&compiled, 0));
    assert_eq!(constants.len(), 6, "the area holds six values");
    let read = |index: usize| {
        let constant = &constants[index];
        assert_eq!(constant.kind, TJS2_CG_VALUE_STRING);
        text(constant)
    };
    assert_eq!(read(0), "first");
    assert_eq!(constants[2].kind, TJS2_CG_VALUE_INTEGER);
    assert_eq!(constants[2].integer, 3);
    assert_eq!(read(4), "second");
}

/// The image holds the byte strings the script names.
///
/// The reference's constant area holds a byte string as a variant of its
/// own kind (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:638-681`).
#[test]
fn the_image_reads_a_byte_string() {
    let compiled = Compiled::compile("var a = <%41 42 43%>;");
    let constants = constants(context(&compiled, 0));
    assert_eq!(constants.len(), 2, "the area holds two values");
    assert_eq!(constants[0].kind, TJS2_CG_VALUE_OCTET);
    assert_eq!(constants[0].octet_length, 3);
    assert_eq!(bytes(&constants[0]), &[0x41, 0x42, 0x43]);
}

/// The image holds the code stream of the context.
///
/// The reference's context holds the intermediate code, the opcode and
/// its operands one word each
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:584-628`), and the
/// epilogue its commit adds is the return of the value and the return
/// itself (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:964-968`).
#[test]
fn the_image_holds_the_code_stream() {
    let compiled = Compiled::compile("");
    let context = context(&compiled, 0);
    assert_eq!(context.kind, TJS2_CG_CONTEXT_TOP_LEVEL);
    assert_eq!(context.code_size, 3, "the context holds three words");
    assert!(!context.code.is_null());
    // SAFETY: the code is the context's own, and it is freed with the
    // handle.
    let code = unsafe { std::slice::from_raw_parts(context.code, context.code_size as usize) };
    assert_eq!(code, [118, 0, 119], "the code is the epilogue");
}

/// The image holds the labels: the places in the script the code names.
///
/// The reference's context records the place in the script of every
/// instruction that has one, and the place in the code the instruction
/// sits at (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:602-627`).
#[test]
fn the_image_holds_the_labels() {
    let compiled = Compiled::compile("var a = 1;\n");
    let labels = labels(context(&compiled, 0));
    assert_eq!(labels.len(), 3, "the context holds three labels");
    // the place in the code and the place in the script
    assert_eq!(labels[0], (0, 9));
    assert_eq!(labels[1], (3, 10));
    // the epilogue the commit adds carries the place the analyser was at
    assert_eq!(labels[2], (7, 11));
}

/// The image holds the line table of the block.
///
/// The reference's block splits its text on the line breaks, and every
/// line contributes its start offset and its length in code units,
/// without the break (`reference/cpp/core/tjs2/tjsScriptBlock.cpp:207-232`).
#[test]
fn the_image_holds_the_line_table() {
    let compiled = Compiled::compile("var a = 1;\nvar b = 2;\n");
    let block = block(&compiled);
    assert_eq!(block.line_count, 2, "the block holds two lines");
    // SAFETY: the lines are the block's own, and they are freed with the
    // handle.
    let lines = unsafe { std::slice::from_raw_parts(block.lines, block.line_count as usize) };
    assert_eq!(lines[0].position, 0);
    assert_eq!(lines[0].length, 10);
    assert_eq!(lines[1].position, 11);
    assert_eq!(lines[1].length, 10);
}

/// The image holds the variable area and the function-argument area of a
/// context.
///
/// The reference's context holds the name space's slots in the frame, and
/// the declared arguments in the registers below them
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:3708-3737`,
/// `reference/cpp/core/tjs2/tjsNamespace.cpp:156-170`).
#[test]
fn the_image_holds_the_variable_and_the_argument_areas() {
    let compiled = Compiled::compile("function f(x) { var y = 1; return x; }");
    let context = context(&compiled, 1);
    assert_eq!(context.kind, TJS2_CG_CONTEXT_FUNCTION);
    // the frame holds the argument's slot and the local variable's
    assert_eq!(registers(context), vec![-3, -4]);
    // the argument holds the first slot of the frame
    assert_eq!(argument_registers(context), vec![-3]);
}

/// The failure of a script that does not compile is a value with a
/// message.
///
/// The reference's block records the first failure and the number of
/// failures it recovered from, and raises on the first one
/// (`reference/cpp/core/tjs2/tjsScriptBlock.cpp:444-446`); its message
/// for a syntax failure is `"Syntax error (%1)"`
/// (`reference/cpp/core/tjs2/tjsError.h:288`).
#[test]
fn the_error_is_a_value_with_a_message() {
    let compiled = Compiled::compile("var a = 1");
    assert_eq!(compiled.status(), TJS2_SCRIPT_ERROR_SYNTAX);
    assert_eq!(compiled.count(), 1, "the compile counted one failure");
    assert_eq!(
        compiled.message(),
        Some("Syntax error (syntax error)".to_string()),
        "the message is the reference's"
    );
}

/// The context kinds are the reference's numbers.
///
/// The reference's context type names the eight kinds the grammar makes
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.h:166-175`), and the image
/// carries the kind the C++ side puts in the context's `ContextType`.
#[test]
fn the_context_kinds_are_the_interface_numbers() {
    let compiled = Compiled::compile(
        "function f() {}\nvar g = function() {};\nproperty p {\n setter (s) {}\n getter () {}\n}\nclass C extends B {}\n",
    );
    let block = block(&compiled);
    assert_eq!(block.context_count, 8, "the block holds eight contexts");
    let kinds: Vec<c_int> = (0..block.context_count)
        .map(|index| context(&compiled, index as usize).kind)
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
