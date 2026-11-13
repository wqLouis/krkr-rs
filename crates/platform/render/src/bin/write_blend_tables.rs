//! Writes the blend family's per-channel tables into the crate's blend
//! module.
//!
//! Three of the family's per-channel functions are tables of 256 by 256
//! entries: the soft light, the colour dodge and the colour burn. Their
//! entries are whole bytes, and the tables are the crate's compile-time
//! constants, so every path that blends reads the same entry for the same
//! pair of channels.
//!
//! The tables are written here, once, and committed; the crate reads them
//! as they are. A test compares every entry with the formula the table
//! stands for, so a stale table fails the suite.
//!
//! ```text
//! cargo run -p render --bin write_blend_tables
//! ```

use std::fs;
use std::path::PathBuf;

/// The number of entries in a table: one per pair of channels.
const ENTRIES: usize = 256 * 256;

/// The soft light's entry for one pair of channels.
///
/// The entry is the truncated value of `255 · (d/255)^p`, with the exponent
/// the source's channel selects.
fn soft_light(source: u8, destination: u8) -> u8 {
    let base = f64::from(destination) / 255.0;
    let value = if source >= 128 {
        base.powf(128.0 / f64::from(source))
    } else {
        base.powf((1.0 - f64::from(source) / 255.0) / 0.5)
    };
    (value * 255.0) as u8
}

/// The colour dodge's entry for one pair of channels.
fn color_dodge(source: u8, destination: u8) -> u8 {
    if 255 - i32::from(source) <= i32::from(destination) {
        255
    } else {
        ((i32::from(destination) * 255) / (255 - i32::from(source))) as u8
    }
}

/// The colour burn's entry for one pair of channels.
fn color_burn(source: u8, destination: u8) -> u8 {
    if i32::from(source) <= 255 - i32::from(destination) {
        0
    } else {
        (255 - ((255 - i32::from(destination)) * 255) / i32::from(source)) as u8
    }
}

/// The table's entries, one per pair of channels.
fn table(entry: fn(u8, u8) -> u8) -> Vec<u8> {
    let mut entries = Vec::with_capacity(ENTRIES);
    for source in 0..=u8::MAX {
        for destination in 0..=u8::MAX {
            entries.push(entry(source, destination));
        }
    }
    entries
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/blend");
    for (name, entry) in [
        ("soft_light.bin", soft_light as fn(u8, u8) -> u8),
        ("color_dodge.bin", color_dodge),
        ("color_burn.bin", color_burn),
    ] {
        let path = directory.join(name);
        fs::write(&path, table(entry))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
