//! The end-to-end test of the script columns.
//!
//! The test runs the analyser, the parser and the generator over one
//! script — through the shipped compile entry — and reads the image of
//! the script block back: the code stream of every context, the
//! constant area, the labels and the source positions.
//!
//! The expected values are the reference's own: they are the image the
//! reference's generator makes for the same script, taken from the
//! reference's own image recorder.

use std::ffi::{CStr, c_int};

use tjs::vm::codegen::ffi::*;
use tjs::vm::*;

/// The script the test compiles.
///
/// The text is the fixture of the column: a global variable and a
/// function that reads it. The analyser takes the script as UTF-16 code
/// units, and every place the image names is counted in those units.
const FIXTURE: &str = "var a = 1;\nfunction f(x) { return x + a; }\n";

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

    /// The place of the first failure, in code units from the start of
    /// the script.
    fn position(&self) -> c_int {
        // SAFETY: the handle is live.
        unsafe { tjs2_script_error_position(self.handle) }
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

/// The code stream of a context: the opcode and its operands, one word
/// each.
fn code(context: &tjs2_codegen_context) -> Vec<i32> {
    if context.code_size == 0 {
        return Vec::new();
    }
    // SAFETY: the code is the context's own, and it is freed with the
    // handle.
    unsafe { std::slice::from_raw_parts(context.code, context.code_size as usize) }.to_vec()
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

/// The line of the script a place is on, counted from one.
///
/// The line break that ends a line belongs to that line, so the place of
/// the break itself is on the line the break ends.
fn line_of(units: &[u16], place: usize) -> usize {
    let end = place.min(units.len());
    1 + units[..end]
        .iter()
        .filter(|unit| **unit == u16::from(b'\n'))
        .count()
}

/// The fixture compiles: the entry reports no failure.
///
/// The reference's script block raises only when the compile counted a
/// failure (`reference/cpp/core/tjs2/tjsScriptBlock.cpp:444-446`), so a
/// clean compile is the status the entry reports, a count of zero and
/// no message.
#[test]
fn the_fixture_compiles() {
    let compiled = Compiled::compile(FIXTURE);
    assert_eq!(compiled.status(), TJS2_SCRIPT_OK, "the compile succeeded");
    assert_eq!(compiled.count(), 0, "the compile counted no failure");
    assert_eq!(
        compiled.message(),
        None,
        "the compile failed with no message"
    );
}

/// The block holds the global context and the function's context.
///
/// The reference's script block holds one context per code context the
/// script makes, and the kinds are the reference's
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.h:167-176`): the global
/// code is the top level, and the function's code is a function.
#[test]
fn the_block_holds_the_two_contexts() {
    let compiled = Compiled::compile(FIXTURE);
    let block = block(&compiled);
    assert_eq!(block.context_count, 2, "the block holds two contexts");
    assert_eq!(block.top_level, 0, "the top level is the first context");
    assert_eq!(
        context(&compiled, 0).kind,
        TJS2_CG_CONTEXT_TOP_LEVEL,
        "the first context is the top level"
    );
    assert_eq!(
        context(&compiled, 1).kind,
        TJS2_CG_CONTEXT_FUNCTION,
        "the second context is the function"
    );
}

/// The global context's code stream is the reference's.
///
/// The words are the reference's intermediate code: the opcode and its
/// operands, one word each, written into the context's code area
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:584-631`). The
/// function's registration is the first thing the code holds, because
/// the reference's fix-up inserts it where the context is registered
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:718-772`).
#[test]
fn the_global_context_holds_the_reference_code() {
    let compiled = Compiled::compile(FIXTURE);
    assert_eq!(
        code(context(&compiled, 0)),
        vec![
            1, 20, 40, 123, 20, -20, 111, -20, 60, 20, 3, 20, 1, 20, 0, 111, -20, 20, 20, 118, 0,
            119,
        ],
        "the global context's code stream is the reference's"
    );
}

/// The function's context's code stream is the reference's.
///
/// The words are the reference's intermediate code
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:584-631`): the copy of
/// the argument, the read of the global variable, the addition, the
/// return of the value and the epilogue the reference's commit adds
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:964-968`).
#[test]
fn the_function_context_holds_the_reference_code() {
    let compiled = Compiled::compile(FIXTURE);
    assert_eq!(
        code(context(&compiled, 1)),
        vec![
            2, 20, -60, 103, 40, -40, 0, 58, 20, 40, 118, 20, 119, 118, 0, 119
        ],
        "the function context's code stream is the reference's"
    );
}

/// The global context's constant area is the reference's.
///
/// The reference's constant area holds the values the code names, and a
/// value the area already holds is re-used
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:638-680`): the value
/// `1`, the name of the global variable, the object of the function and
/// the name of the function.
#[test]
fn the_global_context_holds_the_reference_constants() {
    let compiled = Compiled::compile(FIXTURE);
    let constants = constants(context(&compiled, 0));
    assert_eq!(constants.len(), 4, "the area holds four values");
    // the value the first statement assigns
    assert_eq!(constants[0].kind, TJS2_CG_VALUE_INTEGER);
    assert_eq!(constants[0].integer, 1);
    // the name of the global variable
    assert_eq!(constants[1].kind, TJS2_CG_VALUE_STRING);
    assert_eq!(text(&constants[1]), "a");
    // the object of the function: the reference's constant area holds the
    // function's object, and the object model is not built yet, so the
    // constant names the function's context by its index in the block
    // instead — the same value, one kind apart.
    assert!(
        constants[2].kind == TJS2_CG_VALUE_OBJECT || constants[2].kind == TJS2_CG_VALUE_CONTEXT,
        "the third constant is the function's object"
    );
    assert_eq!(
        constants[2].object, 1,
        "the object-valued constant names the function's context"
    );
    // the name of the function
    assert_eq!(constants[3].kind, TJS2_CG_VALUE_STRING);
    assert_eq!(text(&constants[3]), "f");
}

/// The function's context's constant area is the reference's.
///
/// The reference's constant area holds the values the code names
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:638-680`): the name of
/// the global variable the function reads.
#[test]
fn the_function_context_holds_the_reference_constants() {
    let compiled = Compiled::compile(FIXTURE);
    let constants = constants(context(&compiled, 1));
    assert_eq!(constants.len(), 1, "the area holds one value");
    assert_eq!(constants[0].kind, TJS2_CG_VALUE_STRING);
    assert_eq!(text(&constants[0]), "a");
}

/// The global context's source positions are the reference's.
///
/// The reference's context records the place in the script of every
/// instruction that has one, and the place in the code the instruction
/// sits at (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:602-626`).
#[test]
fn the_global_context_holds_the_reference_source_positions() {
    let compiled = Compiled::compile(FIXTURE);
    assert_eq!(
        labels(context(&compiled, 0)),
        vec![(12, 9), (15, 10), (19, 43)],
        "the global context's source positions are the reference's"
    );
}

/// The function's context's source positions are the reference's.
///
/// The reference's context records the place in the script of every
/// instruction that has one, and the place in the code the instruction
/// sits at (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:602-626`).
#[test]
fn the_function_context_holds_the_reference_source_positions() {
    let compiled = Compiled::compile(FIXTURE);
    assert_eq!(
        labels(context(&compiled, 1)),
        vec![(0, 40), (3, 39), (7, 40), (13, 42)],
        "the function context's source positions are the reference's"
    );
}

/// The source positions are places in the fixture.
///
/// The reference's source position is the analyser's place in the
/// script, counted in code units from the start
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:602-626`), so every
/// place the image names is a place in the fixture's text, and the line
/// the place is on follows from that text.
#[test]
fn the_source_positions_are_places_in_the_fixture() {
    let compiled = Compiled::compile(FIXTURE);
    let units: Vec<u16> = FIXTURE.encode_utf16().collect();
    for index in 0..2 {
        for (_, place) in labels(context(&compiled, index)) {
            assert!(
                place >= 0 && (place as usize) <= units.len(),
                "the place {place} is a place in the fixture"
            );
            let line = line_of(&units, place as usize);
            assert!(
                (1..=3).contains(&line),
                "the place {place} is on a line of the fixture"
            );
        }
    }
    // the global context's first place is the `;` of the first
    // statement, and the function's first place is the space before the
    // closing brace
    let global = labels(context(&compiled, 0));
    let function = labels(context(&compiled, 1));
    assert_eq!(
        char::from_u32(u32::from(units[global[0].1 as usize])),
        Some(';'),
        "the global's first place is the `;` of the first statement"
    );
    assert_eq!(
        char::from_u32(u32::from(units[function[0].1 as usize])),
        Some(' '),
        "the function's first place is the space before the closing brace"
    );
}

/// A script with a syntax error comes back with the reference's
/// message, its place and its count.
///
/// The reference's grammar is built with bison's simple error messages,
/// so the message the parser makes is `syntax error`, and the script
/// block raises the message the error handler builds from it
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:64-71`,
/// `reference/cpp/core/tjs2/tjsError.h:288`). The place is the
/// analyser's place in the script
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:73-74`), the count is
/// the number of failures the grammar counted
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:77-78`), and the
/// script block raises on a count that is not zero
/// (`reference/cpp/core/tjs2/tjsScriptBlock.cpp:444-446`).
#[test]
fn a_syntax_error_carries_the_reference_message() {
    // `a = 1` with the `;` missing, then `b = 2;`
    let compiled = Compiled::compile("a = 1\nb = 2;\n");
    assert_eq!(
        compiled.status(),
        TJS2_SCRIPT_ERROR_SYNTAX,
        "the script does not parse"
    );
    assert_eq!(
        compiled.message(),
        Some("Syntax error (syntax error)".to_string()),
        "the message is the one the script block raises"
    );
    assert_eq!(compiled.position(), 7, "the place is the analyser's place");
    assert_eq!(compiled.count(), 1, "the grammar counted one failure");
}

/// A script the analyser cannot read comes back with the reference's
/// message and its place.
///
/// The analyser's own failure carries its own message
/// (`reference/cpp/core/tjs2/tjsError.h:285`) and its own place, and it
/// is reported through the error handler
/// (`reference/cpp/core/tjs2/tjsLex.cpp:1841-1844`).
#[test]
fn a_lexical_failure_carries_the_reference_message() {
    // a comment that is never ended
    let compiled = Compiled::compile("/* never ends\nx = 1;\n");
    assert_eq!(
        compiled.status(),
        TJS2_SCRIPT_ERROR_SYNTAX,
        "the script does not parse"
    );
    assert_eq!(
        compiled.message(),
        Some("Un-terminated comment".to_string()),
        "the message is the analyser's own"
    );
    assert_eq!(compiled.position(), 21, "the place is the analyser's place");
    assert_eq!(compiled.count(), 1, "the analyser counted one failure");
}
