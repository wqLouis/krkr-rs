//! `emote_player` plugin — the `emoteplayer.dll` loader shim.
//!
//! Behavioral reference: `reference/cpp/plugins/EmotePlayer.cpp`. The entire
//! reference module is:
//!
//! ```cpp
//! #include "ncbind.hpp"
//! #define NCB_MODULE_NAME TJS_W("emoteplayer.dll")
//!
//! static void EmotePlayerPreRegist() {
//!     // Load motionplayer.dll as dependency (matches libkrkr2.so sub_682528)
//!     ncbAutoRegister::LoadModule(TJS_W("motionplayer.dll"));
//! }
//!
//! NCB_PRE_REGIST_CALLBACK(EmotePlayerPreRegist);
//! ```
//!
//! `emoteplayer.dll` registers **no** classes of its own. Its only job is to
//! load `motionplayer.dll`; that DLL owns the whole `Motion` namespace
//! (`Motion`, `Player`, `EmotePlayer`, `D3DEmotePlayer`, `D3DEmoteModule`,
//! `ResourceManager`, `SeparateLayerAdaptor`, `D3DAdaptor`, `SourceCache`,
//! `ObjSource`, `Point`, `Circle`, `Rect`, `Quad`, `LayerGetter`).
//!
//! In the reference, `EmotePlayer` and the other shape/adaptor classes are
//! `Motion.*` subclasses (`reference/cpp/plugins/motionplayer/main.cpp:27-124`
//! `NCB_REGISTER_SUBCLASS_DELAY`, attached by `main.cpp:427-431`), so the
//! reference exposes no top-level `EmotePlayer`; only `Player`
//! (`main.cpp:128`) and the emoteplayer.dll `D3DEmoteModule`/`D3DEmotePlayer`
//! (`main.cpp:565,588`) are top-level classes.
//!
//! In krkr-rs those classes are registered statically by the sibling
//! [`motion_player`](crate::natives::plugins::motion_player) plugin, which the
//! plugin table declares as this entry's dependency and loads first. That port
//! publishes every
//! `Motion.*` subclass top-level (the class-registration API has no
//! namespace-only form) and then aliases it into `Motion`; the extra top-level
//! names are accepted and locked in `docs/surface/runtime_surface.json`. This
//! module therefore keeps only the reference's module boundary and a no-op
//! [`register`]: it must not duplicate any of the `motion_player` class
//! registrations (the compat VM rejects duplicate class names).
//!
//! The loader is kept explicit (rather than deleting the module) so the
//! aggregator's `emote_player::register(ctx)` call remains meaningful and the
//! plugin name/module mapping stays 1:1 with the reference DLL set.

use crate::{CompatCtx, CompatError};

/// Register the plugin's native surface on the VM.
///
/// `emoteplayer.dll` has no native surface of its own — it only loads
/// `motionplayer.dll` — so this is a no-op. The `Motion`/`EmotePlayer` classes
/// are installed by [`motion_player::register`](crate::natives::plugins::motion_player::register).
pub fn register(_ctx: &mut CompatCtx) -> Result<(), CompatError> {
    Ok(())
}
