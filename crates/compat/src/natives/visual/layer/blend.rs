//! The native `Layer.type` (`tTVPLayerType`) vocabulary and its mapping to the
//! render pipeline's generic [`BlendMode`].
//!
//! L2 stores the generic blend vocabulary directly ([`BlendMode`]); the
//! integer numbering scripts read and write as `layer.type` is TVP vocabulary
//! and therefore lives in L3. Every documented native type maps to its real
//! behaviour, so the extended Photoshop blends survive the compat seam instead
//! of collapsing to source-over.
//!
//! Reference: `tTVPLayerType` (`reference/cpp/core/visual/drawable.h:20`).

use api::blend::{BlendMode, DstBlend};

/// `ltBinder` — a container that draws nothing.
pub(super) const LT_BINDER: i64 = 0;
/// `ltOpaque`/`ltCoverRect`.
pub(super) const LT_OPAQUE: i64 = 1;
/// `ltAlpha`/`ltTransparent` (the scene default).
pub(super) const LT_ALPHA: i64 = 2;
/// `ltAdditive`.
pub(super) const LT_ADDITIVE: i64 = 3;
/// `ltSubtractive`.
pub(super) const LT_SUBTRACTIVE: i64 = 4;
/// `ltMultiplicative`.
pub(super) const LT_MULTIPLICATIVE: i64 = 5;
/// `ltEffect` — layer-effect callback, not a bitmap blend.
pub(super) const LT_EFFECT: i64 = 6;
/// `ltFilter` — layer-filter callback, not a bitmap blend.
pub(super) const LT_FILTER: i64 = 7;
/// `ltDodge` — colour dodge.
pub(super) const LT_DODGE: i64 = 8;
/// `ltDarken` — per-channel minimum.
pub(super) const LT_DARKEN: i64 = 9;
/// `ltLighten` — per-channel maximum.
pub(super) const LT_LIGHTEN: i64 = 10;
/// `ltScreen`.
pub(super) const LT_SCREEN: i64 = 11;
/// `ltAddAlpha` — additive with an explicit alpha channel.
pub(super) const LT_ADD_ALPHA: i64 = 12;
/// `ltPsNormal` — Photoshop-style normal (plain source-over).
pub(super) const LT_PS_NORMAL: i64 = 13;
/// `ltPsAdditive`.
pub(super) const LT_PS_ADDITIVE: i64 = 14;
/// `ltPsSubtractive`.
pub(super) const LT_PS_SUBTRACTIVE: i64 = 15;
/// `ltPsMultiplicative`.
pub(super) const LT_PS_MULTIPLICATIVE: i64 = 16;
/// `ltPsScreen`.
pub(super) const LT_PS_SCREEN: i64 = 17;
/// `ltPsOverlay`.
pub(super) const LT_PS_OVERLAY: i64 = 18;
/// `ltPsHardLight`.
pub(super) const LT_PS_HARD_LIGHT: i64 = 19;
/// `ltPsSoftLight`.
pub(super) const LT_PS_SOFT_LIGHT: i64 = 20;
/// `ltPsColorDodge`.
pub(super) const LT_PS_COLOR_DODGE: i64 = 21;
/// `ltPsColorDodge5` — the legacy KiriKiri approximation.
pub(super) const LT_PS_COLOR_DODGE5: i64 = 22;
/// `ltPsColorBurn`.
pub(super) const LT_PS_COLOR_BURN: i64 = 23;
/// `ltPsLighten`.
pub(super) const LT_PS_LIGHTEN: i64 = 24;
/// `ltPsDarken`.
pub(super) const LT_PS_DARKEN: i64 = 25;
/// `ltPsDifference`.
pub(super) const LT_PS_DIFFERENCE: i64 = 26;
/// `ltPsDifference5` — the legacy KiriKiri approximation.
pub(super) const LT_PS_DIFFERENCE5: i64 = 27;
/// `ltPsExclusion`.
pub(super) const LT_PS_EXCLUSION: i64 = 28;

/// Decode a script `Layer.type` into the render pipeline's generic blend mode.
///
/// Every documented blend type maps to its own behaviour, and the core and
/// Photoshop spellings map to distinct variants so the encoder can reproduce
/// the exact script value. `ltBinder` is a container and `ltEffect`/`ltFilter`
/// are layer-effect callbacks, not bitmap blends; none of the three has a
/// generic equivalent, so they degrade to [`BlendMode::SourceOver`] — a binder
/// has no image in the reference anyway. Unknown / out-of-range values degrade
/// safely to source-over.
pub(crate) const fn from_native(value: i64) -> BlendMode {
    match value {
        LT_OPAQUE => BlendMode::Replace,
        LT_ALPHA => BlendMode::SourceOver,
        LT_PS_NORMAL => BlendMode::PsNormal,
        LT_ADDITIVE => BlendMode::Additive,
        LT_PS_ADDITIVE => BlendMode::PsAdditive,
        LT_SUBTRACTIVE => BlendMode::Subtractive,
        LT_PS_SUBTRACTIVE => BlendMode::PsSubtractive,
        LT_MULTIPLICATIVE => BlendMode::Multiply,
        LT_PS_MULTIPLICATIVE => BlendMode::PsMultiplicative,
        LT_SCREEN => BlendMode::Screen,
        LT_PS_SCREEN => BlendMode::PsScreen,
        LT_ADD_ALPHA => BlendMode::AddAlpha,
        LT_DODGE => BlendMode::DestinationDependent(DstBlend::ColorDodge),
        LT_PS_COLOR_DODGE => BlendMode::DestinationDependent(DstBlend::PsColorDodge),
        LT_PS_COLOR_DODGE5 => BlendMode::DestinationDependent(DstBlend::ColorDodge5),
        LT_PS_COLOR_BURN => BlendMode::DestinationDependent(DstBlend::ColorBurn),
        LT_DARKEN => BlendMode::DestinationDependent(DstBlend::Darken),
        LT_PS_DARKEN => BlendMode::DestinationDependent(DstBlend::PsDarken),
        LT_LIGHTEN => BlendMode::DestinationDependent(DstBlend::Lighten),
        LT_PS_LIGHTEN => BlendMode::DestinationDependent(DstBlend::PsLighten),
        LT_PS_DIFFERENCE => BlendMode::DestinationDependent(DstBlend::Difference),
        LT_PS_DIFFERENCE5 => BlendMode::DestinationDependent(DstBlend::Difference5),
        LT_PS_EXCLUSION => BlendMode::DestinationDependent(DstBlend::Exclusion),
        LT_PS_OVERLAY => BlendMode::DestinationDependent(DstBlend::Overlay),
        LT_PS_HARD_LIGHT => BlendMode::DestinationDependent(DstBlend::HardLight),
        LT_PS_SOFT_LIGHT => BlendMode::DestinationDependent(DstBlend::SoftLight),
        LT_BINDER | LT_EFFECT | LT_FILTER => BlendMode::SourceOver,
        _ => BlendMode::SourceOver,
    }
}

/// Encode a generic blend mode as its script `Layer.type`.
///
/// The inverse of [`from_native`]: every distinct native spelling round-trips
/// to itself, including the Photoshop spellings that share a blend formula
/// with a non-Photoshop type.
pub(super) const fn to_native(blend: BlendMode) -> i64 {
    match blend {
        BlendMode::Replace => LT_OPAQUE,
        BlendMode::SourceOver => LT_ALPHA,
        BlendMode::PsNormal => LT_PS_NORMAL,
        BlendMode::Additive => LT_ADDITIVE,
        BlendMode::PsAdditive => LT_PS_ADDITIVE,
        BlendMode::Subtractive => LT_SUBTRACTIVE,
        BlendMode::PsSubtractive => LT_PS_SUBTRACTIVE,
        BlendMode::Multiply => LT_MULTIPLICATIVE,
        BlendMode::PsMultiplicative => LT_PS_MULTIPLICATIVE,
        BlendMode::Screen => LT_SCREEN,
        BlendMode::PsScreen => LT_PS_SCREEN,
        BlendMode::AddAlpha => LT_ADD_ALPHA,
        BlendMode::DestinationDependent(DstBlend::ColorDodge) => LT_DODGE,
        BlendMode::DestinationDependent(DstBlend::PsColorDodge) => LT_PS_COLOR_DODGE,
        BlendMode::DestinationDependent(DstBlend::ColorDodge5) => LT_PS_COLOR_DODGE5,
        BlendMode::DestinationDependent(DstBlend::ColorBurn) => LT_PS_COLOR_BURN,
        BlendMode::DestinationDependent(DstBlend::Darken) => LT_DARKEN,
        BlendMode::DestinationDependent(DstBlend::PsDarken) => LT_PS_DARKEN,
        BlendMode::DestinationDependent(DstBlend::Lighten) => LT_LIGHTEN,
        BlendMode::DestinationDependent(DstBlend::PsLighten) => LT_PS_LIGHTEN,
        BlendMode::DestinationDependent(DstBlend::Difference) => LT_PS_DIFFERENCE,
        BlendMode::DestinationDependent(DstBlend::Difference5) => LT_PS_DIFFERENCE5,
        BlendMode::DestinationDependent(DstBlend::Exclusion) => LT_PS_EXCLUSION,
        BlendMode::DestinationDependent(DstBlend::Overlay) => LT_PS_OVERLAY,
        BlendMode::DestinationDependent(DstBlend::HardLight) => LT_PS_HARD_LIGHT,
        BlendMode::DestinationDependent(DstBlend::SoftLight) => LT_PS_SOFT_LIGHT,
    }
}
