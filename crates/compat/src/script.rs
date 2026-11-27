//! Running a script or an expression from a native member.
//!
//! A few members need a value only the script language can make: an instance
//! of a class no constructor surface exposes, a global read by name, a helper
//! closure. They run the text through the Rust executor, which compiles it
//! and runs its top-level context against the engine's global object, and
//! answers the value the text produced.
//!
//! # The retained form
//!
//! A member runs the text *for a value*, and the value has to outlive the
//! call: it is stored in the member's instance state, written into a
//! dictionary, or handed to a script as the member's answer. The entry
//! points therefore answer [`RetainedValue`]: a scalar as itself, an object
//! as a retention of the id that keeps it alive. An object the Rust object
//! model owns has no engine id; the answer is the typed refusal
//! [`AnswerError`](tjs::vm::runtime::engine::AnswerError) names rather than
//! an empty value.
//!
//! # The scope
//!
//! The text runs in the store the compatibility surface's script entries
//! share, so a function or class an earlier script declared is visible here
//! and the store's class registry answers the names it publishes. The scope
//! carries that store and the engine; the caller supplies its own execution
//! context, which is the `this` the text runs with, and the line offset it
//! reports.
//!
//! # The text, and what a caller may rely on
//!
//! [`ScriptScope::run`] compiles and runs its argument as a script; a
//! top-level `return` is its answer and falling off the end answers `void`.
//! [`ScriptScope::evaluate`] compiles it as an expression and answers its
//! value. A failure keeps the shape the script diagnostic has: `block(line):
//! message`, with the block name the caller supplied.
#![allow(unsafe_code)]

use std::ffi::c_void;

use tjs::vm::runtime::bridge::SharedGlobals;
use tjs::{RetainedValue, Tjs2Engine};

use crate::CompatCtx;
use crate::natives::scripts::{TextScriptError, run_text, script_options};

/// How one member's script runs.
///
/// The default runs the text against the global object, with no line offset:
/// the block the caller names is the block the failure names.
#[derive(Clone, Copy)]
pub(crate) struct ScriptOptions {
    /// The object the text runs with as its `this`, or null for the global
    /// object.
    pub(crate) context: *mut c_void,
    /// The line-number offset the caller reports.
    pub(crate) lineofs: i32,
}

impl Default for ScriptOptions {
    fn default() -> Self {
        Self {
            context: std::ptr::null_mut(),
            lineofs: 0,
        }
    }
}

/// The engine and the shared script store one member's text runs in.
///
/// The store is the one the compatibility surface's script entries share, so
/// what one script declared is visible to the next and the class registry
/// answers the names it publishes. A scope is built once per member call and
/// borrowed by the text it runs.
pub(crate) struct ScriptScope<'a> {
    /// The VM the text compiles and runs on.
    engine: &'a Tjs2Engine,
    /// The global-member store the text runs in.
    globals: SharedGlobals,
}

impl<'a> ScriptScope<'a> {
    /// The scope a member reached through `ctx` runs its text in.
    pub(crate) fn new(engine: &'a Tjs2Engine, ctx: &CompatCtx) -> Self {
        // SAFETY: `ctx` names the installed, live binding, which the caller
        // holds for the call it runs the text for.
        let globals = unsafe { ctx.services() }.script_globals();
        Self { engine, globals }
    }

    /// The engine the scope's text runs on.
    pub(crate) fn engine(&self) -> &'a Tjs2Engine {
        self.engine
    }

    /// Compiles `source` and runs it as a script, answering the value it
    /// produced.
    ///
    /// # Errors
    ///
    /// The compile, block-build or run failure as `block(line): message`, and
    /// the refusal when an object answer cannot be retained.
    pub(crate) fn run(
        &self,
        source: &str,
        block_name: &str,
        options: ScriptOptions,
    ) -> Result<RetainedValue, TextScriptError> {
        run_text(
            self.engine,
            source,
            block_name,
            false,
            script_options(options.context, options.lineofs, false),
            &self.globals,
        )
    }

    /// Compiles `expression` and answers the value it produced.
    ///
    /// # Errors
    ///
    /// The compile, block-build or run failure as `block(line): message`, and
    /// the refusal when an object answer cannot be retained.
    pub(crate) fn evaluate(
        &self,
        expression: &str,
        block_name: &str,
        options: ScriptOptions,
    ) -> Result<RetainedValue, TextScriptError> {
        run_text(
            self.engine,
            expression,
            block_name,
            true,
            script_options(options.context, options.lineofs, true),
            &self.globals,
        )
    }
}
