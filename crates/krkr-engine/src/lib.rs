//! krkr-rs middle layer.
//!
//! Owns all state: the TVP layer system, the storage namespace, the mixer,
//! and every bottom-defined state value (`RenderScene`, `RenderState`,
//! `FileState`, decoder states). Performs actions on the stateless bottom
//! objects. See `docs/migration/02-target-architecture.md`.
//!
//! This crate must not depend on Bevy, wgpu, `ash`, winit, FFmpeg, cpal or any
//! concrete backend ([`docs/migration/02-target-architecture.md`] Invariant B):
//! it talks only to the middle-layer API ([`krkr_api`]).
//!
//! [`docs/migration/02-target-architecture.md`]: ../../../docs/migration/02-target-architecture.md

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod layer_ops;
pub mod raster;
pub mod scene;

pub use layer_ops::{
    COPY_MAIN, COPY_MASK, GammaAdjust, RectI, adjust_gamma, affine_blit, blit_copy,
    blit_copy_clipped, blit_over, blit_plane, blur_bitmap_in_place, clip_rect, colorize,
    composite_primary_layer, convert_add_alpha_to_alpha, convert_alpha_to_add_alpha, copy_9patch,
    do_blur_light, do_drop_shadow, do_gray_scale, fill_color_hold_alpha, fill_color_on_add_alpha,
    fill_color_on_alpha, fill_mask, fill_operate_rect, flip_lr, flip_ud, gaussian_blur, intersect,
    intersect_rect, light_contrast, noise, operate_rect, remove_const_opacity, stretch_blit,
    stretch_blit_mode, stretch_blit_plane, tile_rect,
};
pub use raster::{
    bbox_size, blend_pixel, fill_polygon, fill_polygon_hatch, fill_rect_replace, flatten_arc,
    flatten_cubic, stroke_polyline,
};
pub use scene::{
    BitmapCache, BitmapState, CachedImage, FontState, LT_ADDITIVE, LT_ALPHA, LT_OPAQUE,
    LT_SUBTRACTIVE, LayerState, Rect, Scene, WindowState,
};
