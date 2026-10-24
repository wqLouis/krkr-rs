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
//! # The public surface
//!
//! Everything an L4 host (or a test) can reach is re-exported here, and
//! nothing else is public:
//!
//! | group | items |
//! |---|---|
//! | the host and its context | [`CompatHost`], [`CompatCtx`], [`CompatServices`], [`CompatError`] |
//! | the frame body | [`FrameBody`], [`run_frame_body`] |
//! | the event adapter | [`EventAdapter`], [`EventValue`], [`TjsEvent`] |
//! | the per-frame polls | [`timer_poll`], [`sound_poll`], [`sound_render`], [`video_advance`], [`async_trigger_poll`], [`continuous_handler_poll`], [`movie_poll`] |
//! | the tree-owner hand-offs | [`advance_transitions`], [`advance_self_updating_transitions`], [`paint_poll`], [`refresh_cached_layers`] |
//! | the host seams | [`VideoSource`], [`set_video_source`], [`set_movie_source`], [`AudioDecoder`], [`add_audio_decoder`], [`set_output_latency`], [`set_sound_render_driven`] |
//! | the request drains | [`take_exit_request`], [`take_bring_to_front_requests`], [`take_update_requests`], [`take_capture_releases`] |
//! | the startup script | [`exec_storage`] |
//! | the plugin architecture | [`NativePlugin`], [`PluginOutcome`], [`PluginRegistry`], [`normalize_plugin_name`], [`plugin_stem`], [`builtin_plugin_names`] |
//!
//! Every item above states its ownership, its error contract and its panic
//! and FFI-safety rules in its own documentation. The L2 seam
//! (`engine_api`) and the per-family modules stay crate-internal.
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
#![deny(unsafe_code)]

mod binding;
mod collections;
mod ctx;
// The single seam through which compat names the engine crate.
mod engine_api;
mod event;
mod frame;
mod host;
mod native_util;
mod natives;
mod null_plugin;
mod plugin;
mod plugins_native;
mod resource;
mod storage;
mod text;

pub use ctx::{CompatCtx, CompatServices};
pub use event::{EventAdapter, EventValue, TjsEvent};
pub use frame::{FrameBody, run_frame_body};
pub use host::CompatHost;
// The built-in `Timer` delivery for a host or a test that advances the
// attached registry itself. The frame body's own drain of the `onTimer` posts
// is crate-internal: the frame body runs it, no host calls it.
pub use natives::visual::timer_poll;
// The built-in `VideoOverlay` advance: an L4 [`FrameBody`] forwards its
// `video_poll` step to `video_advance` with its own per-frame `dt`;
// `VideoSource`/`set_video_source` install the host-decoded source.
pub use natives::video::{VideoSource, set_video_source, video_advance};
// The built-in `WaveSoundBuffer` poll, plus the host seam that installs the L1
// media audio decoder (L3 never links a codec).
pub use natives::sound::{
    AudioDecoder, add_audio_decoder, set_output_latency, set_sound_render_driven, sound_poll,
    sound_render,
};
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
// The tree-owner / frame-body hand-offs: the frame body's `frame_tail` step
// advances normal layer transitions and composites the script-cached layers;
// the after-projection step advances the self-updating transitions whose layer
// was drawn. The body also drains the host's window/capture requests.
pub use natives::visual::{
    advance_self_updating_transitions, advance_transitions, paint_poll, refresh_cached_layers,
    take_bring_to_front_requests, take_capture_releases, take_update_requests,
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
///
/// The two variants separate the two failure kinds a host can act on: a
/// [`Register`](Self::Register) failure means the VM refused a class or a
/// member, an [`Engine`](Self::Engine) failure means an L2 operation or the
/// host's own wiring failed. The message is what the host logs; it is not a
/// script-visible string unless a native forwards it.
#[derive(Debug, thiserror::Error)]
pub enum CompatError {
    /// A native class could not be registered.
    #[error("native registration: {0}")]
    Register(String),
    /// An engine (L2) operation failed, or the host is not installed yet.
    #[error("engine: {0}")]
    Engine(String),
}

/// Register the built-in compatibility surface on the VM: the core native
/// classes, the `Plugins` class, the null-plugin hook, and the one plugin
/// module available before any script runs
/// ([`natives::plugins::STARTUP_PLUGINS`]).
///
/// Every other built-in plugin's members appear on the first `Plugins.link`.
/// Called by [`CompatHost::install`]; idempotence is the caller's
/// responsibility (class registration rejects duplicates).
///
/// # Errors
///
/// Returns [`CompatError::Register`] when a class, a member or the null-plugin
/// hook cannot be registered on the VM. A failure leaves the VM without the
/// compatibility surface; the caller must not retry against the same engine,
/// because class registration rejects duplicates.
pub(crate) fn install(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    null_plugin::register_hook(ctx)?;
    natives::register_all(ctx)?;
    plugins_native::register(ctx)
}
