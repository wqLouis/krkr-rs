//! Integration tests for [`krkr_api::error`].

use krkr_api::error::*;

#[test]
fn fs_error_display_is_descriptive() {
    let error = FsError::NotFound {
        path: "/tmp/x".to_owned(),
    };
    assert_eq!(error.to_string(), "file-system path not found: /tmp/x");
}

#[test]
fn media_error_display_is_descriptive() {
    let error = MediaError::Decode {
        format: "png".to_owned(),
        message: "bad crc".to_owned(),
    };
    assert_eq!(error.to_string(), "failed to decode media (png): bad crc");
    assert_eq!(MediaError::EndOfStream.to_string(), "end of media stream");
}

#[test]
fn glyph_missing_glyph_formats_codepoint() {
    let error = GlyphError::MissingGlyph { codepoint: 0x1F600 };
    assert_eq!(error.to_string(), "font has no glyph for U+1F600");
}

#[test]
fn errors_are_cloneable_values() {
    let error = RenderError::SurfaceLost;
    assert_eq!(error.clone(), error);
    assert_eq!(
        AudioError::AlreadyRunning.to_string(),
        "audio stream is already running"
    );
}
