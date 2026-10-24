//! krkr-rs **L4 TJS VM host**.
//!
//! Wraps `tjs` (the C++ VM): lifecycle, the log callback, bytecode
//! compilation, the script cache and the per-frame body. It drives the L3
//! compatibility surface and uses the L1 platform abstraction; it is the
//! topmost layer.
//!
//! [`VmHost::new`] creates the engine and routes its log output through the
//! `log` crate, [`VmHost::exec`] runs a script, and [`VmHost::tick`] runs the
//! frame body.
//!
//! The host owns the engine and the L3 dispatch state; it owns no L2 state.
//! Every L2 value it touches is borrowed through the service handles the
//! composition root attached before the compatibility surface was
//! installed.

#![warn(missing_docs)]
#![deny(unsafe_code)]

mod host;

pub use host::{FrameCtx, VmHost};

/// Errors from the VM host.
#[derive(Debug, thiserror::Error)]
pub enum VmError {
    /// The script engine could not be created.
    ///
    /// The message names the engine's own reason; no script has run yet.
    #[error("vm: {0}")]
    Vm(String),
    /// A script failed to compile or to run.
    ///
    /// The message is the engine's own: it carries the block name and the
    /// line the failure happened on.
    #[error("script: {0}")]
    Script(String),
    /// The compatibility surface failed.
    ///
    /// The message names the native registration, the engine operation or
    /// the poll that failed.
    #[error("compat: {0}")]
    Compat(#[from] compat::CompatError),
}
