//! Integration tests for the top-level [`render::RenderPipeline`] facade.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/lib.rs`.

use std::time::Duration;

use api::error::RenderError;
use render::{
    BackendKind, NULL_MAX_TEXTURE_DIMENSION, PipelineConfig, TextureFormat, capabilities,
    create_render_pipeline, presentation_budget,
};

#[test]
fn default_config_is_headless() {
    let config = PipelineConfig::default();
    assert_eq!(config.backend, BackendKind::Null);
    assert_eq!(config.preferred_format, TextureFormat::Rgba8Srgb);

    let pipeline = create_render_pipeline(config).expect("null pipeline");
    assert_eq!(pipeline.backend(), BackendKind::Null);
    assert_eq!(pipeline.backend().name(), "null");

    let caps = capabilities(&pipeline);
    assert_eq!(caps.backend, BackendKind::Null);
    assert_eq!(caps.max_texture_dimension, NULL_MAX_TEXTURE_DIMENSION);
}

#[test]
fn vulkan_backend_is_either_available_or_reports_unsupported() {
    match create_render_pipeline(PipelineConfig::vulkan()) {
        Ok(pipeline) => assert_eq!(pipeline.backend(), BackendKind::Vulkan),
        Err(error) => assert!(matches!(
            error,
            RenderError::Unsupported { .. } | RenderError::DeviceUnavailable { .. }
        )),
    }

    assert_eq!(BackendKind::Vulkan.name(), "vulkan");
}

#[test]
fn the_presentation_budget_is_two_frame_periods() {
    // reference/cpp/core/visual/impl/BasicDrawDevice.cpp:244
    // reference/cpp/core/visual/impl/VSyncTimingThread.cpp:88-146
    // The reference's only display wait is bounded by its own vsync deadline,
    // so the port waits a bounded part of the frame cadence, not forever.
    let frame = Duration::from_millis(16);
    assert_eq!(
        presentation_budget(frame).expect("valid budget"),
        Duration::from_millis(32)
    );
}

#[test]
fn a_zero_frame_interval_is_a_typed_error_not_a_substituted_wait() {
    let error = presentation_budget(Duration::ZERO).expect_err("a zero interval has no budget");
    assert!(
        matches!(error, RenderError::InvalidArgument { .. }),
        "error was {error:?}"
    );
}

#[test]
fn a_degenerate_frame_interval_is_a_typed_error_not_a_wrapped_wait() {
    let error = presentation_budget(Duration::MAX).expect_err("a doubled maximum overflows");
    assert!(
        matches!(error, RenderError::InvalidArgument { .. }),
        "error was {error:?}"
    );
}
