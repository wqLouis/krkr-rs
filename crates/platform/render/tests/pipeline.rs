//! Integration tests for the top-level [`render::RenderPipeline`] facade.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/lib.rs`.

use api::error::RenderError;
use render::{
    BackendKind, NULL_MAX_TEXTURE_DIMENSION, PipelineConfig, TextureFormat, capabilities,
    create_render_pipeline,
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
        Err(error) => assert!(matches!(error, RenderError::Unsupported { .. })),
    }

    assert_eq!(BackendKind::Vulkan.name(), "vulkan");
}
