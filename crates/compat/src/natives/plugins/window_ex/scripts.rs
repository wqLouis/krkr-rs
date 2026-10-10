//! `Scripts` static members added by `windowEx.dll`.
//!
//! Reference `WindowEx.cpp` (`struct Scripts`): `Scripts.eval` is wrapped so
//! that `Scripts.setEvalErrorLog(flag)` controls whether an evaluation error
//! is written to the log. The default is `true` (the reference's
//! `outputErrorLogOnEval`).
//!
//! The override reproduces the core `Scripts.eval` behavior (evaluate the
//! expression, optional name, ignore the unmarshallable `lineofs`/`context`
//! arguments) and adds the conditional log line.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{Engine, NativeMethodDef, NativeStaticMembers, VAL_VOID, Value};

use crate::binding;
use crate::native_util::{args, report_error, set_bool, value_as_string};
use crate::{CompatCtx, CompatError};

use super::util::{as_bool, finish_retained};

/// Per-binding `Scripts` plugin state.
struct ScriptsExState {
    /// `Scripts.outputErrorLogOnEval`, default `true`.
    output_error_log_on_eval: bool,
}

impl Default for ScriptsExState {
    fn default() -> Self {
        Self {
            output_error_log_on_eval: true,
        }
    }
}

/// `Scripts.eval(content[, name[, lineofs[, context]]])` override.
extern "C" fn scripts_eval(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return report_error(out_error, "Scripts.eval: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: `binding::resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(expression) = arguments.first() else {
        return report_error(out_error, "Scripts.eval requires at least 1 argument");
    };
    let expression = value_as_string(expression);
    let name = arguments.get(1).map(value_as_string).unwrap_or_default();
    if arguments.get(2).is_some_and(|value| value.ty != VAL_VOID) {
        log::warn!("Scripts.eval: the lineofs argument cannot cross the ABI; ignoring");
    }
    if arguments.get(3).is_some_and(|value| value.ty != VAL_VOID) {
        log::warn!("Scripts.eval: the execution-context argument is ignored");
    }
    let log_errors = services.state::<ScriptsExState>().output_error_log_on_eval;
    let result = resolved
        .engine()
        .eval_retained(&expression, &name)
        .map_err(|error| error.to_string());
    if log_errors && let Err(error) = &result {
        log::error!("Scripts.eval({name:?}): {error}");
    }
    finish_retained(out, out_error, services, result)
}

/// `Scripts.setEvalErrorLog(flag)` — store the flag and return the previous
/// one (reference behavior, `reference/cpp/plugins/windowEx.cpp:1535-1539`).
/// The reference signature is `setEvalErrorLog(bool v)`, which ncbind
/// enforces as a one-parameter call (`reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn scripts_set_eval_error_log(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Scripts.setEvalErrorLog requires 1 argument");
    }
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return report_error(
            out_error,
            "Scripts.setEvalErrorLog: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: `binding::resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let enabled = match as_bool(&arguments[0]) {
        Ok(enabled) => enabled,
        Err(error) => {
            return report_error(out_error, &format!("Scripts.setEvalErrorLog: {error}"));
        }
    };
    let mut state = services.state::<ScriptsExState>();
    let previous = state.output_error_log_on_eval;
    state.output_error_log_on_eval = enabled;
    set_bool(out, previous);
    0
}

/// Register the `Scripts` static members added by `windowEx.dll`.
pub(super) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Scripts",
        methods: vec![
            NativeMethodDef {
                name: "eval",
                f: scripts_eval,
            },
            NativeMethodDef {
                name: "setEvalErrorLog",
                f: scripts_set_eval_error_log,
            },
        ],
        properties: vec![],
    })
}
