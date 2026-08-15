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

pub mod sync;

pub use sync::{
    BitmapAssets, SceneCamera, SceneSprite, SharedScene, WindowRoot, clamp_opacity, rect_center,
    sprite_z, sync_scene,
};
