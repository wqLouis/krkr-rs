//! Launch-configuration DTOs and the platform configuration-source seam.
//!
//! [`RuntimeConfig`] is the one resolved value the binary acts on. It is
//! produced with the precedence **launch argument > platform-provided
//! configuration > built-in default**: the composition root parses the
//! arguments, asks the platform for its overrides through [`ConfigSource`],
//! and fills in the defaults. The type lives here so every layer shares one
//! definition; the resolution itself belongs to the composition root.
//!
//! Every field is validated. An unparseable or out-of-range value is a typed
//! [`ConfigError`] naming the option and the accepted range — it is never
//! clamped or defaulted. [`RuntimeConfigOverrides`] lets a platform supply
//! only the fields it actually holds, so the built-in default is applied by
//! the resolver rather than baked into the platform.

use thiserror::Error;

/// Lowest accepted frame-rate limit, in frames per second.
///
/// A lower limit has no positive frame period to pace the loop with.
pub const MIN_FRAME_RATE: u32 = 1;

/// Highest accepted frame-rate limit, in frames per second.
///
/// 1000 fps is the point at which the frame period reaches the platform
/// ticker's 1 ms minimum frame interval; a higher limit would have to be
/// silently clamped, so it is rejected instead.
pub const MAX_FRAME_RATE: u32 = 1000;

/// Built-in default frame-rate limit, in frames per second.
pub const DEFAULT_FRAME_RATE: u32 = 60;

/// Human-readable accepted range for frame-rate errors.
pub const FRAME_RATE_RANGE: &str = "an integer in 1..=1000 frames per second";

/// A validated frame-rate limit in frames per second.
///
/// The limit is the number of frames the host tries to present per second;
/// its period is [`FrameRateLimit::period_secs`]. It cannot be constructed
/// outside [`MIN_FRAME_RATE`]..=[`MAX_FRAME_RATE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameRateLimit(u32);

impl FrameRateLimit {
    /// The built-in default limit (60 fps).
    pub const DEFAULT: Self = Self(DEFAULT_FRAME_RATE);

    /// Wraps `fps`, rejecting a value outside the accepted range.
    ///
    /// `option` is the launch-argument or platform-key name used in the error
    /// (for example `--fps` or `frame_rate`).
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidValue`] when `fps` is outside
    /// [`MIN_FRAME_RATE`]..=[`MAX_FRAME_RATE`].
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

/// How the engine presents the logical scene inside the framebuffer.
///
/// This mirrors the engine's `ScalingMode` at the configuration boundary: the
/// configuration crate cannot name the engine type (the dependency runs the
/// other way), so the composition root maps one to the other. It is a plain
/// value with no behaviour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PresentationScaling {
    /// One logical pixel per framebuffer pixel (the default).
    #[default]
    Game,
    /// Uniform scale that fits the whole scene, letterboxed.
    Fit,
    /// Non-uniform scale that fills the window.
    Fill,
    /// Grow the logical size to the window's aspect, then scale uniformly.
    Expand,
}

impl PresentationScaling {
    /// The built-in default (`Game`).
    pub const DEFAULT: Self = Self::Game;

    /// Human-readable accepted values for error and help text.
    pub const RANGE: &str = "one of: game, fit, fill, expand";

    /// Parses a scaling name.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidValue`] when `value` is not one of the accepted
    /// names.
    pub fn parse(option: &'static str, value: &str) -> Result<Self, ConfigError> {
        match value {
            "game" => Ok(Self::Game),
            "fit" => Ok(Self::Fit),
            "fill" => Ok(Self::Fill),
            "expand" => Ok(Self::Expand),
            other => Err(ConfigError::invalid(option, other.to_owned(), Self::RANGE)),
        }
    }

    /// The canonical name of this mode.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Game => "game",
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Expand => "expand",
        }
    }
}

/// The fully resolved runtime configuration the binary acts on.
///
/// Built by the composition root from the launch arguments, the platform
/// configuration and the built-in defaults, in that precedence. Every field
/// is already validated, so the fields are plain public values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RuntimeConfig {
    /// Frame-rate limit in frames per second; range
    /// [`MIN_FRAME_RATE`]..=[`MAX_FRAME_RATE`], default
    /// [`DEFAULT_FRAME_RATE`] (60). It paces the host loop and the headless
    /// step.
    pub frame_rate_limit: FrameRateLimit,
    /// How the logical scene is presented inside the framebuffer; one of
    /// [`PresentationScaling::Game`], `Fit`, `Fill` or `Expand`, default
    /// [`PresentationScaling::Game`].
    pub scaling: PresentationScaling,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            frame_rate_limit: FrameRateLimit::DEFAULT,
            scaling: PresentationScaling::DEFAULT,
        }
    }
}

/// The subset of [`RuntimeConfig`] a [`ConfigSource`] holds.
///
/// A platform that has no value for a field leaves it `None`; the resolver
/// then falls back to the built-in default. Keeping the platform's answer
/// partial makes the precedence explicit instead of hiding the default
/// inside the platform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RuntimeConfigOverrides {
    /// Platform-provided frame-rate limit, if the platform holds one.
    pub frame_rate_limit: Option<FrameRateLimit>,
    /// Platform-provided presentation scaling, if the platform holds one.
    pub scaling: Option<PresentationScaling>,
}

/// The platform's configuration seam.
///
/// The composition root resolves settings with the precedence *launch
/// argument > platform configuration > built-in default*, but it must not
/// know **where** a platform keeps that configuration: a desktop build may
/// read a file, an Android build its own storage, and a build with no
/// persistent configuration returns no overrides. Each platform supplies its
/// implementation, so the composition root contains no `#[cfg(target_os)]`
/// and no configuration path.
pub trait ConfigSource {
    /// Loads the platform's overrides, or an empty set when the platform
    /// holds no configuration.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when the platform's store exists but cannot be read or
    /// contains an invalid or unknown entry.
    fn load(&self) -> Result<RuntimeConfigOverrides, ConfigError>;
}

/// A [`ConfigSource`] that holds no configuration.
///
/// It yields an empty override set, so every field falls back to the built-in
/// default. This is the valid "no source" implementation for a platform that
/// keeps no persistent configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoConfigSource;

impl ConfigSource for NoConfigSource {
    fn load(&self) -> Result<RuntimeConfigOverrides, ConfigError> {
        Ok(RuntimeConfigOverrides::default())
    }
}

/// Parses `key=value` configuration text into overrides.
///
/// Blank lines and lines whose first non-space character is `#` are ignored.
/// A line without `=` or with an unknown key is an error, as is a value that
/// fails validation; nothing is silently skipped. The recognized keys are
/// `frame_rate` and `scaling`.
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
            "scaling" => {
                overrides.scaling = Some(PresentationScaling::parse("scaling", value)?);
            }
            other => {
                return Err(ConfigError::Source {
                    message: format!(
                        "line {}: unknown key '{other}' (expected 'frame_rate' or 'scaling')",
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
        /// Name of the launch option or platform configuration key.
        option: &'static str,
        /// The rejected value, as written.
        value: String,
        /// Human-readable description of the accepted form and range.
        expected: &'static str,
    },
    /// A platform configuration source could not be read.
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
    fn scaling_parses_every_name() {
        for (text, expected) in [
            ("game", PresentationScaling::Game),
            ("fit", PresentationScaling::Fit),
            ("fill", PresentationScaling::Fill),
            ("expand", PresentationScaling::Expand),
        ] {
            assert_eq!(
                PresentationScaling::parse("--scaling", text).unwrap(),
                expected
            );
        }
        assert_eq!(PresentationScaling::Game.as_str(), "game");
        assert!(PresentationScaling::parse("--scaling", "stretch").is_err());
    }

    #[test]
    fn overrides_parse_keys_comments_and_blanks() {
        let overrides =
            parse_overrides("# a comment\n\nframe_rate = 30\nscaling=fit\n").expect("parse");
        assert_eq!(
            overrides.frame_rate_limit,
            Some(FrameRateLimit::new("frame_rate", 30).unwrap())
        );
        assert_eq!(overrides.scaling, Some(PresentationScaling::Fit));
    }

    #[test]
    fn overrides_reject_unknown_key_and_malformed_line() {
        assert!(parse_overrides("teleport=on").is_err());
        assert!(parse_overrides("frame_rate").is_err());
    }

    #[test]
    fn overrides_reject_a_bad_value() {
        let error = parse_overrides("frame_rate=0").expect_err("must reject");
        assert!(error.to_string().contains("frame_rate"));
        assert!(parse_overrides("scaling=stretch").is_err());
    }

    #[test]
    fn no_source_yields_no_overrides() {
        assert_eq!(
            NoConfigSource.load().expect("load"),
            RuntimeConfigOverrides::default()
        );
    }
}
