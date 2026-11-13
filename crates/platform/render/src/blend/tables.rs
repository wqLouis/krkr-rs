//! The per-channel tables the blend family's table-driven functions read.
//!
//! Three of the family's per-channel functions are tables of 256 by 256
//! entries: the soft light, the colour dodge and the colour burn. Their
//! entries are whole bytes, and the tables are the crate's compile-time
//! constants, so the engine's bitmap path, the software rasterizer and the
//! graphics program all read the same entry for the same pair of channels.
//!
//! The tables are written by `cargo run -p render --bin write_blend_tables`
//! and committed; a test compares every entry with the formula the table
//! stands for.

/// The soft light's entry for every pair of channels.
///
/// The entry is the truncated value of `255 · (d/255)^p`, with the exponent
/// the source's channel selects.
static SOFT_LIGHT: [u8; 65_536] = *include_bytes!("soft_light.bin");

/// The colour dodge's entry for every pair of channels.
static COLOR_DODGE: [u8; 65_536] = *include_bytes!("color_dodge.bin");

/// The colour burn's entry for every pair of channels.
static COLOR_BURN: [u8; 65_536] = *include_bytes!("color_burn.bin");

/// The index of a pair of channels in a table.
///
/// The source's channel selects the row and the destination's channel the
/// column.
#[inline]
fn index(source: u8, destination: u8) -> usize {
    (usize::from(source) << 8) | usize::from(destination)
}

/// The soft light's entry for a pair of channels.
#[inline]
pub(crate) fn soft_light(source: u8, destination: u8) -> u8 {
    SOFT_LIGHT[index(source, destination)]
}

/// The colour dodge's entry for a pair of channels.
#[inline]
pub(crate) fn color_dodge(source: u8, destination: u8) -> u8 {
    COLOR_DODGE[index(source, destination)]
}

/// The colour burn's entry for a pair of channels.
#[inline]
pub(crate) fn color_burn(source: u8, destination: u8) -> u8 {
    COLOR_BURN[index(source, destination)]
}

/// The soft light's whole table.
pub fn soft_light_table() -> &'static [u8; 65_536] {
    &SOFT_LIGHT
}

/// The colour dodge's whole table.
pub fn color_dodge_table() -> &'static [u8; 65_536] {
    &COLOR_DODGE
}

/// The colour burn's whole table.
pub fn color_burn_table() -> &'static [u8; 65_536] {
    &COLOR_BURN
}
