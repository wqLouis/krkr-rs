//! `getabout.dll` plugin.
//!
//! The reference plugin (`reference/cpp/plugins/getabout.cpp`) is a single
//! attachment — `NCB_ATTACH_FUNCTION(getAboutString, System,
//! TVPGetAboutString)` — so it registers one member on the *existing*
//! `System` class and defines no class of its own.
//!
//! That member, `System.getAboutString`, is registered once at startup by the
//! `windowEx` port (`natives/plugins/window_ex/system.rs`), which owns the
//! whole `System` extension surface. Registering it again here would be a
//! duplicate class member (the VM rejects that), so this plugin legitimately
//! installs nothing: linking `getabout.dll` resolves to the built-in table,
//! and the surface is already present.

use crate::{CompatCtx, CompatError};

/// `getabout.dll` adds nothing beyond what the `windowEx` port registers —
/// see the module docs.
pub fn register(_ctx: &mut CompatCtx) -> Result<(), CompatError> {
    Ok(())
}
