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

#![warn(missing_docs)]
#![deny(unsafe_code)]

mod host;

pub use host::{FrameCtx, VmHost};

/// Errors from the VM host.
#[derive(Debug, thiserror::Error)]
pub enum VmError {
    /// The VM could not be created.
    #[error("vm: {0}")]
    Vm(String),
    /// Script compilation or execution failed.
    #[error("script: {0}")]
    Script(String),
    /// The compatibility surface failed.
    #[error("compat: {0}")]
    Compat(#[from] compat::CompatError),
}
