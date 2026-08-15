//! Global preferences — the Rust counterpart of `GlobalConfigManager`
//! (`reference/cpp/core/environ/ConfigManager/GlobalConfigManager.{h,cpp}`).
//!
//! The C++ manager stored preferences in `GlobalPreference.xml` (a flat list
//! of `<Item key="..." value="..."/>` under `<GlobalPreference>`); this port
//! uses a flat JSON object named `config.json` (see
//! [`crate::GLOBAL_CONFIG_FILE`]) in the preference directory:
//!
//! ```json
//! {
//!   "outputlog": true,
//!   "fps_limit": 60,
//!   "menu_handler_opa": 0.15,
//!   "renderer": "software",
//!   "user_language": "zh_cn"
//! }
//! ```
//!
//! Schema-light like the C++ key tree: any top-level key is allowed and
//! unknown keys round-trip untouched. Values may be typed scalars or
//! string-encoded scalars.
//!
//! ## Lookup semantics (ported from `iSysConfigManager::GetValue<T>`)
//!
//! * A present key converts to the requested type (`atoi`/`atof` semantics;
//!   unparseable values yield `0`/`0.0`/`false`, not the default).
//! * A **missing** key records the default into the tree (marking the config
//!   dirty, exactly like the C++ `GetValue`, which calls `SetValue`) and
//!   returns it.
//! * `get_*` falls back to the built-in [`crate::defaults`] table;
//!   `get_*_with_default` uses a caller-supplied default like the C++ call
//!   sites do.
//!
//! `save()`/`save_to()` mirror `SaveToFile`: they are no-ops while the
//! config is clean, and a successful save clears the dirty flag.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::defaults;
use crate::inner::Inner;
use crate::{ConfigError, GLOBAL_CONFIG_FILE};

/// Global preference store (port of `GlobalConfigManager`).
///
/// Methods take `&self`: the key tree and the bound file path live behind
/// interior mutability so a single shared `GlobalConfig` can back several
/// [`crate::IndividualConfig`]s and a [`crate::LocaleConfig`], mirroring the
/// C++ singletons.
#[derive(Debug)]
pub struct GlobalConfig {
    inner: RefCell<Inner>,
    path: RefCell<Option<PathBuf>>,
}

impl GlobalConfig {
    /// An empty, clean config not bound to any file (like the C++ manager
    /// right after construction, before `Initialize` finds anything).
    pub fn new() -> Self {
        Self {
            inner: RefCell::new(Inner::new()),
            path: RefCell::new(None),
        }
    }

    /// Load the config from a JSON file.
    ///
    /// A missing or empty file yields an empty config (no error, no panic);
    /// malformed JSON or a non-object root is an error.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref().to_path_buf();
        let inner = Inner::load(&path)?;
        Ok(Self {
            inner: RefCell::new(inner),
            path: RefCell::new(Some(path)),
        })
    }

    /// The path this config was loaded from (or last saved to), if any.
    pub fn path(&self) -> Option<PathBuf> {
        self.path.borrow().clone()
    }

    /// `SaveToFile`: write the config back to its stored path.
    ///
    /// A no-op when the config is clean (C++ `ConfigUpdated` semantics).
    /// Returns [`ConfigError::NoPath`] if the config has no path and was
    /// never saved — use [`GlobalConfig::save_to`] for an explicit path.
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = self.path.borrow().clone().ok_or(ConfigError::NoPath)?;
        self.save_to(path)
    }

    /// `SaveToFile` at an explicit path (the `save(path)` entry point).
    ///
    /// Also rebinds the stored path, so later [`GlobalConfig::save`] calls
    /// use it. A no-op when the config is clean.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref().to_path_buf();
        self.inner.borrow_mut().save_to(&path)?;
        self.path.replace(Some(path));
        Ok(())
    }

    /// `GetValue<std::string>` with the built-in default.
    pub fn get_string(&self, key: &str) -> String {
        let def = defaults::default_string(key).unwrap_or_default();
        self.get_string_with_default(key, &def)
    }

    /// `GetValue<std::string>` with a caller-supplied default.
    pub fn get_string_with_default(&self, key: &str, def: &str) -> String {
        self.inner.borrow_mut().get_string(key, def)
    }

    /// `GetValue<int>` with the built-in default.
    pub fn get_integer(&self, key: &str) -> i64 {
        let def = defaults::default_integer(key).unwrap_or(0);
        self.get_integer_with_default(key, def)
    }

    /// `GetValue<int>` with a caller-supplied default.
    pub fn get_integer_with_default(&self, key: &str, def: i64) -> i64 {
        self.inner.borrow_mut().get_integer(key, def)
    }

    /// `GetValue<float>` with the built-in default.
    pub fn get_real(&self, key: &str) -> f64 {
        let def = defaults::default_real(key).unwrap_or(0.0);
        self.get_real_with_default(key, def)
    }

    /// `GetValue<float>` with a caller-supplied default.
    pub fn get_real_with_default(&self, key: &str, def: f64) -> f64 {
        self.inner.borrow_mut().get_real(key, def)
    }

    /// `GetValue<bool>` with the built-in default.
    pub fn get_boolean(&self, key: &str) -> bool {
        let def = defaults::default_boolean(key).unwrap_or(false);
        self.get_boolean_with_default(key, def)
    }

    /// `GetValue<bool>` with a caller-supplied default.
    pub fn get_boolean_with_default(&self, key: &str, def: bool) -> bool {
        self.inner.borrow_mut().get_boolean(key, def)
    }

    /// `SetValue`.
    pub fn set_string(&self, key: &str, val: &str) {
        self.inner.borrow_mut().set_string(key, val);
    }

    /// `SetValueInt`.
    pub fn set_integer(&self, key: &str, val: i64) {
        self.inner.borrow_mut().set_integer(key, val);
    }

    /// `SetValueFloat`.
    pub fn set_real(&self, key: &str, val: f64) {
        self.inner.borrow_mut().set_real(key, val);
    }

    /// `SetValueInt` for a boolean (`PreferenceSetValueBool`).
    pub fn set_boolean(&self, key: &str, val: bool) {
        self.inner.borrow_mut().set_boolean(key, val);
    }

    /// `IsValueExist`: is the key present in *this* config's tree?
    pub fn is_value_exist(&self, key: &str) -> bool {
        self.inner.borrow().contains_key(key)
    }

    /// Remove a key; returns whether it was present.
    pub fn remove(&self, key: &str) -> bool {
        self.inner.borrow_mut().remove(key)
    }

    /// Snapshot of the whole key tree (schema-light introspection).
    pub fn values(&self) -> Map<String, Value> {
        self.inner.borrow().values().clone()
    }

    /// Whether any value was written (or a default recorded) since the last
    /// save — the port of the C++ `ConfigUpdated` flag.
    pub fn is_dirty(&self) -> bool {
        self.inner.borrow().dirty
    }

    /// Number of keys in the tree.
    pub fn len(&self) -> usize {
        self.inner.borrow().len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.borrow().is_empty()
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience: the default file name for global preferences.
///
/// The C++ used `GlobalPreference.xml`; the JSON port uses `config.json`
/// (see [`GLOBAL_CONFIG_FILE`]).
pub fn global_config_path(pref_dir: impl AsRef<Path>) -> PathBuf {
    pref_dir.as_ref().join(GLOBAL_CONFIG_FILE)
}
