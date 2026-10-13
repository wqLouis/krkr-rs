//! `json` plugin — a full port of the reference `json.dll` (`wamsoft/json`).
//!
//! The reference attaches four static members to the core `Scripts` class
//! (`json_init` in `Main.cpp`):
//!
//! | member | behavior |
//! |---|---|
//! | `evalJSON(text)` | parse a JSON string, returning an `Array`/`Dictionary`/scalar |
//! | `evalJSONStorage(name[, utf8])` | read `name` through storage and parse it |
//! | `saveJSON(name, obj[, utf8[, newline]])` | serialize `obj` to a JSON file |
//! | `toJSONString(obj[, newline])` | serialize `obj` to a JSON string |
//!
//! Parsing runs through [`serde_json`]: [`to_strict_json`] rewrites the
//! reference's relaxed dialect to strict JSON, and `serde_json` performs the
//! actual parse (values, containers, numbers, escapes, surrogate pairs and
//! trailing-data detection). The writer is [`JsonWriter`], a byte-exact port
//! of the reference's `IWriter`. Both are driven through [`Tjs2Engine`]
//! rather than the raw C++ ABI.
//!
//! # Value mapping
//!
//! TJS2 has no boolean type: `true`/`false` are the integers `1`/`0`, and the
//! reference parser stores JSON `null` (and its `void` extension) as TJS
//! `void`. This port keeps that mapping, so a round-trip through
//! `evalJSON`/`toJSONString` is lossless for every JSON value:
//!
//! | JSON | TJS |
//! |---|---|
//! | `null` | `void` (the reference's `var.Clear()`) |
//! | `true` / `false` | `1` / `0` |
//! | number with `.`/exponent | real |
//! | integer | integer |
//! | string | string |
//! | array | `Array` |
//! | object | `Dictionary` |
//!
//! Input is a superset of strict JSON: [`to_strict_json`] accepts the
//! reference's extensions — `#`/`//`/`/* */` comments, single-quoted
//! strings, `;` element separators, `=`/`=>` key separators, `\xNN`
//! escapes, the `void` keyword and the reference's number forms (`+5`,
//! `007`, `.5`, `1.`) — and rewrites them so `serde_json` sees only strict
//! JSON with identical parsed values. Structural mistakes are reported with
//! `serde_json`'s message; lexer-level failures (bad escapes, unknown
//! keywords, malformed numbers) use [`JsonError`]'s own text, and input
//! deeper than `serde_json`'s recursion limit is a typed error.
//!
//! # Writer
//!
//! [`JsonWriter`] reproduces `IWriter`/`getVariantString` in `Writer.hpp` and
//! `Main.cpp` byte-for-byte for finite values: one space of indentation per
//! nesting level, a newline (CRLF unless `newline` is non-zero) inside every
//! container and after the last member, and `"key":value` members. Any
//! non-`Array` object is serialized as a dictionary, exactly like the
//! reference. A non-finite real (TJS `NaN`/`Infinity`) is a typed error
//! because it has no JSON representation.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

use api::fs::FsPath;
use serde_json::Value as Json;
use tjs::{
    DetachedValue, NativeMethodDef, NativeStaticMembers, RetainedValue, Tjs2Engine, TjsValue,
    VAL_INTEGER, VAL_NULL, VAL_OBJECT, VAL_OCTET, VAL_REAL, VAL_STRING, VAL_VOID, Value,
};

use super::super::system::{resolve, set_string_out, value_as_i64};
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError, CompatServices};

/// Name of the TJS helper that distinguishes TJS `null` from other objects.
///
/// The safe engine API collapses `null` and a real object into
/// `TjsValue::Object`, so the serializer asks the VM directly.
const NULL_HELPER_NAME: &str = "__krkr_json_is_null";

/// TJS helper installed once per engine.
///
/// The ABI installs every plugin helper as a global, but the reference
/// registers no free function here (`reference/cpp/plugins/json/jsonPlugin.cpp:98-103`
/// attaches only the four `Scripts` members), so [`register`] installs this
/// function, retains it, and removes the global binding.
const NULL_HELPER_SOURCE: &str = "function __krkr_json_is_null(v) { return v === null; }";

// ---------------------------------------------------------------------------
// per-VM dispatch state
// ---------------------------------------------------------------------------

/// Retained id of [`NULL_HELPER_NAME`].
///
/// The helper is defined as a script global so the VM compiles it, then
/// retained and its global binding deleted. The engine's retained map dies
/// with the VM, so the id is deliberately leaked on the Rust side — dropping
/// the engine releases the C++ retention.
#[derive(Default)]
struct JsonState {
    null_helper: Option<tjs::Tjs2ValueId>,
}

// ---------------------------------------------------------------------------
// typed errors
// ---------------------------------------------------------------------------

/// A parse or serialization failure.
#[derive(Debug, thiserror::Error)]
enum JsonError {
    #[error("unexpected end of input at byte {offset}")]
    Eof { offset: usize },
    #[error("invalid number at byte {offset}")]
    Number { offset: usize },
    #[error("invalid escape at byte {offset}")]
    Escape { offset: usize },
    #[error("invalid unicode escape at byte {offset}")]
    Unicode { offset: usize },
    #[error("unknown keyword {keyword:?} at byte {offset}")]
    Keyword { keyword: String, offset: usize },
    #[error("real value {value} is not finite and cannot be represented in JSON")]
    NonFinite { value: f64 },
    #[error("{0}")]
    Engine(String),
}

impl From<tjs::TjsError> for JsonError {
    fn from(error: tjs::TjsError) -> Self {
        JsonError::Engine(error.to_string())
    }
}

impl From<String> for JsonError {
    fn from(message: String) -> Self {
        JsonError::Engine(message)
    }
}

// ---------------------------------------------------------------------------
// parser (serde_json + the reference dialect normalization)
// ---------------------------------------------------------------------------

/// Parse `input` as JSON (plus the reference's extensions).
///
/// [`to_strict_json`] rewrites the reference's relaxed dialect to strict
/// JSON text; `serde_json` then does the real parsing — values, containers,
/// numbers, escapes, surrogate pairs and trailing-data detection.
fn parse_json(input: &str) -> Result<Json, JsonError> {
    let normalized = to_strict_json(input)?;
    serde_json::from_str(&normalized).map_err(|error| JsonError::Engine(error.to_string()))
}

/// Rewrite the reference's relaxed JSON dialect to strict JSON text.
///
/// This is deliberately lexer-scale, not a parser: it tracks string and
/// comment context so rewrites never touch string literals, and leaves every
/// structural decision to `serde_json`. It performs the reference's
/// extensions:
///
/// * `#`, `//` and `/* ... */` comments become one separating space;
/// * single-quoted strings become double-quoted (a raw `"` inside is
///   escaped);
/// * `;` → `,` and `=`/`=>` → `:` as element/key separators;
/// * the `void` keyword → `null`; any other bare identifier is rejected
///   with the typed `unknown keyword` error;
/// * `\xNN` escapes → `\u00NN`, with the same hex-digit validation as
///   `\uNNNN`;
/// * number literals are completed or stripped so the value is unchanged
///   but the text is strict (`+5` → `5`, `007` → `7`, `.5` → `0.5`, `1.`
///   → `1.0`).
fn to_strict_json(input: &str) -> Result<String, JsonError> {
    let mut normalizer = Normalizer {
        input,
        pos: 0,
        out: String::with_capacity(input.len()),
    };
    normalizer.run()?;
    Ok(normalizer.out)
}

/// The normalization pass of [`to_strict_json`].
struct Normalizer<'a> {
    input: &'a str,
    /// Byte offset of the next unread character.
    pos: usize,
    /// The strict-JSON rendering built so far.
    out: String,
}

impl<'a> Normalizer<'a> {
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn run(&mut self) -> Result<(), JsonError> {
        loop {
            let Some(ch) = self.peek() else {
                return Ok(());
            };
            match ch {
                '"' | '\'' => self.string(ch)?,
                '#' => {
                    self.bump();
                    self.skip_line();
                    self.out.push(' ');
                }
                '/' => {
                    let input = self.input;
                    if input[self.pos..].starts_with("//") {
                        self.pos += 2;
                        self.skip_line();
                        self.out.push(' ');
                    } else if input[self.pos..].starts_with("/*") {
                        self.pos += 2;
                        self.skip_block_comment();
                        self.out.push(' ');
                    } else {
                        self.bump();
                        self.out.push(ch);
                    }
                }
                ';' => {
                    self.bump();
                    self.out.push(',');
                }
                '=' => {
                    self.bump();
                    if self.peek() == Some('>') {
                        self.bump();
                    }
                    self.out.push(':');
                }
                ws if ws.is_whitespace() => {
                    self.bump();
                    // `serde_json` only accepts ASCII JSON whitespace; the
                    // reference's scanner accepted any whitespace.
                    self.out
                        .push(if ws.is_ascii_whitespace() { ws } else { ' ' });
                }
                head if head.is_ascii_alphabetic() => self.keyword()?,
                digit if digit.is_ascii_digit() || matches!(digit, '+' | '-' | '.') => {
                    self.number()?
                }
                other => {
                    self.bump();
                    self.out.push(other);
                }
            }
        }
    }

    /// Skip the rest of the line (comment body for `#`/`//`), as the
    /// reference's trivia scanner did.
    fn skip_line(&mut self) {
        while let Some(ch) = self.peek() {
            self.pos += ch.len_utf8();
            if ch == '\n' {
                break;
            }
        }
    }

    /// Skip a `/* ... */` comment; an unterminated one consumes the rest of
    /// the input, exactly like the reference's scanner.
    fn skip_block_comment(&mut self) {
        while let Some(ch) = self.bump() {
            if ch == '*' && self.peek() == Some('/') {
                self.pos += 1;
                break;
            }
        }
    }

    /// Copy one string literal to the output as a double-quoted JSON string.
    fn string(&mut self, quote: char) -> Result<(), JsonError> {
        self.bump(); // opening quote
        self.out.push('"');
        loop {
            let offset = self.pos;
            match self.bump() {
                None => return Err(JsonError::Eof { offset }),
                Some(ch) if ch == quote => {
                    self.out.push('"');
                    return Ok(());
                }
                Some('\\') => self.escape()?,
                Some(ch) if (ch as u32) < 0x20 => return Err(JsonError::Escape { offset }),
                // A raw quote inside a single-quoted string.
                Some('"') => self.out.push_str("\\\""),
                Some(ch) => self.out.push(ch),
            }
        }
    }

    /// Copy one backslash escape, applying the reference's escape table.
    fn escape(&mut self) -> Result<(), JsonError> {
        let offset = self.pos;
        match self.bump() {
            None => Err(JsonError::Eof { offset }),
            Some(ch) if matches!(ch, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => {
                self.out.push('\\');
                self.out.push(ch);
                Ok(())
            }
            Some('u') => {
                self.out.push_str("\\u");
                self.hex_digits(4)
            }
            // The reference's non-standard `\xNN` escape.
            Some('x') => {
                self.out.push_str("\\u00");
                self.hex_digits(2)
            }
            // Includes `\'`: the reference's escape table has no single
            // quote, and neither does strict JSON.
            Some(_) => Err(JsonError::Escape { offset }),
        }
    }

    /// Copy exactly `digits` hex digits (the reference's `\u`/`\x`
    /// validation); `serde_json` combines the surrogate pairs.
    fn hex_digits(&mut self, digits: usize) -> Result<(), JsonError> {
        for _ in 0..digits {
            let offset = self.pos;
            match self.bump() {
                Some(ch) if ch.is_ascii_hexdigit() => self.out.push(ch),
                Some(_) => return Err(JsonError::Unicode { offset }),
                None => return Err(JsonError::Eof { offset }),
            }
        }
        Ok(())
    }

    /// Copy a bare keyword; only the reference's `true`/`false`/`null`/
    /// `void` exist (the latter two become `null`), anything else is the
    /// reference's `unknown keyword` error.
    fn keyword(&mut self) -> Result<(), JsonError> {
        let offset = self.pos;
        let start = self.pos;
        while self.peek().is_some_and(|ch| ch.is_ascii_alphabetic()) {
            self.bump();
        }
        let input = self.input;
        match &input[start..self.pos] {
            "true" | "false" => self.out.push_str(&input[start..self.pos]),
            // The reference clears the variant for both keywords.
            "null" | "void" => self.out.push_str("null"),
            keyword => {
                return Err(JsonError::Keyword {
                    keyword: keyword.to_string(),
                    offset,
                });
            }
        }
        Ok(())
    }

    /// Consume the run of ASCII digits at the cursor.
    fn digits(&mut self) -> String {
        let mut digits = String::new();
        while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            if let Some(ch) = self.bump() {
                digits.push(ch);
            }
        }
        digits
    }

    /// Copy one number literal in strict form; the reference accepts `+`,
    /// leading zeros, bare fractions and trailing dots, none of which change
    /// the value.
    fn number(&mut self) -> Result<(), JsonError> {
        let offset = self.pos;
        let mut text = String::new();
        if matches!(self.peek(), Some('+' | '-')) {
            if self.peek() == Some('-') {
                text.push('-');
            }
            self.bump();
        }
        let integer = self.digits();
        if self.peek() == Some('.') {
            self.bump();
            let fraction = self.digits();
            if integer.is_empty() && fraction.is_empty() {
                return Err(JsonError::Number { offset });
            }
            push_integer_digits(&integer, &mut text);
            text.push('.');
            text.push_str(if fraction.is_empty() { "0" } else { &fraction });
        } else if integer.is_empty() {
            return Err(JsonError::Number { offset });
        } else {
            push_integer_digits(&integer, &mut text);
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.bump();
            text.push('e');
            if matches!(self.peek(), Some('+' | '-')) {
                if self.peek() == Some('-') {
                    text.push('-');
                }
                self.bump();
            }
            if !self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                return Err(JsonError::Number { offset });
            }
            text.push_str(&self.digits());
        }
        self.out.push_str(&text);
        // The separating space keeps an adjacent token (`1.+2`) from merging
        // into this literal: `serde_json` sees `1 2` and reports trailing
        // data.
        self.out.push(' ');
        Ok(())
    }
}

/// Integer digits with the reference's leading zeros dropped (`007` → `7`);
/// an empty run (`.5`) becomes `0`.
fn push_integer_digits(digits: &str, out: &mut String) {
    if digits.is_empty() {
        out.push('0');
        return;
    }
    let trimmed = digits.trim_start_matches('0');
    out.push_str(if trimmed.is_empty() { "0" } else { trimmed });
}

// ---------------------------------------------------------------------------
// TJS value bridge
// ---------------------------------------------------------------------------

/// A TJS value owned by the serializer, with object identity preserved.
enum TjsOwned {
    Void,
    Integer(i64),
    Real(f64),
    String(String),
    Object(DetachedValue),
}

impl TjsOwned {
    /// Render this value as a member/array argument, handing the C++ side a
    /// duplicate of an object retention (the retention is consumed by the
    /// conversion, so the local handle stays valid for its own drop).
    fn to_argument(&self, engine: &Tjs2Engine) -> Result<TjsValue, JsonError> {
        Ok(match self {
            TjsOwned::Void => TjsValue::Void,
            TjsOwned::Integer(value) => TjsValue::Integer(*value),
            TjsOwned::Real(value) => TjsValue::Real(*value),
            TjsOwned::String(value) => TjsValue::String(value.clone()),
            TjsOwned::Object(object) => {
                let duplicate = engine.duplicate_retained(object.raw_id())?;
                let id = duplicate.raw_id() as u64;
                std::mem::forget(duplicate);
                TjsValue::Retained(id)
            }
        })
    }
}

/// Read a callback argument into a [`TjsOwned`].
fn owned_from_arg(
    engine: &Tjs2Engine,
    services: &CompatServices,
    value: &Value,
) -> Result<TjsOwned, JsonError> {
    Ok(match value.ty {
        VAL_VOID | VAL_NULL => TjsOwned::Void,
        VAL_INTEGER => TjsOwned::Integer(value.integer),
        VAL_REAL => TjsOwned::Real(value.real),
        VAL_STRING => TjsOwned::String(value_as_string(value)),
        // The reference's `getVariantString` default writes `null`; octets are
        // not part of the JSON value model.
        VAL_OCTET => TjsOwned::Void,
        VAL_OBJECT => {
            let object = engine.retain_object_arg(value)?;
            if is_null(engine, services, &object)? {
                TjsOwned::Void
            } else {
                TjsOwned::Object(object)
            }
        }
        other => {
            return Err(JsonError::Engine(format!(
                "unsupported argument type {other}"
            )));
        }
    })
}

/// Read an engine result into a [`TjsOwned`].
fn owned_from_tjs(
    engine: &Tjs2Engine,
    services: &CompatServices,
    value: TjsValue,
) -> Result<TjsOwned, JsonError> {
    Ok(match value {
        TjsValue::Void => TjsOwned::Void,
        TjsValue::Integer(value) => TjsOwned::Integer(value),
        TjsValue::Real(value) => TjsOwned::Real(value),
        TjsValue::String(value) => TjsOwned::String(value),
        TjsValue::Object => {
            let object = engine.retain_value_detached(&TjsValue::Object)?;
            if is_null(engine, services, &object)? {
                TjsOwned::Void
            } else {
                TjsOwned::Object(object)
            }
        }
        TjsValue::Retained(_) => {
            return Err(JsonError::Engine(
                "unexpected retained value in an engine result".into(),
            ));
        }
    })
}

/// Convert a parsed JSON value into a fresh TJS value.
fn json_to_owned(engine: &Tjs2Engine, json: &Json) -> Result<TjsOwned, JsonError> {
    Ok(match json {
        Json::Null => TjsOwned::Void,
        Json::Bool(value) => TjsOwned::Integer(i64::from(*value)),
        Json::Number(number) => {
            if let Some(value) = number.as_i64() {
                TjsOwned::Integer(value)
            } else {
                // Real, or an integer beyond `i64`: re-parse the literal so
                // an overflowing exponent (`1e400`) saturates to ±infinity,
                // which `serde_json` itself refuses to produce.
                let literal = number.to_string();
                let value = literal
                    .parse::<f64>()
                    .map_err(|error| JsonError::Engine(format!("number {literal:?}: {error}")))?;
                TjsOwned::Real(value)
            }
        }
        Json::String(value) => TjsOwned::String(value.clone()),
        Json::Array(items) => {
            let array = new_object(engine, "[]")?;
            for (index, item) in items.iter().enumerate() {
                // `PropSetByNum(TJS_MEMBERENSURE)` in the reference: a void
                // element (JSON null) is stored, it does not shift later ones.
                let argument = json_to_owned(engine, item)?.to_argument(engine)?;
                engine.set_member(array.raw_id(), &index.to_string(), &argument)?;
            }
            TjsOwned::Object(array)
        }
        Json::Object(entries) => {
            let dict = new_object(engine, "%[]")?;
            for (key, value) in entries {
                let argument = json_to_owned(engine, value)?.to_argument(engine)?;
                engine.set_member(dict.raw_id(), key, &argument)?;
            }
            TjsOwned::Object(dict)
        }
    })
}

/// Evaluate a TJS array/dictionary literal into a retained object.
fn new_object(engine: &Tjs2Engine, expression: &str) -> Result<DetachedValue, JsonError> {
    match engine.eval_retained(expression, "json")? {
        RetainedValue::Object(object) => Ok(object),
        RetainedValue::Value(value) => Err(JsonError::Engine(format!(
            "{expression:?} did not evaluate to an object ({value:?})"
        ))),
    }
}

// ---------------------------------------------------------------------------
// TJS inspection helpers
// ---------------------------------------------------------------------------

/// Whether `value` is TJS `null` (the safe API reports it as an object).
fn is_null(
    engine: &Tjs2Engine,
    services: &CompatServices,
    value: &DetachedValue,
) -> Result<bool, JsonError> {
    let helper = {
        let state = services.state::<JsonState>();
        state.null_helper
    }
    .ok_or_else(|| JsonError::Engine("null helper is not installed".into()))?;
    let function = engine.duplicate_retained(helper)?;
    let argument = engine.duplicate_retained(value.raw_id())?;
    let result =
        engine.call_detached(&function, &[TjsValue::Retained(argument.raw_id() as u64)])?;
    match result {
        TjsValue::Integer(value) => Ok(value != 0),
        TjsValue::Void => Ok(false),
        TjsValue::Real(value) => Ok(value != 0.0),
        other => Err(JsonError::Engine(format!(
            "null check returned {other:?}, expected a boolean"
        ))),
    }
}

/// The `count` property of a TJS `Array`.
fn array_count(engine: &Tjs2Engine, array: &DetachedValue) -> Result<i64, JsonError> {
    match engine.member(array.raw_id(), "count")? {
        TjsValue::Integer(value) => Ok(value),
        TjsValue::Real(value) => Ok(value as i64),
        other => Err(JsonError::Engine(format!(
            "array count is not numeric ({other:?})"
        ))),
    }
}

/// Whether `object`'s class-name chain contains `class` (reference
/// `IsInstanceOf`).
fn class_names_contain(
    engine: &Tjs2Engine,
    object: &DetachedValue,
    class: &str,
) -> Result<bool, JsonError> {
    let names = engine.class_names(object.raw_id())?;
    let count = array_count(engine, &names)?;
    for index in 0..count {
        if let TjsValue::String(name) = engine.member(names.raw_id(), &index.to_string())?
            && name == class
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Enumerate a dictionary's own data members in `EnumMembers` order.
///
/// The ABI exposes no `EnumMembers`, so the reference walk is reproduced
/// through the builtin `Array.assign`, which flattens the object into
/// `[key0, value0, key1, value1, ...]` exactly as `EnumMembers` visits it.
fn enumerate_members(
    engine: &Tjs2Engine,
    services: &CompatServices,
    target: &DetachedValue,
) -> Result<Vec<(String, TjsOwned)>, JsonError> {
    let flat = new_object(engine, "[]")?;
    let argument = {
        let duplicate = engine.duplicate_retained(target.raw_id())?;
        let id = duplicate.raw_id() as u64;
        std::mem::forget(duplicate);
        TjsValue::Retained(id)
    };
    engine.call_member(flat.raw_id(), "assign", &[argument])?;
    let count = array_count(engine, &flat)?;
    let mut entries = Vec::new();
    let mut index = 0;
    while index + 1 < count {
        let key = match engine.member(flat.raw_id(), &index.to_string())? {
            TjsValue::String(value) => value,
            TjsValue::Integer(value) => value.to_string(),
            TjsValue::Real(value) => value.to_string(),
            other => {
                return Err(JsonError::Engine(format!(
                    "unexpected member key {other:?}"
                )));
            }
        };
        let value = engine.member(flat.raw_id(), &(index + 1).to_string())?;
        let owned = owned_from_tjs(engine, services, value)?;
        entries.push((key, owned));
        index += 2;
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// writer
// ---------------------------------------------------------------------------

/// The reference `IWriter`/`getVariantString` (see `Writer.hpp`).
struct JsonWriter<'a> {
    engine: &'a Tjs2Engine,
    services: &'a CompatServices,
    newline: &'static str,
    indent: usize,
    out: String,
}

impl<'a> JsonWriter<'a> {
    fn new(engine: &'a Tjs2Engine, services: &'a CompatServices, newline: i64) -> Self {
        Self {
            engine,
            services,
            newline: if newline == 0 { "\r\n" } else { "\n" },
            indent: 0,
            out: String::new(),
        }
    }

    /// `IWriter::newline`: the configured newline plus the current indent.
    fn newline(&mut self) {
        self.out.push_str(self.newline);
        for _ in 0..self.indent {
            self.out.push(' ');
        }
    }

    fn write_value(&mut self, value: &TjsOwned) -> Result<(), JsonError> {
        match value {
            TjsOwned::Void => self.out.push_str("null"),
            TjsOwned::Integer(value) => self.out.push_str(&value.to_string()),
            TjsOwned::Real(value) => self.out.push_str(&format_real(*value)?),
            TjsOwned::String(value) => quote_string(value, &mut self.out),
            TjsOwned::Object(object) => {
                if class_names_contain(self.engine, object, "Array")? {
                    self.write_array(object)?;
                } else {
                    self.write_dict(object)?;
                }
            }
        }
        Ok(())
    }

    fn write_array(&mut self, array: &DetachedValue) -> Result<(), JsonError> {
        self.out.push('[');
        self.indent += 1;
        self.newline();
        let count = array_count(self.engine, array)?;
        for index in 0..count {
            if index != 0 {
                self.out.push(',');
                self.newline();
            }
            let value = self.engine.member(array.raw_id(), &index.to_string())?;
            self.write_value(&owned_from_tjs(self.engine, self.services, value)?)?;
        }
        self.indent -= 1;
        self.newline();
        self.out.push(']');
        Ok(())
    }

    fn write_dict(&mut self, dict: &DetachedValue) -> Result<(), JsonError> {
        self.out.push('{');
        self.indent += 1;
        self.newline();
        let entries = enumerate_members(self.engine, self.services, dict)?;
        for (index, (key, value)) in entries.iter().enumerate() {
            if index != 0 {
                self.out.push(',');
                self.newline();
            }
            quote_string(key, &mut self.out);
            self.out.push(':');
            self.write_value(value)?;
        }
        self.indent -= 1;
        self.newline();
        self.out.push('}');
        Ok(())
    }
}

/// The reference `TJSRealToString` slot for finite values.
fn format_real(value: f64) -> Result<String, JsonError> {
    if !value.is_finite() {
        return Err(JsonError::NonFinite { value });
    }
    let text = value.to_string();
    Ok(text.strip_prefix('+').map(str::to_owned).unwrap_or(text))
}

/// The reference `quoteString` (a `null` string is the empty string).
fn quote_string(value: &str, out: &mut String) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// result / file marshalling
// ---------------------------------------------------------------------------

/// Write a real result into a native callback's out slot.
fn set_real(out: *mut Value, value: f64) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_REAL;
        (*out).integer = 0;
        (*out).real = value;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Hand a retained object to the C++ side as `VAL_RETAINED` (the retention is
/// consumed by the result conversion).
fn set_retained_out(out: *mut Value, object: DetachedValue) {
    if out.is_null() {
        // No result slot: the drop releases the retention.
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = tjs::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = object.raw_id() as usize;
    }
    std::mem::forget(object);
}

/// Write a parsed value into a native callback's out slot.
///
/// JSON `null` is mapped to TJS `void` (the reference's `var.Clear()`), so a
/// `Void` result is written as `VAL_VOID`, not `VAL_NULL`.
fn write_value_out(ctx: &CompatCtx, out: *mut Value, value: TjsOwned) {
    match value {
        TjsOwned::Void => set_void(out),
        TjsOwned::Integer(value) => set_int(out, value),
        TjsOwned::Real(value) => set_real(out, value),
        TjsOwned::String(value) => set_string_out(ctx, out, &value),
        TjsOwned::Object(object) => set_retained_out(out, object),
    }
}

/// Read a file through the attached storage, or the filesystem when none is
/// attached.
fn read_input(ctx: &CompatCtx, name: &str) -> Result<Vec<u8>, String> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    // The media prefix is L3 syntax; the L2 namespace resolves the bare name.
    let lookup = strip_file_media(name);
    if let Some(pointer) = services.storage() {
        // SAFETY: the host keeps the attached storage alive for the binding.
        return unsafe { &*pointer }
            .read(lookup)
            .map_err(|error| format!("'{name}': {error}"));
    }
    let file_system = fs::FileSystem::host("");
    fs::read(&file_system, &FsPath::host(lookup)).map_err(|error| format!("'{name}': {error}"))
}

/// Whether `name` is a Windows drive-letter path (`C:/...`), which the
/// reference treats as absolute.
fn is_drive_path(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// Resolve a reference stream name to a filesystem path.
fn output_path(ctx: &CompatCtx, filename: &str) -> PathBuf {
    let path = Path::new(filename);
    if path.is_absolute() || is_drive_path(filename) {
        return path.to_path_buf();
    }
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    if let Some(pointer) = services.storage() {
        // SAFETY: the host keeps the attached storage alive for the binding.
        return unsafe { &*pointer }.game_dir().join(path);
    }
    path.to_path_buf()
}

/// Write serialized bytes to the resolved output path.
fn write_output(ctx: &CompatCtx, filename: &str, bytes: &[u8]) -> Result<(), String> {
    let path = output_path(ctx, filename);
    let file_system = fs::FileSystem::host("");
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(&file_system, &FsPath::host(parent))
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    fs::write(&file_system, &FsPath::host(&path), bytes)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Encode `text` as UTF-8 (`utf8`) or the current code page (CP932).
fn encode_text(text: &str, utf8: bool) -> Result<Vec<u8>, String> {
    if utf8 {
        return Ok(text.as_bytes().to_vec());
    }
    let (bytes, _, had_errors) = encoding_rs::SHIFT_JIS.encode(text);
    if had_errors {
        return Err("the text cannot be encoded in the current code page".into());
    }
    Ok(bytes.into_owned())
}

// ---------------------------------------------------------------------------
// native callbacks (`Scripts` static members)
// ---------------------------------------------------------------------------

extern "C" fn native_eval_json(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Scripts.evalJSON: compat context is not bound");
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(text) = arguments.first() else {
        return report_error(out_error, "Scripts.evalJSON requires 1 argument");
    };
    let text = value_as_string(text);
    match parse_json(&text).and_then(|json| json_to_owned(resolved.engine(), &json)) {
        Ok(value) => {
            write_value_out(&resolved.ctx(), out, value);
            0
        }
        Err(error) => report_error(out_error, &format!("Scripts.evalJSON: {error}")),
    }
}

extern "C" fn native_eval_json_storage(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Scripts.evalJSONStorage: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(name) = arguments.first() else {
        return report_error(out_error, "Scripts.evalJSONStorage requires 1 argument");
    };
    let name = value_as_string(name);
    let utf8 = arguments.get(1).map(value_as_i64).unwrap_or(0) != 0;
    let ctx = resolved.ctx();
    let bytes = match read_input(&ctx, &name) {
        Ok(bytes) => bytes,
        Err(error) => {
            return report_error(out_error, &format!("Scripts.evalJSONStorage: {error}"));
        }
    };
    // `utf8 = false` means the current code page (`anaul.tjs:16,19`, matching
    // `encode_text` above). No current-code-page model exists, so name the
    // documented CP932; detection would read CP932 `"あ"` as windows-1251.
    let encoding = if utf8 {
        crate::text::TextEncoding::Named("UTF-8")
    } else {
        crate::text::TextEncoding::Named("cp932")
    };
    let text = match crate::text::decode_text(&bytes, encoding) {
        Ok(text) => text,
        Err(error) => {
            return report_error(
                out_error,
                &format!("Scripts.evalJSONStorage: '{name}': {error}"),
            );
        }
    };
    match parse_json(&text).and_then(|json| json_to_owned(resolved.engine(), &json)) {
        Ok(value) => {
            write_value_out(&ctx, out, value);
            0
        }
        Err(error) => report_error(out_error, &format!("Scripts.evalJSONStorage: {error}")),
    }
}

extern "C" fn native_save_json(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Scripts.saveJSON: compat context is not bound");
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let (Some(filename), Some(value)) = (arguments.first(), arguments.get(1)) else {
        return report_error(out_error, "Scripts.saveJSON requires 2 arguments");
    };
    let filename = value_as_string(filename);
    let utf8 = arguments.get(2).map(value_as_i64).unwrap_or(0) != 0;
    let newline = arguments.get(3).map(value_as_i64).unwrap_or(0);
    let ctx = resolved.ctx();
    // SAFETY: `resolved` is only produced for the installed, live binding.
    let services = unsafe { ctx.services() };
    let owned = match owned_from_arg(resolved.engine(), services, value) {
        Ok(owned) => owned,
        Err(error) => return report_error(out_error, &format!("Scripts.saveJSON: {error}")),
    };
    let mut writer = JsonWriter::new(resolved.engine(), services, newline);
    if let Err(error) = writer.write_value(&owned) {
        return report_error(out_error, &format!("Scripts.saveJSON: {error}"));
    }
    let bytes = match encode_text(&writer.out, utf8) {
        Ok(bytes) => bytes,
        Err(error) => return report_error(out_error, &format!("Scripts.saveJSON: {error}")),
    };
    if let Err(error) = write_output(&ctx, &filename, &bytes) {
        return report_error(out_error, &format!("Scripts.saveJSON: {error}"));
    }
    // The reference's `saveJSON` always leaves `*result = true` (its `false`
    // assignment in the `catch` is dead — `if(result) *result = true;` runs
    // unconditionally afterwards, `reference/cpp/plugins/json/jsonPlugin.cpp:67-70`).
    set_int(out, 1);
    0
}

extern "C" fn native_to_json_string(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Scripts.toJSONString: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(value) = arguments.first() else {
        return report_error(out_error, "Scripts.toJSONString requires 1 argument");
    };
    let newline = arguments.get(1).map(value_as_i64).unwrap_or(0);
    let ctx = resolved.ctx();
    // SAFETY: `resolved` is only produced for the installed, live binding.
    let services = unsafe { ctx.services() };
    let owned = match owned_from_arg(resolved.engine(), services, value) {
        Ok(owned) => owned,
        Err(error) => {
            return report_error(out_error, &format!("Scripts.toJSONString: {error}"));
        }
    };
    let mut writer = JsonWriter::new(resolved.engine(), services, newline);
    if let Err(error) = writer.write_value(&owned) {
        return report_error(out_error, &format!("Scripts.toJSONString: {error}"));
    }
    set_string_out(&ctx, out, &writer.out);
    0
}

// ---------------------------------------------------------------------------
// registration
// ---------------------------------------------------------------------------

/// Register the plugin's native surface on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // Install the null-classification helper once per engine. It is defined
    // as a script global so the VM compiles it, then retained and its global
    // binding removed: the reference exposes no free function for it
    // (`reference/cpp/plugins/json/jsonPlugin.cpp:98-103`).
    // SAFETY: `ctx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    engine
        .exec_script(NULL_HELPER_SOURCE, "json")
        .map_err(|error| {
            CompatError::Register(format!("json: could not install the null helper: {error}"))
        })?;
    let helper = match engine
        .eval_retained(NULL_HELPER_NAME, "json")
        .map_err(|error| {
            CompatError::Register(format!("json: could not resolve the null helper: {error}"))
        })? {
        RetainedValue::Object(object) => {
            let id = object.raw_id();
            // The engine owns the C++ retention and frees it at destruction.
            std::mem::forget(object);
            id
        }
        RetainedValue::Value(value) => {
            return Err(CompatError::Register(format!(
                "json: the null helper is not a function ({value:?})"
            )));
        }
    };
    let global = match engine.eval_retained("global", "json") {
        Ok(RetainedValue::Object(object)) => object,
        Ok(RetainedValue::Value(value)) => {
            return Err(CompatError::Register(format!(
                "json: the global object is not an object ({value:?})"
            )));
        }
        Err(error) => {
            return Err(CompatError::Register(format!(
                "json: could not resolve the global object: {error}"
            )));
        }
    };
    engine
        .delete_member(global.raw_id(), NULL_HELPER_NAME)
        .map_err(|error| {
            CompatError::Register(format!(
                "json: could not remove the null helper global: {error}"
            ))
        })?;
    // SAFETY: `ctx` is only created for a bound, live engine.
    unsafe { ctx.services() }.state::<JsonState>().null_helper = Some(helper);

    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Scripts",
        methods: vec![
            NativeMethodDef {
                name: "evalJSON",
                f: native_eval_json,
            },
            NativeMethodDef {
                name: "evalJSONStorage",
                f: native_eval_json_storage,
            },
            NativeMethodDef {
                name: "saveJSON",
                f: native_save_json,
            },
            NativeMethodDef {
                name: "toJSONString",
                f: native_to_json_string,
            },
        ],
        properties: vec![],
    })?;

    Ok(())
}
