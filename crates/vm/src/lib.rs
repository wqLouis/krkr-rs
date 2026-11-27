//! krkr-rs **L4 TJS VM host**.
//!
//! Wraps `tjs` (the TJS2 VM): lifecycle, the log callback, bytecode
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
    /// A script answer could not cross the VM boundary.
    ///
    /// The message names the shape the host refused, so the failure is not
    /// reported as an empty value.
    #[error("script: {0}")]
    Script(String),
    /// The Rust executor refused the script or its answer.
    ///
    /// The error is the executor's own typed failure: a compile, a block
    /// build, a code fault, or an object entry.
    #[error("engine: {0}")]
    Engine(#[from] tjs::vm::runtime::engine::EngineError),
    /// The compatibility surface failed.
    ///
    /// The message names the native registration, the engine operation or
    /// the poll that failed.
    #[error("compat: {0}")]
    Compat(#[from] compat::CompatError),
}
