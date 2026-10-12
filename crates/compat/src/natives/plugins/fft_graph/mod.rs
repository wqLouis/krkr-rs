//! `fft_graph` plugin — a full port of `reference/cpp/plugins/fftgraph.cpp`.
//!
//! The entire reference module is:
//!
//! ```cpp
//! #include "ncbind.hpp"
//!
//! #define NCB_MODULE_NAME TJS_W("fftgraph.dll")
//!
//! static void InitPlugin() {
//!     TVPExecuteScript(TJS_W("function drawFFTGraph(){}"));
//! }
//!
//! NCB_PRE_REGIST_CALLBACK(InitPlugin);
//! ```
//!
//! `fftgraph.dll` registers exactly one global function, `drawFFTGraph`, and
//! the reference body is empty. It is the stub the reference `util_graph.dll`
//! (and some games) replace with their own implementation, so the contract is
//! only that the *name* exists and is a function.
//!
//! The port executes the same script so the script-visible surface is
//! identical: a global `drawFFTGraph` function that returns `void`. The
//! definition lives on the VM, so a game that later assigns
//! `drawFFTGraph = function(...) {...}` keeps working exactly as it does on
//! the reference.
#![allow(unsafe_code)]

use crate::{CompatCtx, CompatError};

/// Register the plugin's native surface on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.engine() }
        .exec_script("function drawFFTGraph(){}", "fftgraph")
        .map_err(|error| CompatError::Register(format!("fft_graph: {error}")))?;
    Ok(())
}
