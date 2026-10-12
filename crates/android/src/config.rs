//! Resolves the settings the launcher published into a validated
//! [`RuntimeConfig`].
//!
//! `KrkrGameActivity` persists the user's choices in its own settings file and
//! publishes them as the static string fields `pendingFrameRate` and
//! `pendingScaling` before `super.onCreate` enters native code (see
//! [`crate::jni_fields`]). This module is where those raw strings become the
//! one validated value the emulator receives, so the launcher keeps owning
//! configuration loading and the core does no configuration I/O of its own.
//!
//! Each option is optional: an unset field means the launcher had no override
//! and the built-in default applies. A present but unparseable or out-of-range
//! value is a typed [`ConfigError`] naming the option and the accepted range;
//! it is never silently replaced by the default (D11).
//!
//! The Android launcher owns its own default resolution. The frame-rate default
//! is the shared built-in (60 fps). The presentation default is
//! [`PresentationScaling::Fit`], not the desktop's `Game`: a phone must never
//! present the raw logical size, and the picture keeps its aspect rather than
//! being stretched (`docs/android.md` §2.2). The user's stored choice overrides
//! either default.

use api::config::{ConfigError, FrameRateLimit, PresentationScaling, RuntimeConfig};

/// Configuration-key name used in frame-rate errors.
const OPTION_FRAME_RATE: &str = "frame_rate";
/// Configuration-key name used in scaling errors.
const OPTION_SCALING: &str = "scaling";

/// Resolves the launcher's published settings over the Android defaults.
///
/// `frame_rate` and `scaling` are the raw strings `KrkrGameActivity` published;
/// `None` means the field was unset (or empty) and its default applies. A value
/// that fails validation is returned as a [`ConfigError`] naming the option and
/// its accepted range — it is never defaulted.
///
/// # Errors
///
/// [`ConfigError::InvalidValue`] when a present value is unparseable or outside
/// its accepted range.
pub(crate) fn resolve_runtime_config(
    frame_rate: Option<&str>,
    scaling: Option<&str>,
) -> Result<RuntimeConfig, ConfigError> {
    let defaults = RuntimeConfig {
        scaling: PresentationScaling::Fit,
        ..RuntimeConfig::default()
    };
    let frame_rate_limit = match frame_rate {
        Some(value) => FrameRateLimit::parse(OPTION_FRAME_RATE, value)?,
        None => defaults.frame_rate_limit,
    };
    let scaling = match scaling {
        Some(value) => PresentationScaling::parse(OPTION_SCALING, value)?,
        None => defaults.scaling,
    };
    Ok(RuntimeConfig {
        frame_rate_limit,
        scaling,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_fields_use_the_android_defaults() {
        // The reference frame-rate default is 60 fps
        // (`reference/cpp/core/environ/ui/PreferenceConfig.h:304-309`). The
        // `Fit` presentation default is the documented Android divergence
        // (`docs/android.md` §2.2): a phone never presents the raw logical
        // size.
        let runtime = resolve_runtime_config(None, None).expect("defaults");
        assert_eq!(runtime.frame_rate_limit.fps(), 60);
        assert_eq!(runtime.scaling, PresentationScaling::Fit);
    }

    #[test]
    fn present_values_override_the_defaults() {
        let runtime = resolve_runtime_config(Some("30"), Some("game")).expect("valid");
        assert_eq!(runtime.frame_rate_limit.fps(), 30);
        assert_eq!(runtime.scaling, PresentationScaling::Game);
    }

    #[test]
    fn each_field_falls_back_to_its_own_default() {
        let runtime = resolve_runtime_config(Some("120"), None).expect("valid");
        assert_eq!(runtime.frame_rate_limit.fps(), 120);
        assert_eq!(runtime.scaling, PresentationScaling::Fit);

        let runtime = resolve_runtime_config(None, Some("fill")).expect("valid");
        assert_eq!(runtime.frame_rate_limit.fps(), 60);
        assert_eq!(runtime.scaling, PresentationScaling::Fill);
    }

    #[test]
    fn out_of_range_values_are_rejected_with_the_option_and_range() {
        let error = resolve_runtime_config(Some("0"), None)
            .expect_err("0 is out of range")
            .to_string();
        assert!(error.contains("frame_rate"), "{error}");
        assert!(error.contains("1..=1000"), "{error}");

        let error = resolve_runtime_config(None, Some("stretch"))
            .expect_err("unknown scaling")
            .to_string();
        assert!(error.contains("scaling"), "{error}");
        assert!(error.contains("game, fit, fill, expand"), "{error}");
    }

    #[test]
    fn an_invalid_field_is_rejected_even_when_the_other_is_valid() {
        assert!(resolve_runtime_config(Some("30"), Some("stretch")).is_err());
        assert!(resolve_runtime_config(Some("fast"), Some("fit")).is_err());
    }
}
