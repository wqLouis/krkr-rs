//! Core TVP native classes, implemented against the L2 API.
//!
//! Each family lives in its own module with a `register(ctx)` entry point;
//! [`register_all`] is the single call the host makes. The classes are thin
//! adapters: they translate TJS values to and from the L2 (`engine`) state and
//! actions and own no engine state themselves.
//!
//! The `tvp-*` crates are the *behavioral* reference only; they are not
//! copied.

pub mod async_trigger;
pub mod cdda_midi;
pub mod clipboard;
pub mod constants;
pub mod debug;
pub mod image_function;
pub mod input;
pub mod kag;
pub mod menu_item;
pub mod plugins;
pub mod rect;
pub mod scripts;
pub mod sound;
pub mod storages;
pub mod system;
pub mod tree_owner;
pub mod video;
pub mod visual;

use crate::{CompatCtx, CompatError};

/// Register every implemented core native class on the VM.
pub fn register_all(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    constants::register(ctx)?;
    system::register(ctx)?;
    debug::register(ctx)?;
    storages::register(ctx)?;
    scripts::register(ctx)?;
    visual::register(ctx)?;
    input::register(ctx)?;
    sound::register(ctx)?;
    cdda_midi::register(ctx)?;
    async_trigger::register(ctx)?;
    menu_item::register(ctx)?;
    video::register(ctx)?;
    rect::register(ctx)?;
    image_function::register(ctx)?;
    clipboard::register(ctx)?;
    plugins::register(ctx)?;
    tree_owner::register(ctx)?;
    Ok(())
}
