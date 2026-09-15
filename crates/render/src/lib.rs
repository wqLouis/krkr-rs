//! Bevy render crate (`krkr_render`): turns the logical tvp-visual
//! [`Scene`] into rendered Bevy entities.
//!
//! Architecture (see WAVE3.md, SA-5): the VM + natives run on Bevy's main
//! thread and mutate [`Scene`] under a write lock; the systems in this crate
//! read it under a read lock and drive Bevy entities. No Bevy state lives in
//! the natives — everything flows through the shared [`Scene`].
//!
//! Text rendering (fonts → glyph atlas) is milestone 3B and out of scope;
//! [`tvp_visual::scene::FontState`] entries are currently ignored by the
//! sync system.

pub mod blend;
pub mod input_bridge;
pub mod menu;
pub mod runner;
pub mod sync;

pub use blend::{
    LT_ADD_ALPHA, LT_ADDITIVE, LT_ALPHA, LT_BINDER, LT_DARKEN, LT_DODGE, LT_EFFECT, LT_FILTER,
    LT_LIGHTEN, LT_MULTIPLICATIVE, LT_OPAQUE, LT_PS_ADDITIVE, LT_PS_COLOR_BURN, LT_PS_COLOR_DODGE,
    LT_PS_COLOR_DODGE5, LT_PS_DARKEN, LT_PS_DIFFERENCE, LT_PS_DIFFERENCE5, LT_PS_EXCLUSION,
    LT_PS_HARD_LIGHT, LT_PS_LIGHTEN, LT_PS_MULTIPLICATIVE, LT_PS_NORMAL, LT_PS_OVERLAY,
    LT_PS_SCREEN, LT_PS_SOFT_LIGHT, LT_PS_SUBTRACTIVE, LT_SCREEN, LT_SUBTRACTIVE,
    LayerBlendMaterial, LayerBlendMode, LayerBlendPlugin, LayerRenderPath, blend_mode_for,
    blend_state_for, render_path_for,
};
pub use menu::{MenuEntry, MenuNode, MenuPlugin, MenuRoot, MenuUiState, visible_nodes};
pub use sync::{
    BitmapAssets, GpuPrimitives, SceneCamera, SceneSprite, SharedScene, WindowRoot, clamp_opacity,
    rect_center, sprite_z, sync_scene,
};
