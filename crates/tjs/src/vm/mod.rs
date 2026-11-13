//! The Rust VM: the script columns that replace the vendored ones.
//!
//! The script language is the TJS2 dialect the games are written in. This
//! module holds its front end in Rust: the lexical analyser that turns
//! script text into tokens, the parser that turns the tokens into an
//! expression tree, and the code generator that turns the tree into the
//! intermediate code the VM executes.
//!
//! These columns replace the vendored tree beside them, in
//! `crates/tjs/cpp/tjs2/`: the analyser, the grammar and the
//! generator there are what this module is written against. The C
//! interface the vendored tree calls during the replacement is the
//! compile entry at the root of this module; the image it hands back
//! has its shape in `codegen::ffi`.
//!
//! The root of this module is the front end's own door: the compile entry,
//! the shipped seam the vendored VM calls to turn script text into the image
//! of a script block. The entry belongs here because it is where one enters
//! the front end — it drives the analyser, the parser and the generator, and
//! it is what the other side of the seam calls.
//!
//! The handle the entry returns is returned **even when the script does not
//! compile**: the error path needs the message, the position and the count
//! after the front end has failed.
//!
//! The conditional-compilation values go in and come back out, because they
//! outlive a script: a value one script sets is visible in the next one.
//!
//! # The unsafe code
//!
//! This module allows `unsafe` because the compile entry is the seam, and
//! the seam is where the raw pointers live. Every other module of the front
//! end denies it; the image module allows it for the one C string it cuts.

#![allow(unsafe_code)]

pub mod codegen;
pub mod commands;
pub mod lexer;
pub mod parser;
mod stats;

pub use codegen::Generator;
pub use lexer::{LexError, Lexer, PpValues, Token, TokenKind, ValueRef};
pub use parser::{Commands, ParseError, Parsed, Parser};
pub use stats::{CompileStats, compile_stats, compile_stats_enabled, set_compile_stats_enabled};

// ---------------------------------------------------------------------------
// The compile entry: the script text in, the image of the script block out
// ---------------------------------------------------------------------------

use std::ffi::{CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::vm::codegen::ffi::{
    Image, c_string, panic_message, tjs2_codegen_block, tjs2_pp_value, tjs2_pp_values,
};

/// The compile succeeded.
pub const TJS2_SCRIPT_OK: c_int = 0;
/// The script does not parse.
pub const TJS2_SCRIPT_ERROR_SYNTAX: c_int = -1;
/// The front end panicked.
pub const TJS2_SCRIPT_ERROR_PANIC: c_int = -2;
/// A value is out of range.
pub const TJS2_SCRIPT_ERROR_RANGE: c_int = -3;
/// The handle is in a wrong state.
pub const TJS2_SCRIPT_ERROR_STATE: c_int = -4;

/// A compiled script block: the handle owns the image.
///
/// The name is the C name the header uses, so the two agree.
#[allow(non_camel_case_types)]
pub struct tjs2_script {
    /// The image of the script block.
    image: Box<Image>,
    /// The status of the compile.
    status: c_int,
    /// The message of the first failure.
    message: Option<CString>,
    /// The position of the first failure.
    position: c_int,
    /// The number of failures.
    count: c_int,
    /// Whether the script used a conditional-compilation statement.
    using_preprocessor: c_int,
    /// The conditional-compilation values the compile left behind.
    values: Vec<tjs2_pp_value>,
    /// The names the values point at.
    names: Vec<Vec<u16>>,
    /// The values, in the interface's shape.
    pp_values: tjs2_pp_values,
}

impl Drop for tjs2_script {
    fn drop(&mut self) {
        // the values' names point into the name buffers, so the names go
        // first
        self.names.clear();
        self.values.clear();
    }
}

/// Runs the given work, and turns a panic into the failure it carries.
fn guard<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(value) => Ok(value),
        Err(payload) => Err(panic_message(payload)),
    }
}

/// The handle of a front end that panicked.
///
/// The panic happened before the handle could be made, so this one is
/// made here: it carries the failure, and the caller reads the panic's
/// text with `tjs2_script_error_message` instead of losing it.
fn failed(message: &str) -> *mut tjs2_script {
    Box::into_raw(Box::new(tjs2_script {
        image: Box::new(Image::default()),
        status: TJS2_SCRIPT_ERROR_PANIC,
        message: Some(c_string(message)),
        position: -1,
        count: 0,
        using_preprocessor: 0,
        values: Vec::new(),
        names: Vec::new(),
        pp_values: tjs2_pp_values {
            values: std::ptr::null(),
            count: 0,
        },
    }))
}

/// Compiles script text into the image of a script block.
///
/// The text is UTF-16 code units, and every source
/// position and every string constant is counted in code units.
///
/// The handle is returned even when the script does not compile; the
/// failure is then read with `tjs2_script_status` and the
/// `tjs2_script_error_*` calls.
///
/// # Safety
///
/// `script` must be readable for `length` code units, and `values` must be
/// readable when it is not null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_script_compile(
    script: *const u16,
    length: u32,
    expression: c_int,
    result_needed: c_int,
    values: *const tjs2_pp_values,
) -> *mut tjs2_script {
    match guard(|| {
        if script.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: the caller promises the text is readable.
        let units = unsafe { std::slice::from_raw_parts(script, length as usize) };

        // The front end's own measurement: it is off by default, and a
        // compile that is not measured keeps no clock.
        let measurement = stats::Measurement::start();

        let mut lexer = if expression != 0 {
            Lexer::expression(units, result_needed != 0)
        } else {
            Lexer::new(units)
        };

        // the conditional-compilation values the script reads
        if !values.is_null() {
            // SAFETY: the caller promises the values are readable.
            let values = unsafe { &*values };
            if !values.values.is_null() {
                let given = unsafe {
                    // SAFETY: the caller promises the values are readable.
                    std::slice::from_raw_parts(values.values, values.count as usize)
                };
                for value in given {
                    if value.name.is_null() {
                        continue;
                    }
                    // SAFETY: the caller promises the name is readable.
                    let units = unsafe {
                        std::slice::from_raw_parts(value.name, value.name_length as usize)
                    };
                    let name = String::from_utf16_lossy(units);
                    lexer
                        .conditional_values_mut()
                        .set(&name, value.integer as i32);
                }
            }
        }

        let mut generator = Generator::new();
        // The parse window holds the lexer, the parser and the generator: the
        // two timed columns write their own times into the totals, and what
        // is left of the window is the parser's own time. The generator's
        // work is measured at the seam the parser drives it through.
        let (parsed, parse_ns) = measurement
            .timed(|| Parser::new(&mut lexer, stats::Timed::new(&mut generator)).parse());

        // the image of the script block
        let mut image = Box::new(Image::default());
        image.generator = generator;
        // the block's line table
        let (built, image_ns) = measurement.timed(|| {
            image.lines = crate::vm::codegen::ffi::line_table(units);
            crate::vm::codegen::ffi::build_image(&mut image)
        });

        // the first failure and the count of the failures
        let (status, message, position, count) = match (parsed.first_error, built) {
            (Some(error), _) => (
                TJS2_SCRIPT_ERROR_SYNTAX,
                Some(c_string(error.message())),
                error.position(),
                parsed.error_count as c_int,
            ),
            (None, Err(panic)) => (
                TJS2_SCRIPT_ERROR_PANIC,
                Some(c_string(&panic)),
                -1,
                parsed.error_count as c_int,
            ),
            (None, Ok(())) => (TJS2_SCRIPT_OK, None, -1, parsed.error_count as c_int),
        };

        // the conditional-compilation values the script left behind
        let mut names: Vec<Vec<u16>> = Vec::new();
        let mut out: Vec<tjs2_pp_value> = Vec::new();
        for (name, value) in lexer.conditional_values().iter() {
            let units: Vec<u16> = name.encode_utf16().collect();
            let length = units.len() as c_int;
            names.push(units);
            out.push(tjs2_pp_value {
                name: std::ptr::null(),
                name_length: length,
                integer: value as i64,
            });
        }
        let name_base = names.len() - out.len();
        for (index, value) in out.iter_mut().enumerate() {
            value.name = names[name_base + index].as_ptr();
        }

        let handle = Box::new(tjs2_script {
            image,
            status,
            message,
            position,
            count,
            using_preprocessor: c_int::from(lexer.using_preprocessor()),
            values: out,
            names,
            pp_values: tjs2_pp_values {
                values: std::ptr::null(),
                count: 0,
            },
        });
        let handle = Box::into_raw(handle);
        // SAFETY: the handle is live, and the values' buffer does not move.
        let handle = unsafe { &mut *handle };
        handle.pp_values = tjs2_pp_values {
            values: handle.values.as_ptr(),
            count: handle.values.len() as c_int,
        };
        // The whole compile's time, and the phases it holds.
        let total_ns = measurement.elapsed_ns();
        measurement.finish(total_ns, parse_ns, image_ns, length as u64);
        handle
    }) {
        Ok(handle) => handle,
        // the front end panicked: the handle carries the failure, so the
        // panic's text is not lost
        Err(panic) => failed(&panic),
    }
}

/// Frees the handle and every buffer of the image it owns.
///
/// # Safety
///
/// `script` must be a handle from `tjs2_script_compile` that has not been
/// freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_script_free(script: *mut tjs2_script) {
    if script.is_null() {
        return;
    }
    // SAFETY: the caller promises the handle is live and not freed.
    drop(unsafe { Box::from_raw(script) });
}

/// The status of the compile: `TJS2_SCRIPT_OK` or a negative code.
///
/// # Safety
///
/// `script` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_script_status(script: *const tjs2_script) -> c_int {
    // SAFETY: the caller promises the handle is live.
    unsafe { script.as_ref() }
        .map(|script| script.status)
        .unwrap_or(TJS2_SCRIPT_ERROR_PANIC)
}

/// The message of the first failure; owned by the handle.
///
/// # Safety
///
/// `script` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_script_error_message(script: *const tjs2_script) -> *const c_char {
    // SAFETY: the caller promises the handle is live.
    let Some(script) = (unsafe { script.as_ref() }) else {
        return std::ptr::null();
    };
    script
        .message
        .as_ref()
        .map(|message| message.as_ptr())
        .unwrap_or(std::ptr::null())
}

/// The source position of the first failure, in code units from the start
/// of the script.
///
/// # Safety
///
/// `script` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_script_error_position(script: *const tjs2_script) -> c_int {
    // SAFETY: the caller promises the handle is live.
    unsafe { script.as_ref() }
        .map(|script| script.position)
        .unwrap_or(-1)
}

/// The number of failures the front end counted.
///
/// # Safety
///
/// `script` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_script_error_count(script: *const tjs2_script) -> c_int {
    // SAFETY: the caller promises the handle is live.
    unsafe { script.as_ref() }
        .map(|script| script.count)
        .unwrap_or(0)
}

/// Whether the script used a conditional-compilation statement.
///
/// # Safety
///
/// `script` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_script_using_preprocessor(script: *const tjs2_script) -> c_int {
    // SAFETY: the caller promises the handle is live.
    unsafe { script.as_ref() }
        .map(|script| script.using_preprocessor)
        .unwrap_or(0)
}

/// The image of the script block; owned by the handle.
///
/// # Safety
///
/// `script` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_script_block_image(
    script: *const tjs2_script,
) -> *const tjs2_codegen_block {
    // SAFETY: the caller promises the handle is live.
    let Some(script) = (unsafe { script.as_ref() }) else {
        return std::ptr::null();
    };
    script
        .image
        .block
        .as_ref()
        .map(|block| block as *const tjs2_codegen_block)
        .unwrap_or(std::ptr::null())
}

/// The conditional-compilation values the compile left behind; owned by the
/// handle.
///
/// # Safety
///
/// `script` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_script_pp_values(
    script: *const tjs2_script,
) -> *const tjs2_pp_values {
    // SAFETY: the caller promises the handle is live.
    let Some(script) = (unsafe { script.as_ref() }) else {
        return std::ptr::null();
    };
    &script.pp_values as *const tjs2_pp_values
}

#[cfg(test)]
mod tests {
    use std::ffi::CStr;

    use super::*;

    /// A panic in the front end is a failure the caller can read: the
    /// handle carries the panic's text.
    ///
    /// The reference raises on a failure of the compile
    /// (`reference/cpp/core/tjs2/tjsScriptBlock.cpp:444-446`), and the
    /// message it raises with is the first failure's
    /// (`reference/cpp/core/tjs2/tjsError.h:288`).
    #[test]
    fn a_panic_is_an_error_value() {
        let message = guard(|| -> () { panic!("the front end panicked") }).unwrap_err();
        assert_eq!(message, "the front end panicked");
        let handle = failed(&message);
        // SAFETY: the handle is live and not freed.
        unsafe {
            assert_eq!(tjs2_script_status(handle), TJS2_SCRIPT_ERROR_PANIC);
            let text = tjs2_script_error_message(handle);
            assert!(!text.is_null(), "the handle carries the message");
            assert_eq!(
                CStr::from_ptr(text).to_string_lossy(),
                "the front end panicked"
            );
            tjs2_script_free(handle);
        }
    }

    /// The front end's measurement is off until it is switched on, and a
    /// measured compile lands in the totals with its phases.
    ///
    /// The measurement is the front end's own account of where a compile's
    /// time went: the analyser times its tokens, the generator times the
    /// commands the parser drives it with, and the parser's own time is what
    /// the parse window holds besides the two.
    #[test]
    fn a_measured_compile_lands_in_the_totals() {
        assert!(
            !compile_stats_enabled(),
            "the measurement is off until a run asks for it"
        );
        let script = "var a = 1;\nfunction f(x) { return x + a; }\n";
        let units: Vec<u16> = script.encode_utf16().collect();
        let before = compile_stats();
        set_compile_stats_enabled(true);
        // SAFETY: the units are readable for the length the call takes, and
        // the null values pointer means "no conditional-compilation values".
        let handle = unsafe {
            tjs2_script_compile(units.as_ptr(), units.len() as u32, 0, 0, std::ptr::null())
        };
        assert!(!handle.is_null(), "the entry returned a handle");
        // SAFETY: the handle is live and not freed.
        unsafe {
            tjs2_script_free(handle);
        }
        let after = compile_stats();
        set_compile_stats_enabled(false);

        assert!(
            after.compiles > before.compiles,
            "the compile was counted: {} -> {}",
            before.compiles,
            after.compiles
        );
        assert!(
            after.code_units >= before.code_units + units.len() as u64,
            "the compile read its script: {} -> {}",
            before.code_units,
            after.code_units
        );
        assert!(
            after.bytes >= before.bytes + units.len() as u64 * 2,
            "the compile read its script's bytes: {} -> {}",
            before.bytes,
            after.bytes
        );
        assert!(
            after.total_ms > before.total_ms,
            "the compile was timed: {} -> {}",
            before.total_ms,
            after.total_ms
        );
        assert!(
            after.lexer_ms > before.lexer_ms,
            "the analyser timed its tokens: {} -> {}",
            before.lexer_ms,
            after.lexer_ms
        );
        let phases = after.lexer_ms + after.parser_ms + after.generator_ms + after.image_ms;
        assert!(
            after.total_ms + 1.0 >= phases,
            "the phases fit in the compile: {} against {}",
            after.total_ms,
            phases
        );
    }
}
