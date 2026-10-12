//! Blend modes for the render tree.
//!
//! This is the closed set of composition modes the bottom render pipeline
//! must implement. The middle layer maps its own TVP `Layer.type` numbering
//! onto these values; the pipeline never learns about TVP numbering. Most
//! modes map directly to fixed-function blend state; the
//! [`BlendMode::DestinationDependent`] variants require reading the
//! destination and are handled with a dedicated shader path.

/// Destination-dependent blend mode.
///
/// These modes cannot be expressed with fixed-function source/destination
/// factors because the result depends on the destination color per channel.
/// The render pipeline evaluates them with a shader variant that samples the
/// current target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DstBlend {
    /// Photoshop `Color Dodge`: `dst / (1 - src)`.
    ColorDodge,
    /// The legacy `ColorDodge5` approximation used by KiriKiri.
    ColorDodge5,
    /// Photoshop `Color Burn`: `1 - (1 - dst) / src`.
    ColorBurn,
    /// Per-channel minimum.
    Darken,
    /// Per-channel maximum.
    Lighten,
    /// Absolute per-channel difference.
    Difference,
    /// The legacy `Difference5` approximation used by KiriKiri.
    Difference5,
    /// Photoshop `Exclusion`: `src + dst - 2·src·dst`.
    Exclusion,
    /// Photoshop `Overlay` (hard-light with swapped operands).
    Overlay,
    /// Photoshop `Hard Light`.
    HardLight,
    /// Photoshop `Soft Light`.
    SoftLight,
}

/// How a rendered node is composited over the destination.
///
/// The default is [`BlendMode::SourceOver`], matching normal alpha blending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// Normal alpha blending: `src·a + dst·(1 - a)`.
    #[default]
    SourceOver,
    /// Replace the destination (opaque copy; alpha handling per pipeline).
    Replace,
    /// Additive blending: `src·a + dst`.
    Additive,
    /// Subtractive blending: `dst - src·a`.
    Subtractive,
    /// Additive with additional alpha-channel rules; see the current
    /// `render/src/blend.rs` behavior.
    AddAlpha,
    /// Multiplicative blending.
    Multiply,
    /// Screen blending: `src + dst - src·dst`.
    Screen,
    /// A mode that must sample the destination.
    DestinationDependent(DstBlend),
}

impl BlendMode {
    /// Returns `true` if this mode needs to read the destination and
    /// therefore requires the destination-dependent shader path.
    pub const fn is_destination_dependent(&self) -> bool {
        matches!(self, Self::DestinationDependent(_))
    }

    /// Returns the wrapped [`DstBlend`] when this is a
    /// destination-dependent mode.
    pub const fn dst_blend(&self) -> Option<DstBlend> {
        match self {
            Self::DestinationDependent(mode) => Some(*mode),
            _ => None,
        }
    }
}
