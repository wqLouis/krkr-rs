//! Launch-configuration DTOs.
//!
//! [`RuntimeConfig`] is the one resolved value a launcher passes to the
//! emulator. It is produced with the precedence **launch argument > platform
//! configuration > built-in default**, but the loading belongs to the
//! launcher: the desktop CLI reads a `key=value` file through
//! [`parse_overrides`], an Android launcher reads its own store, and both
//! hand the finished value to the same entry point. Nothing here reads a
//! file or knows a platform.
//!
//! Every field is validated. An unparseable or out-of-range value is a typed
//! [`ConfigError`] naming the option and the accepted range — it is never
//! clamped or defaulted. [`RuntimeConfigOverrides`] is the partial shape a
//! parser or launcher holds before the built-in defaults are filled in.

use std::num::NonZeroU64;

use thiserror::Error;

/// The bound on a decoded-image cache.
///
/// [`Self::Disabled`] stores nothing. [`Self::Unlimited`] keeps every entry.
/// [`Self::Bytes`] evicts least-recently-used entries until the cached bytes
/// fit the bound. The byte count is the sum of the decoded RGBA buffers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CacheLimit {
    /// Do not cache decoded images.
    #[default]
    Disabled,
    /// Keep every decoded image.
    Unlimited,
    /// Keep at most this many decoded bytes.
    Bytes(NonZeroU64),
}

impl CacheLimit {
    /// A byte limit; zero means unlimited.
    #[must_use]
    pub fn bytes(limit: u64) -> Self {
        NonZeroU64::new(limit).map_or(Self::Unlimited, Self::Bytes)
    }

    /// Whether the cache stores entries.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// The byte bound, or `None` when caching is disabled. An unlimited cache
    /// reports `Some(0)`.
    #[must_use]
    pub const fn option(self) -> Option<u64> {
        match self {
            Self::Disabled => None,
            Self::Unlimited => Some(0),
            Self::Bytes(bytes) => Some(bytes.get()),
        }
    }

    /// Parses `none`/`off`/`disabled` as no cache, `unlimited` or `0` as an
    /// unlimited cache, or a positive byte count.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidValue`] when the value is not one of those forms.
    pub fn parse(option: &'static str, value: &str) -> Result<Self, ConfigError> {
        let value = value.trim();
        let normalized = value.to_ascii_lowercase();
        match normalized.as_str() {
            "none" | "off" | "disabled" => Ok(Self::Disabled),
            "unlimited" => Ok(Self::Unlimited),
            _ => value.parse::<u64>().map(Self::bytes).map_err(|_| {
                ConfigError::invalid(
                    option,
                    value.to_owned(),
                    "none, unlimited, 0, or a non-negative byte count",
                )
            }),
        }
    }
}

impl From<Option<u64>> for CacheLimit {
    fn from(limit: Option<u64>) -> Self {
        match limit {
            None => Self::Disabled,
            Some(0) => Self::Unlimited,
            Some(bytes) => Self::bytes(bytes),
        }
    }
}

impl From<CacheLimit> for Option<u64> {
    fn from(limit: CacheLimit) -> Self {
        limit.option()
    }
}

/// Lowest accepted frame-rate limit, in frames per second.
///
/// A lower limit has no positive frame period to pace the loop with.
pub(crate) const MIN_FRAME_RATE: u32 = 1;

/// Highest accepted frame-rate limit, in frames per second.
///
/// 1000 fps is the point at which the frame period reaches the platform
/// ticker's 1 ms minimum frame interval; a higher limit would have to be
/// silently clamped, so it is rejected instead.
pub(crate) const MAX_FRAME_RATE: u32 = 1000;

/// Built-in default frame-rate limit, in frames per second.
pub(crate) const DEFAULT_FRAME_RATE: u32 = 60;

/// Human-readable accepted range for frame-rate errors.
pub(crate) const FRAME_RATE_RANGE: &str = "an integer in 1..=1000 frames per second";

/// A validated frame-rate limit in frames per second.
///
/// The limit is the number of frames the host tries to present per second;
/// its period is [`FrameRateLimit::period_secs`]. It cannot be constructed
/// outside `MIN_FRAME_RATE..=MAX_FRAME_RATE`; the accepted range is named in
/// the error a rejected value produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameRateLimit(u32);

impl FrameRateLimit {
    /// The built-in default limit (60 fps).
    pub const DEFAULT: Self = Self(DEFAULT_FRAME_RATE);

    /// Wraps `fps`, rejecting a value outside the accepted range.
    ///
    /// `option` is the launch-argument or configuration-key name used in the
    /// error (for example `--fps` or `frame_rate`).
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidValue`] when `fps` is outside the accepted
    /// range.
    pub fn new(option: &'static str, fps: u32) -> Result<Self, ConfigError> {
        if (MIN_FRAME_RATE..=MAX_FRAME_RATE).contains(&fps) {
            Ok(Self(fps))
        } else {
            Err(ConfigError::invalid(
                option,
                fps.to_string(),
                FRAME_RATE_RANGE,
            ))
        }
    }

    /// Parses a decimal frame rate, then validates it.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidValue`] when `value` is not a decimal integer or
    /// is outside the accepted range.
    pub fn parse(option: &'static str, value: &str) -> Result<Self, ConfigError> {
        let fps = value
            .trim()
            .parse::<u32>()
            .map_err(|_| ConfigError::invalid(option, value.to_owned(), FRAME_RATE_RANGE))?;
        Self::new(option, fps)
    }

    /// The limit in frames per second.
    pub const fn fps(self) -> u32 {
        self.0
    }

    /// The frame period in seconds (`1 / fps`).
    pub fn period_secs(self) -> f64 {
        1.0 / f64::from(self.0)
    }

    /// The frame period in milliseconds (`1000 / fps`).
    pub fn period_ms(self) -> f32 {
        (1000.0 / f64::from(self.0)) as f32
    }
}

/// The fully resolved runtime configuration a launcher passes to the
/// emulator.
///
/// A launcher builds it from its own configuration store and the launch
/// arguments, over the built-in defaults, in that precedence. Every field is
/// already validated, so the fields are plain public values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RuntimeConfig {
    /// Frame-rate limit in frames per second, default 60. The accepted
    /// range is named in the error a rejected value produces. The limit paces
    /// the host loop and the headless step.
    pub frame_rate_limit: FrameRateLimit,
    /// Whether a mounted archive's whole image is read into memory, default
    /// `false`. It trades the archive's size in resident memory for removing
    /// the per-entry reads a mounted archive otherwise does, which is the cost
    /// a slow backing filesystem makes visible. The image is read in the
    /// background, so the mount does not wait for the archive's bytes.
    pub preload_archives: bool,
    /// Whether the running app draws the debug overlay (frame rate and
    /// resident memory) in the top-left corner, default `false`.
    ///
    /// The overlay is a render-only node: it is never a script-visible layer
    /// or bitmap, so enabling it cannot change what a script observes.
    pub debug_overlay: bool,
    /// The decoded-image cache policy, or `None` to leave the engine's
    /// reference default (disabled) in place. `Some(CacheLimit::Disabled)`
    /// clears and disables the cache, `Some(CacheLimit::Unlimited)` keeps
    /// every entry, and `Some(CacheLimit::Bytes(n))` caps the cached bytes.
    /// A startup script can still change it through `System.graphicCacheLimit`.
    pub graphic_cache_limit: Option<CacheLimit>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            frame_rate_limit: FrameRateLimit::DEFAULT,
            preload_archives: false,
            debug_overlay: false,
            graphic_cache_limit: None,
        }
    }
}

/// The subset of [`RuntimeConfig`] a parser or launcher holds.
///
/// A source that has no value for a field leaves it `None`; the launcher then
/// falls back to the built-in default. Keeping the partial shape makes the
/// precedence explicit instead of hiding the default inside the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RuntimeConfigOverrides {
    /// A source's frame-rate limit, if it holds one.
    pub frame_rate_limit: Option<FrameRateLimit>,
    /// A source's archive-preload choice, if it holds one.
    pub preload_archives: Option<bool>,
    /// A source's debug-overlay choice, if it holds one.
    pub debug_overlay: Option<bool>,
    /// A source's decoded-image cache policy, if it holds one.
    pub graphic_cache_limit: Option<CacheLimit>,
}

/// Parses a boolean configuration value, accepting `true` and `false`.
///
/// # Errors
///
/// [`ConfigError::InvalidValue`] when `value` is neither, naming `option` and
/// the accepted values.
pub fn parse_bool(option: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(ConfigError::invalid(
            option,
            other.to_owned(),
            "one of: true, false",
        )),
    }
}

/// The configuration keys [`parse_overrides`] accepts, in the order its
/// unknown-key diagnostic lists them.
const ACCEPTED_KEYS: [&str; 4] = [
    "frame_rate",
    "preload_archives",
    "debug_overlay",
    "graphic_cache_limit",
];

/// Parses `key=value` configuration text into overrides.
///
/// This is the parser a file-backed launcher uses; the text itself is the
/// launcher's business. Blank lines and lines whose first non-space character
/// is `#` are ignored. A line without `=` or with an unknown key is an error,
/// as is a value that fails validation; nothing is silently skipped. The
/// recognized keys are `frame_rate`, `preload_archives`, `debug_overlay` and
/// `graphic_cache_limit`.
///
/// # Errors
///
/// [`ConfigError::Source`] for a malformed line or unknown key, and
/// [`ConfigError::InvalidValue`] for a bad value.
pub fn parse_overrides(text: &str) -> Result<RuntimeConfigOverrides, ConfigError> {
    let mut overrides = RuntimeConfigOverrides::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(|| ConfigError::Source {
            message: format!("line {}: expected 'key=value', found '{line}'", index + 1),
        })?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "frame_rate" => {
                overrides.frame_rate_limit = Some(FrameRateLimit::parse("frame_rate", value)?);
            }
            "preload_archives" => {
                overrides.preload_archives = Some(parse_bool("preload_archives", value)?);
            }
            "debug_overlay" => {
                overrides.debug_overlay = Some(parse_bool("debug_overlay", value)?);
            }
            "graphic_cache_limit" => {
                overrides.graphic_cache_limit =
                    Some(CacheLimit::parse("graphic_cache_limit", value)?);
            }
            other => {
                let keys = ACCEPTED_KEYS
                    .iter()
                    .map(|key| format!("'{key}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ConfigError::Source {
                    message: format!(
                        "line {}: unknown key '{other}' (expected one of: {keys})",
                        index + 1
                    ),
                });
            }
        }
    }
    Ok(overrides)
}

/// Errors produced while reading, parsing or merging configuration.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// A configuration value is unparseable or out of range.
    #[error("{option}: '{value}' is not valid (expected {expected})")]
    InvalidValue {
        /// Name of the launch option or configuration-file key.
        option: &'static str,
        /// The rejected value, as written.
        value: String,
        /// Human-readable description of the accepted form and range.
        expected: &'static str,
    },
    /// A configuration store could not be read.
    #[error("configuration source: {message}")]
    Source {
        /// Human-readable description of the failure.
        message: String,
    },
}

impl ConfigError {
    /// Creates an [`ConfigError::InvalidValue`].
    pub fn invalid(option: &'static str, value: impl Into<String>, expected: &'static str) -> Self {
        Self::InvalidValue {
            option,
            value: value.into(),
            expected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_frame_rate_matches_the_reference() {
        // The reference default is 60 fps (`fps_limit`,
        // `reference/cpp/core/environ/ui/PreferenceConfig.h:304-309`) and it
        // applies it as `1 / fps` (`reference/cpp/core/environ/cocos2d/
        // MainScene.cpp:684-685`).
        assert_eq!(DEFAULT_FRAME_RATE, 60);
        assert_eq!(FrameRateLimit::DEFAULT.fps(), 60);
        assert!((FrameRateLimit::DEFAULT.period_secs() - 1.0 / 60.0).abs() < 1e-12);
    }

    #[test]
    fn frame_rate_accepts_the_range() {
        assert_eq!(FrameRateLimit::new("--fps", 1).unwrap().fps(), 1);
        assert_eq!(FrameRateLimit::new("--fps", 60).unwrap().fps(), 60);
        assert_eq!(FrameRateLimit::new("--fps", 1000).unwrap().fps(), 1000);
    }

    #[test]
    fn frame_rate_rejects_out_of_range_and_garbage() {
        let low = FrameRateLimit::new("--fps", 0).unwrap_err();
        assert!(low.to_string().contains("--fps"));
        assert!(low.to_string().contains("1..=1000"));
        assert!(FrameRateLimit::new("--fps", 1001).is_err());
        assert!(FrameRateLimit::parse("--fps", "abc").is_err());
        assert!(FrameRateLimit::parse("--fps", "-5").is_err());
    }

    #[test]
    fn frame_rate_period() {
        assert!((FrameRateLimit::new("--fps", 50).unwrap().period_secs() - 0.02).abs() < 1e-12);
        assert!((FrameRateLimit::new("--fps", 50).unwrap().period_ms() - 20.0).abs() < 1e-6);
    }

    #[test]
    fn overrides_parse_keys_comments_and_blanks() {
        let overrides = parse_overrides(
            "# a comment\n\nframe_rate = 30\npreload_archives=true\ndebug_overlay=true\ngraphic_cache_limit=1048576\n",
        )
        .expect("parse");
        assert_eq!(
            overrides.frame_rate_limit,
            Some(FrameRateLimit::new("frame_rate", 30).unwrap())
        );
        assert_eq!(overrides.preload_archives, Some(true));
        assert_eq!(overrides.debug_overlay, Some(true));
        assert_eq!(
            overrides.graphic_cache_limit,
            Some(CacheLimit::bytes(1_048_576))
        );
    }

    #[test]
    fn overrides_reject_unknown_key_and_malformed_line() {
        assert!(parse_overrides("teleport=on").is_err());
        assert!(parse_overrides("frame_rate").is_err());
    }

    /// The unknown-key diagnostic lists every accepted key, so a key cannot be
    /// added without the message naming it.
    #[test]
    fn the_unknown_key_diagnostic_names_every_accepted_key() {
        let error = parse_overrides("teleport=on").expect_err("unknown key");
        let message = error.to_string();
        for key in ACCEPTED_KEYS {
            assert!(
                message.contains(key),
                "the unknown-key diagnostic omits '{key}': {message}"
            );
        }
    }

    #[test]
    fn overrides_reject_a_bad_value() {
        let error = parse_overrides("frame_rate=0").expect_err("must reject");
        assert!(error.to_string().contains("frame_rate"));
        assert!(parse_overrides("debug_overlay=yes").is_err());
        assert!(parse_overrides("graphic_cache_limit=-1").is_err());
    }

    #[test]
    fn graphic_cache_limit_parses_disabled_unlimited_and_bytes() {
        assert_eq!(
            CacheLimit::parse("graphic_cache_limit", "none").unwrap(),
            CacheLimit::Disabled
        );
        assert_eq!(
            CacheLimit::parse("graphic_cache_limit", "off").unwrap(),
            CacheLimit::Disabled
        );
        assert_eq!(
            CacheLimit::parse("graphic_cache_limit", "None").unwrap(),
            CacheLimit::Disabled
        );
        assert_eq!(
            CacheLimit::parse("graphic_cache_limit", "0").unwrap(),
            CacheLimit::Unlimited
        );
        assert_eq!(
            CacheLimit::parse("graphic_cache_limit", "unlimited").unwrap(),
            CacheLimit::Unlimited
        );
        assert_eq!(
            CacheLimit::parse("graphic_cache_limit", "1048576").unwrap(),
            CacheLimit::bytes(1_048_576)
        );
    }
}
