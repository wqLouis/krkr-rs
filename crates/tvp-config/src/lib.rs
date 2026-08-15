//! tvp-config — Rust port of the reference engine's ConfigManager subsystem.
//!
//! The C++ sources live in `reference/cpp/core/environ/ConfigManager/`:
//!
//! * `GlobalConfigManager.{h,cpp}` — global preferences, a key/value store
//!   with typed accessors and built-in defaults ([`GlobalConfig`]).
//! * `IndividualConfigManager.{h,cpp}` — per-game preferences layered over
//!   the global ones ([`IndividualConfig`]).
//! * `LocaleConfigManager.{h,cpp}` — localized message catalogs
//!   ([`LocaleConfig`]).
//!
//! The C++ implementation stored everything as XML (`GlobalPreference.xml`,
//! `Kirikiroid2Preference.xml`, `locale/<lang>.xml`). This port keeps the
//! same semantics but uses JSON:
//!
//! | C++ file                    | JSON port                     |
//! |-----------------------------|-------------------------------|
//! | `GlobalPreference.xml`      | [`GLOBAL_CONFIG_FILE`] (`config.json`, preference dir) |
//! | `Kirikiroid2Preference.xml` | [`INDIVIDUAL_CONFIG_FILE`] (`config.json`, game dir)    |
//! | `locale/<lang>.xml`         | `locale/<lang>.json`          |
//!
//! ## Global / individual config files
//!
//! A flat JSON object mapping keys to scalar values — schema-light, like the
//! C++ generic key tree (which stored every value as a string). Values may
//! be typed scalars or string-encoded scalars:
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
//! ## Locale catalog files
//!
//! `locale/<lang>.json`, mirroring the XML `<Locale lang><Item id text/>`:
//!
//! ```json
//! {
//!   "lang": "zh_cn",
//!   "items": [
//!     { "id": "preference_title", "text": "全局设置" }
//!   ]
//! }
//! ```
//!
//! (a flat `{ "tid": "text" }` object is also accepted).
//!
//! See the module docs for the exact lookup semantics; see [`defaults`] for
//! the built-in default table extracted from the C++ call sites.

pub mod defaults;
pub mod global;
pub mod individual;
pub mod locale;

mod inner;

pub use defaults::{
    DefaultValue, default_boolean, default_for, default_integer, default_real, default_string,
};
pub use global::{GlobalConfig, global_config_path};
pub use individual::{IndividualConfig, individual_config_path};
pub use locale::LocaleConfig;

/// Default file name for the global preference file.
///
/// The C++ used `GlobalPreference.xml`; the JSON port uses `config.json`
/// (kept in the preference directory).
pub const GLOBAL_CONFIG_FILE: &str = "config.json";

/// Default file name for the per-game preference file.
///
/// The C++ used `Kirikiroid2Preference.xml`; the JSON port uses `config.json`
/// (kept in the game directory).
pub const INDIVIDUAL_CONFIG_FILE: &str = "config.json";

/// The fallback language code for locale catalogs ("must exist" in the C++
/// package — `GetFilePath` restores to it when the chosen language file is
/// missing).
pub const DEFAULT_LANGUAGE: &str = "en_us";

/// Directory (under the game/app base dir) holding the locale catalogs.
pub const LOCALE_DIR: &str = "locale";

/// Errors produced by the config managers.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Reading or writing a config/catalog file failed at the OS level.
    #[error("failed to read config file `{path}`: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid JSON.
    #[error("failed to parse config file `{path}`: {source}")]
    Parse {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The file is valid JSON but its root is not an object.
    #[error("config file `{path}` is not a JSON object at the top level")]
    NotAnObject { path: std::path::PathBuf },
    /// `save()` was called on a config that has no bound path.
    #[error("no file path configured for this config; use `save_to` with an explicit path")]
    NoPath,
    /// Serializing the config tree failed.
    #[error("failed to serialize config: {0}")]
    Serialize(#[from] serde_json::Error),
}
