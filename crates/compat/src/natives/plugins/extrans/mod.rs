//! `extrans.dll` plugin port: the extended transition types.
//!
//! The reference plugin's `V2Link` registers transition **handler providers**
//! into the engine's global registry; `Layer.beginTransition` then looks them
//! up by name. In krkr-rs the handlers are built into L2 (`engine_api`)
//! and dispatched by `Layer.beginTransition`, so this module registers no TJS
//! classes.
//!
//! The provider names (reference `reference/cpp/plugins/`: `wave.cpp`,
//! `mosaic.cpp`, `turn.cpp`, `rotatetrans.cpp`, `ripple.cpp`) are `wave`,
//! `mosaic`, `turn`, `rotatezoom`, `rotatevanish`, `rotateswap` and
//! `ripple`, alongside the engine's built-in `crossfade`, `universal` and
//! `scroll`.

use crate::{CompatCtx, CompatError};

/// No TJS surface: the transition handlers are built into L2.
pub fn register(_ctx: &mut CompatCtx) -> Result<(), CompatError> {
    Ok(())
}
