//! Running one compiled script: the text in, the top-level call out.
//!
//! The entries below are the ones a host calls instead of the engine's own
//! execute and evaluate entries: each compiles the text with the front end,
//! builds the block the image describes, and runs the block's top-level
//! context against the scope's global object. The object that context reaches
//! is the scope's [`GlobalObject`](crate::vm::runtime::global::GlobalObject), so
//! a script that creates an engine object, reads its members and calls it is
//! running in the Rust executor while the values are still the engine's.
//!
//! # The receiver and the global
//!
//! The top-level call's receiver is the scope's global object, which is also the
//! machine's global. A top-level script runs in the global scope, so a `var` the
//! script declares and a `function` it declares are both members of that one
//! object and outlive the call. The block's top-level context is the code's own
//! structure; it does not receive the script's members. A caller that supplies a
//! context runs the same top-level context with that object as its receiver,
//! which is the execution context the compatibility surface exposes.
//!
//! # The global object and the engine's
//!
//! The scope keeps one global object, so a member one call declares is there
//! for the next: the object owns the members and the class objects and asks the
//! engine's global object as one source for a name it does not hold, which is
//! where the resolution order is written ([`crate::vm::runtime::global`]). A
//! declared function is a member of that object, so it is callable by the call
//! that follows whether the object it was declared on is the global object or a
//! context's.

use std::rc::Rc;

use crate::vm::runtime::bridge::CHandle;
use crate::vm::runtime::bridge::{SharedGlobals, global_members, global_object, scope_global};
use crate::vm::runtime::exec::build::{Block, BuildError};
use crate::vm::runtime::exec::throw::Unwind;
use crate::vm::runtime::exec::throw::{self, FrameTrace};
use crate::vm::runtime::exec::{Fault, Machine};
use crate::vm::runtime::object::{Dispatch, ObjectError};
use crate::vm::runtime::variant::Variant;
use crate::vm::{ParseError, compile, compile_expression_result};
use crate::{RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue};

/// The reason a script did not run.
///
/// Every branch carries the typed failure its own part returned: the front
/// end's compile failure, the builder's, a code fault, or an object the
/// executor reached refusing an entry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The script text does not compile.
    #[error(transparent)]
    Compile(#[from] ParseError),
    /// The compiled image does not describe a block.
    #[error(transparent)]
    Build(#[from] BuildError),
    /// The block's top-level code raised or reached an instruction this
    /// interpreter does not run.
    #[error(transparent)]
    Run(#[from] Unwind),
    /// An object the code reached refused the entry.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// The engine did not hand out its global object.
    #[error("the engine has no global object: {0}")]
    Global(String),
    /// The engine refused the execution context.
    #[error("the execution context cannot be used: {0}")]
    Context(String),
}

/// The reason a script answer cannot leave the runtime.
///
/// A scalar crosses as itself, and an object crosses as the engine's handle or
/// as a retention of it. The retained form publishes an object the Rust object
/// model owns behind an engine anchor; the plain host value has no anchor to
/// carry, so it refuses such an object rather than reporting an empty value.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AnswerError {
    /// The answer is an object with no engine handle.
    #[error(
        "the script result is an object with no engine handle; it cannot cross the VM boundary"
    )]
    ObjectWithoutHandle,
    /// The engine refused to retain an object answer.
    #[error("the script result could not be retained: {0}")]
    Retain(String),
}

/// A script failure and the frames the code passed before it surfaced.
///
/// The trace is innermost first. A failure raised before any code ran — a
/// compile, a block build, or the global object's own refusal — has an empty
/// trace.
#[derive(Debug)]
pub struct ScriptFailure {
    /// The executor failure.
    pub error: Box<EngineError>,
    /// The frames the code passed, innermost first.
    pub trace: Vec<FrameTrace>,
    /// The label of the block that failed.
    name: String,
    /// The source line the failure names, before the caller's offset.
    line: Option<usize>,
    /// The line-number offset the caller gave the block.
    lineofs: i32,
}

impl ScriptFailure {
    /// A failure raised before the block ran.
    fn before_run(
        error: EngineError,
        name: &str,
        source: &str,
        lineofs: i32,
        line: Option<usize>,
    ) -> Self {
        let line = line.or_else(|| match &error {
            EngineError::Compile(parse) => line_at(source, parse.position()),
            _ => None,
        });
        Self {
            error: Box::new(error),
            trace: Vec::new(),
            name: name.to_string(),
            line,
            lineofs,
        }
    }

    /// The source line the failure names, with the caller's offset applied.
    ///
    /// `None` when the failure names no place in the text, which is the case
    /// for a block that was never compiled.
    pub fn line(&self) -> Option<usize> {
        let line = self.line?;
        let shifted = i64::try_from(line).ok()? + i64::from(self.lineofs);
        usize::try_from(shifted.max(1)).ok()
    }

    /// The failure's own message, without the block and line prefix.
    pub fn message(&self) -> String {
        match self.error.as_ref() {
            EngineError::Compile(error) => error.message().to_string(),
            EngineError::Run(unwind) if matches!(unwind.fault(), Fault::Raised(_)) => {
                throw::message_of(unwind)
            }
            EngineError::Run(_) => String::from("script exception"),
            EngineError::Build(error) => error.to_string(),
            EngineError::Object(error) => error.to_string(),
            EngineError::Global(error) => error.clone(),
            EngineError::Context(error) => error.clone(),
        }
    }

    /// The failure as a script diagnostic names it: `block(line): message`.
    pub fn diagnostic(&self) -> String {
        match self.line() {
            Some(line) => format!("{}({line}): {}", self.name, self.message()),
            None => self.message(),
        }
    }
}

/// How one script entry runs its top-level context.
///
/// The defaults run against the engine's global object, at the text's own
/// lines, and treat an expression's value as required. A caller that supplies
/// a context makes that object the top-level receiver; a caller that does not
/// need an expression's value compiles it in a statement context.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScriptOptions {
    /// The script's `this`, or no context for the global object.
    pub context: Option<Tjs2ValueId>,
    /// The line-number offset the caller reports.
    pub lineofs: i32,
    /// Whether an expression's value is required.
    pub result_needed: bool,
}

/// The host value a runtime answer crosses as.
///
/// Scalars cross as themselves. An object crosses as the engine's object
/// marker when it stands for an engine object (or is `null`); an object the
/// Rust object model owns has no engine handle in this plain form, so the
/// answer is refused rather than reported as an empty value.
///
/// # Errors
///
/// [`AnswerError::ObjectWithoutHandle`] for an object the Rust object model
/// owns.
pub fn host_value(answer: Variant) -> Result<TjsValue, AnswerError> {
    match answer {
        Variant::Void => Ok(TjsValue::Void),
        Variant::Integer(value) => Ok(TjsValue::Integer(value)),
        Variant::Real(value) => Ok(TjsValue::Real(value)),
        Variant::String(text) => Ok(TjsValue::String(text.to_string())),
        Variant::Octet(octet) => Ok(TjsValue::Octet(octet.as_slice().to_vec())),
        Variant::Object(closure) => {
            engine_id(&closure)?;
            Ok(TjsValue::Object)
        }
    }
}

/// The retained value a runtime answer crosses as.
///
/// Scalars cross as themselves. An object crosses as a retention of its engine
/// handle, and `null` crosses as a retention of `null`. An object the Rust
/// object model owns is published as an engine object whose store keeps the
/// object itself, so the answer keeps working after the call that made it.
///
/// # Errors
///
/// [`AnswerError::Retain`] when the engine refuses the retention or the
/// object's members cannot be published.
pub fn retained_value(engine: &Tjs2Engine, answer: Variant) -> Result<RetainedValue, AnswerError> {
    retained_value_with_members(engine, answer, &global_members())
}

/// The retained value a runtime answer crosses as, in `globals`'s scope.
///
/// This is [`retained_value`] with the scope that keeps the object a
/// Rust-owned answer stands for.
///
/// # Errors
///
/// [`AnswerError::Retain`] when the engine refuses the retention or the
/// object's members cannot be published.
pub fn retained_value_with_members(
    engine: &Tjs2Engine,
    answer: Variant,
    globals: &SharedGlobals,
) -> Result<RetainedValue, AnswerError> {
    match answer {
        Variant::Void => Ok(RetainedValue::Value(TjsValue::Void)),
        Variant::Integer(value) => Ok(RetainedValue::Value(TjsValue::Integer(value))),
        Variant::Real(value) => Ok(RetainedValue::Value(TjsValue::Real(value))),
        Variant::String(text) => Ok(RetainedValue::Value(TjsValue::String(text.to_string()))),
        Variant::Octet(octet) => Ok(RetainedValue::Value(TjsValue::Octet(
            octet.as_slice().to_vec(),
        ))),
        Variant::Object(closure) => match &closure.object {
            None => engine
                .retain_null()
                .map(RetainedValue::Object)
                .map_err(AnswerError::Retain),
            Some(object) => match object.engine_id() {
                Some(id) => engine
                    .duplicate_retained(id)
                    .map(RetainedValue::Object)
                    .map_err(AnswerError::Retain),
                None => crate::vm::runtime::bridge::retain_host_object(engine, object, globals)
                    .map(RetainedValue::Object)
                    .map_err(|error| AnswerError::Retain(error.to_string())),
            },
        },
    }
}

/// The engine id a variant's object carries, if it carries one.
///
/// `None` is the null object. An object the Rust object model owns has no
/// engine handle, and the plain host value cannot carry one.
fn engine_id(
    closure: &crate::vm::runtime::variant::Closure,
) -> Result<Option<Tjs2ValueId>, AnswerError> {
    match &closure.object {
        None => Ok(None),
        Some(object) => object
            .engine_id()
            .map(Some)
            .ok_or(AnswerError::ObjectWithoutHandle),
    }
}

/// Compiles `script` and runs its top-level context.
///
/// `name` labels the block, as it does in [`compile`]. The answer is what the
/// top-level code stored as its result: the empty variant for a script whose
/// top code falls off its end, and the value of a top-level `return`.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; no
/// step substitutes a value for a failure.
pub fn run(engine: Rc<Tjs2Engine>, script: &str, name: &str) -> Result<Variant, EngineError> {
    run_with_members(engine, script, name, global_members()).map_err(|failure| *failure.error)
}

/// Compiles `script` and runs its top-level context with a shared global.
///
/// The global is the handle the machine uses and the receiver a top-level
/// context runs with. The members the engine cannot hold are kept in `globals`,
/// so a top-level function outlives the call that declared it.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; a
/// code failure carries the frames it passed.
pub fn run_with_members(
    engine: Rc<Tjs2Engine>,
    script: &str,
    name: &str,
    globals: SharedGlobals,
) -> Result<Variant, ScriptFailure> {
    run_with_members_options(engine, script, name, globals, ScriptOptions::default())
}

/// Compiles `script` and runs it with the given global and options.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; a
/// code failure carries the frames it passed.
pub fn run_with_members_options(
    engine: Rc<Tjs2Engine>,
    script: &str,
    name: &str,
    globals: SharedGlobals,
    options: ScriptOptions,
) -> Result<Variant, ScriptFailure> {
    let image = compile(script, name).map_err(|error| {
        ScriptFailure::before_run(error.into(), name, script, options.lineofs, None)
    })?;
    let block = Block::build(&image).map_err(|error| {
        ScriptFailure::before_run(error.into(), name, script, options.lineofs, None)
    })?;
    run_block(engine, block, globals, name, script, options)
}

/// Compiles `expression` and runs its top-level context.
///
/// `name` labels the block, as it does in [`compile`]. The answer is the value
/// the expression produced.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; no
/// step substitutes a value for a failure.
pub fn eval(engine: Rc<Tjs2Engine>, expression: &str, name: &str) -> Result<Variant, EngineError> {
    eval_with_members(engine, expression, name, global_members()).map_err(|failure| *failure.error)
}

/// Compiles `expression` and runs it with a shared global.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; a
/// code failure carries the frames it passed.
pub fn eval_with_members(
    engine: Rc<Tjs2Engine>,
    expression: &str,
    name: &str,
    globals: SharedGlobals,
) -> Result<Variant, ScriptFailure> {
    let options = ScriptOptions {
        result_needed: true,
        ..ScriptOptions::default()
    };
    eval_with_members_options(engine, expression, name, globals, options)
}

/// Compiles `expression` and runs it with the given global and options.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; a
/// code failure carries the frames it passed.
pub fn eval_with_members_options(
    engine: Rc<Tjs2Engine>,
    expression: &str,
    name: &str,
    globals: SharedGlobals,
    options: ScriptOptions,
) -> Result<Variant, ScriptFailure> {
    let image =
        compile_expression_result(expression, name, options.result_needed).map_err(|error| {
            ScriptFailure::before_run(error.into(), name, expression, options.lineofs, None)
        })?;
    let block = Block::build(&image).map_err(|error| {
        ScriptFailure::before_run(error.into(), name, expression, options.lineofs, None)
    })?;
    run_block(engine, block, globals, name, expression, options)
}

/// Runs a built block's top-level context against the scope's global object.
fn run_block(
    engine: Rc<Tjs2Engine>,
    block: Block,
    globals: SharedGlobals,
    name: &str,
    source: &str,
    options: ScriptOptions,
) -> Result<Variant, ScriptFailure> {
    let global = match scope_global(&globals) {
        Some(global) => global,
        None => {
            let value = engine.global_object().map_err(|error| {
                ScriptFailure::before_run(
                    EngineError::Global(error),
                    name,
                    source,
                    options.lineofs,
                    None,
                )
            })?;
            global_object(&engine, &globals, value).map_err(|error| {
                ScriptFailure::before_run(
                    EngineError::Object(error),
                    name,
                    source,
                    options.lineofs,
                    None,
                )
            })?
        }
    };
    let global: Rc<dyn Dispatch> = global;
    let top_level = block
        .top_level()
        .ok_or(BuildError::TopLevel { index: -1 })
        .map_err(|error| {
            ScriptFailure::before_run(
                EngineError::Build(error),
                name,
                source,
                options.lineofs,
                None,
            )
        })?;
    let context = top_level.object().clone();
    let receiver = match options.context {
        None => None,
        Some(id) if id.is_null() => None,
        Some(id) => {
            let retained = engine.retain_object_detached(id).map_err(|error| {
                ScriptFailure::before_run(
                    EngineError::Context(error),
                    name,
                    source,
                    options.lineofs,
                    None,
                )
            })?;
            let store = crate::vm::runtime::bridge::context_members(&globals, id);
            let handle =
                CHandle::with_context(Rc::clone(&engine), retained, store).map_err(|error| {
                    ScriptFailure::before_run(
                        EngineError::Object(error),
                        name,
                        source,
                        options.lineofs,
                        None,
                    )
                })?;
            Some(Rc::new(handle) as Rc<dyn Dispatch>)
        }
    };
    let mut machine = Machine::with_global(global);
    // A top-level context handed no receiver runs with the machine's global
    // object, which is the scope a script's own body runs in.
    match machine.run_context_traced(&context, receiver, &[]) {
        Ok(answer) => Ok(answer),
        Err(unwind) => {
            let trace = unwind.trace().to_vec();
            let line = failure_line(&block, source, &trace);
            Err(ScriptFailure {
                error: Box::new(EngineError::Run(unwind)),
                trace,
                name: name.to_string(),
                line,
                lineofs: options.lineofs,
            })
        }
    }
}

/// The source line a failure names, when its innermost frame can be placed in
/// the block.
fn failure_line(block: &Block, source: &str, trace: &[FrameTrace]) -> Option<usize> {
    let frame = trace.first()?;
    let index = block
        .contexts()
        .iter()
        .position(|record| record.object().name().to_string() == frame.name)
        .or_else(|| block.top_level_index())?;
    let position = block.source_position(index, frame.position)?;
    line_at(source, position)
}

/// The one-based source line that holds `position`, in UTF-16 code units.
fn line_at(source: &str, position: i32) -> Option<usize> {
    let position = usize::try_from(position).ok()?;
    Some(
        1 + source
            .encode_utf16()
            .take(position)
            .filter(|unit| *unit == u16::from(b'\n'))
            .count(),
    )
}
