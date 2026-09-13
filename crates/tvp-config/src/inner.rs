//! Shared key-tree storage backing both the global and the individual
//! config managers — the Rust counterpart of `iSysConfigManager` in
//! `reference/cpp/core/environ/ConfigManager/GlobalConfigManager.h`.
//!
//! The C++ side keeps a generic key tree (`std::unordered_map<std::string,
//! std::string> AllConfig`) where **every** value is a string. This port uses
//! a `serde_json::Map<String, Value>` (schema-light, like the C++), so JSON
//! files may store typed scalars (`"fps_limit": 60`) or string-encoded ones
//! (`"fps_limit": "60"`); the typed accessors accept both.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use serde_json::{Map, Number, Value};

use crate::ConfigError;

/// A schema-light key → scalar-value tree plus a dirty flag.
///
/// The dirty flag mirrors the C++ `ConfigUpdated` member: it is set whenever
/// a value is written **or** read through a missing key (the C++ `GetValue`
/// records the default and marks the config updated), and cleared by a
/// successful save.
#[derive(Debug, Default)]
pub(crate) struct Inner {
    pub(crate) values: Map<String, Value>,
    pub(crate) dirty: bool,
}

impl Inner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Load the config from a JSON file.
    ///
    /// A missing file (or an empty/whitespace-only file) yields an empty
    /// config without error — the C++ `Initialize()` behaves the same way
    /// when `fopen`/`LoadFile` fails. A malformed file or a non-object root
    /// is an error.
    pub(crate) fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::new());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        let values = match value {
            Value::Object(map) => map,
            _ => {
                return Err(ConfigError::NotAnObject {
                    path: path.to_path_buf(),
                });
            }
        };
        Ok(Self {
            values,
            dirty: false,
        })
    }

    /// Write the config back to `path` as pretty JSON.
    ///
    /// Like the C++ `SaveToFile`, this is a **no-op when nothing changed**
    /// (the dirty flag is clear); a successful save clears the flag. Parent
    /// directories are created on demand.
    pub(crate) fn save_to(&mut self, path: &Path) -> Result<(), ConfigError> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let mut json = serde_json::to_string_pretty(&self.values)?;
        json.push('\n');
        fs::write(path, json).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        self.dirty = false;
        Ok(())
    }

    // --- typed accessors (port of the `GetValue<T>` specializations) ---

    /// `GetValue<std::string>`: return the stored string, or record and
    /// return `def` when the key is missing.
    pub(crate) fn get_string(&mut self, key: &str, def: &str) -> String {
        match self.values.get(key) {
            Some(value) => scalar_to_string(value),
            None => {
                self.record(key, Value::String(def.to_owned()));
                def.to_owned()
            }
        }
    }

    /// `GetValue<int>`: like `atoi` on the stored value; a missing key
    /// records and returns `def`. A present-but-unparseable value yields `0`
    /// (matching `atoi` on garbage).
    pub(crate) fn get_integer(&mut self, key: &str, def: i64) -> i64 {
        match self.values.get(key) {
            Some(value) => value_to_i64(value).unwrap_or(0),
            None => {
                self.record(key, Value::from(def));
                def
            }
        }
    }

    /// `GetValue<float>`: like `atof` on the stored value; a missing key
    /// records and returns `def`. A present-but-unparseable value yields
    /// `0.0` (matching `atof` on garbage).
    pub(crate) fn get_real(&mut self, key: &str, def: f64) -> f64 {
        match self.values.get(key) {
            Some(value) => value_to_f64(value).unwrap_or(0.0),
            None => {
                self.record(key, json_number(def));
                def
            }
        }
    }

    /// `GetValue<bool>` (which is `!!GetValue<int>` in the C++): a missing
    /// key records and returns `def`; a present-but-unparseable value yields
    /// `false`.
    pub(crate) fn get_boolean(&mut self, key: &str, def: bool) -> bool {
        match self.values.get(key) {
            Some(value) => value_to_bool(value).unwrap_or(false),
            None => {
                self.record(key, Value::Bool(def));
                def
            }
        }
    }

    // --- setters (port of `SetValue` / `SetValueInt` / `SetValueFloat`) ---

    pub(crate) fn set_string(&mut self, key: &str, val: &str) {
        self.record(key, Value::String(val.to_owned()));
    }

    pub(crate) fn set_integer(&mut self, key: &str, val: i64) {
        self.record(key, Value::from(val));
    }

    pub(crate) fn set_real(&mut self, key: &str, val: f64) {
        self.record(key, json_number(val));
    }

    pub(crate) fn set_boolean(&mut self, key: &str, val: bool) {
        self.record(key, Value::Bool(val));
    }

    pub(crate) fn remove(&mut self, key: &str) -> bool {
        let removed = self.values.remove(key).is_some();
        if removed {
            self.dirty = true;
        }
        removed
    }

    // --- introspection ---

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub(crate) fn values(&self) -> &Map<String, Value> {
        &self.values
    }

    fn record(&mut self, key: &str, value: Value) {
        self.values.insert(key.to_owned(), value);
        self.dirty = true;
    }
}

/// Stringify any scalar the way the C++ would have stored it (values were
/// always strings in `AllConfig`). Non-scalar values (arrays/objects/null)
/// become an empty string.
fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// A lenient `atoi`-style integer parse: a fully numeric string parses
/// directly, otherwise a float string is truncated toward zero.
fn parse_i64_or_f64(s: &str) -> Option<i64> {
    let t = s.trim();
    t.parse::<i64>()
        .ok()
        .or_else(|| t.parse::<f64>().ok().map(|f| f.trunc() as i64))
}

/// C `atoi`: skip leading ASCII whitespace, accept an optional sign, then
/// consume the longest run of decimal digits. Returns 0 when no digit
/// follows (exactly like `atoi`), so a present-but-garbage value yields 0
/// rather than a parse error. Overflow clamps instead of wrapping (the C
/// behavior is undefined there).
fn atoi(s: &str) -> i64 {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let negative = match bytes.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let start = i;
    let mut magnitude: i128 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        magnitude = (magnitude * 10 + (bytes[i] - b'0') as i128).min(i64::MAX as i128 + 1);
        i += 1;
    }
    if i == start {
        return 0;
    }
    let magnitude = magnitude.min(i64::MAX as i128);
    if negative {
        -(magnitude as i64)
    } else {
        magnitude as i64
    }
}

/// C `atof`: parse the longest numeric prefix of `s` (leading whitespace
/// ignored), returning 0.0 when there is none. `atoi`/`atof` both stop at
/// the first non-numeric byte, so `"42abc"` is 42 and `"1.5x"` is 1.5.
fn atof(s: &str) -> f64 {
    let t = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let mut end = t.len();
    while end > 0 {
        if let Ok(v) = t[..end].parse::<f64>() {
            return v;
        }
        end = t[..end].char_indices().next_back().map_or(0, |(i, _)| i);
    }
    0.0
}

/// `atoi` on a JSON value: numbers parse directly (floats truncate toward
/// zero), strings go through the C `atoi` prefix parser ([`atoi`]), anything
/// else is `None`.
pub(crate) fn value_to_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Some(i);
            }
            if let Some(u) = n.as_u64() {
                return i64::try_from(u).ok();
            }
            n.as_f64().map(|f| f.trunc() as i64)
        }
        Value::String(s) => Some(atoi(s)),
        _ => None,
    }
}

/// `atof` on a JSON value: numbers pass through, strings go through the C
/// `atof` prefix parser ([`atof`]).
pub(crate) fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => Some(atof(s)),
        _ => None,
    }
}

/// `!!atoi` on a JSON value: booleans pass through, numbers test non-zero,
/// strings accept `true`/`false`/`1`/`0` (case-insensitive) or a numeric
/// parse (non-zero → true). Anything else is `None` (the caller falls back
/// to `false`, matching `atoi` on garbage).
pub(crate) fn value_to_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Some(i != 0);
            }
            if let Some(u) = n.as_u64() {
                return Some(u != 0);
            }
            n.as_f64().map(|f| f != 0.0)
        }
        Value::String(s) => {
            let t = s.trim();
            let low = t.to_ascii_lowercase();
            match low.as_str() {
                "true" | "1" => return Some(true),
                "false" | "0" => return Some(false),
                _ => {}
            }
            parse_i64_or_f64(t).map(|i| i != 0)
        }
        _ => None,
    }
}

/// A JSON number value; `NaN`/infinite (which JSON cannot represent) are
/// stored as their string form instead.
fn json_number(f: f64) -> Value {
    Number::from_f64(f).map_or_else(|| Value::String(f.to_string()), Value::Number)
}
