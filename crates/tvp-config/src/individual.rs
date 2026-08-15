//! Per-game preferences — the Rust counterpart of `IndividualConfigManager`
//! (`reference/cpp/core/environ/ConfigManager/IndividualConfigManager.{h,cpp}`).
//!
//! The C++ manager stored per-game preferences in `Kirikiroid2Preference.xml`
//! inside the game folder; this port uses `config.json` (see
//! [`crate::INDIVIDUAL_CONFIG_FILE`]) with the same flat JSON shape as the
//! global config.
//!
//! ## Layered lookup (ported from the `IndividualConfigManager::GetValue<T>`
//! specializations)
//!
//! Every lookup first resolves through the global manager — the C++ code
//! evaluates `inherit::GetValue(name, GlobalManager::GetValue(name, defVal))`,
//! so the global read *always* runs, recording defaults into the global tree
//! when keys are missing. Then the individual tree is consulted with the
//! global-resolved value as its default:
//!
//! 1. individual value wins;
//! 2. else the global value;
//! 3. else the built-in [`crate::defaults`] table (or the caller-supplied
//!    default of `get_*_with_default`).
//!
//! The individual file is only written by `save()`/`save_to()`; saving the
//! global config remains a separate call, exactly like the C++ split.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::inner::Inner;
use crate::{ConfigError, GlobalConfig, INDIVIDUAL_CONFIG_FILE};

/// Per-game preference store layered over a shared [`GlobalConfig`] (port of
/// `IndividualConfigManager`).
#[derive(Debug)]
pub struct IndividualConfig<'a> {
    inner: RefCell<Inner>,
    global: &'a GlobalConfig,
    path: RefCell<Option<PathBuf>>,
}

impl<'a> IndividualConfig<'a> {
    /// `UsePreferenceAt(folder)`: bind to `folder/config.json` and load it.
    ///
    /// The C++ returned `false` (and left the manager unbound) when no file
    /// existed; here that case is not an error — you get an empty individual
    /// config, so all lookups fall through to the global/default chain, and
    /// a later save creates the file. Use [`IndividualConfig::check_exist_at`]
    /// for the C++ boolean check.
    pub fn use_preference_at(
        folder: impl AsRef<Path>,
        global: &'a GlobalConfig,
    ) -> Result<Self, ConfigError> {
        let path = folder.as_ref().join(INDIVIDUAL_CONFIG_FILE);
        let inner = Inner::load(&path)?;
        Ok(Self {
            inner: RefCell::new(inner),
            global,
            path: RefCell::new(Some(path)),
        })
    }

    /// `CreatePreferenceAt(folder)`: an empty config bound to
    /// `folder/config.json`. No file is created until the first
    /// `save()`/`save_to()` with a dirty config (the C++ also only writes on
    /// `SaveToFile`).
    pub fn create_preference_at(folder: impl AsRef<Path>, global: &'a GlobalConfig) -> Self {
        let path = folder.as_ref().join(INDIVIDUAL_CONFIG_FILE);
        Self {
            inner: RefCell::new(Inner::new()),
            global,
            path: RefCell::new(Some(path)),
        }
    }

    /// `CheckExistAt(folder)`: does `folder/config.json` exist?
    pub fn check_exist_at(folder: impl AsRef<Path>) -> bool {
        folder.as_ref().join(INDIVIDUAL_CONFIG_FILE).exists()
    }

    /// Load an individual config from an explicit file path.
    pub fn load(path: impl AsRef<Path>, global: &'a GlobalConfig) -> Result<Self, ConfigError> {
        let path = path.as_ref().to_path_buf();
        let inner = Inner::load(&path)?;
        Ok(Self {
            inner: RefCell::new(inner),
            global,
            path: RefCell::new(Some(path)),
        })
    }

    /// The shared global config backing this individual config.
    pub fn global(&self) -> &GlobalConfig {
        self.global
    }

    /// The path this config was loaded from (or last saved to), if any.
    pub fn path(&self) -> Option<PathBuf> {
        self.path.borrow().clone()
    }

    /// Save the *individual* tree to its stored path (see
    /// [`IndividualConfig::save_to`] for the no-op semantics).
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = self.path.borrow().clone().ok_or(ConfigError::NoPath)?;
        self.save_to(path)
    }

    /// Save the *individual* tree to an explicit path; also rebinds the
    /// stored path. A no-op while the config is clean, and it does **not**
    /// save the global config (that is a separate [`GlobalConfig::save`]).
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref().to_path_buf();
        self.inner.borrow_mut().save_to(&path)?;
        self.path.replace(Some(path));
        Ok(())
    }

    /// Individual → global → built-in default (`GetValue<std::string>`).
    pub fn get_string(&self, key: &str) -> String {
        let def = self.global.get_string(key);
        self.inner.borrow_mut().get_string(key, &def)
    }

    /// Individual → global (with caller default) (`GetValue<std::string>`).
    pub fn get_string_with_default(&self, key: &str, def: &str) -> String {
        let def = self.global.get_string_with_default(key, def);
        self.inner.borrow_mut().get_string(key, &def)
    }

    /// Individual → global → built-in default (`GetValue<int>`).
    pub fn get_integer(&self, key: &str) -> i64 {
        let def = self.global.get_integer(key);
        self.inner.borrow_mut().get_integer(key, def)
    }

    /// Individual → global (with caller default) (`GetValue<int>`).
    pub fn get_integer_with_default(&self, key: &str, def: i64) -> i64 {
        let def = self.global.get_integer_with_default(key, def);
        self.inner.borrow_mut().get_integer(key, def)
    }

    /// Individual → global → built-in default (`GetValue<float>`).
    pub fn get_real(&self, key: &str) -> f64 {
        let def = self.global.get_real(key);
        self.inner.borrow_mut().get_real(key, def)
    }

    /// Individual → global (with caller default) (`GetValue<float>`).
    pub fn get_real_with_default(&self, key: &str, def: f64) -> f64 {
        let def = self.global.get_real_with_default(key, def);
        self.inner.borrow_mut().get_real(key, def)
    }

    /// Individual → global → built-in default (`GetValue<bool>`).
    pub fn get_boolean(&self, key: &str) -> bool {
        let def = self.global.get_boolean(key);
        self.inner.borrow_mut().get_boolean(key, def)
    }

    /// Individual → global (with caller default) (`GetValue<bool>`).
    pub fn get_boolean_with_default(&self, key: &str, def: bool) -> bool {
        let def = self.global.get_boolean_with_default(key, def);
        self.inner.borrow_mut().get_boolean(key, def)
    }

    /// Set a value in the *individual* tree only.
    pub fn set_string(&self, key: &str, val: &str) {
        self.inner.borrow_mut().set_string(key, val);
    }

    /// Set an integer in the *individual* tree only.
    pub fn set_integer(&self, key: &str, val: i64) {
        self.inner.borrow_mut().set_integer(key, val);
    }

    /// Set a float in the *individual* tree only.
    pub fn set_real(&self, key: &str, val: f64) {
        self.inner.borrow_mut().set_real(key, val);
    }

    /// Set a boolean in the *individual* tree only.
    pub fn set_boolean(&self, key: &str, val: bool) {
        self.inner.borrow_mut().set_boolean(key, val);
    }

    /// `IsValueExist` on the *individual* tree only (per-manager, like C++).
    pub fn is_value_exist(&self, key: &str) -> bool {
        self.inner.borrow().contains_key(key)
    }

    /// Remove a key from the *individual* tree; returns whether it was
    /// present.
    pub fn remove(&self, key: &str) -> bool {
        self.inner.borrow_mut().remove(key)
    }

    /// Snapshot of the individual key tree.
    pub fn values(&self) -> Map<String, Value> {
        self.inner.borrow().values().clone()
    }

    /// Whether the individual tree was modified since the last save.
    pub fn is_dirty(&self) -> bool {
        self.inner.borrow().dirty
    }

    /// Number of keys in the individual tree.
    pub fn len(&self) -> usize {
        self.inner.borrow().len()
    }

    /// Whether the individual tree is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.borrow().is_empty()
    }
}

/// Convenience: the per-game config file path (see [`INDIVIDUAL_CONFIG_FILE`]).
pub fn individual_config_path(game_dir: impl AsRef<Path>) -> PathBuf {
    game_dir.as_ref().join(INDIVIDUAL_CONFIG_FILE)
}
