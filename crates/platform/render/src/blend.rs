//! The Photoshop blend family's per-channel functions.
//!
//! One table of per-channel functions is owned here. The engine's bitmap
//! path and the GPU's destination-dependent program both read it, so a
//! Photoshop blend means the same thing on the CPU and on the GPU.
//!
//! A mode is a per-channel function and the way its result combines with the
//! source's alpha: the function turns the source's channel and the
//! destination's channel into the blended channel, and the source's alpha
//! mixes that channel into the destination's.
//!
//! Every mode of the family keeps the destination's alpha.
//!
//! The program's source is generated from this table (see
//! [`dst_blend_shader`]), so the shader and the table cannot drift apart.

use api::blend::{BlendMode, DstBlend};

/// A per-channel function of the Photoshop blend family.
///
/// The function turns the source's channel and the destination's channel
/// into the blended channel, before the source's alpha mixes it into the
/// destination's. The screen and the exclusion function can exceed a byte;
/// the mix brings the result back into one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelFunction {
    /// The source's channel.
    Source,
    /// The saturating sum of the two channels.
    SaturatedSum,
    /// The saturating difference of the destination and the source's
    /// complement: the larger of zero and `d + s - 255`.
    SaturatedDifference,
    /// The product of the two channels, truncated by eight bits.
    TruncatedProduct,
    /// The screen blend's channel: `d + s - (d·s >> 8)`.
    Screen,
    /// The overlay table's entry for the source and the destination.
    Overlay,
    /// The overlay table's entry for the destination and the source.
    SwappedOverlay,
    /// The soft-light table's entry for the source and the destination.
    SoftLight,
    /// The colour-dodge table's entry for the source and the destination.
    ColorDodge,
    /// The colour-burn table's entry for the source and the destination.
    ColorBurn,
    /// The larger of the two channels.
    Lighten,
    /// The smaller of the two channels.
    Darken,
    /// The absolute difference of the two channels.
    Difference,
    /// The exclusion blend's channel: `d + s - (d·s >> 7)`.
    Exclusion,
}

/// How a mode's per-channel function combines with the source's alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Composition {
    /// The function's result is mixed into the destination's channel by the
    /// source's alpha.
    MixByAlpha,
    /// The source's channel fades by the source's alpha first, and the
    /// function's result replaces the destination's channel.
    ReplaceFaded,
}

/// One mode of the Photoshop blend family.
///
/// A mode is a per-channel function and the way its result combines with the
/// source's alpha. The mode's index is the one the destination-dependent
/// program numbers it with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhotoshopBlend {
    /// The mode's index in the destination-dependent program.
    index: u32,
    /// The mode's per-channel function.
    function: ChannelFunction,
    /// How the function's result combines with the source's alpha.
    composition: Composition,
}

impl PhotoshopBlend {
    /// The Photoshop normal blend: the source's channel, mixed into the
    /// destination's by the source's alpha.
    pub const NORMAL: Self = Self {
        index: 0,
        function: ChannelFunction::Source,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop additive blend: the saturating sum of the two channels.
    pub const ADDITIVE: Self = Self {
        index: 1,
        function: ChannelFunction::SaturatedSum,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop subtractive blend: the saturating difference of the
    /// destination and the source's complement.
    pub const SUBTRACTIVE: Self = Self {
        index: 2,
        function: ChannelFunction::SaturatedDifference,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop multiplicative blend: the product of the two channels,
    /// truncated by eight bits.
    pub const MULTIPLICATIVE: Self = Self {
        index: 3,
        function: ChannelFunction::TruncatedProduct,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop screen blend.
    pub const SCREEN: Self = Self {
        index: 4,
        function: ChannelFunction::Screen,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop overlay blend: the overlay table.
    pub const OVERLAY: Self = Self {
        index: 5,
        function: ChannelFunction::Overlay,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop hard-light blend: the overlay table with the operands
    /// swapped.
    pub const HARD_LIGHT: Self = Self {
        index: 6,
        function: ChannelFunction::SwappedOverlay,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop soft-light blend: the soft-light table.
    pub const SOFT_LIGHT: Self = Self {
        index: 7,
        function: ChannelFunction::SoftLight,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop colour-dodge blend: the colour-dodge table.
    pub const COLOR_DODGE: Self = Self {
        index: 8,
        function: ChannelFunction::ColorDodge,
        composition: Composition::MixByAlpha,
    };
    /// The legacy Photoshop colour-dodge blend: the source fades by the
    /// source's alpha and the colour-dodge table replaces the channel.
    pub const COLOR_DODGE5: Self = Self {
        index: 9,
        function: ChannelFunction::ColorDodge,
        composition: Composition::ReplaceFaded,
    };
    /// The Photoshop colour-burn blend: the colour-burn table.
    pub const COLOR_BURN: Self = Self {
        index: 10,
        function: ChannelFunction::ColorBurn,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop lighten blend: the larger of the two channels.
    pub const LIGHTEN: Self = Self {
        index: 11,
        function: ChannelFunction::Lighten,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop darken blend: the smaller of the two channels.
    pub const DARKEN: Self = Self {
        index: 12,
        function: ChannelFunction::Darken,
        composition: Composition::MixByAlpha,
    };
    /// The Photoshop difference blend: the absolute difference of the two
    /// channels.
    pub const DIFFERENCE: Self = Self {
        index: 13,
        function: ChannelFunction::Difference,
        composition: Composition::MixByAlpha,
    };
    /// The legacy Photoshop difference blend: the source fades by the source's
    /// alpha and the difference replaces the channel.
    pub const DIFFERENCE5: Self = Self {
        index: 14,
        function: ChannelFunction::Difference,
        composition: Composition::ReplaceFaded,
    };
    /// The Photoshop exclusion blend.
    pub const EXCLUSION: Self = Self {
        index: 15,
        function: ChannelFunction::Exclusion,
        composition: Composition::MixByAlpha,
    };

    /// Every mode of the family, in the order the destination-dependent
    /// program numbers them.
    pub const ALL: [Self; 16] = [
        Self::NORMAL,
        Self::ADDITIVE,
        Self::SUBTRACTIVE,
        Self::MULTIPLICATIVE,
        Self::SCREEN,
        Self::OVERLAY,
        Self::HARD_LIGHT,
        Self::SOFT_LIGHT,
        Self::COLOR_DODGE,
        Self::COLOR_DODGE5,
        Self::COLOR_BURN,
        Self::LIGHTEN,
        Self::DARKEN,
        Self::DIFFERENCE,
        Self::DIFFERENCE5,
        Self::EXCLUSION,
    ];

    /// The mode's index in the destination-dependent program.
    pub const fn index(self) -> u32 {
        self.index
    }

    /// The mode's per-channel function.
    pub const fn function(self) -> ChannelFunction {
        self.function
    }

    /// How the mode's function combines with the source's alpha.
    pub const fn composition(self) -> Composition {
        self.composition
    }

    /// The mode's name, as the layer types spell it.
    pub const fn name(self) -> &'static str {
        match self.function {
            ChannelFunction::Source => "normal",
            ChannelFunction::SaturatedSum => "additive",
            ChannelFunction::SaturatedDifference => "subtractive",
            ChannelFunction::TruncatedProduct => "multiplicative",
            ChannelFunction::Screen => "screen",
            ChannelFunction::Overlay => "overlay",
            ChannelFunction::SwappedOverlay => "hard light",
            ChannelFunction::SoftLight => "soft light",
            ChannelFunction::ColorDodge => match self.composition {
                Composition::MixByAlpha => "colour dodge",
                Composition::ReplaceFaded => "colour dodge 5",
            },
            ChannelFunction::ColorBurn => "colour burn",
            ChannelFunction::Lighten => "lighten",
            ChannelFunction::Darken => "darken",
            ChannelFunction::Difference => match self.composition {
                Composition::MixByAlpha => "difference",
                Composition::ReplaceFaded => "difference 5",
            },
            ChannelFunction::Exclusion => "exclusion",
        }
    }

    /// Blends one channel.
    ///
    /// `weight` is the source's alpha, scaled by the opacity.
    #[inline]
    pub fn blend_channel(self, source: u8, destination: u8, weight: u32) -> u8 {
        match self.composition {
            Composition::MixByAlpha => mix(
                destination,
                self.function.apply(source, destination),
                weight,
            ),
            Composition::ReplaceFaded => {
                self.function.apply(fade(source, weight), destination) as u8
            }
        }
    }

    /// Blends one pixel held as a 32-bit ARGB word.
    ///
    /// The destination's alpha is kept: every mode of the family holds it.
    #[inline]
    pub fn blend_word(self, destination: u32, source: u32, opacity: u8) -> u32 {
        let weight = source_weight(source >> 24, opacity);
        let red = self.blend_channel((source >> 16) as u8, (destination >> 16) as u8, weight);
        let green = self.blend_channel((source >> 8) as u8, (destination >> 8) as u8, weight);
        let blue = self.blend_channel(source as u8, destination as u8, weight);
        (u32::from(red) << 16)
            | (u32::from(green) << 8)
            | u32::from(blue)
            | (destination & 0xff00_0000)
    }

    /// Blends one pixel held as four RGBA bytes.
    ///
    /// The destination's alpha is kept.
    #[inline]
    pub fn blend_pixel(self, destination: [u8; 4], source: [u8; 4], opacity: u8) -> [u8; 4] {
        let word = (u32::from(destination[0]) << 16)
            | (u32::from(destination[1]) << 8)
            | u32::from(destination[2])
            | (u32::from(destination[3]) << 24);
        let source_word = (u32::from(source[0]) << 16)
            | (u32::from(source[1]) << 8)
            | u32::from(source[2])
            | (u32::from(source[3]) << 24);
        let blended = self.blend_word(word, source_word, opacity);
        [
            (blended >> 16) as u8,
            (blended >> 8) as u8,
            blended as u8,
            destination[3],
        ]
    }
}

impl ChannelFunction {
    /// Applies the function to one channel.
    ///
    /// The result is the blended channel before the source's alpha mixes it
    /// into the destination's; the screen and the exclusion function can
    /// exceed a byte, and the mix brings the result back into one.
    #[inline]
    pub fn apply(self, source: u8, destination: u8) -> i32 {
        match self {
            Self::Source => i32::from(source),
            Self::SaturatedSum => (i32::from(source) + i32::from(destination)).min(255),
            Self::SaturatedDifference => (i32::from(source) + i32::from(destination) - 255).max(0),
            Self::TruncatedProduct => (i32::from(source) * i32::from(destination)) >> 8,
            Self::Screen => {
                i32::from(source) + i32::from(destination)
                    - ((i32::from(source) * i32::from(destination)) >> 8)
            }
            Self::Overlay => i32::from(overlay_channel(source, destination)),
            Self::SwappedOverlay => i32::from(overlay_channel(destination, source)),
            Self::SoftLight => i32::from(soft_light_channel(source, destination)),
            Self::ColorDodge => i32::from(color_dodge_channel(source, destination)),
            Self::ColorBurn => i32::from(color_burn_channel(source, destination)),
            Self::Lighten => i32::from(source.max(destination)),
            Self::Darken => i32::from(source.min(destination)),
            Self::Difference => i32::from(source.abs_diff(destination)),
            Self::Exclusion => {
                i32::from(source) + i32::from(destination)
                    - ((i32::from(source) * i32::from(destination)) >> 7)
            }
        }
    }
}

/// The Photoshop mode a blend mode names, when it names one.
///
/// The modes that are not part of the family — the plain colour dodge, the
/// plain darken and the plain lighten — have no entry.
pub fn photoshop_blend(mode: BlendMode) -> Option<PhotoshopBlend> {
    match mode {
        BlendMode::PsNormal => Some(PhotoshopBlend::NORMAL),
        BlendMode::PsAdditive => Some(PhotoshopBlend::ADDITIVE),
        BlendMode::PsSubtractive => Some(PhotoshopBlend::SUBTRACTIVE),
        BlendMode::PsMultiplicative => Some(PhotoshopBlend::MULTIPLICATIVE),
        BlendMode::PsScreen => Some(PhotoshopBlend::SCREEN),
        BlendMode::DestinationDependent(kind) => match kind {
            DstBlend::PsColorDodge => Some(PhotoshopBlend::COLOR_DODGE),
            DstBlend::ColorDodge5 => Some(PhotoshopBlend::COLOR_DODGE5),
            DstBlend::ColorBurn => Some(PhotoshopBlend::COLOR_BURN),
            DstBlend::PsDarken => Some(PhotoshopBlend::DARKEN),
            DstBlend::PsLighten => Some(PhotoshopBlend::LIGHTEN),
            DstBlend::Difference => Some(PhotoshopBlend::DIFFERENCE),
            DstBlend::Difference5 => Some(PhotoshopBlend::DIFFERENCE5),
            DstBlend::Exclusion => Some(PhotoshopBlend::EXCLUSION),
            DstBlend::Overlay => Some(PhotoshopBlend::OVERLAY),
            DstBlend::HardLight => Some(PhotoshopBlend::HARD_LIGHT),
            DstBlend::SoftLight => Some(PhotoshopBlend::SOFT_LIGHT),
            DstBlend::ColorDodge | DstBlend::Darken | DstBlend::Lighten => None,
        },
        BlendMode::SourceOver
        | BlendMode::Replace
        | BlendMode::Additive
        | BlendMode::Subtractive
        | BlendMode::AddAlpha
        | BlendMode::Multiply
        | BlendMode::Screen => None,
    }
}

/// The weight the source's alpha mixes a channel with.
///
/// The full opacity mixes with the source's alpha; a lower opacity scales
/// the source's alpha by it.
#[inline]
pub fn source_weight(source_alpha: u32, opacity: u8) -> u32 {
    if opacity == 255 {
        source_alpha
    } else {
        (source_alpha * u32::from(opacity)) >> 8
    }
}

/// Mixes the blended channel into the destination's by the source's alpha.
///
/// The difference is signed and the shift rounds towards negative infinity,
/// so a channel below the destination's darkens it.
#[inline]
fn mix(destination: u8, blended: i32, weight: u32) -> u8 {
    let difference = blended - i32::from(destination);
    (i32::from(destination) + ((difference * weight as i32) >> 8)) as u8
}

/// The source's channel faded by the source's alpha.
#[inline]
fn fade(source: u8, weight: u32) -> u8 {
    ((u32::from(source) * weight) >> 8) as u8
}

/// The overlay table's entry: the overlay of a source channel over a
/// destination channel.
#[inline]
fn overlay_channel(source: u8, destination: u8) -> u8 {
    let product = i32::from(source) * i32::from(destination) * 2;
    let value = if destination < 128 {
        product / 255
    } else {
        (i32::from(source) + i32::from(destination)) * 2 - product / 255 - 255
    };
    value.clamp(0, 255) as u8
}

/// The colour-dodge table's entry.
#[inline]
fn color_dodge_channel(source: u8, destination: u8) -> u8 {
    if 255 - i32::from(source) <= i32::from(destination) {
        255
    } else {
        ((i32::from(destination) * 255) / (255 - i32::from(source))) as u8
    }
}

/// The colour-burn table's entry.
#[inline]
fn color_burn_channel(source: u8, destination: u8) -> u8 {
    if i32::from(source) <= 255 - i32::from(destination) {
        0
    } else {
        (255 - ((255 - i32::from(destination)) * 255) / i32::from(source)) as u8
    }
}

/// The soft-light table's entry.
///
/// The entry is the truncated value of `255 · (d/255)^p`, with the exponent
/// the table is built from.
#[inline]
fn soft_light_channel(source: u8, destination: u8) -> u8 {
    let base = f64::from(destination) / 255.0;
    let value = if source >= 128 {
        base.powf(128.0 / f64::from(source))
    } else {
        base.powf((1.0 - f64::from(source) / 255.0) / 0.5)
    };
    (value * 255.0) as u8
}

/// The soft-light table the GPU reads.
///
/// The table holds the entry for every source and destination channel, with
/// the source's channel selecting the row. The GPU reads it because a
/// shader's floating-point power cannot reproduce the table's entries.
pub fn soft_light_table() -> Vec<u8> {
    let mut table = Vec::with_capacity(256 * 256);
    for source in 0..=u8::MAX {
        for destination in 0..=u8::MAX {
            table.push(soft_light_channel(source, destination));
        }
    }
    table
}
