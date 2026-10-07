//! Transition native classes: `Trans`, `ChainItemBase`.
//!
//! Behavioral reference: `main`'s `crates/tvp-natives/src/extrans.rs` and
//! `chain_item_base.rs`. Composes L2 [`engine::layer_ops`] pixel operations.
//! Not implemented yet.

use crate::{CompatCtx, CompatError};

/// Register the transition classes.
pub fn register(_ctx: &mut CompatCtx) -> Result<(), CompatError> {
    Ok(())
}
