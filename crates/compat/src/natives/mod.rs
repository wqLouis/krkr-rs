//! Core TVP native classes (doc 08 §3), implemented against the L2 API.
//!
//! Each family lives in its own module with a `register(ctx)` entry point;
//! [`register_all`] is the single call the host makes. The classes are thin
//! adapters: they translate TJS values to and from the L2 (`engine`) state and
//! actions and own no engine state themselves.
//!
//! The old `tvp-*` crates (on `main`) are the *behavioral* reference only —
//! they were written against the pre-layer engine and hid state in process
//! globals, so they are not copied. Families are added here as they land.

pub mod async_trigger;
pub mod constants;
pub mod debug;
pub mod input;
pub mod kag;
pub mod menu_item;
pub mod scripts;
pub mod storages;
pub mod system;
pub mod trans;
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
    kag::register(ctx)?;
    visual::register(ctx)?;
    input::register(ctx)?;
    trans::register(ctx)?;
    async_trigger::register(ctx)?;
    menu_item::register(ctx)?;
    video::register(ctx)?;
    Ok(())
}
