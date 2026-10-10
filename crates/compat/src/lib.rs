//! krkr-rs **L3 KiriKiri compatibility surface**.
//!
//! Native classes (`System`, `Storages`, `Scripts`, `Window`/`Layer`/
//! `Bitmap`, …), the KAG parser, the L1→TJS event adapter and the static
//! native plugins. It implements the script API by *composing* L2
//! primitives; it owns no engine state and contains no platform code.
//!
//! The plugin/VM architecture:
//!
//! * [`NativePlugin`] / [`PluginRegistry`] — the statically built-in plugin
//!   set and activation by name;
//! * the **null plugin object** (the `null_plugin` module) for DLLs we
//!   cannot load;
//! * [`CompatCtx`] / [`CompatServices`] — the explicit L2 service handles
//!   natives register against;
//! * [`CompatHost`] — owns the dispatch state and installs the one
//!   process-wide engine → context binding the C ABI forces;
//! * [`EventAdapter`] — the L1 → L3 → L4 event mapping skeleton;
//! * [`FrameBody`] / [`run_frame_body`] — the ordered per-frame poll body.
//!
//! # The one process-wide binding
//!
//! The C++ ABI hands native callbacks only the raw `tjs2_engine*` and
//! `tjs` exposes no user-data slot. `compat` therefore keeps
//! exactly **one** process-wide engine → context table (the private
//! `binding` module): it maps the raw engine pointer to the Rust
//! [`Tjs2Engine`](tjs::Tjs2Engine), the [`CompatServices`] and the
//! [`PluginRegistry`]. It is L3-only (never L1), holds dispatch state and
//! never game state, and is installed/removed by [`CompatHost`].

#![warn(missing_docs)]

mod binding;
mod ctx;
mod event;
mod frame;
mod host;
mod native_util;
mod natives;
mod null_plugin;
mod plugin;
mod plugins_native;
mod text;

pub use ctx::{CompatCtx, CompatServices};
pub use event::{EventAdapter, EventValue, TjsEvent};
pub use frame::{FrameBody, run_frame_body};
pub use host::CompatHost;
// The built-in `Timer` poll: an L4 [`FrameBody`] forwards its `timer_poll`
// step here (the visual natives' registry lives in this crate).
pub use natives::visual::timer_poll;
// The built-in `VideoOverlay` poll: an L4 [`FrameBody`] forwards its `video_poll`
// step here; `video_advance` is exposed for hosts that drive it directly.
pub use natives::video::{video_advance, video_poll};
// The built-in `WaveSoundBuffer` poll, plus the host seam that installs the L1
// media audio decoder (L3 never links a codec).
pub use natives::sound::{AudioDecoder, add_audio_decoder, sound_poll, sound_render};
// The disc-sound host seam that installs the L1 media audio decoder for
// `CDDASoundBuffer`/`MIDISoundBuffer` (mirrors [`add_audio_decoder`]).
pub use natives::cdda_midi::set_disc_audio_decoder;
// The built-in `System` poll and the host's exit-request drain: an L4
// [`FrameBody`] forwards its `continuous_handler_poll` step here and the frame
// loop consumes `take_exit_request`.
pub use natives::system::{continuous_handler_poll, take_exit_request};
// The core of `Scripts.execStorage` without the native wrapper: the
// composition root runs the startup sequence through this.
pub use natives::scripts::exec_storage;
// The built-in `AsyncTrigger` poll: an L4 [`FrameBody`] forwards its
// `async_trigger_poll` step here (deferred script callbacks).
pub use natives::async_trigger::async_trigger_poll;
// The tree-owner / frame-body hand-offs: the frame body's `project_and_render`
// step advances layer transitions and drains the host's window/capture
// requests.
pub use natives::visual::{
    advance_transitions, paint_poll, take_bring_to_front_requests, take_capture_releases,
    take_update_requests,
};
// The `LayerExMovie` frame body: an L4 [`FrameBody`] forwards its per-frame
// movie step here, and `set_movie_source` installs the host's decoded source
// (the analogue of the reference `TVPAddContinuousEventHook`).
pub use natives::plugins::layer_ex::{movie_poll, set_movie_source};
pub use plugin::{NativePlugin, PluginOutcome, PluginRegistry, normalize_plugin_name, plugin_stem};

/// The DLL names of every ported plugin (the `Plugins.link` targets).
///
/// A host or test can link them all to expose the full plugin surface; the
/// product leaves linking to the game.
pub fn builtin_plugin_names() -> impl Iterator<Item = &'static str> {
    natives::plugins::BUILTIN_PLUGINS.iter().map(|def| def.name)
}

/// Errors from the compatibility surface.
#[derive(Debug, thiserror::Error)]
pub enum CompatError {
    /// A native class could not be registered.
    #[error("native registration: {0}")]
    Register(String),
    /// An engine (L2) operation failed, or the host is not installed yet.
    #[error("engine: {0}")]
    Engine(String),
}

/// Register the built-in compatibility surface on the VM: the `Plugins`
/// native class and the null-plugin hook.
///
/// Called by [`CompatHost::install`]; idempotence is the caller's
/// responsibility (class registration rejects duplicates).
pub fn install(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    null_plugin::register_hook(ctx)?;
    natives::register_all(ctx)?;
    plugins_native::register(ctx)
}
