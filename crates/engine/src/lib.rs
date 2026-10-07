//! krkr-rs middle layer.
//!
//! Owns all state: the TVP layer system, the storage namespace, the mixer,
//! and every bottom-defined state value (`RenderScene`, `RenderState`,
//! `FileState`, decoder states). Performs actions on the stateless bottom
//! objects. See `docs/migration/02-target-architecture.md`.
//!
//! This crate must not depend on Bevy, wgpu, `ash`, winit, FFmpeg, cpal or any
//! render/media backend ([`docs/migration/02-target-architecture.md`] Invariant
//! B). The storage namespace is the exception described in
//! `docs/migration/06-file-system.md`: it is built over the bottom
//! [`fs::FileSystem`] object, while the remaining bottom services are
//! reached through the API types ([`api`]).
//!
//! [`docs/migration/02-target-architecture.md`]: ../../../docs/migration/02-target-architecture.md

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod layer_ops;
pub mod projection;
pub mod raster;
pub mod scene;
pub mod sound;
pub mod startup;
pub mod storage;
pub mod text;

// Archive name normalization is a shared storage-naming rule; the parser
// itself now lives in L1 `fs::xp3`.
pub use fs::xp3::normalize_in_archive_name;
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
pub use startup::{StartupError, StartupScript, StartupScripts, discover_startup};
pub use storage::{
    ATTR_DIRECTORY, ATTR_NORMAL, Location, MountError, Storage, StorageMetadata, StorageReadError,
};
// The four `LT_*` values above already exist in `scene`; the projection adds
// the rest of the native set without duplicating them.
pub use projection::{
    ComposedLayer, LT_ADD_ALPHA, LT_BINDER, LT_DARKEN, LT_DODGE, LT_EFFECT, LT_FILTER, LT_LIGHTEN,
    LT_MULTIPLICATIVE, LT_PS_ADDITIVE, LT_PS_COLOR_BURN, LT_PS_COLOR_DODGE, LT_PS_COLOR_DODGE5,
    LT_PS_DARKEN, LT_PS_DIFFERENCE, LT_PS_DIFFERENCE5, LT_PS_EXCLUSION, LT_PS_HARD_LIGHT,
    LT_PS_LIGHTEN, LT_PS_MULTIPLICATIVE, LT_PS_NORMAL, LT_PS_OVERLAY, LT_PS_SCREEN,
    LT_PS_SOFT_LIGHT, LT_PS_SUBTRACTIVE, LT_SCREEN, blend_mode_for, clamp_opacity, compose_layer,
    project_scene, project_window, resolve_blend, visible_image_region,
};
