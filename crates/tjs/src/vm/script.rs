//! The compile entry: the script text in, the image of the script block
//! out.
//!
//! This is the shipped seam. The entry compiles the text with the Rust
//! front end — the analyser, the parser and the generator — and hands
//! back the image of the script block; the caller copies out of the
//! image and frees the handle.
//!
//! The handle is returned **even when the script does not compile**: the
//! error path needs the message, the position and the count after the
//! front end has failed.
//!
//! The conditional-compilation values go in and come back out, because
//! they outlive a script: a value one script sets is visible in the next
//! one.

#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::vm::codegen::Generator;
use crate::vm::codegen::ffi::{
    TJS2_CG_ERROR_PANIC, c_string, tjs2_codegen, tjs2_codegen_block, tjs2_pp_value, tjs2_pp_values,
};
use crate::vm::lexer::Lexer;
use crate::vm::parser::Parser;

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
    /// The generator, which owns the image.
    codegen: Box<tjs2_codegen>,
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

thread_local! {
    /// The message of the last panic, for the case where the handle could
    /// not be told about it.
    static PANIC_MESSAGE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

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
                "the front end panicked".to_string()
            };
            // the message is kept in a thread-local so that the caller
            // can read it after the fact
            PANIC_MESSAGE.with(|slot| *slot.borrow_mut() = Some(message));
            fallback
        }
    }
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
    guard(std::ptr::null_mut(), || {
        if script.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: the caller promises the text is readable.
        let units = unsafe { std::slice::from_raw_parts(script, length as usize) };

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
        let parsed = Parser::new(&mut lexer, &mut generator).parse();

        // the image of the script block
        let mut codegen = Box::new(tjs2_codegen::default());
        codegen.generator = generator;
        // the block's line table
        codegen.lines = crate::vm::codegen::ffi::line_table(units);
        let image = crate::vm::codegen::ffi::build_image(&mut codegen);
        if image != TJS2_CG_ERROR_PANIC {
            codegen.error = image;
        }

        // the first failure and the count of the failures
        let (status, message, position, count) = match parsed.first_error {
            Some(error) => (
                TJS2_SCRIPT_ERROR_SYNTAX,
                Some(c_string(error.message())),
                error.position(),
                parsed.error_count as c_int,
            ),
            None => (TJS2_SCRIPT_OK, None, -1, parsed.error_count as c_int),
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
            codegen,
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
        handle
    })
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
        .codegen
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
