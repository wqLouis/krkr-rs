//! Integration tests for non-BMP (surrogate-pair) strings crossing the
//! Rust <-> engine boundary.
//!
//! Latent bug fixed here: every non-BMP character used to arrive at Rust as
//! a run of U+FFFD, because the glue narrowed UTF-16 through
//! `ttstr::AsStdString()` -> `TJS_wcstombs` -> `utf8_wctomb`, which encodes
//! each UTF-16 code unit on its own (CESU-8 for a surrogate pair — invalid
//! UTF-8 that `String::from_utf8_lossy` turns into replacement characters).
//! The glue now decodes surrogate pairs (`u16_to_utf8` in
//! `cpp/tjs2_abi.cpp`), so a literal like "😀🎶" must round-trip verbatim
//! in every direction: eval results, native arguments, and native returns.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::sync::{Mutex, MutexGuard};

use tjs::{
    NativeClassBuilder, NativeMethodDef, RetainedValue, Tjs2Engine, TjsValue, VAL_STRING, Value,
};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Two non-BMP characters: U+1F600 (4 UTF-8 bytes, 2 UTF-16 code units)
/// and U+1F3B6 — together a surrogate pair on either side of a BMP char.
const NON_BMP: &str = "😀🎶";

/// Every string argument the `Echo` native observed, in call order.
static RECEIVED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn received() -> Vec<String> {
    RECEIVED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

thread_local! {
    /// Scratch buffer for the native's string result: the C++ trampoline
    /// reads it after the callback returns, so it must outlive the callback
    /// frame — the same pattern the crate's own tests use.
    static STRING_OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// `Echo.echo(s)` — records the received string (proving it arrived as
/// valid UTF-8) and returns it verbatim.
extern "C" fn echo(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return 1;
    }
    // SAFETY: argv points at argc entries for the duration of the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let text = if args[0].ty == VAL_STRING && !args[0].string.is_null() {
        // SAFETY: the C++ side hands out a NUL-terminated UTF-8 string for
        // the duration of the callback. `to_str` (strict, not lossy) is the
        // assertion: CESU-8 from the old narrowing path would fail here.
        match unsafe { CStr::from_ptr(args[0].string) }.to_str() {
            Ok(text) => text.to_owned(),
            Err(_) => "<invalid-utf8>".to_owned(),
        }
    } else {
        "<not-a-string>".to_owned()
    };
    RECEIVED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push(text.clone());
    STRING_OUT.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        buf.extend_from_slice(text.as_bytes());
        buf.push(0);
        // SAFETY: out is the valid return slot provided by the trampoline;
        // the C++ side copies the bytes immediately after the callback.
        unsafe {
            (*out).ty = VAL_STRING;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = buf.as_ptr().cast();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = 0;
        }
    });
    0
}

fn echo_builder() -> NativeClassBuilder<'static> {
    NativeClassBuilder {
        name: "Echo",
        methods: vec![NativeMethodDef {
            name: "echo",
            f: echo,
        }],
        properties: vec![],
    }
}

#[test]
fn eval_returns_non_bmp_string_literals_verbatim() {
    let _lock = vm_lock();
    RECEIVED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
    let e = Tjs2Engine::new().unwrap();

    // A lone surrogate-pair character as the whole literal.
    assert_eq!(
        e.eval("\"😀\"", "non-bmp").unwrap(),
        TjsValue::String("😀".to_string())
    );
    // Embedded between BMP characters (pair in the middle of the buffer).
    assert_eq!(
        e.eval("\"a😀z\"", "non-bmp").unwrap(),
        TjsValue::String("a😀z".to_string())
    );
    // Two pairs in one literal, built through format! so the Rust side
    // definitely hands the engine raw UTF-8.
    assert_eq!(
        e.eval(&format!("\"{NON_BMP}\""), "non-bmp").unwrap(),
        TjsValue::String(NON_BMP.to_string())
    );
}

#[test]
fn non_bmp_string_round_trips_through_a_native() {
    let _lock = vm_lock();
    RECEIVED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class(&echo_builder()).unwrap();

    // Engine -> Rust (argument): the native must observe the exact string
    // as valid UTF-8, and the echoed result must come back verbatim.
    e.exec_script(
        &format!("var input = \"{NON_BMP}\"; var output = Echo.echo(input);"),
        "non-bmp",
    )
    .unwrap();

    assert_eq!(
        received(),
        vec![NON_BMP.to_string()],
        "the native must receive the exact string as valid UTF-8"
    );
    // Engine -> Rust (result slot): both the original and the echoed
    // string evaluate back to the exact bytes.
    assert_eq!(
        e.eval("input", "non-bmp").unwrap(),
        TjsValue::String(NON_BMP.to_string())
    );
    assert_eq!(
        e.eval("output", "non-bmp").unwrap(),
        TjsValue::String(NON_BMP.to_string())
    );
}

#[test]
fn non_bmp_string_passed_from_rust_into_a_native_round_trips() {
    let _lock = vm_lock();
    RECEIVED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
    let e = Tjs2Engine::new().unwrap();
    e.register_native_class(&echo_builder()).unwrap();

    // Rust -> engine (argument) -> native argument slot -> engine return
    // value -> Rust: one call exercises both conversion directions.
    let echo_fn = match e.eval_retained("Echo.echo", "non-bmp").unwrap() {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => panic!("Echo.echo must be an object: {other:?}"),
    };
    let back = e
        .call_detached(&echo_fn, &[TjsValue::String(NON_BMP.to_string())])
        .unwrap();
    assert_eq!(
        back,
        TjsValue::String(NON_BMP.to_string()),
        "a string passed in from Rust must round-trip through the native"
    );
    assert_eq!(
        received(),
        vec![NON_BMP.to_string()],
        "the native must observe the Rust-supplied string verbatim"
    );
}
