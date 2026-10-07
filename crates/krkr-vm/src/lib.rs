//! krkr-rs **L4 TJS VM host**.
//!
//! Wraps `tjs2-sys` (the C++ VM): lifecycle, the log callback, bytecode
//! compilation, the script cache and the per-frame body. It drives the L3
//! compatibility surface and uses the L1 platform abstraction; it is the
//! topmost layer.
//!
//! See `docs/migration/08-tjs-vm-and-native-plugins.md`.

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
    Compat(#[from] krkr_compat::CompatError),
}
