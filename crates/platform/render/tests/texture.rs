//! Integration tests for texture creation and upload validation.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/lib.rs`.

use api::error::RenderError;
use api::geometry::PixelRegion;
use render::{RenderPipeline, TextureDesc, begin_render_state, create_texture, upload_texture};
use testkit::headless_window;

#[test]
fn upload_validates_region_and_length() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let texture = create_texture(&pipeline, TextureDesc::new(4, 4)).expect("texture");

    let out_of_bounds = upload_texture(
        &pipeline,
        &mut state,
        &texture,
        PixelRegion::new(3, 0, 2, 1),
        &[0u8; 8],
    );
    assert!(matches!(out_of_bounds, Err(RenderError::Upload { .. })));

    let wrong_len = upload_texture(
        &pipeline,
        &mut state,
        &texture,
        PixelRegion::new(0, 0, 2, 1),
        &[0u8; 4],
    );
    assert!(matches!(wrong_len, Err(RenderError::Upload { .. })));

    let empty = upload_texture(&pipeline, &mut state, &texture, PixelRegion::ZERO, &[]);
    assert!(matches!(empty, Err(RenderError::Upload { .. })));
}

#[test]
fn create_texture_rejects_invalid_descriptors() {
    let pipeline = RenderPipeline::null();
    assert!(matches!(
        create_texture(&pipeline, TextureDesc::new(0, 4)),
        Err(RenderError::InvalidArgument { .. })
    ));
    assert!(matches!(
        create_texture(&pipeline, TextureDesc::new(4, 0)),
        Err(RenderError::InvalidArgument { .. })
    ));
    assert!(matches!(
        create_texture(&pipeline, TextureDesc::new(4, 4).with_mip_levels(0)),
        Err(RenderError::InvalidArgument { .. })
    ));
    assert!(matches!(
        create_texture(&pipeline, TextureDesc::new(u32::MAX, 1)),
        Err(RenderError::InvalidArgument { .. })
    ));
}

/// The Vulkan upload ring merges every texture of one `upload_textures` call
/// into a single queue submit, so a frame's dirty bitmaps cost one submit, not
/// one per texture.
///
/// A headless CI machine has no window surface, so this returns early when the
/// Vulkan device or surface cannot be created. On a machine with a GPU it
/// asserts the submit count, which is the host-observable half of the ring
/// contract (the null backend cannot count GPU submits).
#[test]
fn vulkan_upload_batch_is_one_submit_or_skips() {
    let Ok(pipeline) = render::create_render_pipeline(render::PipelineConfig::vulkan()) else {
        return;
    };
    let Ok((_surface, mut state)) = render::begin_render_state(&pipeline, &headless_window())
    else {
        return;
    };
    let a = render::create_texture(&pipeline, render::TextureDesc::new(4, 4)).expect("texture a");
    let b = render::create_texture(&pipeline, render::TextureDesc::new(4, 4)).expect("texture b");
    let a_pixels = [1u8; 16];
    let b_pixels = [2u8; 16];

    let before = render::vulkan::upload_submit_count(&state).expect("vulkan state");
    render::upload_textures(
        &pipeline,
        &mut state,
        &[
            render::TextureUpload {
                texture: &a,
                region: PixelRegion::new(0, 0, 2, 2),
                pixels: &a_pixels,
            },
            render::TextureUpload {
                texture: &b,
                region: PixelRegion::new(0, 0, 2, 2),
                pixels: &b_pixels,
            },
        ],
    )
    .expect("batch upload");
    let after = render::vulkan::upload_submit_count(&state).expect("vulkan state");
    assert_eq!(
        after - before,
        1,
        "two textures in one batch must be one submit"
    );
}
