//! The C interface of the lexical analyser.
//!
//! The interface is what the vendored parser reaches the analyser
//! through: a plain C representation, no Rust types across the boundary.
//! The state of one analyser is an opaque handle, a token kind is the
//! grammar's own number, and a value is delivered in the form the native
//! side already uses — an integer, a real, a string of code units, a buffer of
//! bytes, or the null object.
//!
//! Every entry point catches a panic and reports it as an error, so a
//! failure never unwinds into the caller.
//!
//! The interface is safe to call from several threads as long as one handle
//! is used by one thread at a time.

#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::lexer::{LexError, Lexer, Literal, TokenKind};

/// The analyser read the script to its end.
pub const TJS2_LEX_OK: c_int = 0;

/// A string, a regular expression or a binary literal was not terminated.
pub const TJS2_LEX_ERROR_LITERAL: c_int = 1;

/// A number could not be parsed.
pub const TJS2_LEX_ERROR_NUMBER: c_int = 2;

/// A comment was not terminated.
pub const TJS2_LEX_ERROR_COMMENT: c_int = 3;

/// A character cannot begin a token.
pub const TJS2_LEX_ERROR_CHARACTER: c_int = 4;

/// A conditional-compilation statement or expression is malformed.
pub const TJS2_LEX_ERROR_CONDITIONAL: c_int = 5;

/// The analyser panicked. The message carries the panic's text.
pub const TJS2_LEX_ERROR_PANIC: c_int = 6;

/// The token carries no value.
pub const TJS2_LEX_VALUE_NONE: c_int = 0;

/// The token carries the null object.
pub const TJS2_LEX_VALUE_NULL: c_int = 1;

/// The token carries an integer.
pub const TJS2_LEX_VALUE_INTEGER: c_int = 2;

/// The token carries a real.
pub const TJS2_LEX_VALUE_REAL: c_int = 3;

/// The token carries a string of code units.
pub const TJS2_LEX_VALUE_STRING: c_int = 4;

/// The token carries a buffer of bytes.
pub const TJS2_LEX_VALUE_OCTET: c_int = 5;

/// The text of a failure as a C string.
///
/// A C string cannot hold a zero byte, so the text is cut at the first
/// one: the reason a failure carries is at the beginning of its text, and
/// the cut is a transformation the caller can see.
fn c_string(text: &str) -> CString {
    let end = text.find('\0').unwrap_or(text.len());
    let mut bytes = text.as_bytes()[..end].to_vec();
    bytes.push(0);
    // SAFETY: the bytes hold no zero byte before the one just added, so
    // the buffer is a C string.
    unsafe { CString::from_vec_unchecked(bytes) }
}

/// The state of one analyser.
///
/// The handle is opaque to the native side: it is created by
/// [`tjs2_lexer_new`] and freed by [`tjs2_lexer_free`].
#[allow(non_camel_case_types)]
pub struct tjs2_lexer {
    /// The analyser, when it could be made.
    lexer: Option<Lexer>,
    /// The error of the last read, or [`TJS2_LEX_OK`].
    error: c_int,
    /// The text of the error of the last read.
    message: CString,
    /// The kind of the value of the last token.
    value_kind: c_int,
    /// The integer of the last token.
    value_integer: i64,
    /// The real of the last token.
    value_real: f64,
    /// The code units of the last token.
    value_string: Vec<u16>,
    /// The bytes of the last token.
    value_octet: Vec<u8>,
}

impl tjs2_lexer {
    /// The error code of a failure.
    fn error_code(error: &LexError) -> c_int {
        match error {
            LexError::UnterminatedLiteral { .. } => TJS2_LEX_ERROR_LITERAL,
            LexError::Number { .. } => TJS2_LEX_ERROR_NUMBER,
            LexError::UnterminatedComment { .. } => TJS2_LEX_ERROR_COMMENT,
            LexError::InvalidCharacter { .. } => TJS2_LEX_ERROR_CHARACTER,
            LexError::ConditionalCompilation { .. } => TJS2_LEX_ERROR_CONDITIONAL,
        }
    }

    /// The text of a failure.
    fn error_message(error: &LexError) -> CString {
        c_string(&error.to_string())
    }

    /// The text of a panic.
    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> CString {
        let text = payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "the analyser panicked".to_string());
        c_string(&text)
    }

    /// Record the value of the last token.
    fn set_value(&mut self, value: Option<&Literal>) {
        self.value_kind = TJS2_LEX_VALUE_NONE;
        self.value_integer = 0;
        self.value_real = 0.0;
        self.value_string.clear();
        self.value_octet.clear();
        let Some(value) = value else {
            return;
        };
        match value {
            Literal::Null => self.value_kind = TJS2_LEX_VALUE_NULL,
            Literal::Integer(number) => {
                self.value_kind = TJS2_LEX_VALUE_INTEGER;
                self.value_integer = *number;
            }
            Literal::Real(number) => {
                self.value_kind = TJS2_LEX_VALUE_REAL;
                self.value_real = *number;
            }
            Literal::String(string) => {
                self.value_kind = TJS2_LEX_VALUE_STRING;
                self.value_string.extend_from_slice(string.units());
            }
            Literal::Octet(bytes) => {
                self.value_kind = TJS2_LEX_VALUE_OCTET;
                self.value_octet.extend_from_slice(bytes);
            }
        }
    }
}

/// Create an analyser for a script.
///
/// The script is a sequence of UTF-16 code units; `length` is their
/// number. The analyser copies the text, so the caller may free it as soon
/// as this call returns.
///
/// `expression_mode` reads the script as an expression and `result_needed`
/// asks for the token that returns the expression's value; the second is
/// only read in the expression mode.
///
/// The returned handle belongs to the caller and must be freed with
/// [`tjs2_lexer_free`]. A null pointer means the allocation failed.
///
/// # Safety
///
/// `script` must point at `length` readable code units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_new(
    script: *const u16,
    length: u32,
    expression_mode: c_int,
    result_needed: c_int,
) -> *mut tjs2_lexer {
    let mut handle = Box::new(tjs2_lexer {
        lexer: None,
        error: TJS2_LEX_OK,
        message: CString::default(),
        value_kind: TJS2_LEX_VALUE_NONE,
        value_integer: 0,
        value_real: 0.0,
        value_string: Vec::new(),
        value_octet: Vec::new(),
    });
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the caller promises `length` readable code units.
        let units: Vec<u16> = if script.is_null() || length == 0 {
            Vec::new()
        } else {
            // SAFETY: as above.
            let units = unsafe { std::slice::from_raw_parts(script, length as usize) };
            units.to_vec()
        };
        handle.lexer = Some(if expression_mode != 0 {
            Lexer::expression(&units, result_needed != 0)
        } else {
            Lexer::new(&units)
        });
    }));
    if let Err(payload) = result {
        // A panic must not unwind into the caller. The text is kept in the
        // handle, so the caller reads it with the error functions.
        handle.error = TJS2_LEX_ERROR_PANIC;
        handle.message = tjs2_lexer::panic_message(payload);
    }
    Box::into_raw(handle)
}

/// Free an analyser.
///
/// The handle must not be used afterwards. A null pointer is ignored.
///
/// # Safety
///
/// `lexer` must be a handle from [`tjs2_lexer_new`] that was not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_free(lexer: *mut tjs2_lexer) {
    if lexer.is_null() {
        return;
    }
    // SAFETY: the caller promises the handle is live and not freed.
    drop(unsafe { Box::from_raw(lexer) });
}

/// Read the next token.
///
/// The return value is the token's kind as the grammar numbers it, zero
/// when the script has ended, or a negative value when the analyser
/// failed. After a failure the error code is [`tjs2_lexer_error`] and the
/// text is [`tjs2_lexer_error_message`].
///
/// The value of the token is read with the `tjs2_lexer_value_*` functions.
///
/// # Safety
///
/// `lexer` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_next(lexer: *mut tjs2_lexer) -> c_int {
    // SAFETY: the caller promises the handle is live and not freed.
    let Some(lexer) = (unsafe { lexer.as_mut() }) else {
        return TJS2_LEX_ERROR_PANIC;
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(analyser) = lexer.lexer.as_mut() else {
            lexer.error = TJS2_LEX_ERROR_PANIC;
            return -1;
        };
        match analyser.next_token() {
            Ok(Some(token)) => {
                lexer.error = TJS2_LEX_OK;
                lexer.set_value(token.value.as_ref());
                token.kind as c_int
            }
            Ok(None) => {
                lexer.error = TJS2_LEX_OK;
                lexer.set_value(None);
                0
            }
            Err(error) => {
                lexer.error = tjs2_lexer::error_code(&error);
                lexer.message = tjs2_lexer::error_message(&error);
                lexer.set_value(None);
                -1
            }
        }
    }));
    match result {
        Ok(kind) => kind,
        Err(payload) => {
            lexer.error = TJS2_LEX_ERROR_PANIC;
            lexer.message = tjs2_lexer::panic_message(payload);
            lexer.set_value(None);
            -1
        }
    }
}

/// The error code of the last read.
///
/// The value is [`TJS2_LEX_OK`] when the last read succeeded.
///
/// # Safety
///
/// `lexer` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_error(lexer: *const tjs2_lexer) -> c_int {
    // SAFETY: the caller promises the handle is live.
    // SAFETY: the caller promises the handle is live and not freed.
    unsafe { lexer.as_ref() }
        .map(|lexer| lexer.error)
        .unwrap_or(TJS2_LEX_ERROR_PANIC)
}

/// The text of the error of the last read.
///
/// The text is a NUL-terminated string owned by the handle; it stays valid
/// until the next read on the same handle. A null pointer means there is
/// no error text.
///
/// # Safety
///
/// `lexer` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_error_message(lexer: *const tjs2_lexer) -> *const c_char {
    // SAFETY: the caller promises the handle is live.
    // SAFETY: the caller promises the handle is live and not freed.
    unsafe { lexer.as_ref() }
        .map(|lexer| lexer.message.as_ptr())
        .unwrap_or(std::ptr::null())
}

/// The kind of the value of the last token.
///
/// The value is one of the `TJS2_LEX_VALUE_*` constants.
///
/// # Safety
///
/// `lexer` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_value_kind(lexer: *const tjs2_lexer) -> c_int {
    // SAFETY: the caller promises the handle is live.
    // SAFETY: the caller promises the handle is live and not freed.
    unsafe { lexer.as_ref() }
        .map(|lexer| lexer.value_kind)
        .unwrap_or(TJS2_LEX_VALUE_NONE)
}

/// The integer of the last token.
///
/// The value is zero unless the token carries an integer.
///
/// # Safety
///
/// `lexer` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_value_integer(lexer: *const tjs2_lexer) -> i64 {
    // SAFETY: the caller promises the handle is live.
    // SAFETY: the caller promises the handle is live and not freed.
    unsafe { lexer.as_ref() }
        .map(|lexer| lexer.value_integer)
        .unwrap_or(0)
}

/// The real of the last token.
///
/// The value is zero unless the token carries a real.
///
/// # Safety
///
/// `lexer` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_value_real(lexer: *const tjs2_lexer) -> f64 {
    // SAFETY: the caller promises the handle is live.
    // SAFETY: the caller promises the handle is live and not freed.
    unsafe { lexer.as_ref() }
        .map(|lexer| lexer.value_real)
        .unwrap_or(0.0)
}

/// The code units of the last token.
///
/// The length is written to `length` when it is not null. The pointer is
/// owned by the handle and stays valid until the next read on the same
/// handle. A token that carries no string has a length of zero.
///
/// # Safety
///
/// `lexer` must be a live handle and `length` must be null or point at a
/// writable `uint32_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_value_string(
    lexer: *const tjs2_lexer,
    length: *mut u32,
) -> *const u16 {
    // SAFETY: the caller promises the handle is live.
    // SAFETY: the caller promises the handle is live and not freed.
    let Some(lexer) = (unsafe { lexer.as_ref() }) else {
        return std::ptr::null();
    };
    if !length.is_null() {
        // SAFETY: the caller promises the pointer is writable.
        unsafe { *length = lexer.value_string.len() as u32 };
    }
    // the pointer is always valid: an empty string has no code units, and
    // the length says so
    lexer.value_string.as_ptr()
}

/// The bytes of the last token.
///
/// The length is written to `length` when it is not null. The pointer is
/// owned by the handle and stays valid until the next read on the same
/// handle. A token that carries no buffer has a length of zero.
///
/// # Safety
///
/// `lexer` must be a live handle and `length` must be null or point at a
/// writable `uint32_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_value_octet(
    lexer: *const tjs2_lexer,
    length: *mut u32,
) -> *const u8 {
    // SAFETY: the caller promises the handle is live.
    // SAFETY: the caller promises the handle is live and not freed.
    let Some(lexer) = (unsafe { lexer.as_ref() }) else {
        return std::ptr::null();
    };
    if !length.is_null() {
        // SAFETY: the caller promises the pointer is writable.
        unsafe { *length = lexer.value_octet.len() as u32 };
    }
    // the pointer is always valid: an empty buffer has no bytes, and the
    // length says so
    lexer.value_octet.as_ptr()
}

/// The position the analyser has read up to, counted in code units.
///
/// # Safety
///
/// `lexer` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_position(lexer: *const tjs2_lexer) -> u32 {
    // SAFETY: the caller promises the handle is live.
    // SAFETY: the caller promises the handle is live and not freed.
    unsafe { lexer.as_ref() }
        .map(|lexer| {
            lexer
                .lexer
                .as_ref()
                .map_or(0, |analyser| analyser.current_position() as u32)
        })
        .unwrap_or(0)
}

/// Tell the analyser that the next token is a regular expression literal.
///
/// The call must come before the next token is read.
///
/// # Safety
///
/// `lexer` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_expect_regexp(lexer: *mut tjs2_lexer) {
    // SAFETY: the caller promises the handle is live and not freed.
    let Some(lexer) = (unsafe { lexer.as_mut() }) else {
        return;
    };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(analyser) = lexer.lexer.as_mut() {
            analyser.expect_regexp();
        }
    }));
}

/// Tell the analyser that the next name is an ordinary name, not a
/// reserved word.
///
/// The call must come before the next token is read.
///
/// # Safety
///
/// `lexer` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_lexer_expect_bare_word(lexer: *mut tjs2_lexer) {
    // SAFETY: the caller promises the handle is live and not freed.
    let Some(lexer) = (unsafe { lexer.as_mut() }) else {
        return;
    };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(analyser) = lexer.lexer.as_mut() {
            analyser.expect_bare_word();
        }
    }));
}

/// The token kind of the end of the script.
pub const TJS2_LEX_TOKEN_END: c_int = 0;

/// The token kind of a name.
pub const TJS2_LEX_TOKEN_SYMBOL: c_int = TokenKind::Symbol as c_int;
