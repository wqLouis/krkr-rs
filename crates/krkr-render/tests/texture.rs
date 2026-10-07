//! Integration tests for texture creation and upload validation.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/lib.rs`.

mod common;

use krkr_api::error::RenderError;
use krkr_api::geometry::PixelRegion;
use krkr_render::{
    RenderPipeline, TextureDesc, begin_render_state, create_texture, upload_texture,
};

#[test]
fn upload_validates_region_and_length() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) =
        begin_render_state(&pipeline, &common::headless_window()).expect("begin");
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
