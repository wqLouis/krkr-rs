//! Writing the presented frame to a file.
//!
//! The frame the CPU rasterizer composited is the picture the window shows,
//! and a file is how a human looks at it: the dump writes the frame as a
//! PNG, so a frame can be inspected without a display.

use std::path::Path;

use media::{Media, encode_image};
use render::PresentedFrame;

/// Writes `frame` to `path` as a PNG.
///
/// # Errors
///
/// Returns the encoder's message when the frame cannot be encoded, and the
/// file system's message when the file cannot be written.
pub fn write_frame(media: &Media, path: &Path, frame: &PresentedFrame) -> Result<(), String> {
    let bytes = encode_image(media, frame.width, frame.height, &frame.pixels, "png")
        .map_err(|error| error.to_string())?;
    std::fs::write(path, bytes).map_err(|error| format!("cannot write {}: {error}", path.display()))
}
