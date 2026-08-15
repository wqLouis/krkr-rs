//! Localized message catalogs — the Rust counterpart of
//! `LocaleConfigManager` (`reference/cpp/core/environ/ConfigManager/LocaleConfigManager.{h,cpp}`).
//!
//! The C++ manager loaded `locale/<lang>.xml`, a flat list of
//! `<Item id="..." text="..."/>` elements under `<Locale lang="...">`. This
//! port uses `locale/<lang>.json` with the same structure (the root `lang`
//! attribute is ignored, exactly like the C++ reader):
//!
//! ```json
//! {
//!   "lang": "zh_cn",
//!   "items": [
//!     { "id": "preference_title", "text": "全局设置" },
//!     { "id": "preference_output_log", "text": "打印日志" }
//!   ]
//! }
//! ```
//!
//! For convenience a flat `{ "tid": "text", ... }` object is also accepted
//! (a top-level `lang`/`items` key is treated as metadata).
//!
//! ## Language selection and fallback (ported faithfully)
//!
//! * [`LocaleConfig::load_with_global`] mirrors `Initialize(sysLang)`: the
//!   global `user_language` preference wins; if empty, `sysLang` is used.
//! * [`LocaleConfig::load`] takes the language directly.
//! * `GetFilePath()` logic: if `locale/<lang>.json` does **not** exist, the
//!   language falls back to `en_us` (the "default language config, must
//!   exist" in the C++ comment). If even that is missing, the catalog is
//!   empty — no panic (the C++ would recurse forever here; the port does
//!   not).
//! * [`LocaleConfig::get_text`]: the C++ `GetText` returned the tid itself
//!   for a missing key. This port returns `Option<String>` instead: `None`
//!   when missing, after first trying the default-language catalog when the
//!   active catalog is a different language (a documented extension — the
//!   C++ only ever consults the one chosen catalog).

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use serde_json::{Map, Value};

use crate::{ConfigError, DEFAULT_LANGUAGE, GlobalConfig, LOCALE_DIR};

/// A loaded message catalog for one language.
#[derive(Debug)]
pub struct LocaleConfig {
    /// The language code in effect after file-fallback resolution.
    lang: String,
    /// tid → text for the active language.
    catalog: HashMap<String, String>,
    /// The default-language (`en_us`) catalog, loaded when the active
    /// language differs.
    fallback: Option<HashMap<String, String>>,
}

impl LocaleConfig {
    /// Load `locale/<lang>.json` under `base_dir`, falling back to the
    /// default language file when it is missing.
    pub fn load(base_dir: impl AsRef<Path>, language: &str) -> Result<Self, ConfigError> {
        Self::load_impl(base_dir.as_ref(), language)
    }

    /// `Initialize(sysLang)`: the global `user_language` preference overrides
    /// `sysLang`; an empty preference falls back to `sysLang`.
    ///
    /// Note that, like the C++ `GetValue<std::string>("user_language", "")`,
    /// a missing `user_language` is recorded into the global config (marking
    /// it dirty).
    pub fn load_with_global(
        base_dir: impl AsRef<Path>,
        sys_lang: &str,
        global: &GlobalConfig,
    ) -> Result<Self, ConfigError> {
        let lang = global.get_string("user_language");
        let lang = if lang.is_empty() {
            sys_lang
        } else {
            lang.as_str()
        };
        Self::load_impl(base_dir.as_ref(), lang)
    }

    /// The language code in effect (after fallback to the default language).
    pub fn lang(&self) -> &str {
        &self.lang
    }

    /// Whether the active language is the default language (`en_us`).
    pub fn is_default_language(&self) -> bool {
        self.lang == DEFAULT_LANGUAGE
    }

    /// Number of entries in the active catalog.
    pub fn len(&self) -> usize {
        self.catalog.len()
    }

    /// Whether the active catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.catalog.is_empty()
    }

    /// `GetText(tid)`: the localized text for the active language.
    ///
    /// Missing keys fall back to the default-language catalog when the
    /// active language differs, then to `None` (the C++ returned the tid
    /// itself — call `.unwrap_or_else(|| key.to_owned())` for that).
    pub fn get_text(&self, key: &str) -> Option<String> {
        if let Some(text) = self.catalog.get(key) {
            return Some(text.clone());
        }
        if let Some(fallback) = &self.fallback
            && let Some(text) = fallback.get(key)
        {
            return Some(text.clone());
        }
        None
    }

    fn load_impl(base_dir: &Path, language: &str) -> Result<Self, ConfigError> {
        let mut lang = language.to_owned();
        let mut path = base_dir.join(LOCALE_DIR).join(format!("{lang}.json"));
        // GetFilePath(): a missing language file restores the default
        // language config (which "must exist" in the C++ package).
        if !path.exists() && lang != DEFAULT_LANGUAGE {
            lang = DEFAULT_LANGUAGE.to_owned();
            path = base_dir.join(LOCALE_DIR).join(format!("{lang}.json"));
        }
        let catalog = load_catalog(&path)?;
        let fallback = if lang != DEFAULT_LANGUAGE {
            let fallback_path = base_dir
                .join(LOCALE_DIR)
                .join(format!("{DEFAULT_LANGUAGE}.json"));
            Some(load_catalog(&fallback_path)?)
        } else {
            None
        };
        Ok(Self {
            lang,
            catalog,
            fallback,
        })
    }
}

/// Read one catalog file. A missing (or empty) file is an empty catalog; a
/// malformed file or a non-object root is an error.
fn load_catalog(path: &Path) -> Result<HashMap<String, String>, ConfigError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(HashMap::new());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    match value {
        Value::Object(map) => Ok(catalog_from_object(&map)),
        _ => Err(ConfigError::NotAnObject {
            path: path.to_path_buf(),
        }),
    }
}

/// Extract tid → text from a catalog object.
///
/// Entries are the `<Item id text/>` counterpart: an array of
/// `{ "id", "text" }` objects (entries missing either field are skipped,
/// like the C++ `if (key && val)`), or a flat `{ tid: text }` object.
fn catalog_from_object(map: &Map<String, Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    match map.get("items") {
        Some(Value::Array(items)) => {
            for item in items {
                if let Some((id, text)) = item_id_text(item) {
                    out.insert(id, text);
                }
            }
        }
        Some(Value::Object(flat)) => {
            for (id, text) in flat {
                if let Some(text) = value_text(text) {
                    out.insert(id.clone(), text);
                }
            }
        }
        // Form 2: a flat { tid: text } object (the `lang` key is metadata).
        _ => {
            for (id, text) in map {
                if id == "lang" {
                    continue;
                }
                if let Some(text) = value_text(text) {
                    out.insert(id.clone(), text);
                }
            }
        }
    }
    out
}

/// `{ "id": ..., "text": ... }` → `(id, text)`, or `None` if either field is
/// unusable (mirrors the C++ `if (key && val)` skip).
fn item_id_text(item: &Value) -> Option<(String, String)> {
    let obj = item.as_object()?;
    let id = obj.get("id")?.as_str()?;
    let text = value_text(obj.get("text")?)?;
    Some((id.to_owned(), text))
}

/// A scalar value as catalog text (strings pass through; numbers/booleans
/// are stringified).
fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
