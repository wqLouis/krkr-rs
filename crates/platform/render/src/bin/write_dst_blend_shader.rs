//! Writes the generated destination-dependent blend shader into the crate's
//! shader directory.
//!
//! The shader's source is generated from the blend table, so the committed
//! file and the table cannot disagree; this binary writes the generated
//! source over the committed one.

use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shaders/dst_blend.frag");
    fs::write(&path, render::blend_shader::dst_blend_shader())?;
    println!("wrote {}", path.display());
    Ok(())
}
