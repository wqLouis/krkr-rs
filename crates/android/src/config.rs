//! Resolves the settings the launcher published into a validated
//! [`RuntimeConfig`].
//!
//! `KrkrGameActivity` persists the user's choices in its own settings file and
//! publishes them as the static string fields `pendingFrameRate`,
//! `pendingPreloadArchives`, `pendingDebugOverlay` and `pendingLogLevel` before
//! `super.onCreate` enters native code (see [`crate::jni_fields`]). This module
//! is where those raw strings become the one validated value the emulator
//! receives, so the launcher keeps owning configuration loading and the core
//! does no configuration I/O of its own.
//!
//! Each option is optional: an unset field means the launcher had no override
//! and the built-in default applies. A present but unparseable or out-of-range
//! value is a typed [`ConfigError`] naming the option and the accepted values;
//! it is never silently replaced by the default.
//!
//! The Android launcher owns its own default resolution, and every default is
//! the shared built-in: the frame-rate default is 60 fps, the preload default
//! is `false`, the debug-overlay default is `false`, and the log-level default
//! is `info`, which omits the VM's diagnostic detail. The user's stored choice
//! overrides each default.

use api::config::{ConfigError, FrameRateLimit, RuntimeConfig, parse_bool};

/// Configuration-key name used in frame-rate errors.
const OPTION_FRAME_RATE: &str = "frame_rate";
/// Configuration-key name used in archive-preload errors.
const OPTION_PRELOAD_ARCHIVES: &str = "preload_archives";
/// Configuration-key name used in debug-overlay errors.
const OPTION_DEBUG_OVERLAY: &str = "debug_overlay";
/// Configuration-key name used in log-level errors.
const OPTION_LOG_LEVEL: &str = "log_level";

/// A log-level filter the launcher selects for the engine logger.
///
/// The names are the ones `env_logger`'s `RUST_LOG` filter accepts, so the
/// launcher value and an environment override select the same thing. `Off`
/// silences every record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LogLevel {
    /// Every record is discarded.
    Off,
    /// Only failures.
    Error,
    /// Failures and recoverable problems.
    Warn,
    /// Failures, warnings and normal lifecycle records.
    Info,
    /// Adds diagnostic records, including the TJS VM's exception dumps.
    Debug,
    /// Adds per-frame and per-value tracing.
    Trace,
}

impl LogLevel {
    /// The level used when the launcher publishes no value.
    ///
    /// This is a built-in default, not a fallback that hides a failure: an
    /// unset field means the launcher had no override, which is different
    /// from a present-but-invalid value.
    pub(crate) const DEFAULT: Self = Self::Info;

    /// Human-readable accepted values, as written in errors.
    pub(crate) const ACCEPTED: &'static str = "one of: off, error, warn, info, debug, trace";

    /// Parses a level name.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidValue`] when `value` is not one of the accepted
    /// names.
    pub(crate) fn parse(option: &'static str, value: &str) -> Result<Self, ConfigError> {
        match value {
            "off" => Ok(Self::Off),
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            other => Err(ConfigError::invalid(
                option,
                other.to_owned(),
                Self::ACCEPTED,
            )),
        }
    }

    /// The filter string this level selects.
    pub(crate) fn as_filter(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// Resolves the launcher's log level over the built-in default.
///
/// `None` means the launcher published no value, so the documented default
/// [`LogLevel::DEFAULT`] applies. A present value that is not one of
/// [`LogLevel::ACCEPTED`] is a typed [`ConfigError`] naming the option and the
/// accepted values — it is never silently replaced by the default.
///
/// # Errors
///
/// [`ConfigError::InvalidValue`] when `value` is not an accepted level name.
pub(crate) fn resolve_log_level(value: Option<&str>) -> Result<LogLevel, ConfigError> {
    match value {
        Some(value) => LogLevel::parse(OPTION_LOG_LEVEL, value),
        None => Ok(LogLevel::DEFAULT),
    }
}

/// Resolves the launcher's published settings over the Android defaults.
///
/// Each argument is a raw string `KrkrGameActivity` published; `None` means the
/// field was unset (or empty) and its default applies. A value that fails
/// validation is returned as a [`ConfigError`] naming the option and its
/// accepted values — it is never defaulted.
///
/// # Errors
///
/// [`ConfigError::InvalidValue`] when a present value is unparseable or outside
/// its accepted range.
pub(crate) fn resolve_runtime_config(
    frame_rate: Option<&str>,
    preload_archives: Option<&str>,
    debug_overlay: Option<&str>,
) -> Result<RuntimeConfig, ConfigError> {
    let defaults = RuntimeConfig::default();
    let frame_rate_limit = match frame_rate {
        Some(value) => FrameRateLimit::parse(OPTION_FRAME_RATE, value)?,
        None => defaults.frame_rate_limit,
    };
    let preload_archives = match preload_archives {
        Some(value) => parse_bool(OPTION_PRELOAD_ARCHIVES, value)?,
        None => defaults.preload_archives,
    };
    let debug_overlay = match debug_overlay {
        Some(value) => parse_bool(OPTION_DEBUG_OVERLAY, value)?,
        None => defaults.debug_overlay,
    };
    Ok(RuntimeConfig {
        frame_rate_limit,
        preload_archives,
        debug_overlay,
        graphic_cache_limit: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_fields_use_the_android_defaults() {
        // The reference frame-rate default is 60 fps
        // (`reference/cpp/core/environ/ui/PreferenceConfig.h:304-309`).
        // Archives are not preloaded, the shared built-in default
        // (`docs/runtime-config.md` §2); the debug overlay is off by default.
        let runtime = resolve_runtime_config(None, None, None).expect("defaults");
        assert_eq!(runtime.frame_rate_limit.fps(), 60);
        assert!(!runtime.preload_archives);
        assert!(!runtime.debug_overlay);
    }

    #[test]
    fn present_values_override_the_defaults() {
        let runtime =
            resolve_runtime_config(Some("30"), Some("true"), Some("true")).expect("valid");
        assert_eq!(runtime.frame_rate_limit.fps(), 30);
        assert!(runtime.preload_archives);
        assert!(runtime.debug_overlay);
    }

    #[test]
    fn each_field_falls_back_to_its_own_default() {
        let runtime = resolve_runtime_config(Some("120"), None, None).expect("valid");
        assert_eq!(runtime.frame_rate_limit.fps(), 120);
        assert!(!runtime.preload_archives);
        assert!(!runtime.debug_overlay);

        let runtime = resolve_runtime_config(None, Some("false"), None).expect("valid");
        assert!(!runtime.preload_archives);

        // The overlay falls back independently of the options beside it.
        let runtime = resolve_runtime_config(None, None, Some("true")).expect("valid");
        assert!(runtime.debug_overlay);
        assert!(!runtime.preload_archives);
    }

    /// The overlay resolves on and off like the other booleans; an unset value
    /// means the launcher had no override and the shared default (`false`)
    /// applies, while an unparseable one is rejected with its option name.
    #[test]
    fn the_debug_overlay_resolves_on_and_off() {
        let on = resolve_runtime_config(None, None, Some("true")).expect("true is valid");
        assert!(on.debug_overlay);
        let off = resolve_runtime_config(None, None, Some("false")).expect("false is valid");
        assert!(!off.debug_overlay);
        let unset = resolve_runtime_config(None, None, None).expect("defaults");
        assert!(!unset.debug_overlay);
    }

    #[test]
    fn out_of_range_values_are_rejected_with_the_option_and_range() {
        let error = resolve_runtime_config(Some("0"), None, None)
            .expect_err("0 is out of range")
            .to_string();
        assert!(error.contains("frame_rate"), "{error}");
        assert!(error.contains("1..=1000"), "{error}");

        let error = resolve_runtime_config(None, Some("yes"), None)
            .expect_err("unknown preload value")
            .to_string();
        assert!(error.contains("preload_archives"), "{error}");
        assert!(error.contains("true, false"), "{error}");

        let error = resolve_runtime_config(None, None, Some("yes"))
            .expect_err("unknown debug-overlay value")
            .to_string();
        assert!(error.contains("debug_overlay"), "{error}");
        assert!(error.contains("true, false"), "{error}");
    }

    #[test]
    fn an_invalid_field_is_rejected_even_when_the_others_are_valid() {
        assert!(resolve_runtime_config(Some("30"), Some("true"), Some("true")).is_ok());
        assert!(resolve_runtime_config(Some("fast"), Some("false"), None).is_err());
        assert!(resolve_runtime_config(Some("60"), Some("yes"), None).is_err());
        assert!(resolve_runtime_config(Some("60"), None, Some("on")).is_err());
    }

    #[test]
    fn every_log_level_name_parses_to_its_filter() {
        for (text, expected) in [
            ("off", LogLevel::Off),
            ("error", LogLevel::Error),
            ("warn", LogLevel::Warn),
            ("info", LogLevel::Info),
            ("debug", LogLevel::Debug),
            ("trace", LogLevel::Trace),
        ] {
            let level = resolve_log_level(Some(text)).expect("accepted level");
            assert_eq!(level, expected);
            // The filter string the logger receives is the name the launcher
            // chose, so the setting and `RUST_LOG` select the same thing.
            assert_eq!(level.as_filter(), text);
        }
    }

    #[test]
    fn an_unset_log_level_takes_the_documented_default() {
        // An unset field means the launcher had no override; the engine stays
        // at `info`, which omits the VM's per-frame and exception-dump detail.
        assert_eq!(resolve_log_level(None).expect("default"), LogLevel::Info);
        assert_eq!(LogLevel::DEFAULT, LogLevel::Info);
    }

    #[test]
    fn an_unknown_log_level_is_rejected_with_the_option_and_every_accepted_value() {
        let error = resolve_log_level(Some("verbose")).expect_err("unknown level");
        let message = error.to_string();
        assert!(message.contains("log_level"), "{message}");
        for level in ["off", "error", "warn", "info", "debug", "trace"] {
            assert!(
                message.contains(level),
                "the rejected-value error omits '{level}': {message}"
            );
        }
    }

    /// The accepted-values text names every level, so a level cannot be added
    /// without the error message naming it.
    #[test]
    fn the_accepted_log_level_text_names_every_level() {
        for level in [
            LogLevel::Off,
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ] {
            assert!(
                LogLevel::ACCEPTED.contains(level.as_filter()),
                "the accepted-values text omits {:?}: {}",
                level,
                LogLevel::ACCEPTED
            );
        }
    }
}
