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

pub mod constants;
pub mod debug;
pub mod kag;
pub mod scripts;
pub mod storages;
pub mod system;

use crate::{CompatCtx, CompatError};

/// Register every implemented core native class on the VM.
pub fn register_all(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    constants::register(ctx)?;
    system::register(ctx)?;
    debug::register(ctx)?;
    storages::register(ctx)?;
    scripts::register(ctx)?;
    kag::register(ctx)?;
    Ok(())
}
