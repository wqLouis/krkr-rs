//! krkr-rs **L3 KiriKiri compatibility surface**.
//!
//! Native classes (`System`, `Storages`, `Scripts`, `Window`/`Layer`/
//! `Bitmap`, …), the KAG parser, the L1→TJS event adapter and the static
//! native plugins. It implements the script API by *composing* L2
//! primitives; it owns no engine state and contains no platform code.
//!
//! See `docs/migration/08-tjs-vm-and-native-plugins.md`.

/// Errors from the compatibility surface.
#[derive(Debug, thiserror::Error)]
pub enum CompatError {
    /// A native class could not be registered.
    #[error("native registration: {0}")]
    Register(String),
    /// An engine (L2) operation failed.
    #[error("engine: {0}")]
    Engine(String),
    /// The compatibility surface is not implemented yet.
    #[error("unsupported: {0}")]
    Unsupported(String),
}
