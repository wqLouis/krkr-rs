//! Integration tests for [`api::error`].

use api::error::*;

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

// The reference is the oracle for behaviour, not for error strings: it raises a
// failed device creation distinctly from an unsupported technique and names the
// underlying code (reference/cpp/core/visual/impl/BasicDrawDevice.cpp:113-123,
// reference/cpp/core/base/impl/MsgImpl.h:131-133). This only checks that the
// wording keeps that distinction and never reads as "not supported".
#[test]
fn render_device_unavailable_is_not_reported_as_unsupported() {
    let error = RenderError::DeviceUnavailable {
        message: "no usable Vulkan physical device was handed back".to_owned(),
    };
    assert_eq!(
        error.to_string(),
        "render device unavailable: no usable Vulkan physical device was handed back"
    );
    assert!(!error.to_string().contains("not supported"));
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
